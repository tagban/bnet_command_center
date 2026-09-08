//! Channels and the operator rules.
//!
//! These rules *are* the product on a warnet, so they are written down and tested rather
//! than left as whatever the handler code happens to do. See `docs/WARNET.md` §3.
//!
//! For a **federated** channel this type is a local mirror; the hub owns the roster and
//! assigns a monotonic sequence number to every event, so "who got operator first" has
//! exactly one answer on every node. For a **local** channel this type is the authority.

use std::collections::BTreeSet;

use cairn_proto::chat::{channel_flags, user_flags};

/// Stable account identifier. Hub-assigned in a federation.
pub type AccountId = u64;

/// Who owns a channel's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelClass {
    /// Exists on one node only; that node is the authority.
    Local,
    /// Spans the federation; the hub is the authority and the sequencer.
    Federated,
    /// Hub-owned, auto-joined, cannot be deleted by an operator.
    Official,
}

impl ChannelClass {
    /// Whether the first user to enter is granted operator.
    ///
    /// Official channels never auto-grant: they are server furniture, and handing
    /// operator to whoever reconnects first after a restart is how "official" channels
    /// get taken over.
    #[must_use]
    pub const fn grants_first_operator(self) -> bool {
        matches!(self, Self::Local | Self::Federated)
    }

    /// Whether the channel survives becoming empty.
    #[must_use]
    pub const fn persists_when_empty(self) -> bool {
        matches!(self, Self::Official)
    }
}

/// A user present in a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Account identifier.
    pub account: AccountId,
    /// Display name as shown in chat events.
    pub name: String,
    /// Chat flags (operator, squelched, no-UDP, …).
    pub flags: u32,
}

/// Why a join was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinDenial {
    /// Channel is at capacity — `EID_CHANNELFULL`.
    Full,
    /// Account is banned from this channel — `EID_CHANNELRESTRICTED`.
    Banned,
    /// Already present.
    AlreadyPresent,
}

/// Result of a successful join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinOutcome {
    /// Whether this user was granted operator on entry.
    pub granted_operator: bool,
    /// The user's resulting flags.
    pub flags: u32,
}

/// Result of a departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaveOutcome {
    /// Set when operator passed to a designated heir.
    pub new_operator: Option<AccountId>,
    /// Whether the channel is now empty.
    pub now_empty: bool,
    /// Whether the caller should destroy the channel.
    pub should_destroy: bool,
}

/// Why an operator action was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpError {
    /// The actor is not the channel operator.
    NotOperator,
    /// The target is not in the channel.
    NotPresent,
    /// The action would target the operator themselves.
    CannotTargetSelf,
}

/// A chat channel.
#[derive(Debug, Clone)]
pub struct Channel {
    name: Vec<u8>,
    display: String,
    class: ChannelClass,
    flags: u32,
    max_users: usize,
    members: Vec<Member>,
    operator: Option<AccountId>,
    heirs: Vec<AccountId>,
    bans: BTreeSet<AccountId>,
    seq: u64,
}

impl Channel {
    /// Create a channel. `name` should already be normalised with
    /// [`cairn_proto::chat::normalize_channel_name`].
    #[must_use]
    pub fn new(name: Vec<u8>, display: String, class: ChannelClass, max_users: usize) -> Self {
        let flags = match class {
            ChannelClass::Official => channel_flags::PUBLIC | channel_flags::SYSTEM,
            _ => channel_flags::PUBLIC,
        };
        Self {
            name,
            display,
            class,
            flags,
            max_users,
            members: Vec::new(),
            operator: None,
            heirs: Vec::new(),
            bans: BTreeSet::new(),
            seq: 0,
        }
    }

    /// Normalised lookup name.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Display name for `EID_CHANNEL`.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Channel class.
    #[must_use]
    pub const fn class(&self) -> ChannelClass {
        self.class
    }

    /// Channel flags.
    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    /// Current members, in join order.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the channel is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The current operator, if any.
    #[must_use]
    pub const fn operator(&self) -> Option<AccountId> {
        self.operator
    }

    /// Whether an account holds operator.
    #[must_use]
    pub fn is_operator(&self, account: AccountId) -> bool {
        self.operator == Some(account)
    }

