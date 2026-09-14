//! Bridge from the async connection-handling side to the synchronous [`bnetcc_storage`]
//! trait, on its own OS thread.
//!
//! Storage is synchronous by design — it sits behind an actor, so `async fn` in the trait
//! would buy nothing and would cost dyn-compatibility (see `bnetcc_storage::Storage`'s own
//! docs). Session tasks talk to that actor over a channel and `.await` a reply, rather than
//! blocking a tokio worker thread on a disk write. Accounts write through immediately
//! (losing a registration is not recoverable the way losing a profile edit is); there is no
//! buffering to flush here.

use std::sync::mpsc as sync_mpsc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use bnetcc_core::AccountId;
use bnetcc_storage::attr::{Actor, AttrKey, AttrMap, AttrSchema};
use bnetcc_storage::model::{Character, Credential, NewAccount};
use bnetcc_storage::{Storage, StorageError};
use tokio::sync::oneshot;

use crate::node::Account;

/// Attribute holding an account's admin-assigned user flags (a decimal `u32`).
const FLAGS_ATTR: &str = r"System\Flags";
/// Attribute holding the epoch-seconds of an account's last successful logon.
const LAST_LOGIN_ATTR: &str = r"System\LastLogin";

/// One account's row in the admin user list.
#[derive(Debug, Clone)]
pub struct UserSummary {
    /// Stable id.
    pub id: AccountId,
    /// Display name.
    pub name: String,
    /// Registration time, epoch seconds.
    pub created_at: u64,
    /// Last successful logon, epoch seconds; `None` if never (or before tracking began).
    pub last_login: Option<u64>,
    /// Wins summed across every product's `Record\` counters.
    pub wins: u64,
    /// Losses summed across every product's `Record\` counters.
    pub losses: u64,
    /// Admin-assigned flags currently stored on the account.
    pub flags: u32,
}

/// The outcome of a finished game, as folded into an account's `Record\` counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameOutcome {
    Win,
    Loss,
    Draw,
    Disconnect,
}

impl GameOutcome {
    /// Map a `SID_GAMERESULT` per-slot result code. `0` (empty slot / no result) and any
    /// unknown value map to `None` so they are not recorded.
    #[must_use]
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Win),
            2 => Some(Self::Loss),
            3 => Some(Self::Draw),
            4 => Some(Self::Disconnect),
            _ => None,
        }
    }

    /// What `Record\<product>\<n>\last game result` holds after this outcome.
    const fn result_word(self) -> &'static str {
        match self {
            Self::Win => "WIN",
            Self::Loss => "LOSS",
            Self::Draw => "DRAW",
            Self::Disconnect => "DISCONNECT",
        }
    }

    /// The `Record\<product>\0\<leaf>` counter this outcome increments.
    const fn leaf(self) -> &'static str {
        match self {
            Self::Win => "wins",
            Self::Loss => "losses",
            Self::Draw => "draws",
            Self::Disconnect => "disconnects",
        }
    }
}

/// Why account creation was refused.
#[derive(Debug, Clone)]
pub enum CreateAccountError {
    /// The name is already registered (case-insensitively).
    NameTaken,
    /// The name fails validation. The string is for logs, never the client.
    Invalid(String),
    /// The backend itself failed, or the actor thread is gone.
    Backend(String),
}

/// Why character creation was refused.
#[derive(Debug, Clone)]
pub enum CreateCharacterError {
    /// Some account already holds the name.
    NameTaken,
    /// The backend failed, or the actor thread is gone. For logs.
    Backend(String),
}

