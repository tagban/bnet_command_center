//! Bridged identities — the presence model behind `docs/BRIDGES.md`.
//!
//! A bridge lets people join a Battle.net channel from Discord, a Ragnarok Online guild
//! chat, an FFXI addon, or anything else. The load-bearing decision is that a bridged
//! user is a **first-class channel presence**, not relayed text: it has a name, an account
//! id, chat flags and a roster entry, so whisper, squelch, kick, ban, federation and the
//! operator rules all work without knowing bridges exist.
//!
//! This module is the part that makes that safe: deriving a Battle.net name from a remote
//! one, reserving the namespace so bridged identity cannot be spoofed, keeping the mapping
//! stable across renames so moderation survives, and preventing two bridges in one channel
//! from echoing each other forever.
//!
//! No I/O and no transport. The WebSocket and line-protocol front ends sit on top.

use std::collections::HashMap;

use crate::channel::AccountId;

/// Separates a bridge tag from the remote name: `D:alice`.
///
/// **This character is reserved.** A real account may not contain it, which is what makes
/// a bridged name unforgeable — see [`is_bridged_name`] and
/// [`name_is_reserved_for_bridges`].
pub const SEPARATOR: char = ':';

/// Longest a Battle.net name may be, including the tag and separator.
pub const MAX_NAME: usize = 15;

/// Longest a bridge tag may be.
pub const MAX_TAG: usize = 3;

/// How many bridges a message may traverse before it is dropped as a loop.
///
/// Three is generous: Discord → Battle.net → RO is two. Anything deeper is a
/// misconfiguration, and the counter that records it is how you find out.
pub const MAX_HOPS: usize = 3;

/// Used when a remote display name has nothing usable left after sanitising.
///
/// A user called `🌸🌸🌸` still needs *a* name, and failing the join instead would be a
/// worse outcome than an ugly one.
pub const FALLBACK_STEM: &str = "user";

/// Why a bridge value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    /// The tag was empty or longer than [`MAX_TAG`].
    TagLength(usize),
    /// The tag contained something other than ASCII letters and digits.
    TagCharacters,
    /// The remote identity had no id.
    EmptyRemoteId,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TagLength(n) => {
                write!(f, "bridge tag is {n} characters; must be 1 to {MAX_TAG}")
            }
            Self::TagCharacters => write!(f, "bridge tag must be ASCII letters and digits"),
            Self::EmptyRemoteId => write!(f, "remote identity has no id"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// A bridge's short prefix, such as `D` for Discord or `RO` for Ragnarok Online.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BridgeTag(String);

impl BridgeTag {
    /// Validate and create a tag.
    ///
    /// # Errors
    ///
    /// [`BridgeError::TagLength`] or [`BridgeError::TagCharacters`].
    pub fn new(raw: &str) -> Result<Self, BridgeError> {
        if raw.is_empty() || raw.len() > MAX_TAG {
            return Err(BridgeError::TagLength(raw.len()));
        }
        if !raw.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(BridgeError::TagCharacters);
        }
        Ok(Self(raw.to_string()))
    }

    /// The tag as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Characters this tag consumes in a name, including the separator.
    #[must_use]
    pub fn prefix_len(&self) -> usize {
        self.0.len() + 1
    }
}

impl std::fmt::Display for BridgeTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a name belongs to the bridge namespace, and to which tag.
///
/// Used both to route and to **reserve**: account registration must refuse any name for
/// which this returns `Some`, or a real user could register `D:alice` and impersonate a
/// Discord user to everyone in the channel.
#[must_use]
pub fn is_bridged_name(name: &str) -> Option<BridgeTag> {
    let (tag, rest) = name.split_once(SEPARATOR)?;
    if rest.is_empty() {
        return None;
    }
    BridgeTag::new(tag).ok()
}

/// Whether a name may not be registered by a real account.
///
/// Deliberately stricter than [`is_bridged_name`]: **any** occurrence of the separator is
/// refused, not only well-formed ones. A name like `weird:` is not a valid bridged name,
/// but allowing it invites confusion about a namespace whose whole value is being
/// unambiguous.
#[must_use]
pub fn name_is_reserved_for_bridges(name: &str) -> bool {
    name.contains(SEPARATOR)
}

