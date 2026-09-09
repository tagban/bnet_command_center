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

use bnetcc_storage::model::{Credential, NewAccount};
use bnetcc_storage::{Storage, StorageError};
use tokio::sync::oneshot;

use crate::node::Account;

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
