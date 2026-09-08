//! Account attributes and their access control.
//!
//! Battle.net models an account as a bag of backslash-separated keys —
//! `System\Username`, `profile\location`, `Record\SEXP\0\wins`. PvPGN stores them with no
//! access control at all, and **CVE-2004-2705 was exactly that**: a crafted stats request
//! returned arbitrary attributes, including the password hash, to an unauthenticated
//! peer.
//!
//! Atlas fixed the shape of this by giving every key a read level and a write level. We
//! take that model and make it structural: the password digest is `Internal`/`Internal`,
//! so no client-facing read path can reach it *by construction* rather than by every
//! handler remembering to check.

use std::collections::BTreeMap;
use std::fmt;

use bnetcc_core::AccountId;

/// Who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// An unauthenticated peer, or another user asking about someone else.
    Other,
    /// The account's owner.
    Owner(AccountId),
    /// The server itself. Never a value derived from network input.
    Internal,
    /// Server staff acting through an admin path.
    Admin,
}

/// The privilege a key requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Anyone, including other users. `profile\location` is like this.
    Any,
    /// Only the account's owner (or staff, or the server).
    Owner,
    /// Only the server. Not reachable from any client-facing path, ever.
    Internal,
}

impl Level {
    /// Whether `actor` satisfies this level for a record owned by `owner`.
    #[must_use]
    pub fn permits(self, actor: Actor, owner: AccountId) -> bool {
        match self {
            Self::Any => true,
            Self::Owner => match actor {
                Actor::Internal | Actor::Admin => true,
                Actor::Owner(id) => id == owner,
                Actor::Other => false,
            },
            // Deliberately excludes Admin. An admin command that needs a password digest
            // is a bug in that command, not a reason to widen the rule.
            Self::Internal => matches!(actor, Actor::Internal),
        }
    }
}

/// Read and write privilege for one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acl {
    /// Level required to read.
    pub read: Level,
    /// Level required to write.
    pub write: Level,
}

impl Acl {
    /// Readable by anyone, writable by the owner. Profile fields.
    pub const PUBLIC: Self = Self {
        read: Level::Any,
        write: Level::Owner,
    };
    /// Readable and writable only by the owner.
    pub const PRIVATE: Self = Self {
        read: Level::Owner,
        write: Level::Owner,
    };
    /// Readable by anyone, writable only by the server. Ladder records.
    pub const SERVER_PUBLIC: Self = Self {
        read: Level::Any,
        write: Level::Internal,
    };
    /// Server-only in both directions. Credentials.
    pub const SECRET: Self = Self {
        read: Level::Internal,
        write: Level::Internal,
    };
}

/// A normalised attribute key.
///
/// Battle.net keys are case-insensitive, so the canonical form is lowercased. Normalising
/// in one place is what stops `System\Username` and `system\username` becoming two
/// attributes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttrKey(String);

impl AttrKey {
    /// Normalise a key.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        Self(raw.trim().replace('/', "\\").to_ascii_lowercase())
    }

    /// The canonical string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first path segment, e.g. `system` for `system\username`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.0.split('\\').next().unwrap_or("")
    }
}

impl fmt::Display for AttrKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A set of attributes.
pub type AttrMap = BTreeMap<AttrKey, String>;

/// Well-known keys, so they are spelled once.
pub mod keys {
    /// Registered display name.
    pub const USERNAME: &str = r"System\Username";
    /// `XSHA1(lowercase(password))`. Password-equivalent — never leaves the hub.
    pub const PASSWORD_DIGEST: &str = r"System\Password Digest";
    /// SRP salt (WarCraft III).
    pub const SRP_SALT: &str = r"System\SRP Salt";
    /// SRP verifier (WarCraft III). Cannot impersonate the client, so it may be cached
    /// at a federation node — see `docs/FEDERATION.md` §4.
    pub const SRP_VERIFIER: &str = r"System\SRP Verifier";
    /// Account flags.
    pub const FLAGS: &str = r"System\Flags";
    /// Last successful logon, seconds since the Unix epoch.
    pub const LAST_LOGON: &str = r"System\Last Logon";
    /// Friend list.
    pub const FRIENDS: &str = r"System\Friends";
    /// Profile location, as shown by `/whois`.
    pub const PROFILE_LOCATION: &str = r"profile\location";
    /// Profile description.
    pub const PROFILE_DESCRIPTION: &str = r"profile\description";
}

