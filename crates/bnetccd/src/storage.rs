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
use bnetcc_storage::model::{Credential, NewAccount};
use bnetcc_storage::{Storage, StorageError};
use tokio::sync::oneshot;

use crate::node::Account;

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

enum Command {
    AccountByName {
        name: String,
        resp: oneshot::Sender<Option<Account>>,
    },
    CreateAccount {
        name: String,
        password_hash: [u8; 20],
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
    /// Read requested attribute keys for an account by name, already filtered to what an
    /// *other* party may read (records, profile, non-secret system keys) — never secrets.
    /// Replies with an empty map if the account is unknown or storage fails.
    ReadReadableAttrs {
        account_name: String,
        keys: Vec<AttrKey>,
        resp: oneshot::Sender<AttrMap>,
    },
}

/// Handle held by every session task. Cheap to clone — it is just a channel sender.
#[derive(Clone)]
pub struct StorageHandle(sync_mpsc::Sender<Command>);

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

    /// Register a new account with an X-SHA-1 password digest.
    pub async fn create_account(
        &self,
        name: &str,
        password_hash: [u8; 20],
    ) -> Result<Account, CreateAccountError> {
        let (resp, rx) = oneshot::channel();
        if self
            .0
            .send(Command::CreateAccount {
                name: name.to_string(),
                password_hash,
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
                    Command::AccountByName { name, resp } => {
                        let found = backend
                            .account_by_name(&name)
                            .ok()
                            .flatten()
                            .and_then(to_account);
                        let _ = resp.send(found);
                    }
                    Command::CreateAccount { name, password_hash, resp } => {
                        let req = NewAccount {
                            name: name.clone(),
                            credential: Credential::Xsha1 { digest: password_hash },
                            created_at: now_secs(),
                            attrs: bnetcc_storage::attr::AttrMap::new(),
                        };
                        let result = match backend.create_account(req) {
                            Ok(a) => to_account(a).ok_or_else(|| {
                                CreateAccountError::Backend(
                                    "newly created account did not carry an XSHA1 credential"
                                        .into(),
                                )
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
    match a.credential {
        Credential::Xsha1 { digest } => Some(Account {
            id: a.id,
            name: a.name,
            password_hash: digest,
        }),
        // WC3's SRP accounts don't exist yet on this path (SID_AUTH_ACCOUNTCREATE/LOGON
        // are unimplemented — see docs/HANDOFF.md). Nothing calling this today can
        // produce one, but returning `None` rather than panicking keeps that true if
        // storage is ever shared with a future SRP path that populates the same table.
        Credential::Srp { .. } => None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