/// Reduce a remote display name to something a Battle.net client can carry.
///
/// Drops anything that is not ASCII alphanumeric or one of `-_[]`, since the remainder is
/// either unrepresentable in ISO 8859-1 or actively dangerous (a relayed control byte
/// makes the *receiving* client disconnect and IP-ban us for five minutes). Falls back to
/// [`FALLBACK_STEM`] when nothing survives.
#[must_use]
pub fn sanitize_remote_name(display: &str) -> String {
    let cleaned: String = display
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '[' | ']'))
        .collect();
    if cleaned.is_empty() {
        FALLBACK_STEM.to_string()
    } else {
        cleaned
    }
}

/// Build a candidate Battle.net name for a remote user.
///
/// `suffix` disambiguates collisions: `None` for the first holder, `Some(2)` for the
/// next, matching Battle.net's own `#2` convention for duplicate logins rather than
/// inventing a new one. The stem is truncated so the whole name fits [`MAX_NAME`].
#[must_use]
pub fn derive_name(tag: &BridgeTag, display: &str, suffix: Option<u32>) -> String {
    let stem = sanitize_remote_name(display);
    let suffix_str = suffix.map_or_else(String::new, |n| format!("#{n}"));
    let budget = MAX_NAME
        .saturating_sub(tag.prefix_len())
        .saturating_sub(suffix_str.len());

    let mut truncated = String::with_capacity(budget);
    for c in stem.chars().take(budget) {
        truncated.push(c);
    }
    if truncated.is_empty() {
        // The tag plus suffix already consumed the budget. Better a bare tag than a
        // panic; the tag alone still identifies the bridge.
        return format!("{tag}{SEPARATOR}{suffix_str}");
    }
    format!("{tag}{SEPARATOR}{truncated}{suffix_str}")
}

/// One bridged user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedUser {
    /// Stable account id. **Never changes**, even when the display name does — which is
    /// what makes a ban survive the user renaming themselves on the far side.
    pub account: AccountId,
    /// The id the remote platform uses. The registry's primary key.
    pub remote_id: String,
    /// Current Battle.net name.
    pub name: String,
}

/// What an upsert did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upsert {
    /// A new identity appeared.
    Added(BridgedUser),
    /// A known identity changed display name.
    Renamed {
        /// The user, with its new name.
        user: BridgedUser,
        /// The name it had before.
        previous: String,
    },
    /// Nothing changed.
    Unchanged(BridgedUser),
}

impl Upsert {
    /// The user, whatever happened.
    #[must_use]
    pub fn user(&self) -> &BridgedUser {
        match self {
            Self::Added(u) | Self::Unchanged(u) | Self::Renamed { user: u, .. } => u,
        }
    }
}

/// The identities one bridge is presenting.
#[derive(Debug, Clone)]
pub struct BridgeRegistry {
    tag: BridgeTag,
    by_remote: HashMap<String, BridgedUser>,
    /// Lowercased name to remote id, for collision checks and reverse lookup.
    by_name: HashMap<String, String>,
}

impl BridgeRegistry {
    /// A registry for one bridge.
    #[must_use]
    pub fn new(tag: BridgeTag) -> Self {
        Self {
            tag,
            by_remote: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    /// This bridge's tag.
    #[must_use]
    pub const fn tag(&self) -> &BridgeTag {
        &self.tag
    }

    /// How many identities are present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_remote.len()
    }

    /// Whether no identities are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_remote.is_empty()
    }

    /// Look up by the remote platform's id.
    #[must_use]
    pub fn by_remote_id(&self, remote_id: &str) -> Option<&BridgedUser> {
        self.by_remote.get(remote_id)
    }