enum Command {
    Characters {
        account_id: AccountId,
        resp: oneshot::Sender<Result<Vec<Character>, String>>,
    },
    CharacterByName {
        name: String,
        resp: oneshot::Sender<Option<Character>>,
    },
    /// Every realm character of every account (for the ladder).
    AllCharacters {
        resp: oneshot::Sender<Vec<Character>>,
    },
    CreateCharacter {
        character: Character,
        resp: oneshot::Sender<Result<(), CreateCharacterError>>,
    },
    UpdateCharacter {
        character: Character,
        resp: oneshot::Sender<Result<bool, String>>,
    },
    DeleteCharacter {
        account_id: AccountId,
        name: String,
        resp: oneshot::Sender<Result<bool, String>>,
    },
    AccountByName {
        name: String,
        resp: oneshot::Sender<Option<Account>>,
    },
    CreateAccount {
        name: String,
        credential: Credential,
        resp: oneshot::Sender<Result<Account, CreateAccountError>>,
    },
    /// Increment one `Record\<product>\0\<counter>` for an account, atomically on the
    /// storage thread (read-modify-write with no cross-task race). Replies with the new
    /// count, or an error string for logs.
    RecordGame {
        account_id: AccountId,
        product: String,
        outcome: GameOutcome,
        resp: oneshot::Sender<Result<u64, String>>,
    },
    /// Record a ladder (or Iron Man) game: its counter, the new rating against opponents rated
    /// `opponent`, the high rating, and the last game and its result. Replies the new rating.
    RecordLadderGame {
        account_id: AccountId,
        product: String,
        league: bnetcc_core::ladder::League,
        outcome: GameOutcome,
        opponent: u32,
        resp: oneshot::Sender<Result<u32, String>>,
    },
    /// Every account's record in a product's league (the ladder standings' raw rows).
    LadderRows {
        product: String,
        league: bnetcc_core::ladder::League,
        resp: oneshot::Sender<Vec<bnetcc_core::ladder::LadderRow>>,
    },
    /// Read requested attribute keys for an account by name, already filtered to what an
    /// *other* party may read (records, profile, non-secret system keys) — never secrets.
    /// Replies with an empty map if the account is unknown or storage fails.
    ReadReadableAttrs {
        account_name: String,
        keys: Vec<AttrKey>,
        resp: oneshot::Sender<AttrMap>,
    },
    /// A page of accounts for the admin user list, each with its records and stored flags.
    ListUsers {
        offset: u64,
        limit: u32,
        resp: oneshot::Sender<Vec<UserSummary>>,
    },
    /// Read an account's stored admin-assigned flags (applied at logon).
    UserFlags {
        account_id: AccountId,
        resp: oneshot::Sender<u32>,
    },
    /// Replace an account's stored admin-assigned flags.
    SetUserFlags {
        account_id: AccountId,
        flags: u32,
        resp: oneshot::Sender<Result<(), String>>,
    },
    /// Reset an account's password from plaintext (write-through). The stored credential's
    /// family decides what is derived and stored.
    ResetPassword {
        account_id: AccountId,
        password: String,
        resp: oneshot::Sender<Result<(), String>>,
    },
    /// Permanently delete an account and its data (write-through).
    DeleteUser {
        account_id: AccountId,
        resp: oneshot::Sender<Result<(), String>>,
    },
    /// Record a successful logon time. Fire-and-forget: no reply, so a login is never
    /// slowed by the write, and a failure is a missing timestamp, nothing worse.
    RecordLogin {
        account_id: AccountId,
        when: u64,
    },
}

/// Sum wins/losses across every product's `Record\<product>\0\{wins,losses}` counters, and
/// pull the stored flags and last-login timestamp, from one account's full attribute set.
fn summarize_attrs(attrs: &AttrMap) -> (u64, u64, Option<u64>, u32) {
    let (mut wins, mut losses) = (0u64, 0u64);
    let (mut last_login, mut flags) = (None, 0u32);
    for (k, v) in attrs {
        let key = k.as_str().to_ascii_lowercase();
        if key.ends_with(r"\0\wins") {
            wins = wins.saturating_add(v.parse().unwrap_or(0));
        } else if key.ends_with(r"\0\losses") {
            losses = losses.saturating_add(v.parse().unwrap_or(0));
        } else if key == FLAGS_ATTR.to_ascii_lowercase() {
            flags = v.parse().unwrap_or(0);
        } else if key == LAST_LOGIN_ATTR.to_ascii_lowercase() {
            last_login = v.parse().ok();
        }
    }
    (wins, losses, last_login, flags)
}

/// Handle held by every session task. Cheap to clone — it is just a channel sender.
#[derive(Clone)]
pub struct StorageHandle(sync_mpsc::Sender<Command>);

impl std::fmt::Debug for StorageHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StorageHandle")
    }
}

impl StorageHandle {
    /// Look up an account by name, case-insensitively.
    pub async fn account_by_name(&self, name: &str) -> Option<Account> {
        let (resp, rx) = oneshot::channel();
        if self
            .0
            .send(Command::AccountByName { name: name.to_string(), resp })
            .is_err()
        {
            return None;
        }
        rx.await.unwrap_or(None)
    }

