//! Write-behind batching.
//!
//! Attribute writes are buffered and flushed in batches; **account creation, credential
//! changes and bans are write-through and never batched.** That split is the whole
//! design:
//!
//! - Losing a batch of attribute updates on an unclean shutdown costs a few minutes of
//!   ladder records and profile edits. Annoying, recoverable.
//! - Losing an account registration, a password change, or a ban is not recoverable and
//!   in two of those three cases is a security problem.
//!
//! PvPGN gets both halves wrong in the same loop: `accountlist_save()` runs on **every
//! iteration** of the main loop, writing up to 100 dirty accounts synchronously, on the
//! same thread as the socket handling. It pays the latency of a write-through design and
//! gets the durability of neither.

use std::collections::HashMap;

use bnetcc_core::AccountId;

use crate::attr::{AttrKey, AttrMap};
use crate::model::{Account, Ban, BanScope, Credential, NewAccount};
use crate::{Result, Storage};

/// When to flush buffered attribute writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushPolicy {
    /// Flush once this many accounts are dirty.
    pub max_dirty_accounts: usize,
    /// Flush once this many individual attributes are dirty.
    pub max_dirty_attrs: usize,
    /// Flush at least this often, in milliseconds. This bounds data loss on a crash.
    pub interval_ms: u64,
}

impl Default for FlushPolicy {
    fn default() -> Self {
        Self {
            max_dirty_accounts: 256,
            max_dirty_attrs: 4096,
            // Thirty seconds. The number that matters is "how much do we lose on a hard
            // kill", and thirty seconds of profile edits is an acceptable answer.
            interval_ms: 30_000,
        }
    }
}

/// Buffers attribute writes over a backend.
///
/// Reads are served from the buffer first, so a caller always sees its own writes even
/// before they reach the backend.
#[derive(Debug)]
pub struct WriteBehind<B: Storage> {
    backend: B,
    dirty: HashMap<AccountId, AttrMap>,
    dirty_attrs: usize,
    policy: FlushPolicy,
    last_flush_ms: u64,
    flushes: u64,
    batched_writes: u64,
}

impl<B: Storage> WriteBehind<B> {
    /// Wrap a backend.
    #[must_use]
    pub fn new(backend: B, policy: FlushPolicy, now_ms: u64) -> Self {
        Self {
            backend,
            dirty: HashMap::new(),
            dirty_attrs: 0,
            policy,
            last_flush_ms: now_ms,
            flushes: 0,
            batched_writes: 0,
        }
    }

    /// Accounts with unflushed attributes.
    #[must_use]
    pub fn dirty_accounts(&self) -> usize {
        self.dirty.len()
    }

    /// Individual unflushed attributes.
    #[must_use]
    pub const fn dirty_attrs(&self) -> usize {
        self.dirty_attrs
    }

    /// Flushes performed, for metrics.
    #[must_use]
    pub const fn flush_count(&self) -> u64 {
        self.flushes
    }

    /// Attribute writes absorbed by the buffer, for metrics.
    ///
    /// The ratio of this to [`Self::flush_count`] is the batching factor, and it is the
    /// number that tells you whether this layer is earning its complexity.
    #[must_use]
    pub const fn batched_writes(&self) -> u64 {
        self.batched_writes
    }

    /// Borrow the backend. For tests and for admin paths that must bypass the buffer.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Take the backend, **discarding any unflushed buffer**.
    ///
    /// This is what an unclean shutdown looks like, and it exists so that the durability
    /// contract can be tested rather than asserted.
    #[must_use]
    pub fn into_backend(self) -> B {
        self.backend
    }

    /// Flush if the policy says it is due.
    ///
    /// Called from a timer. Returns whether a flush happened.
    ///
    /// # Errors
    ///
    /// Backend failure. The buffer is **retained** on error so nothing is lost; the
    /// caller should log and let the next tick retry.
    pub fn tick(&mut self, now_ms: u64) -> Result<bool> {
        if self.dirty.is_empty() {
            self.last_flush_ms = now_ms;
            return Ok(false);
        }
        let due = self.dirty.len() >= self.policy.max_dirty_accounts
            || self.dirty_attrs >= self.policy.max_dirty_attrs
            || now_ms.saturating_sub(self.last_flush_ms) >= self.policy.interval_ms;
        if !due {
            return Ok(false);
        }
        self.flush_now(now_ms)?;
        Ok(true)
    }

