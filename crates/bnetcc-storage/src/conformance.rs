//! A conformance suite every [`Storage`] backend must pass.
//!
//! Written against the observable behaviour of [`crate::MemoryStorage`], which is the
//! reference. Call [`run`] from a `#[test]` in each backend:
//!
//! ```ignore
//! #[test]
//! fn sqlite_backend_conforms() {
//!     bnetcc_storage::conformance::run(&mut SqliteStorage::open_in_memory().unwrap());
//! }
//! ```
//!
//! Having one suite rather than per-backend tests is what makes swapping SQLite for
//! Postgres a decision rather than a rewrite — and it is what catches the subtle
//! divergences (case folding, merge-versus-replace on attribute writes, which ban scope
//! wins) that otherwise only show up in production on one backend.

use crate::attr::{AttrKey, AttrMap};
use crate::model::{Ban, BanScope, Character, Credential, NewAccount};
use crate::{Storage, StorageError};

/// A `NewAccount` with sensible defaults, for tests.
#[must_use]
pub fn account(name: &str) -> NewAccount {
    NewAccount {
        name: name.to_string(),
        credential: Credential::Xsha1 { digest: [1; 20] },
        created_at: 1_700_000_000,
        attrs: AttrMap::new(),
    }
}

fn attrs(pairs: &[(&str, &str)]) -> AttrMap {
    pairs
        .iter()
        .map(|(k, v)| (AttrKey::new(k), (*v).to_string()))
        .collect()
}

/// Run the full suite. Panics on the first divergence.
///
/// # Panics
///
/// If the backend does not match the reference behaviour.
pub fn run<S: Storage>(s: &mut S) {
    accounts(s);
    attributes(s);
    bans(s);
    counting(s);
    enumeration_and_deletion(s);
    characters(s);
}

/// Account creation, lookup and credentials.
///
/// # Panics
///
/// On divergence.
pub fn accounts<S: Storage>(s: &mut S) {
    let created = s.create_account(account("Zealot")).expect("create");
    assert_eq!(created.name, "Zealot", "display case must be preserved");

    // Lookup is case-insensitive in both directions.
    for probe in ["Zealot", "zealot", "ZEALOT", "ZeAlOt"] {
        let found = s
            .account_by_name(probe)
            .expect("lookup")
            .unwrap_or_else(|| panic!("{probe} not found"));
        assert_eq!(found.id, created.id);
    }
    assert_eq!(
        s.account_by_id(created.id).expect("by id").unwrap().name,
        "Zealot"
    );
    assert!(s.account_by_name("nobody").expect("miss").is_none());
    assert!(s.account_by_id(999_999).expect("miss").is_none());

    // Names are unique regardless of case.
    assert_eq!(
        s.create_account(account("zealot")).unwrap_err(),
        StorageError::NameTaken
    );

    // Invalid names are rejected before anything is written.
    assert!(matches!(
        s.create_account(account("")).unwrap_err(),
        StorageError::InvalidName(_)
    ));
    assert!(matches!(
        s.create_account(account(&"a".repeat(64))).unwrap_err(),
        StorageError::InvalidName(_)
    ));

    // Credentials round-trip, including the SRP variant.
    let srp = Credential::Srp {
        salt: [3; 32],
        verifier: [4; 32],
    };
    s.set_credential(created.id, srp.clone()).expect("set cred");
    assert_eq!(
        s.account_by_id(created.id).unwrap().unwrap().credential,
        srp
    );
    assert_eq!(
        s.set_credential(999_999, srp).unwrap_err(),
        StorageError::NoSuchAccount
    );

    // Attributes supplied at creation are stored.
    let with_attrs = NewAccount {
        attrs: attrs(&[(r"profile\location", "Reykjavik")]),
        ..account("Templar")
    };
    let t = s.create_account(with_attrs).expect("create with attrs");
    assert_eq!(
        s.attrs_all(t.id).expect("attrs")[&AttrKey::new(r"profile\location")],
        "Reykjavik"
    );
}

