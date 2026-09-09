//! Channels and the operator rules.
//!
//! These rules *are* the product on a warnet, so they are written down and tested rather
//! than left as whatever the handler code happens to do. See `docs/WARNET.md` §3.
//!
//! For a **federated** channel this type is a local mirror; the hub owns the roster and
//! assigns a monotonic sequence number to every event, so "who got operator first" has
//! exactly one answer on every node. For a **local** channel this type is the authority.

use std::collections::BTreeSet;

use bnetcc_proto::chat::{channel_flags, user_flags};

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
    /// The user's statstring, echoed in `EID_SHOWUSER`/`EID_JOIN` so other clients can
    /// render their product and icon. Opaque to the server — see
    /// [`bnetcc_proto::statstring`].
    pub statstring: Vec<u8>,
}

/// Why a join was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinDenial {
    /// Channel is at capacity — `EID_CHANNELFULL`.
    Full,
    /// Account is banned from this channel — `EID_CHANNELRESTRICTED`.
    Banned,
    /// Entry is restricted (e.g. a flag-gated channel the user lacks the flag for) —
    /// `EID_CHANNELRESTRICTED`.
    Restricted,
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
    /// How operator is auto-granted on join. Defaults from the class, but the node
    /// overrides it per channel-name convention at creation — see [`Channel::set_op_grant`]
    /// and [`op_grant_for_name`].
    op_grant: OpGrant,
    /// Whether this channel survives becoming empty, beyond what its class dictates. Set
    /// for operator-defined persistent channels — see [`Channel::set_persist`].
    persist: bool,
}

impl Channel {
    /// Create a channel. `name` should already be normalised with
    /// [`bnetcc_proto::chat::normalize_channel_name`].
    #[must_use]
    pub fn new(name: Vec<u8>, display: String, class: ChannelClass, max_users: usize) -> Self {
        let flags = match class {
            ChannelClass::Official => channel_flags::PUBLIC | channel_flags::SYSTEM,
            _ => channel_flags::PUBLIC,
        };
        let op_grant = if class.grants_first_operator() {
            OpGrant::FirstArrival
        } else {
            OpGrant::None
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
            op_grant,
            persist: false,
        }
    }

    /// Set how operator is auto-granted. The node calls this at creation based on the
    /// channel-name convention and config; see [`op_grant_for_name`].
    pub fn set_op_grant(&mut self, op_grant: OpGrant) {
        self.op_grant = op_grant;
    }

