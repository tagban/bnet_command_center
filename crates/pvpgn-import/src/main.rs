//! `bnetcc-pvpgn-import` — bring a PvPGN server's players into Command Center.
//!
//! Reads PvPGN's accounts (plain-file storage, or a SQL dump of its database) and, optionally,
//! its Diablo II character server's characters, and writes them into a Command Center database.
//! Without `--apply` it only reports what it would do. See `docs/PVPGN-IMPORT.md`.

#![forbid(unsafe_code)]

mod d2;
mod plan;
mod source;

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use bnetcc_storage::model::{Ban, BanScope, Character, Credential, NewAccount};
use bnetcc_storage::{Storage, StorageError};
use bnetcc_storage_sqlite::SqliteStorage;
use clap::{Parser, ValueEnum};

use crate::plan::{HashOrder, Options, Plan, SrpOrder};

/// Bring a PvPGN server's accounts, profiles, records and Diablo II characters into Command Center.
///
/// Stop both servers first. Without --apply nothing is written: you get a report of what would
/// be imported and what would be left out.
#[derive(Debug, Parser)]
#[command(name = "bnetcc-pvpgn-import", version)]
struct Args {
    /// PvPGN's plain-file account directory (`storage_path = file:mode=plain;dir=…`).
    #[arg(long, value_name = "DIR", conflicts_with = "sql_dump")]
    users: Option<PathBuf>,
    /// A SQL dump of PvPGN's database (mysqldump, pg_dump --inserts, or sqlite3 .dump).
    #[arg(long, value_name = "FILE")]
    sql_dump: Option<PathBuf>,
    /// The Diablo II character server's charinfo directory (with --charsave).
    #[arg(long, value_name = "DIR", requires = "charsave")]
    charinfo: Option<PathBuf>,
    /// The Diablo II character server's charsave directory (with --charinfo).
    #[arg(long, value_name = "DIR", requires = "charinfo")]
    charsave: Option<PathBuf>,
    /// The Command Center database to import into (`[storage] path`, usually bnetccd.db).
    #[arg(long, value_name = "FILE")]
    db: PathBuf,
    /// `[server] realm` from bnetccd.toml: WarCraft III accounts become `Name@realm`.
    #[arg(long, default_value = "bncc")]
    realm: String,
    /// How PvPGN wrote password hashes. Confirm with --check-password.
    #[arg(long, value_enum)]
    hash_order: Option<HashOrderArg>,
    /// How PvPGN wrote WarCraft III salts and verifiers. Confirm with --check-password.
    #[arg(long, value_enum)]
    srp_order: Option<SrpOrderArg>,
    /// Ask for one account's password (typed, never stored) and find the hash and SRP orders
    /// that match it. Use an account whose password you know, such as your own.
    #[arg(long, value_name = "ACCOUNT")]
    check_password: Option<String>,
    /// Write the import. Without it, only report.
    #[arg(long)]
    apply: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HashOrderArg {
    /// Five hex words, each sent little-endian (PvPGN's usual form).
    Words,
    /// Hex bytes in order.
    Bytes,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SrpOrderArg {
    /// Hex bytes in order.
    AsIs,
    /// A big-endian number, reversed into the logon's bytes.
    Reversed,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Read a password from the terminal without echoing it (where `stty` exists).
fn read_password(prompt: &str) -> std::io::Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let hidden = std::process::Command::new("stty").arg("-echo").stdin(std::process::Stdio::inherit()).status().is_ok_and(|s| s.success());
    let mut line = String::new();
    let result = std::io::stdin().lock().read_line(&mut line);
    if hidden {
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
        eprintln!();
    }
    result?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Which orders make `password` match the account's stored hash and SRP fields.
fn check_password(accounts: &[source::PvpgnAccount], name: &str, password: &str) -> (Option<HashOrder>, Option<SrpOrder>, String) {
    let creds = plan::credentials_by_name(accounts);
    let Some((hash, srp)) = creds.get(&name.to_ascii_lowercase()) else {
        return (None, None, format!("no account named {name} in the PvPGN data"));
    };
    let mut report = Vec::new();
    let expected = bnetcc_crypto::xsha1::password_hash(password);
    let hash_order = hash.as_deref().and_then(|h| [HashOrder::Words, HashOrder::Bytes].into_iter().find(|&o| plan::digest(h, o) == Some(expected)));
    match (hash, hash_order) {
        (None, _) => report.push("no StarCraft/Diablo II password hash".to_string()),
        (Some(_), Some(order)) => report.push(format!("password hash matches, order: {order:?}")),
        (Some(_), None) => report.push("password hash does NOT match that password in either order".to_string()),
    }
    let srp_order = srp.as_ref().and_then(|(salt, verifier)| {
        [SrpOrder::AsIs, SrpOrder::Reversed].into_iter().find(|&o| {
            match (plan::srp_field(salt, o), plan::srp_field(verifier, o)) {
                (Some(salt), Some(stored)) => bnetcc_crypto::nls::verifier(name, password, &salt) == stored,
                _ => false,
            }
        })
    });
    match (srp, srp_order) {
        (None, _) => report.push("no WarCraft III salt and verifier".to_string()),
        (Some(_), Some(order)) => report.push(format!("WarCraft III verifier matches, order: {order:?}")),
        (Some(_), None) => report.push("WarCraft III verifier does NOT match that password in either order".to_string()),
    }
    (hash_order, srp_order, report.join("; "))
}

fn print_plan(plan: &Plan, characters: &[d2::PlannedCharacter], character_notes: &[String]) {
    let xsha1 = plan.accounts.iter().filter(|a| matches!(a.credential, Credential::Xsha1 { .. })).count();
    println!("Accounts to create: {} ({} StarCraft/Diablo II/Warcraft II, {} WarCraft III)", plan.accounts.len(), xsha1, plan.accounts.len() - xsha1);
    println!("  with bans from PvPGN locks: {}", plan.accounts.iter().filter(|a| a.ban.is_some()).count());
    println!("Diablo II characters to create: {}", characters.len());
    if !plan.skipped.is_empty() || !character_notes.is_empty() {
        println!("Left out:");
        for note in plan.skipped.iter().chain(character_notes) {
            println!("  - {note}");
        }
    }
    if plan.srp_left > 0 {
        println!("WarCraft III credentials not imported: {} (confirm the order with --check-password, or give --srp-order)", plan.srp_left);
    }
    if !plan.admins.is_empty() {
        println!("PvPGN admins (grant them in bnetccd.toml [admins] accounts if you want): {}", plan.admins.join(", "));
    }
    if !plan.operators.is_empty() {
        println!("PvPGN operators (no global equivalent; not carried over): {}", plan.operators.join(", "));
    }
    println!(
        "Not carried over: {} e-mail addresses, {} friends lists, {} mutes; clans are not imported.",
        plan.emails_left, plan.friends_left, plan.mutes_left
    );
}

/// Write the plan. Returns (accounts made, characters made, notes).
fn apply(storage: &mut SqliteStorage, plan: &Plan, characters: &[d2::PlannedCharacter]) -> Result<(usize, usize, Vec<String>), StorageError> {
    let mut notes = Vec::new();
    let mut made = 0;
    let mut ids = BTreeMap::new();
    for account in &plan.accounts {
        let request = NewAccount { name: account.name.clone(), credential: account.credential.clone(), created_at: account.created_at, attrs: account.attrs.clone() };
        match storage.create_account(request) {
            Ok(created) => {
                made += 1;
                if let Some((reason, expires_at)) = &account.ban {
                    storage.ban_put(Ban { account: created.id, scope: BanScope::Node, reason: reason.clone(), applied_at: now(), expires_at: *expires_at })?;
                }
                ids.insert(created.name.to_ascii_lowercase(), created.id);
            }
            Err(StorageError::NameTaken) => notes.push(format!("{}: the name is already taken on this server; left as it is", account.name)),
            Err(e) => return Err(e),
        }
    }
    let mut chars = 0;
    for c in characters {
        let owner = match ids.get(&c.account.to_ascii_lowercase()) {
            Some(id) => Some(*id),
            None => storage.account_by_name(&c.account)?.filter(|a| matches!(a.credential, Credential::Xsha1 { .. })).map(|a| a.id),
        };
        let Some(account) = owner else {
            notes.push(format!("{}: its account {} is not on this server", c.name, c.account));
            continue;
        };
        let character = Character {
            account,
            name: c.name.clone(),
            class: c.class,
            status: c.status,
            level: c.level,
            progression: c.progression,
            created_at: c.last_played.min(now()),
            last_played: c.last_played,
            save: Some(c.save.clone()),
        };
        match storage.create_character(character) {
            Ok(()) => chars += 1,
            Err(StorageError::NameTaken) => notes.push(format!("{}: a character by that name already exists on this server", c.name)),
            Err(e) => return Err(e),
        }
    }
    storage.flush()?;
    Ok((made, chars, notes))
}

fn run(args: Args) -> Result<(), String> {
    let accounts = match (&args.users, &args.sql_dump) {
        (Some(dir), _) => source::read_plain_dir(dir).map_err(|e| e.to_string())?,
        (None, Some(dump)) => source::read_sql_dump(dump).map_err(|e| e.to_string())?,
        (None, None) if args.charinfo.is_some() => Vec::new(),
        (None, None) => return Err("give PvPGN's accounts with --users DIR or --sql-dump FILE (and/or characters with --charinfo and --charsave)".into()),
    };
    println!("Read {} PvPGN accounts.", accounts.len());

    let mut hash_order = args.hash_order.map(|o| match o {
        HashOrderArg::Words => HashOrder::Words,
        HashOrderArg::Bytes => HashOrder::Bytes,
    });
    let mut srp_order = args.srp_order.map(|o| match o {
        SrpOrderArg::AsIs => SrpOrder::AsIs,
        SrpOrderArg::Reversed => SrpOrder::Reversed,
    });
    if let Some(name) = &args.check_password {
        let password = read_password(&format!("Password for {name} on the PvPGN server: ")).map_err(|e| e.to_string())?;
        let (found_hash, found_srp, report) = check_password(&accounts, name, &password);
        println!("Checking {name}: {report}");
        hash_order = hash_order.or(found_hash);
        srp_order = srp_order.or(found_srp);
    }

    let has_hashes = accounts.iter().any(|a| a.get(r"BNET\acct\passhash1").is_some_and(|h| !h.trim().is_empty()));
    let opts = Options { realm: args.realm.clone(), hash_order: hash_order.unwrap_or(HashOrder::Words), srp_order, now: now() };
    let plan = plan::plan(&accounts, &opts);
    let (characters, character_notes) = match (&args.charinfo, &args.charsave) {
        (Some(info), Some(save)) => d2::read_characters(info, save),
        _ => (Vec::new(), Vec::new()),
    };
    print_plan(&plan, &characters, &character_notes);

    if !args.apply {
        println!("\nNothing written (dry run). Run again with --apply to import.");
        return Ok(());
    }
    if has_hashes && hash_order.is_none() {
        return Err("before writing, confirm how PvPGN stored passwords: --check-password YOUR_ACCOUNT (or --hash-order words|bytes if you are sure)".into());
    }
    let mut storage = SqliteStorage::open(&args.db).map_err(|e| format!("{}: {e:?}", args.db.display()))?;
    let (made, chars, notes) = apply(&mut storage, &plan, &characters).map_err(|e| format!("import stopped: {e:?}"))?;
    for note in &notes {
        println!("  - {note}");
    }
    println!("\nImported {made} accounts and {chars} Diablo II characters into {}.", args.db.display());
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bnetcc-pvpgn-import: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::PvpgnAccount;

    #[test]
    fn a_known_password_picks_the_orders_that_match() {
        let words: String = bnetcc_crypto::xsha1::xsha1(b"hunter2").iter().map(|w| format!("{w:08x}")).collect();
        let salt = [7u8; 32];
        let verifier = bnetcc_crypto::nls::verifier("Thrall", "hunter2", &salt);
        let big_endian = |b: &[u8; 32]| b.iter().rev().map(|x| format!("{x:02x}")).collect::<String>();
        let account = PvpgnAccount {
            origin: "test".into(),
            attrs: [
                (r"BNET\acct\username", "Thrall".to_string()),
                (r"BNET\acct\passhash1", words),
                (r"BNET\acct\salt", big_endian(&salt)),
                (r"BNET\acct\verifier", big_endian(&verifier)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        };
        let (hash, srp, _) = check_password(std::slice::from_ref(&account), "thrall", "HUNTER2");
        assert_eq!((hash, srp), (Some(HashOrder::Words), Some(SrpOrder::Reversed)));
        let (hash, srp, report) = check_password(&[account], "Thrall", "wrong");
        assert_eq!((hash, srp), (None, None));
        assert!(report.contains("does NOT match"));
    }

    #[test]
    fn applying_creates_accounts_bans_and_characters_once() {
        let words: String = bnetcc_crypto::xsha1::xsha1(b"pw").iter().map(|w| format!("{w:08x}")).collect();
        let account = PvpgnAccount {
            origin: "test".into(),
            attrs: [(r"BNET\acct\username", "Tyrael"), (r"BNET\acct\passhash1", words.as_str()), (r"BNET\auth\lock", "true"), (r"profile\location", "Heaven")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let opts = Options { realm: "bncc".into(), hash_order: HashOrder::Words, srp_order: None, now: 1_800_000_000 };
        let plan = plan::plan(&[account], &opts);
        let save = d2_formats::d2s::Save::new("Archangel", 3, 0x20, 1_136_073_600, &[]).to_bytes();
        let characters = vec![
            d2::PlannedCharacter { account: "tyrael".into(), name: "Archangel".into(), class: 3, status: 0x20, level: 1, progression: 0, last_played: 1_136_073_600, save: save.clone() },
            d2::PlannedCharacter { account: "nobody".into(), name: "Lost".into(), class: 0, status: 0, level: 1, progression: 0, last_played: 0, save },
        ];
        let mut storage = SqliteStorage::open_in_memory().unwrap();
        let (made, chars, notes) = apply(&mut storage, &plan, &characters).unwrap();
        assert_eq!((made, chars, notes.len()), (1, 1, 1), "{notes:?}");
        let stored = storage.account_by_name("tyrael").unwrap().unwrap();
        assert_eq!(stored.credential, Credential::Xsha1 { digest: bnetcc_crypto::xsha1::password_hash("pw") });
        assert!(storage.ban_get(stored.id, 1_800_000_000).unwrap().is_some());
        assert_eq!(storage.characters(stored.id).unwrap()[0].name, "Archangel");
        let (again, _, notes) = apply(&mut storage, &plan, &[]).unwrap();
        assert_eq!((again, notes.len()), (0, 1), "a second run leaves what is there");
    }
}
