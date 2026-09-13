//! Storage domain types.

use bnetcc_core::AccountId;

use crate::attr::AttrMap;

/// How an account proves who it is.
///
/// The two variants are not interchangeable, and the difference decides the whole
/// federation identity design (`docs/FEDERATION.md` §4):
///
/// - [`Self::Xsha1`] holds `h1 = XSHA1(lowercase(password))`, which is
///   **password-equivalent**. Anyone holding it can authenticate as the user anywhere, so
///   it must never leave the hub and XSHA-1 logons are hub-proxied.
/// - [`Self::Srp`] holds a salt and verifier. The verifier permits *server* impersonation
///   but not *client* impersonation — recovering the exponent is a discrete log — so it
///   can safely be cached at a semi-trusted node and WarCraft III logons verify at the
///   edge with no hub round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// StarCraft, Brood War, Diablo I/II, Warcraft II BNE.
    Xsha1 {
        /// `XSHA1(lowercase(password))`.
        digest: [u8; 20],
    },
    /// WarCraft III (NLS/SRP).
    Srp {
        /// Salt.
        salt: [u8; 32],
        /// Verifier `g^x mod N`.
        verifier: [u8; 32],
    },
}

impl Credential {
    /// Whether this credential may be replicated to a federation node.
    ///
    /// Answering this in one place, next to the definition, is what stops the question
    /// being re-litigated at every call site.
    #[must_use]
    pub const fn safe_to_cache_at_node(&self) -> bool {
        match self {
            Self::Xsha1 { .. } => false,
            Self::Srp { .. } => true,
        }
    }
}

/// A stored account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Stable identifier.
    pub id: AccountId,
    /// Display name as registered, preserving case.
    pub name: String,
    /// Authentication material.
    pub credential: Credential,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: u64,
}

/// A request to create an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    /// Display name as the user typed it.
    pub name: String,
    /// Authentication material.
    pub credential: Credential,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: u64,
    /// Attributes to set at creation.
    pub attrs: AttrMap,
}

/// How far a ban reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BanScope {
    /// This node only. The operator's own decision, no approval needed.
    Node,
    /// The whole federation. Hub-applied; a node may request one with evidence.
    Network,
}

/// A ban record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    /// The banned account.
    pub account: AccountId,
    /// How far it reaches.
    pub scope: BanScope,
    /// Reason shown to the user, and to moderators in the audit log.
    pub reason: String,
    /// When it was applied, seconds since the Unix epoch.
    pub applied_at: u64,
    /// When it expires; `None` is permanent.
    pub expires_at: Option<u64>,
}

impl Ban {
    /// Whether this ban is in force at `now`.
    #[must_use]
    pub fn active_at(&self, now: u64) -> bool {
        match self.expires_at {
            None => true,
            Some(expires) => now < expires,
        }
    }
}

/// A Diablo II closed-realm character, owned by one account.
///
/// Character names are unique **realm-wide**, case-insensitively, not merely per account:
/// a name is an identity in chat (`Name*Account`), in game lists, and to a game server's
/// save handling, so two accounts may never hold the same one.
///
/// The fields are what the character-select screen and the chat portrait are drawn from.
/// `save` is the game server's own `.d2s` blob once a game server has written one; the
/// realm never interprets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Character {
    /// The owning account.
    pub account: AccountId,
    /// Name as created, preserving case.
    pub name: String,
    /// Class, `0` Amazon … `6` Assassin.
    pub class: u8,
    /// The `.d2s` status bits: `0x04` hardcore, `0x08` dead, `0x20` expansion, `0x40` ladder.
    pub status: u8,
    /// Character level, `1..=99`.
    pub level: u8,
    /// Difficulty/act progression (the title a character has earned).
    pub progression: u8,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: u64,
    /// Last selected, seconds since the Unix epoch.
    pub last_played: u64,
    /// The game server's save, if one has been written. Opaque to the realm.
    pub save: Option<Vec<u8>>,
}