    /// Set whether the channel survives becoming empty (a defined persistent channel).
    pub fn set_persist(&mut self, persist: bool) {
        self.persist = persist;
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
    pub fn join(
        &mut self,
        account: AccountId,
        name: String,
        base_flags: u32,
        statstring: Vec<u8>,
    ) -> Result<JoinOutcome, JoinDenial> {
        if self.bans.contains(&account) {
            return Err(JoinDenial::Banned);
        }
        if self.position(account).is_some() {
            return Err(JoinDenial::AlreadyPresent);
        }
        if self.members.len() >= self.max_users {
            return Err(JoinDenial::Full);
        }

        let granted_operator = self.operator.is_none()
            && match &self.op_grant {
                OpGrant::None => false,
                OpGrant::FirstArrival => self.members.is_empty(),
                // "Op <name>" / "Clan <name>": only the named account is auto-opped, and
                // whenever they arrive — not merely whoever shows up first.
                OpGrant::NameMatch(op_name) => name.eq_ignore_ascii_case(op_name),
            };
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
            statstring,
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
            should_destroy: self.members.is_empty()
                && !self.class.persists_when_empty()
                && !self.persist,
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

/// How a channel name is classified for operator-grant and access rules. Battle.net
/// channel names follow conventions the server reads to decide behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// An operator or clan channel (`Op <name>`, `Clan <tag>`). Members who belong are
    /// auto-opped; these are staff/clan spaces.
    OpOrClan,
    /// A `Public *` channel — a shared public space where operator is never auto-granted.
    Public,
    /// A private, user-created named channel — auto-op is a per-node config choice.
    Private,
}

/// Classify a channel by its display name (case-insensitive). ASCII-only prefixes, since
/// the conventions (`Op `, `Clan `, `Public `) are ASCII.
#[must_use]
pub fn classify_name(display: &str) -> NameKind {
    let lower = display.trim().to_ascii_lowercase();
    if lower.starts_with("op ") || lower.starts_with("clan ") {
        NameKind::OpOrClan
    } else if lower.starts_with("public") {
        NameKind::Public
    } else {
        NameKind::Private
    }
}

/// How a channel auto-grants operator on join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpGrant {
    /// No auto-grant; operator is only ever assigned explicitly.
    None,
    /// The first arrival to the empty channel is granted operator.
    FirstArrival,
    /// Only the account whose name matches (case-insensitively) is granted operator, and
    /// whenever they join. Used by `Op <name>` / `Clan <name>` channels.
    NameMatch(String),
}

/// The operator-grant policy for a channel of this name.
///
/// - `Op <name>` / `Clan <name>`: [`OpGrant::NameMatch`] on `<name>` — only that account
///   is auto-opped (e.g. "Op Fish" ops only "Fish"). An `Op`/`Clan` with no name grants
///   to nobody.
/// - `Public *`: [`OpGrant::None`].
/// - Private: first arrival if `auto_op_private`, else nobody.
#[must_use]
pub fn op_grant_for_name(display: &str, auto_op_private: bool) -> OpGrant {
    match classify_name(display) {
        NameKind::OpOrClan => match designated_op_name(display) {
            Some(name) => OpGrant::NameMatch(name),
            None => OpGrant::None,
        },
        NameKind::Public => OpGrant::None,
        NameKind::Private => {
            if auto_op_private {
                OpGrant::FirstArrival
            } else {
                OpGrant::None
            }
        }
    }
}

/// The `<name>` in `Op <name>` / `Clan <name>`, preserving its original case. `None` if
/// there is no name after the prefix.
#[must_use]
pub fn designated_op_name(display: &str) -> Option<String> {
    let trimmed = display.trim();
    for prefix in ["op ", "clan "] {
        if trimmed.len() > prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let name = trimmed[prefix.len()..].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Whether a telnet / chat-gateway user may enter a channel by this name.
///
/// Gateway users are confined to `Public *` channels plus an operator-configured
/// allowlist (e.g. "Open Tech Support"); they must never reach arbitrary private or
/// Op/Clan channels. `extra` is that allowlist, matched case-insensitively. Enforce this
/// at the gateway's channel-join command once that is implemented.
#[must_use]
pub fn gateway_may_join(display: &str, extra: &[String]) -> bool {
    if classify_name(display) == NameKind::Public {
        return true;
    }
    let lower = display.trim().to_ascii_lowercase();
    extra.iter().any(|c| c.trim().to_ascii_lowercase() == lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(class: ChannelClass) -> Channel {
        Channel::new(b"op clan xyz".to_vec(), "Op Clan xyz".into(), class, 40)
    }

    fn join(c: &mut Channel, id: AccountId) -> Result<JoinOutcome, JoinDenial> {
        c.join(id, format!("user{id}"), 0, Vec::new())
    }

    #[test]
    fn channel_names_classify_by_convention() {
        assert_eq!(classify_name("Op Warlord"), NameKind::OpOrClan);
        assert_eq!(classify_name("Clan xYz"), NameKind::OpOrClan);
        assert_eq!(classify_name("Public Chat"), NameKind::Public);
        assert_eq!(classify_name("Public"), NameKind::Public);
        assert_eq!(classify_name("Brood War USA-1"), NameKind::Private);
        assert_eq!(classify_name("my hangout"), NameKind::Private);
    }

    #[test]
    fn operator_grant_follows_the_name_rules() {
        // Op/Clan grant to the *named* account only, regardless of the private toggle.
        assert_eq!(op_grant_for_name("Op Warlord", false), OpGrant::NameMatch("Warlord".into()));
        assert_eq!(op_grant_for_name("Clan xYz", false), OpGrant::NameMatch("xYz".into()));
        // An Op/Clan channel with no name grants to nobody.
        assert_eq!(op_grant_for_name("Op", false), OpGrant::None);
        // Public never auto-ops, even with the toggle on.
        assert_eq!(op_grant_for_name("Public Chat", true), OpGrant::None);
        // Private follows the toggle.
        assert_eq!(op_grant_for_name("my hangout", false), OpGrant::None);
        assert_eq!(op_grant_for_name("my hangout", true), OpGrant::FirstArrival);
    }

    #[test]
    fn op_channel_ops_only_the_matching_name() {
        // "Op Fish": whoever arrives first is NOT opped unless they are Fish.
        let mut c = Channel::new(b"op fish".to_vec(), "Op Fish".into(), ChannelClass::Local, 40);
        c.set_op_grant(op_grant_for_name("Op Fish", false));
        let intruder = c.join(1, "Grunt".into(), 0, Vec::new()).unwrap();
        assert!(!intruder.granted_operator, "a non-matching user must not be opped");
        let fish = c.join(2, "fish".into(), 0, Vec::new()).unwrap();
        assert!(fish.granted_operator, "the named account is opped whenever it joins");
        assert!(c.is_operator(2));
    }

    #[test]
    fn telnet_users_are_confined_to_public_and_the_allowlist() {
        let extra = vec!["Open Tech Support".to_string()];
        assert!(gateway_may_join("Public Chat", &extra), "public is allowed");
        assert!(gateway_may_join("open tech support", &extra), "allowlist, case-insensitive");
        assert!(!gateway_may_join("Op Warlord", &extra), "op channels are off-limits");
        assert!(!gateway_may_join("Clan xYz", &extra), "clan channels are off-limits");
        assert!(!gateway_may_join("someones private room", &extra), "private is off-limits");
    }

    #[test]
    fn a_channel_with_auto_op_off_never_grants_operator() {
        let mut c = Channel::new(b"public chat".to_vec(), "Public Chat".into(), ChannelClass::Local, 40);
        c.set_op_grant(OpGrant::None);
        let out = join(&mut c, 1).unwrap();
        assert!(!out.granted_operator, "public channels must not auto-op");
        assert_eq!(out.flags & user_flags::OPERATOR, 0);
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
                        let _ = c.join(a, format!("u{a}"), 0, Vec::new());
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
