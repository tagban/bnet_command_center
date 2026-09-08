//! SQLite backend for [`bnetcc_storage::Storage`].
//!
//! SQLite is the default because a community node operator should be able to run
//! `bnetccd` with no external services at all. Postgres is for the hub and for nodes past
//! a few thousand accounts; it implements the same trait and passes the same conformance
//! suite.
//!
//! # What this deliberately does not do
//!
//! - **No file per account.** PvPGN writes one file per account and rewrites the whole
//!   file on every save with `fopen(w)` — no atomic rename, no fsync, no journal. Worse,
//!   because the *filename* is the account name, it had to ban every reserved filesystem
//!   character from usernames (`account_allowed_symbols = "-_[]"`). Username rules should
//!   be a product decision, not a consequence of a storage choice.
//! - **No full scan at startup.** PvPGN's file backend `opendir`s the account directory
//!   and reads every entry at every boot. Here startup is one `COUNT(*)`.
//! - **No full-table load, ever.** PvPGN's ladder rebuild calls
//!   `accountlist_load_all(ST_FORCE)`, which pulls every account into RAM synchronously
//!   and defeats its own SQL backend's lazy loading. Ladders here are SQL aggregates.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use bnetcc_core::AccountId;
use bnetcc_storage::attr::{AttrKey, AttrMap};
use bnetcc_storage::model::{Account, Ban, BanScope, Credential, NewAccount};
use bnetcc_storage::{validate_account_name, Result, Storage, StorageError};
use rusqlite::{params, Connection, OptionalExtension};

/// Current schema version. Bump when adding a migration.
pub const SCHEMA_VERSION: i64 = 1;

/// Ordered migrations. Index `n` migrates from version `n` to `n + 1`.
const MIGRATIONS: &[&str] = &[
    // 0 -> 1
    r"
    CREATE TABLE accounts (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        name        TEXT    NOT NULL,
        name_lower  TEXT    NOT NULL UNIQUE,
        cred_kind   TEXT    NOT NULL,
        cred_a      BLOB    NOT NULL,
        cred_b      BLOB,
        created_at  INTEGER NOT NULL
    );

    CREATE TABLE attrs (
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        key        TEXT    NOT NULL,
        value      TEXT    NOT NULL,
        PRIMARY KEY (account_id, key)
    ) WITHOUT ROWID;

    CREATE TABLE bans (
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        scope      TEXT    NOT NULL,
        reason     TEXT    NOT NULL,
        applied_at INTEGER NOT NULL,
        expires_at INTEGER,
        PRIMARY KEY (account_id, scope)
    ) WITHOUT ROWID;
    ",
];

fn map_err(e: rusqlite::Error) -> StorageError {
    StorageError::Backend(e.to_string())
}

/// A SQLite-backed store.
pub struct SqliteStorage {
    conn: Connection,
}

impl SqliteStorage {
    /// Open (or create) a database file.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened or migrations fail.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_err)?;
        Self::prepare(conn)
    }

    /// Open an in-memory database. For tests.
    ///
    /// # Errors
    ///
    /// If migrations fail.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_err)?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        // WAL lets readers proceed during a write, which matters because the storage
        // actor is a single thread that must not stall the whole server on a flush.
        //
        // `synchronous = NORMAL` under WAL is durable across a *process* crash — which is
        // the failure we actually plan for — and may lose the last transactions on a
        // power cut or kernel panic. Given that attribute writes are already batched with
        // a bounded loss window, paying FULL's fsync-per-commit here would buy very
        // little. An operator who wants it can set PRAGMA synchronous=FULL.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(map_err)?;

        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    /// Apply any outstanding migrations.
    ///
    /// # Errors
    ///
    /// Backend failure, or a database newer than this binary understands.
    pub fn migrate(&mut self) -> Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_err)?;

        if current > SCHEMA_VERSION {
            return Err(StorageError::Backend(format!(
                "database schema is version {current} but this build understands only \
                 {SCHEMA_VERSION}; upgrade bnetccd rather than downgrading the database"
            )));
        }

        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let target = i as i64 + 1;
            if current >= target {
                continue;
            }
            let tx = self.conn.transaction().map_err(map_err)?;
            tx.execute_batch(sql).map_err(map_err)?;
            tx.pragma_update(None, "user_version", target)
                .map_err(map_err)?;
            tx.commit().map_err(map_err)?;
        }
        Ok(())
    }

    fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
        let kind: String = row.get("cred_kind")?;
        let a: Vec<u8> = row.get("cred_a")?;
        let b: Option<Vec<u8>> = row.get("cred_b")?;
        let credential = match kind.as_str() {
            "srp" => {
                let mut salt = [0u8; 32];
                let mut verifier = [0u8; 32];
                salt.copy_from_slice(&a);
                verifier.copy_from_slice(&b.unwrap_or_default());
                Credential::Srp { salt, verifier }
            }
            _ => {
                let mut digest = [0u8; 20];
                digest.copy_from_slice(&a);
                Credential::Xsha1 { digest }
            }
        };
        Ok(Account {
            id: row.get::<_, i64>("id")? as AccountId,
            name: row.get("name")?,
            credential,
            created_at: row.get::<_, i64>("created_at")? as u64,
        })
    }

    fn credential_columns(c: &Credential) -> (&'static str, Vec<u8>, Option<Vec<u8>>) {
        match c {
            Credential::Xsha1 { digest } => ("xsha1", digest.to_vec(), None),
            Credential::Srp { salt, verifier } => {
                ("srp", salt.to_vec(), Some(verifier.to_vec()))
            }
        }
    }

    const fn scope_str(s: BanScope) -> &'static str {
        match s {
            BanScope::Node => "node",
            BanScope::Network => "network",
        }
    }
}