/// Maps keys to ACLs.
///
/// Rules are matched longest-prefix-first, so `system\srp verifier` can be tightened
/// without restating every `system\` key. Anything unmatched gets [`Self::fallback`],
/// which is deliberately **not** public: a key nobody thought about should not be
/// world-readable.
#[derive(Debug, Clone)]
pub struct AttrSchema {
    /// Prefix rules, longest first.
    rules: Vec<(String, Acl)>,
    fallback: Acl,
}

impl Default for AttrSchema {
    /// The default schema. Credentials are `SECRET`; records are server-written and
    /// publicly readable; profile fields are owner-written and publicly readable.
    fn default() -> Self {
        let mut s = Self {
            rules: Vec::new(),
            fallback: Acl::PRIVATE,
        };
        // Credentials. These three are why the whole mechanism exists.
        s.set(keys::PASSWORD_DIGEST, Acl::SECRET);
        s.set(keys::SRP_SALT, Acl::SECRET);
        s.set(keys::SRP_VERIFIER, Acl::SECRET);
        // Anything else under system\ is server-managed but not secret.
        s.set(r"System", Acl::SERVER_PUBLIC);
        // Ladder and game records: the server writes, everyone reads.
        s.set(r"Record", Acl::SERVER_PUBLIC);
        // Profile: the owner writes, everyone reads.
        s.set(r"profile", Acl::PUBLIC);
        s
    }
}

impl AttrSchema {
    /// A schema with no rules; everything falls back to `fallback`.
    #[must_use]
    pub fn empty(fallback: Acl) -> Self {
        Self {
            rules: Vec::new(),
            fallback,
        }
    }

    /// Add or replace a prefix rule.
    pub fn set(&mut self, prefix: &str, acl: Acl) {
        let key = AttrKey::new(prefix).0;
        self.rules.retain(|(p, _)| p != &key);
        self.rules.push((key, acl));
        // Longest prefix wins, so keep the list sorted by descending length.
        self.rules.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    }

    /// The ACL in force for a key.
    #[must_use]
    pub fn acl(&self, key: &AttrKey) -> Acl {
        for (prefix, acl) in &self.rules {
            if key.0 == *prefix || key.0.starts_with(&format!("{prefix}\\")) {
                return *acl;
            }
        }
        self.fallback
    }

    /// Whether `actor` may read `key` on `owner`'s account.
    #[must_use]
    pub fn can_read(&self, key: &AttrKey, actor: Actor, owner: AccountId) -> bool {
        self.acl(key).read.permits(actor, owner)
    }

    /// Whether `actor` may write `key` on `owner`'s account.
    #[must_use]
    pub fn can_write(&self, key: &AttrKey, actor: Actor, owner: AccountId) -> bool {
        self.acl(key).write.permits(actor, owner)
    }

    /// Drop every entry `actor` may not read.
    ///
    /// This is the function every client-facing read path must go through. It is cheaper
    /// to call than to reason about, which is the point.
    #[must_use]
    pub fn filter_readable(&self, attrs: AttrMap, actor: Actor, owner: AccountId) -> AttrMap {
        attrs
            .into_iter()
            .filter(|(k, _)| self.can_read(k, actor, owner))
            .collect()
    }