    /// Flush unconditionally.
    ///
    /// # Errors
    ///
    /// Backend failure, with the buffer retained.
    pub fn flush_now(&mut self, now_ms: u64) -> Result<()> {
        // Drain into a local first so that a mid-way backend failure cannot leave the
        // buffer holding writes it already applied.
        let pending: Vec<(AccountId, AttrMap)> = self.dirty.drain().collect();

        // Attempt every dirty account rather than stopping at the first failure — one
        // account the backend refuses (a stale id, a lock) must not strand every other
        // account's writes in the buffer until it is resolved.
        let mut first_err = None;
        for (id, attrs) in pending {
            if let Err(e) = self.backend.attrs_put(id, attrs.clone()) {
                // Put it back so the next attempt retries rather than losing it.
                let slot = self.dirty.entry(id).or_default();
                for (k, v) in attrs {
                    slot.insert(k, v);
                }
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        self.dirty_attrs = self.dirty.values().map(BTreeLen::len_of).sum();
        if let Some(e) = first_err {
            return Err(e);
        }
        self.last_flush_ms = now_ms;
        self.flushes += 1;
        self.backend.flush()
    }
}

/// Tiny helper so the recount above reads clearly.
trait BTreeLen {
    fn len_of(&self) -> usize;
}
impl BTreeLen for AttrMap {
    fn len_of(&self) -> usize {
        self.len()
    }
}

impl<B: Storage> Storage for WriteBehind<B> {
    fn account_by_name(&mut self, name: &str) -> Result<Option<Account>> {
        self.backend.account_by_name(name)
    }

    fn account_by_id(&mut self, id: AccountId) -> Result<Option<Account>> {
        self.backend.account_by_id(id)
    }

    /// Write-through. See the module docs.
    fn create_account(&mut self, req: NewAccount) -> Result<Account> {
        self.backend.create_account(req)
    }

    /// Write-through. A password change that did not persist is a security problem.
    fn set_credential(&mut self, id: AccountId, credential: Credential) -> Result<()> {
        self.backend.set_credential(id, credential)
    }

    fn attrs_get(&mut self, id: AccountId, keys: &[AttrKey]) -> Result<AttrMap> {
        let mut out = self.backend.attrs_get(id, keys)?;
        if let Some(pending) = self.dirty.get(&id) {
            for key in keys {
                if let Some(v) = pending.get(key) {
                    out.insert(key.clone(), v.clone());
                }
            }
        }
        Ok(out)
    }

    fn attrs_all(&mut self, id: AccountId) -> Result<AttrMap> {
        let mut out = self.backend.attrs_all(id)?;
        if let Some(pending) = self.dirty.get(&id) {
            for (k, v) in pending {
                out.insert(k.clone(), v.clone());
            }
        }
        Ok(out)
    }

    /// Buffered. Visible immediately to readers, durable at the next flush.
    fn attrs_put(&mut self, id: AccountId, attrs: AttrMap) -> Result<()> {
        if attrs.is_empty() {
            return Ok(());
        }
        let slot = self.dirty.entry(id).or_default();
        for (k, v) in attrs {
            if slot.insert(k, v).is_none() {
                self.dirty_attrs += 1;
            }
            self.batched_writes += 1;
        }
        Ok(())
    }

    /// Write-through.
    fn ban_put(&mut self, ban: Ban) -> Result<()> {
        self.backend.ban_put(ban)
    }

    fn ban_get(&mut self, id: AccountId, now: u64) -> Result<Option<Ban>> {
        self.backend.ban_get(id, now)
    }

    fn ban_clear(&mut self, id: AccountId, scope: BanScope) -> Result<()> {
        self.backend.ban_clear(id, scope)
    }

    fn account_count(&mut self) -> Result<u64> {
        self.backend.account_count()
    }

    fn flush(&mut self) -> Result<()> {
        let now = self.last_flush_ms;
        self.flush_now(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::AttrKey;
    use crate::conformance;
    use crate::memory::MemoryStorage;

    fn wb() -> WriteBehind<MemoryStorage> {
        WriteBehind::new(MemoryStorage::new(), FlushPolicy::default(), 0)
    }

    fn attrs(pairs: &[(&str, &str)]) -> AttrMap {
        pairs
            .iter()
            .map(|(k, v)| (AttrKey::new(k), (*v).to_string()))
            .collect()
    }

    #[test]
    fn write_behind_passes_the_conformance_suite() {
        // Buffering must be invisible to a caller. If the same suite passes through the
        // buffer and against the raw backend, the layer is transparent.
        conformance::run(&mut wb());
    }

    #[test]
    fn a_reader_sees_its_own_unflushed_writes() {
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "Reykjavik")]))
            .unwrap();
        assert_eq!(s.dirty_accounts(), 1);

        let got = s.attrs_get(a.id, &[AttrKey::new(r"profile\location")]).unwrap();
        assert_eq!(got.get(&AttrKey::new(r"profile\location")).unwrap(), "Reykjavik");
        // …and the backend has not seen it yet.
        assert!(!s
            .backend_mut()
            .attrs_all(a.id)
            .unwrap()
            .contains_key(&AttrKey::new(r"profile\location")));
    }