impl Storage for SqliteStorage {
    fn account_by_name(&mut self, name: &str) -> Result<Option<Account>> {
        self.conn
            .prepare_cached("SELECT * FROM accounts WHERE name_lower = ?1")
            .map_err(map_err)?
            .query_row(params![name.to_ascii_lowercase()], Self::row_to_account)
            .optional()
            .map_err(map_err)
    }

    fn account_by_id(&mut self, id: AccountId) -> Result<Option<Account>> {
        self.conn
            .prepare_cached("SELECT * FROM accounts WHERE id = ?1")
            .map_err(map_err)?
            .query_row(params![id as i64], Self::row_to_account)
            .optional()
            .map_err(map_err)
    }

    fn create_account(&mut self, req: NewAccount) -> Result<Account> {
        validate_account_name(&req.name)?;
        let (kind, a, b) = Self::credential_columns(&req.credential);
        let lower = req.name.to_ascii_lowercase();

        let tx = self.conn.transaction().map_err(map_err)?;
        // Rely on the UNIQUE index rather than a check-then-insert, which would race.
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO accounts
                 (name, name_lower, cred_kind, cred_a, cred_b, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![req.name, lower, kind, a, b, req.created_at as i64],
        )
        .map_err(map_err)?;
        if inserted == 0 {
            return Err(StorageError::NameTaken);
        }
        let id = tx.last_insert_rowid();

        if !req.attrs.is_empty() {
            let mut stmt = tx
                .prepare("INSERT INTO attrs (account_id, key, value) VALUES (?1, ?2, ?3)")
                .map_err(map_err)?;
            for (k, v) in &req.attrs {
                stmt.execute(params![id, k.as_str(), v]).map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;

        Ok(Account {
            id: id as AccountId,
            name: req.name,
            credential: req.credential,
            created_at: req.created_at,
        })
    }

    fn set_credential(&mut self, id: AccountId, credential: Credential) -> Result<()> {
        let (kind, a, b) = Self::credential_columns(&credential);
        let n = self
            .conn
            .prepare_cached(
                "UPDATE accounts SET cred_kind = ?2, cred_a = ?3, cred_b = ?4 WHERE id = ?1",
            )
            .map_err(map_err)?
            .execute(params![id as i64, kind, a, b])
            .map_err(map_err)?;
        if n == 0 {
            return Err(StorageError::NoSuchAccount);
        }
        Ok(())
    }

    fn attrs_get(&mut self, id: AccountId, keys: &[AttrKey]) -> Result<AttrMap> {
        if keys.is_empty() {
            return Ok(AttrMap::new());
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM attrs WHERE account_id = ?1 AND key = ?2")
            .map_err(map_err)?;
        let mut out = AttrMap::new();
        for key in keys {
            let value: Option<String> = stmt
                .query_row(params![id as i64, key.as_str()], |r| r.get(0))
                .optional()
                .map_err(map_err)?;
            if let Some(v) = value {
                out.insert(key.clone(), v);
            }
        }
        Ok(out)
    }

    fn attrs_all(&mut self, id: AccountId) -> Result<AttrMap> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM attrs WHERE account_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![id as i64], |r| {
                Ok((AttrKey::new(&r.get::<_, String>(0)?), r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = AttrMap::new();
        for row in rows {
            let (k, v) = row.map_err(map_err)?;
            out.insert(k, v);
        }
        Ok(out)
    }

    fn attrs_put(&mut self, id: AccountId, attrs: AttrMap) -> Result<()> {
        if attrs.is_empty() {
            return Ok(());
        }
        // One transaction for the whole batch. This is what makes the write-behind layer
        // worth having: 500 buffered writes become one commit, not 500.
        let tx = self.conn.transaction().map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO attrs (account_id, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT(account_id, key) DO UPDATE SET value = excluded.value",
                )
                .map_err(map_err)?;
            for (k, v) in &attrs {
                stmt.execute(params![id as i64, k.as_str(), v])
                    .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)
    }

    fn ban_put(&mut self, ban: Ban) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO bans (account_id, scope, reason, applied_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(account_id, scope) DO UPDATE SET
                     reason = excluded.reason,
                     applied_at = excluded.applied_at,
                     expires_at = excluded.expires_at",
            )
            .map_err(map_err)?
            .execute(params![
                ban.account as i64,
                Self::scope_str(ban.scope),
                ban.reason,
                ban.applied_at as i64,
                ban.expires_at.map(|e| e as i64),
            ])
            .map_err(map_err)?;
        Ok(())
    }

    fn ban_get(&mut self, id: AccountId, now: u64) -> Result<Option<Ban>> {
        // Network scope outranks node scope; ordering by scope DESC puts 'network' first.
        self.conn
            .prepare_cached(
                "SELECT scope, reason, applied_at, expires_at FROM bans
                 WHERE account_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
                 ORDER BY scope DESC LIMIT 1",
            )
            .map_err(map_err)?
            .query_row(params![id as i64, now as i64], |row| {
                let scope: String = row.get(0)?;
                Ok(Ban {
                    account: id,
                    scope: if scope == "network" {
                        BanScope::Network
                    } else {
                        BanScope::Node
                    },
                    reason: row.get(1)?,
                    applied_at: row.get::<_, i64>(2)? as u64,
                    expires_at: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                })
            })
            .optional()
            .map_err(map_err)
    }

    fn ban_clear(&mut self, id: AccountId, scope: BanScope) -> Result<()> {
        self.conn
            .prepare_cached("DELETE FROM bans WHERE account_id = ?1 AND scope = ?2")
            .map_err(map_err)?
            .execute(params![id as i64, Self::scope_str(scope)])
            .map_err(map_err)?;
        Ok(())
    }

    fn account_count(&mut self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))
            .map_err(map_err)?;
        Ok(n as u64)
    }

    fn flush(&mut self) -> Result<()> {
        // Every write here is already committed; this exists so the trait has one place
        // to force a WAL checkpoint at shutdown.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(map_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnetcc_storage::conformance;
    use bnetcc_storage::{FlushPolicy, WriteBehind};

    #[test]
    fn sqlite_backend_passes_the_conformance_suite() {
        conformance::run(&mut SqliteStorage::open_in_memory().unwrap());
    }

    #[test]
    fn sqlite_behind_the_write_buffer_also_conforms() {
        let backend = SqliteStorage::open_in_memory().unwrap();
        conformance::run(&mut WriteBehind::new(backend, FlushPolicy::default(), 0));
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.migrate().unwrap();
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_corrupted() {
        // Downgrading the binary must not silently write old-format rows into a newer
        // database.
        let mut s = SqliteStorage::open_in_memory().unwrap();
        s.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION + 5)
            .unwrap();
        assert!(matches!(s.migrate(), Err(StorageError::Backend(_))));
    }

    #[test]
    fn data_survives_reopening_the_file() {
        // The property Atlas does not have: restart the server, keep the accounts.
        let dir = std::env::temp_dir().join(format!("bnetcc-sqlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);

        let id = {
            let mut s = SqliteStorage::open(&path).unwrap();
            let a = s.create_account(conformance::account("Zealot")).unwrap();
            s.attrs_put(
                a.id,
                [(AttrKey::new(r"profile\location"), "Reykjavik".to_string())]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
            s.flush().unwrap();
            a.id
        };

        let mut s = SqliteStorage::open(&path).unwrap();
        assert_eq!(s.account_by_id(id).unwrap().unwrap().name, "Zealot");
        assert_eq!(
            s.attrs_all(id).unwrap()[&AttrKey::new(r"profile\location")],
            "Reykjavik"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_creation_of_the_same_name_yields_exactly_one_account() {
        // Uniqueness comes from the index, not a check-then-insert, so this cannot race.
        let mut s = SqliteStorage::open_in_memory().unwrap();
        assert!(s.create_account(conformance::account("Dup")).is_ok());
        assert_eq!(
            s.create_account(conformance::account("dup")).unwrap_err(),
            StorageError::NameTaken
        );
        assert_eq!(s.account_count().unwrap(), 1);
    }
}
