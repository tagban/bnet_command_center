//! Command Center persistence.
//!
//! # Why the trait is synchronous
//!
//! [`Storage`] has no `async` in it, and that is deliberate. The architecture puts
//! storage behind an **actor**: async tasks send commands over a bounded channel and a
//! dedicated thread runs plain blocking code against the database. Once that boundary
//! exists, making the trait async buys nothing and costs a great deal — `async fn` in
//! traits is not dyn-compatible, and every backend would need `async_trait` boxing.
//!
//! The failure this avoids is PvPGN's: it calls `mysql_query()` — the *synchronous*
//! libmysqlclient API — directly on its single event-loop thread. A cold login therefore
//! freezes every other connection on the server for a network round trip. The problem was
//! never that the call was blocking; it was that it blocked *on the reactor*.
//!
//! # Layers
//!
//! ```text
//!   handlers ──▶ WriteBehind<B> ──▶ B: Storage ──▶ SQLite / Postgres / memory
//!                     │
//!                     └── batches attribute writes; account creation,
//!                         credential changes and bans are write-through
//! ```
//!
//! This crate holds the trait, the ACL model, the write-behind layer, the in-memory
//! reference backend and the conformance suite — all dependency-free. A concrete database
//! backend lives in its own crate (`bnetcc-storage-sqlite`) and proves itself by passing
//! [`conformance::run`].
//!
//! [`AttrSchema`] sits above all of it and is what every client-facing path filters
//! through, so the password digest is unreachable by construction rather than by
//! vigilance.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod attr;
pub mod conformance;
pub mod memory;
pub mod model;
pub mod write_behind;

use bnetcc_core::AccountId;

pub use attr::{Acl, Actor, AttrKey, AttrMap, AttrSchema, Level};
pub use memory::MemoryStorage;
pub use model::{Account, Ban, BanScope, Character, Credential, NewAccount};
pub use write_behind::{FlushPolicy, WriteBehind};

/// A storage failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The account name is already registered.
    NameTaken,
    /// No such account.
    NoSuchAccount,
    /// The name violates the account-name rules.
    InvalidName(String),
    /// The backend failed. The string is for logs, never for a client.
    Backend(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTaken => write!(f, "account name already registered"),
            Self::NoSuchAccount => write!(f, "no such account"),
            Self::InvalidName(why) => write!(f, "invalid account name: {why}"),
            Self::Backend(e) => write!(f, "storage backend error: {e}"),
        }
    }
}

impl std::error::Error for StorageError {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Persistence for accounts, attributes and bans.
///
/// Implementations may block. They are always driven from the storage actor's own thread,
/// never from an async task.
pub trait Storage: Send + 'static {
    /// Look up an account by name, case-insensitively.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn account_by_name(&mut self, name: &str) -> Result<Option<Account>>;

    /// Look up an account by id.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn account_by_id(&mut self, id: AccountId) -> Result<Option<Account>>;

    /// Create an account.
    ///
    /// **Write-through.** Never batched: a user who registers and is then told the server
    /// restarted and lost them is a worse outcome than a slower registration.
    ///
    /// # Errors
    ///
    /// [`StorageError::NameTaken`], [`StorageError::InvalidName`], or backend failure.
    fn create_account(&mut self, req: NewAccount) -> Result<Account>;

    /// Replace an account's credential.
    ///
    /// **Write-through**, for the same reason as creation, and additionally because a
    /// password change that silently did not persist is a security problem.
    ///
    /// # Errors
    ///
    /// [`StorageError::NoSuchAccount`] or backend failure.
    fn set_credential(&mut self, id: AccountId, credential: Credential) -> Result<()>;

    /// Read attributes. Missing keys are simply absent from the result.
    ///
    /// This returns **unfiltered** data; the caller must pass it through
    /// [`AttrSchema::filter_readable`] before anything reaches a client.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn attrs_get(&mut self, id: AccountId, keys: &[AttrKey]) -> Result<AttrMap>;

    /// Read every attribute on an account.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn attrs_all(&mut self, id: AccountId) -> Result<AttrMap>;

    /// Write attributes, merging into what is already stored.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn attrs_put(&mut self, id: AccountId, attrs: AttrMap) -> Result<()>;

    /// Apply a ban. **Write-through.**
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn ban_put(&mut self, ban: Ban) -> Result<()>;

    /// The active ban on an account, if any.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn ban_get(&mut self, id: AccountId, now: u64) -> Result<Option<Ban>>;

    /// Lift a ban.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn ban_clear(&mut self, id: AccountId, scope: BanScope) -> Result<()>;