/// Attribute read/write semantics.
///
/// # Panics
///
/// On divergence.
pub fn attributes<S: Storage>(s: &mut S) {
    let a = s.create_account(account("Archon")).expect("create");

    // An account with no attributes reads back empty, not an error.
    assert!(s.attrs_all(a.id).expect("empty").is_empty());
    assert!(s
        .attrs_get(a.id, &[AttrKey::new(r"profile\location")])
        .expect("miss")
        .is_empty());

    s.attrs_put(
        a.id,
        attrs(&[
            (r"profile\location", "Reykjavik"),
            (r"Record\SEXP\0\wins", "3"),
        ]),
    )
    .expect("put");

    // Writes merge; they do not replace the whole set.
    s.attrs_put(a.id, attrs(&[(r"Record\SEXP\0\losses", "1")]))
        .expect("put 2");
    let all = s.attrs_all(a.id).expect("all");
    assert_eq!(all.len(), 3, "a second write must not clear the first");

    // A later write to the same key wins.
    s.attrs_put(a.id, attrs(&[(r"Record\SEXP\0\wins", "4")]))
        .expect("put 3");
    assert_eq!(
        s.attrs_all(a.id).expect("all")[&AttrKey::new(r"Record\SEXP\0\wins")],
        "4"
    );

    // Selective reads return only what was asked for, and silently omit misses.
    let got = s
        .attrs_get(
            a.id,
            &[
                AttrKey::new(r"Record\SEXP\0\wins"),
                AttrKey::new(r"nothing\here"),
            ],
        )
        .expect("get");
    assert_eq!(got.len(), 1);

    // Keys are case-insensitive.
    let got = s
        .attrs_get(a.id, &[AttrKey::new(r"RECORD\sexp\0\WINS")])
        .expect("case");
    assert_eq!(got.len(), 1, "attribute keys must fold case");

    // Empty writes are a no-op, not an error.
    s.attrs_put(a.id, AttrMap::new()).expect("empty put");

    // Attributes on a non-existent account must not panic. Whether they are stored or
    // dropped is backend-defined; not crashing is not.
    let _ = s.attrs_put(999_999, attrs(&[("x", "y")]));

    // Values survive a flush.
    s.flush().expect("flush");
    assert_eq!(s.attrs_all(a.id).expect("after flush").len(), 3);
    s.flush().expect("flush is idempotent");
}

/// Ban application, expiry and scope precedence.
///
/// # Panics
///
/// On divergence.
pub fn bans<S: Storage>(s: &mut S) {
    let a = s.create_account(account("Dragoon")).expect("create");
    assert!(s.ban_get(a.id, 0).expect("no ban").is_none());

    s.ban_put(Ban {
        account: a.id,
        scope: BanScope::Node,
        reason: "flooding".into(),
        applied_at: 100,
        expires_at: Some(200),
    })
    .expect("ban");

    assert!(s.ban_get(a.id, 150).expect("active").is_some());
    assert!(
        s.ban_get(a.id, 200).expect("expired").is_none(),
        "a ban must lapse at its expiry, not after it"
    );
    assert!(s.ban_get(a.id, 999).expect("expired").is_none());

    // A network ban outranks a node ban when both are present.
    s.ban_put(Ban {
        account: a.id,
        scope: BanScope::Network,
        reason: "ladder forgery".into(),
        applied_at: 100,
        expires_at: None,
    })
    .expect("network ban");
    let active = s.ban_get(a.id, 150).expect("active").expect("some");
    assert_eq!(active.scope, BanScope::Network);

    // Clearing one scope leaves the other.
    s.ban_clear(a.id, BanScope::Network).expect("clear");
    let active = s.ban_get(a.id, 150).expect("active").expect("node ban remains");
    assert_eq!(active.scope, BanScope::Node);
    s.ban_clear(a.id, BanScope::Node).expect("clear");
    assert!(s.ban_get(a.id, 150).expect("cleared").is_none());

    // Clearing a ban that does not exist is not an error.
    s.ban_clear(a.id, BanScope::Network).expect("idempotent");
}

/// Account counting.
///
/// # Panics
///
/// On divergence.
pub fn counting<S: Storage>(s: &mut S) {
    let before = s.account_count().expect("count");
    s.create_account(account("Reaver")).expect("create");
    assert_eq!(s.account_count().expect("count"), before + 1);
    // A failed creation must not change the count.
    let _ = s.create_account(account("Reaver"));
    assert_eq!(s.account_count().expect("count"), before + 1);
}