    /// Look up by Battle.net name, case-insensitively.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&BridgedUser> {
        let remote = self.by_name.get(&name.to_ascii_lowercase())?;
        self.by_remote.get(remote)
    }

    /// Announce or update a remote identity.
    ///
    /// `account` is used only when the identity is new; an existing identity keeps the id
    /// it was given. That is the property moderation depends on — a ban targets the
    /// account, so it survives the user renaming themselves on the far side.
    ///
    /// # Errors
    ///
    /// [`BridgeError::EmptyRemoteId`] if the bridge supplied no remote id.
    pub fn upsert(
        &mut self,
        remote_id: &str,
        display: &str,
        account: AccountId,
    ) -> Result<Upsert, BridgeError> {
        if remote_id.is_empty() {
            return Err(BridgeError::EmptyRemoteId);
        }

        let existing = self.by_remote.get(remote_id).cloned();
        let keep_account = existing.as_ref().map_or(account, |u| u.account);
        let name = self.allocate_name(display, remote_id);

        match existing {
            Some(prev) if prev.name == name => Ok(Upsert::Unchanged(prev)),
            Some(prev) => {
                self.by_name.remove(&prev.name.to_ascii_lowercase());
                let user = BridgedUser {
                    account: keep_account,
                    remote_id: remote_id.to_string(),
                    name: name.clone(),
                };
                self.by_name
                    .insert(name.to_ascii_lowercase(), remote_id.to_string());
                self.by_remote.insert(remote_id.to_string(), user.clone());
                Ok(Upsert::Renamed {
                    user,
                    previous: prev.name,
                })
            }
            None => {
                let user = BridgedUser {
                    account,
                    remote_id: remote_id.to_string(),
                    name: name.clone(),
                };
                self.by_name
                    .insert(name.to_ascii_lowercase(), remote_id.to_string());
                self.by_remote.insert(remote_id.to_string(), user.clone());
                Ok(Upsert::Added(user))
            }
        }
    }

    /// Find a free name for `display`, ignoring any name currently held by `remote_id`.
    fn allocate_name(&self, display: &str, remote_id: &str) -> String {
        let free = |candidate: &str| match self.by_name.get(&candidate.to_ascii_lowercase()) {
            None => true,
            Some(holder) => holder == remote_id,
        };

        let first = derive_name(&self.tag, display, None);
        if free(&first) {
            return first;
        }
        for n in 2..=99u32 {
            let candidate = derive_name(&self.tag, display, Some(n));
            if free(&candidate) {
                return candidate;
            }
        }
        // Ninety-eight collisions on one stem. Fall back to the remote id, which is
        // unique by definition, rather than looping or panicking.
        derive_name(&self.tag, remote_id, None)
    }

    /// Remove an identity, returning it if it was present.
    pub fn remove(&mut self, remote_id: &str) -> Option<BridgedUser> {
        let user = self.by_remote.remove(remote_id)?;
        self.by_name.remove(&user.name.to_ascii_lowercase());
        Some(user)
    }
}

/// Where a message came from, so it is never sent back there.
///
/// Two bridges in one channel will echo each other forever unless this exists from the
/// first version. Retrofitting loop detection onto a deployed bridge protocol means every
/// bridge already written is wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginChain {
    hops: Vec<BridgeTag>,
}

impl OriginChain {
    /// A message originating on Battle.net itself.
    #[must_use]
    pub const fn local() -> Self {
        Self { hops: Vec::new() }
    }

    /// A message that entered through one bridge.
    #[must_use]
    pub fn from_bridge(tag: BridgeTag) -> Self {
        Self { hops: vec![tag] }
    }