    /// Number of registered accounts.
    ///
    /// Used at startup to size the account cache. **Must be a counting query**, never a
    /// full load — PvPGN's file backend scans the whole account directory at every boot,
    /// and its ladder rebuild calls `accountlist_load_all(ST_FORCE)`, pulling every
    /// account into RAM synchronously and defeating the SQL backend's lazy loading.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn account_count(&mut self) -> Result<u64>;

    /// A page of accounts ordered by id, for the admin user list: skip `offset`, return at
    /// most `limit`. **Bounded and paged**, never a full load (see [`Self::account_count`]);
    /// the admin UI pages through rather than pulling every account into RAM.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn list_accounts(&mut self, offset: u64, limit: u32) -> Result<Vec<Account>>;

    /// Permanently delete an account together with its attributes and bans. **Write-through.**
    /// Idempotent: deleting an absent account is `Ok(())`.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn delete_account(&mut self, id: AccountId) -> Result<()>;

    /// Every Diablo II character owned by an account, oldest first.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn characters(&mut self, account: AccountId) -> Result<Vec<Character>>;

    /// A character by name, realm-wide and case-insensitively.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn character_by_name(&mut self, name: &str) -> Result<Option<Character>>;

    /// Create a character. **Write-through.**
    ///
    /// # Errors
    ///
    /// [`StorageError::NameTaken`] if any account already holds the name,
    /// [`StorageError::NoSuchAccount`] if the owner does not exist, or backend failure.
    fn create_character(&mut self, character: Character) -> Result<()>;

    /// Replace a stored character's mutable fields (class and name are its identity and do
    /// not change). **Write-through.** Returns `false` if the owner holds no such character.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn update_character(&mut self, character: &Character) -> Result<bool>;

    /// Delete one of an account's characters. **Write-through.** Returns `false` if the
    /// account holds no character of that name — including one held by another account.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn delete_character(&mut self, account: AccountId, name: &str) -> Result<bool>;

    /// Flush anything buffered. Called on a timer and at shutdown.
    ///
    /// # Errors
    ///
    /// Backend failure.
    fn flush(&mut self) -> Result<()>;
}

/// Validate an account name.
///
/// The rules are a **product decision**, deliberately not a storage artifact. PvPGN
/// restricts legal usernames to `-_[]` because it stores one file per account and the
/// filename *is* the name, so every reserved filesystem character had to be banned. We
/// store names as data, so the only constraints are protocol and sanity.
///
/// # Errors
///
/// [`StorageError::InvalidName`] with a message suitable for showing the user.
pub fn validate_account_name(name: &str) -> Result<()> {
    const MIN: usize = 2; // Shortest name allowed — a product decision, see docs.
    const MAX: usize = 15; // Battle.net truncates beyond this.
    // `Name@realm` designates a realm-scoped account — WarCraft III's SRP accounts live in
    // their own realm (`docs/WARCRAFT3.md` §3.6). The length rules apply to the bare name
    // and the realm is checked on its own, so a 15-character name keeps its whole budget
    // whichever realm it is in.
    let (name, realm) = split_realm(name);
    if let Some(realm) = realm {
        if realm.is_empty() || realm.len() > MAX {
            return Err(StorageError::InvalidName(format!(
                "realm must be 1 to {MAX} characters"
            )));
        }
        if !realm.is_ascii() || realm.bytes().any(|b| b <= 0x20 || b == 0x7F) {
            return Err(StorageError::InvalidName(
                "realm contains whitespace, control or non-ASCII characters".into(),
            ));
        }
    }
    if name.len() < MIN {
        return Err(StorageError::InvalidName(format!(
            "shorter than {MIN} characters"
        )));
    }
    if name.len() > MAX {
        return Err(StorageError::InvalidName(format!(
            "longer than {MAX} characters"
        )));
    }
    if !name.is_ascii() {
        return Err(StorageError::InvalidName("non-ASCII characters".into()));
    }
    // Any letter, digit or symbol is allowed — but nothing that breaks the wire. Chat,
    // statstrings and the gateway are space-delimited and line-oriented, so a space, tab,
    // CR/LF or other control byte in a name would corrupt those; reject the whole
    // whitespace/control class rather than a hand-picked symbol denylist.
    if name.bytes().any(|b| b <= 0x20 || b == 0x7F) {
        return Err(StorageError::InvalidName(
            "whitespace or control characters".into(),
        ));
    }
    let alnum = name.bytes().filter(u8::is_ascii_alphanumeric).count();
    if alnum == 0 {
        return Err(StorageError::InvalidName(
            "must contain at least one letter or digit".into(),
        ));
    }
    // The bridge namespace is reserved. Without this, someone registers `D:alice` and
    // impersonates a Discord user to everyone in the channel — see docs/BRIDGES.md §3.
    if bnetcc_core::bridge::name_is_reserved_for_bridges(name) {
        return Err(StorageError::InvalidName(format!(
            "'{}' is reserved for bridged identities",
            bnetcc_core::bridge::SEPARATOR
        )));
    }
    Ok(())
}

