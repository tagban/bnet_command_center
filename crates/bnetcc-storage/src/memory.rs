//! In-memory reference backend.
//!
//! Used by tests and by `--ephemeral` runs. It is the *reference*: the conformance suite
//! is written against its observable behaviour, and every other backend must match it.
//!
//! It is emphatically **not** a deployment option, and the reason is instructive. Atlas
//! has only this — `AccountsDb` is a `ConcurrentDictionary` that is never serialised, so
//! every account, friend list and clan evaporates on restart. That is a large part of why
//! Atlas looks so stable: a server with no persistence layer has no persistence bugs, no
//! schema migrations, no fsync stalls and no disk in the request path. It is not a model
//! to copy, and `bnetccd` refuses to start with this backend unless `--ephemeral` is passed
//! explicitly.

use std::collections::HashMap;

use bnetcc_core::AccountId;

use crate::attr::{AttrKey, AttrMap};
use crate::model::{Account, Ban, BanScope, Credential, NewAccount};
use crate::{validate_account_name, Result, Storage, StorageError};

/// An in-memory [`Storage`] implementation.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    accounts: HashMap<AccountId, Account>,
    /// Lowercased name to id.
    by_name: HashMap<String, AccountId>,
    attrs: HashMap<AccountId, AttrMap>,
    bans: HashMap<(AccountId, BanScope), Ban>,
    next_id: AccountId,
}

impl MemoryStorage {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Storage for MemoryStorage {
    fn account_by_name(&mut self, name: &str) -> Result<Option<Account>> {
        let id = self.by_name.get(&name.to_ascii_lowercase()).copied();
        Ok(id.and_then(|id| self.accounts.get(&id).cloned()))
    }

    fn account_by_id(&mut self, id: AccountId) -> Result<Option<Account>> {
        Ok(self.accounts.get(&id).cloned())
    }

    fn create_account(&mut self, req: NewAccount) -> Result<Account> {
        validate_account_name(&req.name)?;
        let key = req.name.to_ascii_lowercase();
        if self.by_name.contains_key(&key) {
            return Err(StorageError::NameTaken);
        }
        self.next_id += 1;
        let account = Account {
            id: self.next_id,
            name: req.name,
            credential: req.credential,
            created_at: req.created_at,
        };
        self.by_name.insert(key, account.id);
        self.accounts.insert(account.id, account.clone());
        if !req.attrs.is_empty() {
            self.attrs.insert(account.id, req.attrs);
        }
        Ok(account)
    }

    fn set_credential(&mut self, id: AccountId, credential: Credential) -> Result<()> {
        let account = self
            .accounts
            .get_mut(&id)
            .ok_or(StorageError::NoSuchAccount)?;
        account.credential = credential;
        Ok(())
    }

    fn attrs_get(&mut self, id: AccountId, keys: &[AttrKey]) -> Result<AttrMap> {
        let Some(all) = self.attrs.get(&id) else {
            return Ok(AttrMap::new());
        };
        Ok(keys
            .iter()
            .filter_map(|k| all.get(k).map(|v| (k.clone(), v.clone())))
            .collect())
    }

    fn attrs_all(&mut self, id: AccountId) -> Result<AttrMap> {
        Ok(self.attrs.get(&id).cloned().unwrap_or_default())
    }

    fn attrs_put(&mut self, id: AccountId, attrs: AttrMap) -> Result<()> {
        if attrs.is_empty() {
            return Ok(());
        }
        self.attrs.entry(id).or_default().extend(attrs);
        Ok(())
    }

    fn ban_put(&mut self, ban: Ban) -> Result<()> {
        self.bans.insert((ban.account, ban.scope), ban);
        Ok(())
    }

    fn ban_get(&mut self, id: AccountId, now: u64) -> Result<Option<Ban>> {
        // Network scope outranks node scope when both are present.
        for scope in [BanScope::Network, BanScope::Node] {
            if let Some(ban) = self.bans.get(&(id, scope)) {
                if ban.active_at(now) {
                    return Ok(Some(ban.clone()));
                }
            }
        }
        Ok(None)
    }

    fn ban_clear(&mut self, id: AccountId, scope: BanScope) -> Result<()> {
        self.bans.remove(&(id, scope));
        Ok(())
    }

    fn account_count(&mut self) -> Result<u64> {
        Ok(self.accounts.len() as u64)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance;

    #[test]
    fn memory_backend_passes_the_conformance_suite() {
        conformance::run(&mut MemoryStorage::new());
    }

    #[test]
    fn ids_are_not_reused_and_start_at_one() {
        let mut s = MemoryStorage::new();
        let a = s.create_account(conformance::account("one")).unwrap();
        let b = s.create_account(conformance::account("two")).unwrap();
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);
        assert_ne!(a.id, b.id);
    }
}