    /// Split a write into the parts `actor` may and may not perform.
    ///
    /// Returns `(accepted, rejected_keys)`. Rejecting silently would let a client believe
    /// a write landed; the caller should report the rejected keys.
    #[must_use]
    pub fn partition_writable(
        &self,
        attrs: AttrMap,
        actor: Actor,
        owner: AccountId,
    ) -> (AttrMap, Vec<AttrKey>) {
        let mut ok = AttrMap::new();
        let mut denied = Vec::new();
        for (k, v) in attrs {
            if self.can_write(&k, actor, owner) {
                ok.insert(k, v);
            } else {
                denied.push(k);
            }
        }
        (ok, denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: AccountId = 7;
    const OTHER: AccountId = 9;

    fn k(s: &str) -> AttrKey {
        AttrKey::new(s)
    }

    #[test]
    fn keys_normalise_case_and_separator() {
        assert_eq!(k(r"System\Username"), k(r"system\username"));
        assert_eq!(k("System/Username"), k(r"system\username"));
        assert_eq!(k("  profile\\Location  ").as_str(), r"profile\location");
        assert_eq!(k(r"Record\SEXP\0\wins").namespace(), "record");
    }

    #[test]
    fn the_password_digest_is_unreadable_by_everyone_but_the_server() {
        // The whole reason this module exists. CVE-2004-2705 was a crafted stats request
        // returning exactly this value to an unauthenticated peer.
        let s = AttrSchema::default();
        let key = k(keys::PASSWORD_DIGEST);
        assert!(!s.can_read(&key, Actor::Other, OWNER));
        assert!(!s.can_read(&key, Actor::Owner(OWNER), OWNER));
        assert!(
            !s.can_read(&key, Actor::Admin, OWNER),
            "an admin command needing the digest is a bug in that command"
        );
        assert!(s.can_read(&key, Actor::Internal, OWNER));
    }

    #[test]
    fn srp_material_is_equally_protected() {
        let s = AttrSchema::default();
        for key in [keys::SRP_SALT, keys::SRP_VERIFIER] {
            let key = k(key);
            assert!(!s.can_read(&key, Actor::Owner(OWNER), OWNER));
            assert!(!s.can_write(&key, Actor::Owner(OWNER), OWNER));
        }
    }

    #[test]
    fn profile_fields_are_public_to_read_and_owner_to_write() {
        let s = AttrSchema::default();
        let key = k(keys::PROFILE_LOCATION);
        assert!(s.can_read(&key, Actor::Other, OWNER));
        assert!(s.can_write(&key, Actor::Owner(OWNER), OWNER));
        assert!(
            !s.can_write(&key, Actor::Owner(OTHER), OWNER),
            "one user must not edit another's profile"
        );
    }

    #[test]
    fn records_are_readable_by_all_and_writable_only_by_the_server() {
        let s = AttrSchema::default();
        let key = k(r"Record\SEXP\0\wins");
        assert!(s.can_read(&key, Actor::Other, OWNER));
        assert!(!s.can_write(&key, Actor::Owner(OWNER), OWNER), "no self-reported wins");
        assert!(s.can_write(&key, Actor::Internal, OWNER));
    }

    #[test]
    fn unknown_keys_default_to_private_not_public() {
        // A key nobody thought about must not be world-readable.
        let s = AttrSchema::default();
        let key = k(r"experimental\something");
        assert!(!s.can_read(&key, Actor::Other, OWNER));
        assert!(s.can_read(&key, Actor::Owner(OWNER), OWNER));
    }

    #[test]
    fn longest_prefix_wins() {
        // `system` is SERVER_PUBLIC but `system\password digest` is SECRET, and the more
        // specific rule must win regardless of insertion order.
        let s = AttrSchema::default();
        assert_eq!(s.acl(&k(keys::FLAGS)), Acl::SERVER_PUBLIC);
        assert_eq!(s.acl(&k(keys::PASSWORD_DIGEST)), Acl::SECRET);
    }

    #[test]
    fn a_prefix_rule_does_not_match_a_longer_word() {
        // `profile` must not match `profiles\...`, only `profile` and `profile\...`.
        let mut s = AttrSchema::empty(Acl::PRIVATE);
        s.set("profile", Acl::PUBLIC);
        assert!(s.can_read(&k(r"profile\location"), Actor::Other, OWNER));
        assert!(!s.can_read(&k(r"profiles\location"), Actor::Other, OWNER));
    }

    #[test]
    fn filter_readable_strips_secrets() {
        let s = AttrSchema::default();
        let mut attrs = AttrMap::new();
        attrs.insert(k(keys::PASSWORD_DIGEST), "deadbeef".into());
        attrs.insert(k(keys::PROFILE_LOCATION), "Reykjavik".into());
        attrs.insert(k(r"Record\SEXP\0\wins"), "12".into());

        let visible = s.filter_readable(attrs, Actor::Other, OWNER);
        assert_eq!(visible.len(), 2);
        assert!(!visible.contains_key(&k(keys::PASSWORD_DIGEST)));
    }

    #[test]
    fn partition_writable_reports_what_it_refused() {
        // Silently dropping a write lets a client believe it landed.
        let s = AttrSchema::default();
        let mut attrs = AttrMap::new();
        attrs.insert(k(keys::PROFILE_LOCATION), "Reykjavik".into());
        attrs.insert(k(r"Record\SEXP\0\wins"), "9999".into());

        let (ok, denied) = s.partition_writable(attrs, Actor::Owner(OWNER), OWNER);
        assert_eq!(ok.len(), 1);
        assert_eq!(denied, vec![k(r"Record\SEXP\0\wins")]);
    }

    #[test]
    fn internal_satisfies_every_level() {
        let s = AttrSchema::default();
        for key in [keys::PASSWORD_DIGEST, keys::PROFILE_LOCATION, keys::FLAGS] {
            assert!(s.can_read(&k(key), Actor::Internal, OWNER));
            assert!(s.can_write(&k(key), Actor::Internal, OWNER));
        }
    }
}