/// Split `Name@realm` into `("Name", Some("realm"))`; a name with no `@` is `(name, None)`.
///
/// The split is at the **last** `@`, so a realm never contains one. This is the single
/// definition of what a realm-qualified account name looks like.
#[must_use]
pub fn split_realm(name: &str) -> (&str, Option<&str>) {
    match name.rfind('@') {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_qualified_names_apply_the_length_rules_to_the_bare_name() {
        assert!(validate_account_name("Tagban@bncc").is_ok());
        assert!(validate_account_name(&format!("{}@bncc", "a".repeat(15))).is_ok());
        assert!(validate_account_name(&format!("{}@bncc", "a".repeat(16))).is_err());
        assert!(validate_account_name("a@bncc").is_err(), "bare name too short");
        assert!(validate_account_name("ab@").is_err(), "empty realm");
        assert!(validate_account_name("ab@x y").is_err(), "whitespace in realm");
        assert!(validate_account_name(&format!("ab@{}", "r".repeat(16))).is_err());
        assert_eq!(split_realm("Tagban@bncc"), ("Tagban", Some("bncc")));
        assert_eq!(split_realm("a@b@c"), ("a@b", Some("c")));
        assert_eq!(split_realm("Tagban"), ("Tagban", None));
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for n in ["Zealot", "gg", "user_1", "[CLAN]Bob", "x-y", "12345"] {
            assert!(validate_account_name(n).is_ok(), "{n} was rejected");
        }
    }

    #[test]
    fn names_must_be_at_least_two_characters() {
        assert!(validate_account_name("a").is_err(), "one char should be too short");
        assert!(validate_account_name("ab").is_ok(), "two chars is the minimum");
        assert!(validate_account_name("x1").is_ok());
    }

    #[test]
    fn names_punctuation_is_not_restricted_by_a_storage_choice() {
        // PvPGN bans these because the filename is the account name. We do not store
        // names as filenames, so there is no reason to inherit that.
        for n in ["a.b", "a*b", "a?b", "a!b", "ab@b"] {
            assert!(validate_account_name(n).is_ok(), "{n} was rejected");
        }
    }

    #[test]
    fn the_bridge_namespace_cannot_be_registered() {
        // Otherwise a real user registers D:alice and impersonates a bridged Discord
        // user to everyone in the channel. Enforced here because this is where accounts
        // are actually created, not only in the module that describes the rule.
        for n in ["D:alice", "RO:Bahamut", "a:b", "weird:"] {
            let err = validate_account_name(n).unwrap_err();
            assert!(
                matches!(err, StorageError::InvalidName(ref m) if m.contains("reserved")),
                "{n} gave {err:?}"
            );
        }
    }

    #[test]
    fn names_are_bounded_at_the_protocol_limit() {
        assert!(validate_account_name(&"a".repeat(15)).is_ok());
        assert!(validate_account_name(&"a".repeat(16)).is_err());
    }

    #[test]
    fn empty_whitespace_and_control_characters_are_rejected() {
        for n in ["", "   ", " Bob", "Bob ", "Bo\nb", "Bo\0b"] {
            assert!(validate_account_name(n).is_err(), "{n:?} was accepted");
        }
    }

    #[test]
    fn a_name_with_no_alphanumerics_is_rejected() {
        assert!(validate_account_name("___").is_err());
        assert!(validate_account_name("_a_").is_ok());
    }

    #[test]
    fn only_srp_material_may_be_cached_at_a_node() {
        assert!(!Credential::Xsha1 { digest: [0; 20] }.safe_to_cache_at_node());
        assert!(Credential::Srp {
            salt: [0; 32],
            verifier: [0; 32]
        }
        .safe_to_cache_at_node());
    }

    #[test]
    fn ban_expiry_is_respected() {
        let mut ban = Ban {
            account: 1,
            scope: BanScope::Node,
            reason: "flooding".into(),
            applied_at: 100,
            expires_at: Some(200),
        };
        assert!(ban.active_at(150));
        assert!(!ban.active_at(200));
        assert!(!ban.active_at(999));
        ban.expires_at = None;
        assert!(ban.active_at(u64::MAX));
    }
}