    #[test]
    fn a_later_write_supersedes_an_earlier_buffered_one() {
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "one")])).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "two")])).unwrap();
        assert_eq!(s.dirty_attrs(), 1, "the same key must not count twice");
        s.flush_now(1).unwrap();
        assert_eq!(
            s.backend_mut().attrs_all(a.id).unwrap()[&AttrKey::new(r"profile\location")],
            "two"
        );
    }

    #[test]
    fn account_creation_is_write_through() {
        // Losing a registration is not recoverable; losing a profile edit is.
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        assert!(s.backend_mut().account_by_id(a.id).unwrap().is_some());
        assert_eq!(s.dirty_accounts(), 0);
    }

    #[test]
    fn credential_changes_and_bans_are_write_through() {
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.set_credential(a.id, Credential::Xsha1 { digest: [9; 20] })
            .unwrap();
        assert_eq!(
            s.backend_mut().account_by_id(a.id).unwrap().unwrap().credential,
            Credential::Xsha1 { digest: [9; 20] }
        );

        s.ban_put(Ban {
            account: a.id,
            scope: BanScope::Node,
            reason: "flooding".into(),
            applied_at: 0,
            expires_at: None,
        })
        .unwrap();
        assert!(s.backend_mut().ban_get(a.id, 0).unwrap().is_some());
        assert_eq!(s.dirty_accounts(), 0);
    }

    #[test]
    fn losing_the_buffer_loses_only_attributes() {
        // Simulates a hard kill: discard the buffer unflushed and inspect what survived.
        // This is the durability contract, stated as a test rather than as a promise.
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "lost")]))
            .unwrap();

        let mut backend = s.into_backend();
        assert!(
            backend.account_by_id(a.id).unwrap().is_some(),
            "the account must survive an unclean shutdown"
        );
        assert!(
            backend.attrs_all(a.id).unwrap().is_empty(),
            "unflushed attributes are expected to be lost — that is the trade"
        );
    }

    #[test]
    fn the_dirty_account_threshold_triggers_a_flush() {
        let policy = FlushPolicy {
            max_dirty_accounts: 3,
            max_dirty_attrs: usize::MAX,
            interval_ms: u64::MAX,
        };
        let mut s = WriteBehind::new(MemoryStorage::new(), policy, 0);
        let mut ids = Vec::new();
        for i in 0..3 {
            let a = s.create_account(conformance::account(&format!("u{i}"))).unwrap();
            ids.push(a.id);
            s.attrs_put(a.id, attrs(&[(r"profile\location", "x")])).unwrap();
        }
        assert!(s.tick(1).unwrap(), "three dirty accounts should flush");
        assert_eq!(s.dirty_accounts(), 0);
        for id in ids {
            assert!(!s.backend_mut().attrs_all(id).unwrap().is_empty());
        }
    }

    #[test]
    fn the_attribute_threshold_triggers_a_flush() {
        let policy = FlushPolicy {
            max_dirty_accounts: usize::MAX,
            max_dirty_attrs: 4,
            interval_ms: u64::MAX,
        };
        let mut s = WriteBehind::new(MemoryStorage::new(), policy, 0);
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(
            a.id,
            attrs(&[
                (r"Record\SEXP\0\wins", "1"),
                (r"Record\SEXP\0\losses", "2"),
                (r"Record\SEXP\0\disconnects", "3"),
            ]),
        )
        .unwrap();
        assert!(!s.tick(1).unwrap(), "three attributes is under the threshold");
        s.attrs_put(a.id, attrs(&[(r"profile\location", "x")])).unwrap();
        assert!(s.tick(2).unwrap(), "the fourth crosses it");
    }

    #[test]
    fn the_interval_bounds_how_much_can_be_lost() {
        let policy = FlushPolicy {
            max_dirty_accounts: usize::MAX,
            max_dirty_attrs: usize::MAX,
            interval_ms: 30_000,
        };
        let mut s = WriteBehind::new(MemoryStorage::new(), policy, 0);
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "x")])).unwrap();
        assert!(!s.tick(29_999).unwrap());
        assert!(s.tick(30_000).unwrap());
    }

    #[test]
    fn an_idle_server_does_not_flush() {
        // PvPGN calls accountlist_save() on every single loop iteration. An idle server
        // should do no storage work at all.
        let mut s = wb();
        for t in 0..1000 {
            assert!(!s.tick(t * 1000).unwrap());
        }
        assert_eq!(s.flush_count(), 0);
    }

    #[test]
    fn batching_actually_batches() {
        // The ratio this asserts is the justification for the whole layer existing.
        let mut s = wb();
        let a = s.create_account(conformance::account("zealot")).unwrap();
        for i in 0..500 {
            s.attrs_put(a.id, attrs(&[(r"Record\SEXP\0\wins", &i.to_string())]))
                .unwrap();
        }
        s.flush_now(1).unwrap();
        assert_eq!(s.batched_writes(), 500);
        assert_eq!(s.flush_count(), 1, "500 writes became one backend batch");
    }

    /// A backend that fails every attribute write, to check the retry contract.
    struct FailingAttrs(MemoryStorage);

    impl Storage for FailingAttrs {
        fn account_by_name(&mut self, n: &str) -> Result<Option<Account>> {
            self.0.account_by_name(n)
        }
        fn account_by_id(&mut self, i: AccountId) -> Result<Option<Account>> {
            self.0.account_by_id(i)
        }
        fn create_account(&mut self, r: NewAccount) -> Result<Account> {
            self.0.create_account(r)
        }
        fn set_credential(&mut self, i: AccountId, c: Credential) -> Result<()> {
            self.0.set_credential(i, c)
        }
        fn attrs_get(&mut self, i: AccountId, k: &[AttrKey]) -> Result<AttrMap> {
            self.0.attrs_get(i, k)
        }
        fn attrs_all(&mut self, i: AccountId) -> Result<AttrMap> {
            self.0.attrs_all(i)
        }
        fn attrs_put(&mut self, _i: AccountId, _a: AttrMap) -> Result<()> {
            Err(crate::StorageError::Backend("disk on fire".into()))
        }
        fn ban_put(&mut self, b: Ban) -> Result<()> {
            self.0.ban_put(b)
        }
        fn ban_get(&mut self, i: AccountId, n: u64) -> Result<Option<Ban>> {
            self.0.ban_get(i, n)
        }
        fn ban_clear(&mut self, i: AccountId, s: BanScope) -> Result<()> {
            self.0.ban_clear(i, s)
        }
        fn account_count(&mut self) -> Result<u64> {
            self.0.account_count()
        }
        fn flush(&mut self) -> Result<()> {
            self.0.flush()
        }
    }

    #[test]
    fn a_failed_flush_retains_the_buffer_rather_than_losing_it() {
        let mut s = WriteBehind::new(
            FailingAttrs(MemoryStorage::new()),
            FlushPolicy::default(),
            0,
        );
        let a = s.create_account(conformance::account("zealot")).unwrap();
        s.attrs_put(a.id, attrs(&[(r"profile\location", "keep me")]))
            .unwrap();
        assert!(s.flush_now(1).is_err());
        assert_eq!(s.dirty_accounts(), 1, "the buffer must be retained on failure");
        assert_eq!(s.dirty_attrs(), 1);
        // And the value is still readable, so the caller's view is unchanged.
        let got = s.attrs_get(a.id, &[AttrKey::new(r"profile\location")]).unwrap();
        assert_eq!(got[&AttrKey::new(r"profile\location")], "keep me");
    }
}