    /// Register a new account with the given credential (an X-SHA-1 digest, or a WarCraft
    /// III salt and verifier for a realm-qualified `Name@<realm>`).
    pub async fn create_account(
        &self,
        name: &str,
        credential: Credential,
    ) -> Result<Account, CreateAccountError> {
        let (resp, rx) = oneshot::channel();
        if self
            .0
            .send(Command::CreateAccount {
                name: name.to_string(),
                credential,
                resp,
            })
            .is_err()
        {
            return Err(CreateAccountError::Backend("storage actor is gone".into()));
        }
        rx.await
            .unwrap_or_else(|_| Err(CreateAccountError::Backend("storage actor is gone".into())))
    }

    /// Record one finished-game outcome against an account's `Record\<product>\0\` counters.
    /// Returns the new counter value, or an error string (actor gone or backend failure).
    pub async fn record_game(
        &self,
        account_id: AccountId,
        product: &str,
        outcome: GameOutcome,
    ) -> Result<u64, String> {
        let (resp, rx) = oneshot::channel();
        if self
            .0
            .send(Command::RecordGame {
                account_id,
                product: product.to_string(),
                outcome,
                resp,
            })
            .is_err()
        {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Record a ladder game for an account; the new rating.
    pub async fn record_ladder_game(
        &self,
        account_id: AccountId,
        product: &str,
        league: bnetcc_core::ladder::League,
        outcome: GameOutcome,
        opponent: u32,
    ) -> Result<u32, String> {
        let (resp, rx) = oneshot::channel();
        let product = product.to_string();
        if self.0.send(Command::RecordLadderGame { account_id, product, league, outcome, opponent, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Every account's record in a product's league.
    pub async fn ladder_rows(&self, product: &str, league: bnetcc_core::ladder::League) -> Vec<bnetcc_core::ladder::LadderRow> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::LadderRows { product: product.to_string(), league, resp }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Read the given keys for an account by name, filtered to what any peer may read. Safe
    /// to hand straight to a client: secrets and unknown/private keys never appear.
    pub async fn read_readable_attrs(&self, account_name: &str, keys: Vec<AttrKey>) -> AttrMap {
        let (resp, rx) = oneshot::channel();
        if self
            .0
            .send(Command::ReadReadableAttrs {
                account_name: account_name.to_string(),
                keys,
                resp,
            })
            .is_err()
        {
            return AttrMap::new();
        }
        rx.await.unwrap_or_default()
    }

    /// A page of accounts for the admin user list.
    pub async fn list_users(&self, offset: u64, limit: u32) -> Vec<UserSummary> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::ListUsers { offset, limit, resp }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// An account's stored admin-assigned flags (`0` if none or on failure).
    pub async fn user_flags(&self, account_id: AccountId) -> u32 {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::UserFlags { account_id, resp }).is_err() {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    /// Replace an account's stored admin-assigned flags.
    pub async fn set_user_flags(&self, account_id: AccountId, flags: u32) -> Result<(), String> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::SetUserFlags { account_id, flags, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Reset an account's password from plaintext. An X-SHA-1 account gets a new digest; a
    /// WarCraft III realm account gets a fresh salt and verifier.
    pub async fn reset_password(&self, account_id: AccountId, password: &str) -> Result<(), String> {
        let (resp, rx) = oneshot::channel();
        let password = password.to_string();
        if self.0.send(Command::ResetPassword { account_id, password, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Permanently delete an account.
    pub async fn delete_user(&self, account_id: AccountId) -> Result<(), String> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::DeleteUser { account_id, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Record a successful logon time, fire-and-forget (never blocks the login).
    pub fn record_login(&self, account_id: AccountId, when: u64) {
        let _ = self.0.send(Command::RecordLogin { account_id, when });
    }

    /// An account's Diablo II realm characters, oldest first.
    pub async fn characters(&self, account_id: AccountId) -> Result<Vec<Character>, String> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::Characters { account_id, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Every realm character, account by account. Empty on failure.
    pub async fn all_characters(&self) -> Vec<Character> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::AllCharacters { resp }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// A realm character by name (realm-wide, case-insensitive). `None` on failure too.
    pub async fn character_by_name(&self, name: &str) -> Option<Character> {
        let (resp, rx) = oneshot::channel();
        let name = name.to_string();
        if self.0.send(Command::CharacterByName { name, resp }).is_err() {
            return None;
        }
        rx.await.unwrap_or(None)
    }

    /// Create a realm character. Write-through.
    pub async fn create_character(&self, character: Character) -> Result<(), CreateCharacterError> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::CreateCharacter { character, resp }).is_err() {
            return Err(CreateCharacterError::Backend("storage actor is gone".into()));
        }
        rx.await
            .unwrap_or_else(|_| Err(CreateCharacterError::Backend("storage actor is gone".into())))
    }

    /// Replace a character's mutable fields. `Ok(false)` if the owner holds no such character.
    pub async fn update_character(&self, character: Character) -> Result<bool, String> {
        let (resp, rx) = oneshot::channel();
        if self.0.send(Command::UpdateCharacter { character, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }

    /// Delete one of an account's characters. `Ok(false)` if it holds none of that name.
    pub async fn delete_character(&self, account_id: AccountId, name: &str) -> Result<bool, String> {
        let (resp, rx) = oneshot::channel();
        let name = name.to_string();
        if self.0.send(Command::DeleteCharacter { account_id, name, resp }).is_err() {
            return Err("storage actor is gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("storage actor is gone".into()))
    }
}

/// Start the storage actor on a dedicated thread, which owns `backend` for the life of
/// the process.
pub fn spawn(mut backend: Box<dyn Storage + Send>) -> StorageHandle {
    let (tx, rx) = sync_mpsc::channel::<Command>();
    thread::Builder::new()
        .name("storage".into())
        .spawn(move || {
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Command::Characters { account_id, resp } => {
                        let _ = resp.send(backend.characters(account_id).map_err(|e| e.to_string()));
                    }
                    Command::CharacterByName { name, resp } => {
                        let _ = resp.send(backend.character_by_name(&name).ok().flatten());
                    }
                    Command::AllCharacters { resp } => {
                        let mut all = Vec::new();
                        let mut offset = 0u64;
                        while let Ok(page) = backend.list_accounts(offset, 500) {
                            if page.is_empty() {
                                break;
                            }
                            offset += page.len() as u64;
                            for account in page {
                                all.extend(backend.characters(account.id).unwrap_or_default());
                            }
                        }
                        let _ = resp.send(all);
                    }
                    Command::CreateCharacter { character, resp } => {
                        let result = backend.create_character(character).map_err(|e| match e {
                            StorageError::NameTaken => CreateCharacterError::NameTaken,
                            other => CreateCharacterError::Backend(other.to_string()),
                        });
                        let _ = resp.send(result);
                    }
                    Command::UpdateCharacter { character, resp } => {
                        let _ = resp.send(backend.update_character(&character).map_err(|e| e.to_string()));
                    }
                    Command::DeleteCharacter { account_id, name, resp } => {
                        let _ = resp.send(
                            backend.delete_character(account_id, &name).map_err(|e| e.to_string()),
                        );
                    }
                    Command::AccountByName { name, resp } => {
                        let found = backend
                            .account_by_name(&name)
                            .ok()
                            .flatten()
                            .and_then(to_account);
                        let _ = resp.send(found);
                    }
                    Command::CreateAccount { name, credential, resp } => {
                        let req = NewAccount {
                            name: name.clone(),
                            credential,
                            created_at: now_secs(),
                            attrs: bnetcc_storage::attr::AttrMap::new(),
                        };
                        let result = match backend.create_account(req) {
                            Ok(a) => to_account(a).ok_or_else(|| {
                                CreateAccountError::Backend("account conversion failed".into())
                            }),
                            Err(StorageError::NameTaken) => Err(CreateAccountError::NameTaken),
                            Err(StorageError::InvalidName(why)) => {
                                Err(CreateAccountError::Invalid(why))
                            }
                            Err(e) => Err(CreateAccountError::Backend(e.to_string())),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RecordGame { account_id, product, outcome, resp } => {
                        // Read-modify-write is safe here: this thread is the sole writer, so
                        // no other task can interleave between the get and the put.
                        let key = AttrKey::new(&format!(
                            r"Record\{product}\0\{}",
                            outcome.leaf()
                        ));
                        let result = (|| {
                            let current = backend
                                .attrs_get(account_id, std::slice::from_ref(&key))?
                                .get(&key)
                                .and_then(|v| v.parse::<u64>().ok())
                                .unwrap_or(0);
                            let next = current.saturating_add(1);
                            let mut one = AttrMap::new();
                            one.insert(key.clone(), next.to_string());
                            backend.attrs_put(account_id, one)?;
                            Ok::<u64, StorageError>(next)
                        })()
                        .map_err(|e: StorageError| e.to_string());
                        let _ = resp.send(result);
                    }
                    Command::RecordLadderGame { account_id, product, league, outcome, opponent, resp } => {
                        use bnetcc_core::ladder::{rating_after, Outcome, START_RATING};
                        let n = league.index();
                        let key = |leaf: &str| AttrKey::new(&format!(r"Record\{product}\{n}\{leaf}"));
                        let keys = [key(outcome.leaf()), key("rating"), key("high rating")];
                        let result = (|| {
                            let current = backend.attrs_get(account_id, &keys)?;
                            let number = |k: &AttrKey| current.get(k).and_then(|v| v.parse::<u64>().ok());
                            let count = number(&keys[0]).unwrap_or(0).saturating_add(1);
                            let rating = number(&keys[1]).map_or(START_RATING, |r| r as u32);
                            let played = match outcome {
                                GameOutcome::Win => Outcome::Win,
                                GameOutcome::Loss => Outcome::Loss,
                                GameOutcome::Draw => Outcome::Draw,
                                GameOutcome::Disconnect => Outcome::Disconnect,
                            };
                            let next = rating_after(rating, opponent, played);
                            let high = number(&keys[2]).map_or(START_RATING, |r| r as u32).max(next);
                            let now = now_secs();
                            let mut put = AttrMap::new();
                            put.insert(keys[0].clone(), count.to_string());
                            put.insert(keys[1].clone(), next.to_string());
                            put.insert(keys[2].clone(), high.to_string());
                            put.insert(key("last game"), now.to_string());
                            put.insert(key("last game result"), outcome.result_word().to_string());
                            backend.attrs_put(account_id, put)?;
                            Ok::<u32, StorageError>(next)
                        })()
                        .map_err(|e: StorageError| e.to_string());
                        let _ = resp.send(result);
                    }
                    Command::LadderRows { product, league, resp } => {
                        use bnetcc_core::ladder::{LadderRow, START_RATING};
                        let n = league.index();
                        let leaves = ["wins", "losses", "disconnects", "rating", "high rating", "last game"];
                        let keys: Vec<AttrKey> = leaves.iter().map(|leaf| AttrKey::new(&format!(r"Record\{product}\{n}\{leaf}"))).collect();
                        let mut rows = Vec::new();
                        let mut offset = 0u64;
                        while let Ok(page) = backend.list_accounts(offset, 500) {
                            if page.is_empty() {
                                break;
                            }
                            offset += page.len() as u64;
                            for account in page {
                                let Ok(attrs) = backend.attrs_get(account.id, &keys) else { continue };
                                let get = |i: usize| attrs.get(&keys[i]).and_then(|v| v.parse::<u64>().ok());
                                let row = LadderRow {
                                    name: account.name,
                                    wins: get(0).unwrap_or(0) as u32,
                                    losses: get(1).unwrap_or(0) as u32,
                                    disconnects: get(2).unwrap_or(0) as u32,
                                    rating: get(3).map_or(START_RATING, |r| r as u32),
                                    high_rating: get(4).map_or(START_RATING, |r| r as u32),
                                    last_game: get(5).unwrap_or(0),
                                };
                                if row.games() > 0 {
                                    rows.push(row);
                                }
                            }
                        }
                        let _ = resp.send(rows);
                    }
                    Command::ListUsers { offset, limit, resp } => {
                        let users = backend
                            .list_accounts(offset, limit)
                            .map(|accounts| {
                                accounts
                                    .into_iter()
                                    .map(|a| {
                                        let attrs = backend.attrs_all(a.id).unwrap_or_default();
                                        let (wins, losses, last_login, flags) =
                                            summarize_attrs(&attrs);
                                        UserSummary {
                                            id: a.id,
                                            name: a.name,
                                            created_at: a.created_at,
                                            last_login,
                                            wins,
                                            losses,
                                            flags,
                                        }
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let _ = resp.send(users);
                    }
                    Command::UserFlags { account_id, resp } => {
                        let key = AttrKey::new(FLAGS_ATTR);
                        let flags = backend
                            .attrs_get(account_id, std::slice::from_ref(&key))
                            .ok()
                            .and_then(|m| m.get(&key).and_then(|v| v.parse::<u32>().ok()))
                            .unwrap_or(0);
                        let _ = resp.send(flags);
                    }
                    Command::SetUserFlags { account_id, flags, resp } => {
                        let mut one = AttrMap::new();
                        one.insert(AttrKey::new(FLAGS_ATTR), flags.to_string());
                        let result = backend.attrs_put(account_id, one).map_err(|e| e.to_string());
                        let _ = resp.send(result);
                    }
                    Command::ResetPassword { account_id, password, resp } => {
                        // Derive the same family the account already has: the family is the
                        // account's identity (an SRP account lives in a realm and only a
                        // WarCraft III client can use it), not a property of the password.
                        let result = match backend.account_by_id(account_id) {
                            Ok(Some(acct)) => {
                                let credential = match acct.credential {
                                    Credential::Xsha1 { .. } => Credential::Xsha1 {
                                        digest: bnetcc_crypto::password_hash(&password),
                                    },
                                    Credential::Srp { .. } => {
                                        // The verifier binds the bare name (the client
                                        // hashes what the user typed, upper-cased), never
                                        // the realm suffix.
                                        let (bare, _) = bnetcc_storage::split_realm(&acct.name);
                                        let salt: [u8; 32] = rand::random();
                                        let verifier =
                                            bnetcc_crypto::nls::verifier(bare, &password, &salt);
                                        Credential::Srp { salt, verifier }
                                    }
                                };
                                backend
                                    .set_credential(account_id, credential)
                                    .map_err(|e| e.to_string())
                            }
                            Ok(None) => Err("no such account".to_string()),
                            Err(e) => Err(e.to_string()),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteUser { account_id, resp } => {
                        let result = backend.delete_account(account_id).map_err(|e| e.to_string());
                        let _ = resp.send(result);
                    }
                    Command::RecordLogin { account_id, when } => {
                        let mut one = AttrMap::new();
                        one.insert(AttrKey::new(LAST_LOGIN_ATTR), when.to_string());
                        let _ = backend.attrs_put(account_id, one);
                    }
                    Command::ReadReadableAttrs { account_name, keys, resp } => {
                        // Look up the owner, read the requested keys, and hand back only what
                        // an *other* party may see. Actor::Other is the safe floor: it never
                        // exposes owner-only or secret keys, so this cannot regress
                        // CVE-2004-2705 even when the requester is the owner.
                        let filtered = backend
                            .account_by_name(&account_name)
                            .ok()
                            .flatten()
                            .and_then(|acct| {
                                let attrs = backend.attrs_get(acct.id, &keys).ok()?;
                                Some(AttrSchema::default().filter_readable(
                                    attrs,
                                    Actor::Other,
                                    acct.id,
                                ))
                            })
                            .unwrap_or_default();
                        let _ = resp.send(filtered);
                    }
                }
            }
        })
        .expect("spawn storage actor thread");
    StorageHandle(tx)
}

fn to_account(a: bnetcc_storage::model::Account) -> Option<Account> {
    Some(Account {
        id: a.id,
        name: a.name,
        credential: a.credential,
    })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnetcc_proto::chat::user_flags;
    use bnetcc_storage::memory::MemoryStorage;

    #[tokio::test]
    async fn user_management_round_trip_through_the_actor() {
        let h = spawn(Box::new(MemoryStorage::new()));
        let acct = h
            .create_account("Zealot", Credential::Xsha1 { digest: [1u8; 20] })
            .await
            .expect("create");

        // Flags start empty, round-trip through set/get.
        assert_eq!(h.user_flags(acct.id).await, 0);
        let flags = user_flags::ADMIN | user_flags::BLIZZARD_REP | user_flags::SPEAKER;
        h.set_user_flags(acct.id, flags).await.expect("set flags");
        assert_eq!(h.user_flags(acct.id).await, flags);

        // A recorded login and a couple of game results show up in the summary. record_login
        // is fire-and-forget, but the actor processes its channel in order, so the awaited
        // record_game calls that follow guarantee it has been applied before the list read.
        h.record_login(acct.id, 1_700_000_500);
        h.record_game(acct.id, "SEXP", GameOutcome::Win).await.unwrap();
        h.record_game(acct.id, "SEXP", GameOutcome::Loss).await.unwrap();

        let users = h.list_users(0, 10).await;
        let u = users.iter().find(|u| u.id == acct.id).expect("account is listed");
        assert_eq!(u.name, "Zealot");
        assert_eq!(u.flags, flags);
        assert_eq!(u.wins, 1);
        assert_eq!(u.losses, 1);
        assert_eq!(u.last_login, Some(1_700_000_500));

        // A reset derives the account's own family from the plaintext. Then delete; the
        // account is gone afterwards.
        h.reset_password(acct.id, "newpass").await.expect("reset");
        let reset = h.account_by_name("Zealot").await.expect("still there");
        assert_eq!(
            reset.credential,
            Credential::Xsha1 { digest: bnetcc_crypto::password_hash("newpass") }
        );
        h.delete_user(acct.id).await.expect("delete");
        assert!(h.account_by_name("Zealot").await.is_none());
        assert!(h.list_users(0, 10).await.iter().all(|u| u.id != acct.id));
    }

    #[tokio::test]
    async fn ladder_games_rate_and_list_by_league() {
        use bnetcc_core::ladder::League;
        let h = spawn(Box::new(MemoryStorage::new()));
        let a = h.create_account("Raynor", Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
        let b = h.create_account("Kerrigan", Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
        h.create_account("Idle", Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
        assert_eq!(h.record_ladder_game(a.id, "STAR", League::Ladder, GameOutcome::Win, 1000).await, Ok(1016));
        assert_eq!(h.record_ladder_game(a.id, "STAR", League::Ladder, GameOutcome::Loss, 1000).await, Ok(999));
        h.record_ladder_game(b.id, "STAR", League::IronMan, GameOutcome::Disconnect, 1000).await.unwrap();

        let ladder = h.ladder_rows("STAR", League::Ladder).await;
        assert_eq!(ladder.len(), 1, "only players with ladder games: {ladder:?}");
        let r = &ladder[0];
        assert_eq!((r.name.as_str(), r.wins, r.losses, r.rating, r.high_rating), ("Raynor", 1, 1, 999, 1016));
        assert!(r.last_game > 0);
        let iron = h.ladder_rows("STAR", League::IronMan).await;
        assert_eq!((iron[0].name.as_str(), iron[0].disconnects, iron[0].rating), ("Kerrigan", 1, 984));
        assert!(h.ladder_rows("SEXP", League::Ladder).await.is_empty(), "ladders are per product");
        let normal = h.read_readable_attrs("Raynor", vec![AttrKey::new(r"Record\STAR\1\last game result")]).await;
        assert_eq!(normal.values().next().map(String::as_str), Some("LOSS"));
    }

    #[tokio::test]
    async fn every_accounts_characters_are_listed_for_the_ladder() {
        let h = spawn(Box::new(MemoryStorage::new()));
        for (account, names) in [("One", ["Aa", "Ab"]), ("Two", ["Ba", "Bb"])] {
            let acct = h.create_account(account, Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
            for name in names {
                let c = Character { account: acct.id, name: name.into(), class: 0, status: 0, level: 1, progression: 0, created_at: 0, last_played: 0, save: None };
                h.create_character(c).await.unwrap();
            }
        }
        let mut names: Vec<String> = h.all_characters().await.into_iter().map(|c| c.name).collect();
        names.sort();
        assert_eq!(names, ["Aa", "Ab", "Ba", "Bb"]);
    }

    #[tokio::test]
    async fn a_realm_account_resets_to_a_fresh_salt_and_verifier() {
        // A WarCraft III account is stored as `Name@realm` with an SRP credential. An
        // operator reset must derive a verifier that the *bare* name's client proof matches.
        let h = spawn(Box::new(MemoryStorage::new()));
        let acct = h
            .create_account("Zealot@bncc", Credential::Srp { salt: [1u8; 32], verifier: [2u8; 32] })
            .await
            .expect("create");
        h.reset_password(acct.id, "hunter2").await.expect("reset");
        let reset = h.account_by_name("zealot@BNCC").await.expect("still there");
        let Credential::Srp { salt, verifier } = reset.credential else {
            panic!("family must be preserved");
        };
        assert_ne!(salt, [1u8; 32], "a reset draws a new salt");
        assert_eq!(verifier, bnetcc_crypto::nls::verifier("Zealot", "hunter2", &salt));
    }
}