/// Paged enumeration and account deletion, for the admin user list.
///
/// # Panics
///
/// On divergence.
pub fn enumeration_and_deletion<S: Storage>(s: &mut S) {
    let a = s.create_account(account("EnumOne")).expect("create");
    let b = s.create_account(account("EnumTwo")).expect("create");
    s.attrs_put(a.id, attrs(&[(r"profile\location", "keep")])).expect("attr");

    // A page large enough for everything includes both new accounts, and the list is
    // ordered by id ascending.
    let total = s.account_count().expect("count");
    let all = s.list_accounts(0, u32::try_from(total).unwrap_or(u32::MAX)).expect("list");
    assert!(all.iter().any(|acc| acc.id == a.id));
    assert!(all.iter().any(|acc| acc.id == b.id));
    assert!(all.windows(2).all(|w| w[0].id < w[1].id), "list_accounts is ordered by id");

    // `limit` caps the page and `offset` advances the window.
    let first = s.list_accounts(0, 1).expect("first");
    assert_eq!(first.len(), 1);
    let second = s.list_accounts(1, 1).expect("second");
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id, "offset skips the earlier account");

    // Deletion removes the account, its lookups, and its attributes.
    s.delete_account(a.id).expect("delete");
    assert!(s.account_by_id(a.id).expect("by id").is_none());
    assert!(s.account_by_name("EnumOne").expect("by name").is_none());
    assert!(s.attrs_all(a.id).expect("attrs").is_empty(), "attributes are removed with the account");
    // The freed name can be registered again, with a fresh id.
    let reborn = s.create_account(account("EnumOne")).expect("recreate");
    assert_ne!(reborn.id, a.id, "a deleted id is not reused");
    // Deleting an absent account is a no-op, not an error.
    s.delete_account(999_999).expect("idempotent delete");
}

fn character(account: bnetcc_core::AccountId, name: &str) -> Character {
    Character {
        account,
        name: name.to_string(),
        class: 1,
        status: 0x20,
        level: 1,
        progression: 0,
        created_at: 1_700_000_000,
        last_played: 1_700_000_000,
        save: None,
    }
}

/// Diablo II realm characters: realm-wide unique names, per-account listing, update and
/// deletion scoped to the owner, and removal with the account.
///
/// # Panics
///
/// On divergence.
pub fn characters<S: Storage>(s: &mut S) {
    let owner = s.create_account(account("CharOwner")).expect("create");
    let other = s.create_account(account("CharOther")).expect("create");
    assert!(s.characters(owner.id).expect("list").is_empty());

    s.create_character(character(owner.id, "Tyrael")).expect("create char");
    s.create_character(character(owner.id, "Deckard")).expect("create second char");
    let listed = s.characters(owner.id).expect("list");
    let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Tyrael", "Deckard"], "listed oldest first, case preserved");

    // Names are unique across the whole realm, case-insensitively.
    assert_eq!(
        s.create_character(character(other.id, "TYRAEL")),
        Err(StorageError::NameTaken),
        "another account may not take a held name"
    );
    assert_eq!(s.create_character(character(owner.id, "tyrael")), Err(StorageError::NameTaken));
    assert_eq!(
        s.create_character(character(999_999, "Orphan")),
        Err(StorageError::NoSuchAccount)
    );

    let found = s.character_by_name("tyRAEL").expect("lookup").expect("present");
    assert_eq!(found.account, owner.id);
    assert_eq!(found.name, "Tyrael");

    // Update touches the mutable fields, and only for the owner.
    let mut upgraded = found.clone();
    upgraded.status = 0x24;
    upgraded.level = 12;
    upgraded.save = Some(vec![0x55, 0xAA, 0x96]);
    assert!(s.update_character(&upgraded).expect("update"));
    let reread = s.character_by_name("Tyrael").expect("lookup").expect("present");
    assert_eq!((reread.status, reread.level), (0x24, 12));
    assert_eq!(reread.save.as_deref(), Some(&[0x55, 0xAA, 0x96][..]));
    let mut stolen = upgraded.clone();
    stolen.account = other.id;
    assert!(!s.update_character(&stolen).expect("update"), "a non-owner cannot update");

    // Deletion is scoped to the owner and frees the name.
    assert!(!s.delete_character(other.id, "Tyrael").expect("delete"), "not theirs to delete");
    assert!(s.delete_character(owner.id, "TYRAEL").expect("delete"));
    assert!(s.character_by_name("Tyrael").expect("lookup").is_none());
    assert!(!s.delete_character(owner.id, "Tyrael").expect("delete"), "already gone");
    s.create_character(character(other.id, "Tyrael")).expect("a freed name can be reused");

    // Characters go with their account.
    s.delete_account(owner.id).expect("delete account");
    assert!(s.character_by_name("Deckard").expect("lookup").is_none());
}