    /// The event sequence number most recently applied.
    ///
    /// For a federated channel this is the hub's number, which is what makes ordering
    /// identical on every node. For a local channel it is this node's own counter.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn position(&self, account: AccountId) -> Option<usize> {
        self.members.iter().position(|m| m.account == account)
    }

    /// Add a user to the channel.
    ///
    /// The first occupant of a local or federated channel receives operator. **Rejoining
    /// never restores operator** — a kicked or departed operator returns as a normal
    /// user, which is what stops op-cycling from being a war tactic.
    ///
    /// # Errors
    ///
    /// [`JoinDenial`] when the channel is full, the account is banned, or it is already
    /// present.
    pub fn join(&mut self, account: AccountId, name: String, base_flags: u32) -> Result<JoinOutcome, JoinDenial> {
        if self.bans.contains(&account) {
            return Err(JoinDenial::Banned);
        }
        if self.position(account).is_some() {
            return Err(JoinDenial::AlreadyPresent);
        }
        if self.members.len() >= self.max_users {
            return Err(JoinDenial::Full);
        }

        let granted_operator =
            self.members.is_empty() && self.operator.is_none() && self.class.grants_first_operator();
        let flags = if granted_operator {
            base_flags | user_flags::OPERATOR
        } else {
            base_flags & !user_flags::OPERATOR
        };
        if granted_operator {
            self.operator = Some(account);
        }
        self.members.push(Member {
            account,
            name,
            flags,
        });
        self.bump();
        Ok(JoinOutcome {
            granted_operator,
            flags,
        })
    }

    /// Remove a user.
    ///
    /// If the departing user held operator, it passes to the first designated heir who
    /// is **still present**; otherwise the channel is left with no operator. Operator
    /// does not drift to the longest-present user, because that would make leaving a way
    /// to hand operator to a rival.
    pub fn leave(&mut self, account: AccountId) -> LeaveOutcome {
        let Some(idx) = self.position(account) else {
            return LeaveOutcome::default();
        };
        self.members.remove(idx);
        self.heirs.retain(|&h| h != account);
        self.bump();

        let mut out = LeaveOutcome {
            now_empty: self.members.is_empty(),
            should_destroy: self.members.is_empty() && !self.class.persists_when_empty(),
            ..LeaveOutcome::default()
        };

        if self.operator == Some(account) {
            self.operator = None;
            let heir = self
                .heirs
                .iter()
                .copied()
                .find(|&h| self.position(h).is_some());
            if let Some(h) = heir {
                self.operator = Some(h);
                if let Some(pos) = self.position(h) {
                    self.members[pos].flags |= user_flags::OPERATOR;
                }
                self.heirs.retain(|&x| x != h);
                out.new_operator = Some(h);
            }
        }
        out
    }

    /// Nominate an heir. The heir inherits only if present when the operator leaves.
    ///
    /// # Errors
    ///
    /// [`OpError`] if the actor is not the operator, the target is absent, or the
    /// operator designated themselves.
    pub fn designate(&mut self, by: AccountId, heir: AccountId) -> Result<(), OpError> {
        if !self.is_operator(by) {
            return Err(OpError::NotOperator);
        }
        if by == heir {
            return Err(OpError::CannotTargetSelf);
        }
        if self.position(heir).is_none() {
            return Err(OpError::NotPresent);
        }
        self.heirs.retain(|&h| h != heir);
        self.heirs.push(heir);
        self.bump();
        Ok(())
    }

    /// Remove a user from the channel without banning them.
    ///
    /// # Errors
    ///
    /// [`OpError`] if the actor is not the operator or the target is absent.
    pub fn kick(&mut self, by: AccountId, target: AccountId) -> Result<LeaveOutcome, OpError> {
        if !self.is_operator(by) {
            return Err(OpError::NotOperator);
        }
        if by == target {
            return Err(OpError::CannotTargetSelf);
        }
        if self.position(target).is_none() {
            return Err(OpError::NotPresent);
        }
        Ok(self.leave(target))
    }

    /// Ban a user from the channel and remove them.
    ///
    /// # Errors
    ///
    /// [`OpError`] if the actor is not the operator.
    pub fn ban(&mut self, by: AccountId, target: AccountId) -> Result<LeaveOutcome, OpError> {
        if !self.is_operator(by) {
            return Err(OpError::NotOperator);
        }
        if by == target {
            return Err(OpError::CannotTargetSelf);
        }
        self.bans.insert(target);
        Ok(self.leave(target))
    }

    /// Lift a channel ban.
    ///
    /// # Errors
    ///
    /// [`OpError::NotOperator`] if the actor does not hold operator.
    pub fn unban(&mut self, by: AccountId, target: AccountId) -> Result<bool, OpError> {
        if !self.is_operator(by) {
            return Err(OpError::NotOperator);
        }
        Ok(self.bans.remove(&target))
    }

    /// Whether an account is banned here.
    #[must_use]
    pub fn is_banned(&self, account: AccountId) -> bool {
        self.bans.contains(&account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(class: ChannelClass) -> Channel {
        Channel::new(b"op clan xyz".to_vec(), "Op Clan xyz".into(), class, 40)
    }

    fn join(c: &mut Channel, id: AccountId) -> Result<JoinOutcome, JoinDenial> {
        c.join(id, format!("user{id}"), 0)
    }

    #[test]
    fn first_occupant_gets_operator() {
        let mut c = chan(ChannelClass::Local);
        let out = join(&mut c, 1).unwrap();
        assert!(out.granted_operator);
        assert_eq!(out.flags & user_flags::OPERATOR, user_flags::OPERATOR);
        assert!(c.is_operator(1));
    }

    #[test]
    fn second_occupant_does_not() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        let out = join(&mut c, 2).unwrap();
        assert!(!out.granted_operator);
        assert_eq!(out.flags & user_flags::OPERATOR, 0);
    }

    #[test]
    fn official_channels_never_auto_grant_operator() {
        // Otherwise whoever reconnects first after a restart owns the official channel.
        let mut c = chan(ChannelClass::Official);
        assert!(!join(&mut c, 1).unwrap().granted_operator);
        assert_eq!(c.operator(), None);
    }

    #[test]
    fn operator_does_not_transfer_without_an_heir() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        let out = c.leave(1);
        assert_eq!(out.new_operator, None);
        assert_eq!(c.operator(), None, "operator must not drift to the next user");
    }

    #[test]
    fn operator_transfers_to_a_present_heir() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        c.designate(1, 2).unwrap();
        let out = c.leave(1);
        assert_eq!(out.new_operator, Some(2));
        assert!(c.is_operator(2));
        assert_eq!(
            c.members()[0].flags & user_flags::OPERATOR,
            user_flags::OPERATOR,
            "the heir's flags must be updated too"
        );
    }

    #[test]
    fn designation_lapses_if_the_heir_left_first() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        join(&mut c, 3).unwrap();
        c.designate(1, 2).unwrap();
        c.leave(2);
        let out = c.leave(1);
        assert_eq!(out.new_operator, None);
        assert_eq!(c.operator(), None);
    }

    #[test]
    fn later_heirs_inherit_when_earlier_ones_are_gone() {
        let mut c = chan(ChannelClass::Local);
        for id in 1..=3 {
            join(&mut c, id).unwrap();
        }
        c.designate(1, 2).unwrap();
        c.designate(1, 3).unwrap();
        c.leave(2);
        assert_eq!(c.leave(1).new_operator, Some(3));
    }

    #[test]
    fn rejoining_does_not_restore_operator() {
        // Otherwise op-cycling becomes a war tactic.
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        c.leave(1);
        let out = join(&mut c, 1).unwrap();
        assert!(!out.granted_operator);
        assert_eq!(c.operator(), None);
    }

    #[test]
    fn an_empty_channel_grants_operator_to_the_next_arrival() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        let out = c.leave(1);
        assert!(out.now_empty);
        assert!(out.should_destroy);
        // In practice the caller destroys it here; if it is kept, the invariant holds.
        assert!(join(&mut c, 2).unwrap().granted_operator);
    }

    #[test]
    fn official_channels_survive_becoming_empty() {
        let mut c = chan(ChannelClass::Official);
        join(&mut c, 1).unwrap();
        let out = c.leave(1);
        assert!(out.now_empty);
        assert!(!out.should_destroy);
    }

    #[test]
    fn non_operators_cannot_designate_kick_or_ban() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        assert_eq!(c.designate(2, 1), Err(OpError::NotOperator));
        assert_eq!(c.kick(2, 1), Err(OpError::NotOperator));
        assert_eq!(c.ban(2, 1), Err(OpError::NotOperator));
        assert_eq!(c.unban(2, 1), Err(OpError::NotOperator));
    }

    #[test]
    fn operators_cannot_target_themselves() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        assert_eq!(c.designate(1, 1), Err(OpError::CannotTargetSelf));
        assert_eq!(c.kick(1, 1), Err(OpError::CannotTargetSelf));
        assert_eq!(c.ban(1, 1), Err(OpError::CannotTargetSelf));
    }

    #[test]
    fn designating_an_absent_user_fails() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        assert_eq!(c.designate(1, 99), Err(OpError::NotPresent));
    }

    #[test]
    fn banning_removes_and_blocks_rejoin() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        c.ban(1, 2).unwrap();
        assert_eq!(c.len(), 1);
        assert!(c.is_banned(2));
        assert_eq!(join(&mut c, 2), Err(JoinDenial::Banned));
        c.unban(1, 2).unwrap();
        assert!(join(&mut c, 2).is_ok());
    }

    #[test]
    fn kicking_does_not_ban() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        c.kick(1, 2).unwrap();
        assert!(!c.is_banned(2));
        assert!(join(&mut c, 2).is_ok());
    }

    #[test]
    fn capacity_is_enforced() {
        let mut c = Channel::new(b"small".to_vec(), "Small".into(), ChannelClass::Local, 2);
        join(&mut c, 1).unwrap();
        join(&mut c, 2).unwrap();
        assert_eq!(join(&mut c, 3), Err(JoinDenial::Full));
    }

    #[test]
    fn double_join_is_rejected() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        assert_eq!(join(&mut c, 1), Err(JoinDenial::AlreadyPresent));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn leaving_a_channel_you_are_not_in_is_a_no_op() {
        let mut c = chan(ChannelClass::Local);
        join(&mut c, 1).unwrap();
        let out = c.leave(999);
        assert_eq!(out, LeaveOutcome::default());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn every_mutation_advances_the_sequence() {
        // Ordering across a federation is by this number and never by wall clock, so it
        // must move on every state change.
        let mut c = chan(ChannelClass::Local);
        let mut last = c.seq();
        for step in 0..4 {
            match step {
                0 => {
                    join(&mut c, 1).unwrap();
                }
                1 => {
                    join(&mut c, 2).unwrap();
                }
                2 => c.designate(1, 2).unwrap(),
                _ => {
                    c.leave(1);
                }
            }
            assert!(c.seq() > last, "step {step} did not advance the sequence");
            last = c.seq();
        }
    }

    /// The scenario that has to be unambiguous on a warnet: a contested channel where
    /// operator changes hands repeatedly. Whatever the outcome, it must be a pure
    /// function of the event order — which is what the hub sequencer guarantees.
    #[test]
    fn contested_channel_is_deterministic() {
        let replay = |ops: &[(u8, AccountId, AccountId)]| -> Option<AccountId> {
            let mut c = chan(ChannelClass::Federated);
            for &(op, a, b) in ops {
                match op {
                    0 => {
                        let _ = c.join(a, format!("u{a}"), 0);
                    }
                    1 => {
                        c.leave(a);
                    }
                    2 => {
                        let _ = c.designate(a, b);
                    }
                    _ => {
                        let _ = c.ban(a, b);
                    }
                }
            }
            c.operator()
        };
        let script = [
            (0, 1, 0),
            (0, 2, 0),
            (0, 3, 0),
            (2, 1, 3),
            (3, 1, 2),
            (1, 1, 0),
            (0, 4, 0),
            (2, 3, 4),
            (1, 3, 0),
        ];
        assert_eq!(replay(&script), Some(4));
        // Same script, replayed: identical result. Two nodes applying the hub's
        // sequence therefore agree.
        assert_eq!(replay(&script), replay(&script));
    }
}