    /// How many bridges this message has traversed.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.hops.len()
    }

    /// Whether the message may be delivered to a bridge.
    ///
    /// False if that bridge already handled it, or if the chain is at [`MAX_HOPS`].
    #[must_use]
    pub fn may_deliver_to(&self, tag: &BridgeTag) -> bool {
        if self.hops.len() >= MAX_HOPS {
            return false;
        }
        !self.hops.iter().any(|t| t == tag)
    }

    /// Record that the message passed through a bridge.
    ///
    /// Returns false and changes nothing when delivery was not permitted, so a caller
    /// that forgets to check still cannot build a loop.
    pub fn push(&mut self, tag: BridgeTag) -> bool {
        if !self.may_deliver_to(&tag) {
            return false;
        }
        self.hops.push(tag);
        true
    }

    /// The bridges traversed, in order.
    #[must_use]
    pub fn hops(&self) -> &[BridgeTag] {
        &self.hops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(s: &str) -> BridgeTag {
        BridgeTag::new(s).expect("valid tag")
    }

    #[test]
    fn tags_are_short_and_alphanumeric() {
        assert!(BridgeTag::new("D").is_ok());
        assert!(BridgeTag::new("RO").is_ok());
        assert!(BridgeTag::new("FF1").is_ok());
        assert_eq!(BridgeTag::new(""), Err(BridgeError::TagLength(0)));
        assert_eq!(BridgeTag::new("TOOLONG"), Err(BridgeError::TagLength(7)));
        assert_eq!(BridgeTag::new("D:"), Err(BridgeError::TagCharacters));
        assert_eq!(BridgeTag::new("a b"), Err(BridgeError::TagCharacters));
    }

    #[test]
    fn derived_names_fit_the_protocol_limit() {
        // 15 characters is a hard client limit; a longer name is truncated by the
        // client, which would break every lookup that round-trips through it.
        for display in [
            "alice",
            "a-very-long-discord-display-name-indeed",
            "x",
            "0123456789012345678901234567890123456789",
        ] {
            let n = derive_name(&tag("D"), display, None);
            assert!(n.len() <= MAX_NAME, "{n:?} is {} chars", n.len());
            let n = derive_name(&tag("FF1"), display, Some(99));
            assert!(n.len() <= MAX_NAME, "{n:?} is {} chars", n.len());
        }
    }

    #[test]
    fn derived_names_carry_the_tag() {
        assert_eq!(derive_name(&tag("D"), "alice", None), "D:alice");
        assert_eq!(derive_name(&tag("RO"), "Bahamut", None), "RO:Bahamut");
        assert_eq!(derive_name(&tag("FF"), "Tarutaru", Some(2)), "FF:Tarutaru#2");
    }

    #[test]
    fn unrepresentable_characters_are_dropped_not_relayed() {
        // A relayed control byte makes the receiving client disconnect and IP-ban us for
        // five minutes, so this is a correctness requirement rather than tidiness.
        assert_eq!(sanitize_remote_name("al\rice\n"), "alice");
        assert_eq!(sanitize_remote_name("Ali¢e"), "Alie");
        assert_eq!(sanitize_remote_name("[GM]Bob"), "[GM]Bob");
        assert_eq!(sanitize_remote_name("a b c"), "abc");
    }

    #[test]
    fn a_name_with_nothing_usable_still_gets_one() {
        assert_eq!(sanitize_remote_name("🌸🌸🌸"), FALLBACK_STEM);
        assert_eq!(derive_name(&tag("D"), "🌸🌸🌸", None), "D:user");
    }

    #[test]
    fn the_bridge_namespace_is_recognised_and_reserved() {
        assert_eq!(is_bridged_name("D:alice"), Some(tag("D")));
        assert_eq!(is_bridged_name("RO:Bahamut"), Some(tag("RO")));
        assert_eq!(is_bridged_name("Zealot"), None);
        assert_eq!(is_bridged_name("D:"), None, "a bare tag is not an identity");

        // Reservation is stricter than recognition: any separator at all is refused for
        // a real account, or someone registers D:alice and impersonates a Discord user.
        assert!(name_is_reserved_for_bridges("D:alice"));
        assert!(name_is_reserved_for_bridges("weird:"));
        assert!(name_is_reserved_for_bridges("TOOLONGTAG:x"));
        assert!(!name_is_reserved_for_bridges("Zealot"));
    }

    #[test]
    fn a_new_identity_is_added_once() {
        let mut r = BridgeRegistry::new(tag("D"));
        let out = r.upsert("discord-1", "alice", 100).unwrap();
        assert_eq!(out, Upsert::Added(out.user().clone()));
        assert_eq!(out.user().name, "D:alice");
        assert_eq!(out.user().account, 100);

        // Announcing it again changes nothing.
        let again = r.upsert("discord-1", "alice", 999).unwrap();
        assert!(matches!(again, Upsert::Unchanged(_)));
        assert_eq!(again.user().account, 100, "the account id must not change");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn a_rename_keeps_the_account_id() {
        // The property moderation depends on: a ban targets the account, so it survives
        // the user renaming themselves on Discord.
        let mut r = BridgeRegistry::new(tag("D"));
        r.upsert("discord-1", "alice", 100).unwrap();
        let out = r.upsert("discord-1", "alicia", 999).unwrap();
        match out {
            Upsert::Renamed { user, previous } => {
                assert_eq!(previous, "D:alice");
                assert_eq!(user.name, "D:alicia");
                assert_eq!(user.account, 100, "id must survive a rename");
            }
            other => panic!("expected a rename, got {other:?}"),
        }
        assert_eq!(r.len(), 1);
        assert!(r.by_name("D:alice").is_none(), "the old name must be freed");
        assert_eq!(r.by_name("D:alicia").unwrap().account, 100);
    }

    #[test]
    fn collisions_get_a_numeric_suffix() {
        let mut r = BridgeRegistry::new(tag("D"));
        let a = r.upsert("discord-1", "alice", 1).unwrap();
        let b = r.upsert("discord-2", "alice", 2).unwrap();
        let c = r.upsert("discord-3", "alice", 3).unwrap();
        assert_eq!(a.user().name, "D:alice");
        assert_eq!(b.user().name, "D:alice#2");
        assert_eq!(c.user().name, "D:alice#3");
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn a_freed_name_is_reused() {
        let mut r = BridgeRegistry::new(tag("D"));
        r.upsert("discord-1", "alice", 1).unwrap();
        r.upsert("discord-2", "alice", 2).unwrap();
        r.remove("discord-1");
        let c = r.upsert("discord-3", "alice", 3).unwrap();
        assert_eq!(c.user().name, "D:alice", "the vacated name is available");
    }

    #[test]
    fn distinct_remote_users_with_the_same_display_stay_distinct() {
        let mut r = BridgeRegistry::new(tag("D"));
        r.upsert("discord-1", "alice", 1).unwrap();
        r.upsert("discord-2", "alice", 2).unwrap();
        assert_ne!(
            r.by_remote_id("discord-1").unwrap().name,
            r.by_remote_id("discord-2").unwrap().name
        );
        assert_eq!(r.by_name("d:alice").unwrap().remote_id, "discord-1");
        assert_eq!(r.by_name("D:ALICE#2").unwrap().remote_id, "discord-2");
    }

    #[test]
    fn removing_an_unknown_identity_is_harmless() {
        let mut r = BridgeRegistry::new(tag("D"));
        assert!(r.remove("nobody").is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn an_empty_remote_id_is_refused() {
        let mut r = BridgeRegistry::new(tag("D"));
        assert_eq!(r.upsert("", "alice", 1), Err(BridgeError::EmptyRemoteId));
    }

    #[test]
    fn a_message_is_never_returned_to_its_own_bridge() {
        // Without this, two bridges in one channel ping-pong forever.
        let discord = tag("D");
        let ragnarok = tag("RO");
        let chain = OriginChain::from_bridge(discord.clone());
        assert!(!chain.may_deliver_to(&discord));
        assert!(chain.may_deliver_to(&ragnarok));
    }

    #[test]
    fn locally_originated_messages_go_everywhere() {
        let chain = OriginChain::local();
        assert_eq!(chain.depth(), 0);
        assert!(chain.may_deliver_to(&tag("D")));
        assert!(chain.may_deliver_to(&tag("RO")));
    }

    #[test]
    fn the_hop_limit_stops_a_multi_bridge_loop() {
        let mut chain = OriginChain::local();
        assert!(chain.push(tag("A")));
        assert!(chain.push(tag("B")));
        assert!(chain.push(tag("C")));
        assert_eq!(chain.depth(), MAX_HOPS);
        assert!(
            !chain.push(tag("D")),
            "a message past the hop limit must be dropped"
        );
        assert_eq!(chain.depth(), MAX_HOPS, "a refused push changes nothing");
    }

    #[test]
    fn push_refuses_a_repeat_even_without_a_prior_check() {
        // A caller that forgets to consult may_deliver_to still cannot build a loop.
        let mut chain = OriginChain::from_bridge(tag("D"));
        assert!(!chain.push(tag("D")));
        assert_eq!(chain.hops(), &[tag("D")]);
    }

    #[test]
    fn a_realistic_two_bridge_channel_settles() {
        // Discord and Ragnarok both bridged into one channel. A Discord message reaches
        // Ragnarok and Battle.net, and comes back to neither.
        let discord = tag("D");
        let ragnarok = tag("RO");

        let mut chain = OriginChain::from_bridge(discord.clone());
        assert!(!chain.may_deliver_to(&discord));
        assert!(chain.may_deliver_to(&ragnarok));
        assert!(chain.push(ragnarok.clone()));
        // Having now traversed both, it can go to neither.
        assert!(!chain.may_deliver_to(&discord));
        assert!(!chain.may_deliver_to(&ragnarok));
    }
}
