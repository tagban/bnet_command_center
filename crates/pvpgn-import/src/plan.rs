//! Turning PvPGN accounts into Command Center accounts.
//!
//! What carries over:
//!
//! - **Name and password.** `BNET\acct\passhash1` is `XSHA1(lowercase(password))`, the digest
//!   StarCraft, Brood War, Diablo II and Warcraft II log on with; it becomes the account's X-SHA-1
//!   credential as it is, so players keep their passwords. PvPGN writes it as five 8-digit hex
//!   words; [`HashOrder`] says how those become the 20 bytes.
//! - **WarCraft III.** An account with `BNET\acct\salt` and `BNET\acct\verifier` also becomes a
//!   `Name@realm` account, Command Center's own namespace for WarCraft III's SRP logons, with its
//!   `WAR3`/`W3XP` records. [`SrpOrder`] says which way round PvPGN wrote the two numbers.
//! - **Profile** (`profile\…`), **records** (`Record\…`), creation time and last logon.
//! - **Locks** (`BNET\auth\lock`) become this server's bans.
//!
//! What does not, and is reported instead: e-mail addresses and last-logon addresses (Command
//! Center keeps neither), admin and operator rights (grant them in `bnetccd.toml` `[admins]`),
//! mutes, friends lists and clans (not stored by Command Center yet).

use std::collections::{BTreeMap, BTreeSet};

use bnetcc_storage::attr::{AttrKey, AttrMap};
use bnetcc_storage::model::Credential;

use crate::source::PvpgnAccount;

/// How PvPGN's 40 hex digits of `passhash1` become the digest's 20 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashOrder {
    /// Five words, each written as 8 hex digits and sent little-endian: `01234567…` is the bytes
    /// `67 45 23 01 …`.
    Words,
    /// The digits are the bytes in order.
    Bytes,
}

/// How PvPGN's hex salt and verifier become the 32 bytes each the WarCraft III logon uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrpOrder {
    /// The digits are the bytes in order.
    AsIs,
    /// The digits are a big-endian number; the logon's bytes are little-endian.
    Reversed,
}

/// What the plan needs to know about the target server.
#[derive(Debug, Clone)]
pub struct Options {
    /// `[server] realm`: the suffix of WarCraft III account names.
    pub realm: String,
    /// See [`HashOrder`].
    pub hash_order: HashOrder,
    /// See [`SrpOrder`]; `None` leaves WarCraft III accounts out.
    pub srp_order: Option<SrpOrder>,
    /// Now, seconds since the Unix epoch: the creation time of accounts without one.
    pub now: u64,
}

/// An account to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAccount {
    /// Its name (`Name` or `Name@realm`).
    pub name: String,
    /// Its credential.
    pub credential: Credential,
    /// Creation time.
    pub created_at: u64,
    /// Its attributes.
    pub attrs: AttrMap,
    /// A ban to apply: reason, and when it ends (`None` never).
    pub ban: Option<(String, Option<u64>)>,
    /// The PvPGN account it came from.
    pub origin: String,
}

/// The whole import, before anything is written.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Accounts to create.
    pub accounts: Vec<PlannedAccount>,
    /// Accounts left out, with why.
    pub skipped: Vec<String>,
    /// Accounts that were admins on PvPGN.
    pub admins: Vec<String>,
    /// Accounts that were operators on PvPGN.
    pub operators: Vec<String>,
    /// How many accounts had an e-mail address, which is not carried over.
    pub emails_left: usize,
    /// How many accounts had a friends list, which is not carried over.
    pub friends_left: usize,
    /// How many accounts were muted, which is not carried over.
    pub mutes_left: usize,
    /// How many WarCraft III credentials were left out for want of [`Options::srp_order`].
    pub srp_left: usize,
}

/// Hex digits to bytes.
fn hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// `passhash1` as a digest.
#[must_use]
pub fn digest(passhash: &str, order: HashOrder) -> Option<[u8; 20]> {
    let bytes: [u8; 20] = hex(passhash)?.try_into().ok()?;
    Some(match order {
        HashOrder::Bytes => bytes,
        HashOrder::Words => {
            let mut out = [0u8; 20];
            for (word, chunk) in bytes.chunks(4).enumerate() {
                let value = u32::from_be_bytes(chunk.try_into().ok()?);
                out[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            out
        }
    })
}

/// A hex salt or verifier as the logon's 32 bytes.
#[must_use]
pub fn srp_field(value: &str, order: SrpOrder) -> Option<[u8; 32]> {
    let mut bytes = hex(value)?;
    if bytes.len() > 32 {
        return None;
    }
    if order == SrpOrder::Reversed {
        bytes.reverse();
    }
    bytes.resize(32, 0);
    bytes.try_into().ok()
}

fn is_true(v: Option<&str>) -> bool {
    v.is_some_and(|v| v.trim().eq_ignore_ascii_case("true") || v.trim() == "1")
}

fn number(v: Option<&str>) -> Option<u64> {
    v.and_then(|v| v.trim().parse::<u64>().ok()).filter(|&n| n > 0)
}

/// Whether a record key belongs to WarCraft III.
fn warcraft_record(key: &str) -> bool {
    let product = key.split('\\').nth(1).unwrap_or("");
    product.eq_ignore_ascii_case("WAR3") || product.eq_ignore_ascii_case("W3XP")
}

/// Plan the import of `accounts`.
#[must_use]
pub fn plan(accounts: &[PvpgnAccount], opts: &Options) -> Plan {
    let mut out = Plan::default();
    let mut names = BTreeSet::new();
    for account in accounts {
        let Some(name) = account.get(r"BNET\acct\username").map(str::trim).map(str::to_string) else { continue };
        if let Err(e) = bnetcc_storage::validate_account_name(&name) {
            out.skipped.push(format!("{name} ({}): not a valid Command Center name: {e}", account.origin));
            continue;
        }
        if !names.insert(name.to_ascii_lowercase()) {
            out.skipped.push(format!("{name} ({}): the same name as an account before it", account.origin));
            continue;
        }
        let created_at = number(account.get(r"BNET\acct\ctime")).unwrap_or(opts.now);
        let mut common = AttrMap::new();
        let mut warcraft = AttrMap::new();
        for (key, value) in &account.attrs {
            let lower = key.to_ascii_lowercase();
            if lower.starts_with("profile\\") && !value.is_empty() {
                common.insert(AttrKey::new(key), value.clone());
            } else if lower.starts_with("record\\") && !value.is_empty() {
                let target = if warcraft_record(key) { &mut warcraft } else { &mut common };
                target.insert(AttrKey::new(key), value.clone());
            }
        }
        if let Some(last) = number(account.get(r"BNET\acct\lastlogin_time")) {
            common.insert(AttrKey::new(r"System\LastLogin"), last.to_string());
        }
        let ban = is_true(account.get(r"BNET\auth\lock")).then(|| {
            let reason = account.get(r"BNET\auth\lockreason").filter(|r| !r.trim().is_empty()).unwrap_or("Locked on the PvPGN server");
            (reason.to_string(), number(account.get(r"BNET\auth\lockuntil")).filter(|&until| until > opts.now))
        });
        if is_true(account.get(r"BNET\auth\admin")) {
            out.admins.push(name.clone());
        }
        if is_true(account.get(r"BNET\auth\operator")) {
            out.operators.push(name.clone());
        }
        out.emails_left += usize::from(account.get(r"BNET\acct\email").is_some_and(|e| !e.trim().is_empty()));
        out.friends_left += usize::from(number(account.get(r"friend\count")).is_some());
        out.mutes_left += usize::from(is_true(account.get(r"BNET\auth\mute")));

        let mut made = false;
        if let Some(passhash) = account.get(r"BNET\acct\passhash1").filter(|h| !h.trim().is_empty()) {
            match digest(passhash, opts.hash_order) {
                Some(digest) => {
                    let mut attrs = common.clone();
                    if account.get(r"BNET\acct\salt").is_none() {
                        attrs.extend(warcraft.clone());
                    }
                    out.accounts.push(PlannedAccount {
                        name: name.clone(),
                        credential: Credential::Xsha1 { digest },
                        created_at,
                        attrs,
                        ban: ban.clone(),
                        origin: account.origin.clone(),
                    });
                    made = true;
                }
                None => out.skipped.push(format!("{name} ({}): its password hash is not 40 hex digits", account.origin)),
            }
        }
        if let (Some(salt), Some(verifier)) = (account.get(r"BNET\acct\salt"), account.get(r"BNET\acct\verifier")) {
            match opts.srp_order {
                None => out.srp_left += 1,
                Some(order) => match (srp_field(salt, order), srp_field(verifier, order)) {
                    (Some(salt), Some(verifier)) => {
                        let mut attrs = warcraft;
                        if !made {
                            attrs.extend(common);
                        }
                        out.accounts.push(PlannedAccount {
                            name: format!("{name}@{}", opts.realm),
                            credential: Credential::Srp { salt, verifier },
                            created_at,
                            attrs,
                            ban,
                            origin: account.origin.clone(),
                        });
                        made = true;
                    }
                    _ => out.skipped.push(format!("{name} ({}): its WarCraft III salt or verifier is not hex", account.origin)),
                },
            }
        }
        if !made && account.get(r"BNET\acct\passhash1").map_or(true, |h| h.trim().is_empty()) && account.get(r"BNET\acct\salt").is_none() {
            out.skipped.push(format!("{name} ({}): no password", account.origin));
        }
    }
    out
}

/// An account's stored password material: its hash, and its WarCraft III salt and verifier.
pub type StoredCredentials = (Option<String>, Option<(String, String)>);

/// Every account's passhash and SRP fields by lowercase name, for checking a password.
#[must_use]
pub fn credentials_by_name(accounts: &[PvpgnAccount]) -> BTreeMap<String, StoredCredentials> {
    accounts
        .iter()
        .filter_map(|a| {
            let name = a.get(r"BNET\acct\username")?.trim().to_ascii_lowercase();
            let hash = a.get(r"BNET\acct\passhash1").map(str::to_string);
            let srp = a.get(r"BNET\acct\salt").zip(a.get(r"BNET\acct\verifier")).map(|(s, v)| (s.to_string(), v.to_string()));
            Some((name, (hash, srp)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(pairs: &[(&str, &str)]) -> PvpgnAccount {
        PvpgnAccount { origin: "test".into(), attrs: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    fn opts(srp: Option<SrpOrder>) -> Options {
        Options { realm: "bncc".into(), hash_order: HashOrder::Words, srp_order: srp, now: 1_800_000_000 }
    }

    #[test]
    fn digests_read_as_words_or_bytes() {
        let hex = "0123456789abcdef0011223344556677deadbeef";
        assert_eq!(&digest(hex, HashOrder::Words).unwrap()[..8], &[0x67, 0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0x89]);
        assert_eq!(&digest(hex, HashOrder::Bytes).unwrap()[..4], &[0x01, 0x23, 0x45, 0x67]);
        assert_eq!(digest("abc", HashOrder::Words), None);
        let words: String = bnetcc_crypto::xsha1::xsha1(b"password").iter().map(|w| format!("{w:08x}")).collect();
        assert_eq!(digest(&words, HashOrder::Words), Some(bnetcc_crypto::xsha1::password_hash("PassWord")), "words as PvPGN writes them");
    }

    #[test]
    fn accounts_carry_their_password_profile_records_and_lock() {
        let a = account(&[
            (r"BNET\acct\username", "Raynor"),
            (r"BNET\acct\passhash1", "0123456789abcdef0011223344556677deadbeef"),
            (r"BNET\acct\ctime", "1136073600"),
            (r"BNET\acct\lastlogin_time", "1136080000"),
            (r"BNET\acct\email", "someone@example.com"),
            (r"BNET\auth\admin", "true"),
            (r"BNET\auth\lock", "true"),
            (r"profile\location", "Mar Sara"),
            (r"profile\sex", ""),
            (r"Record\SEXP\0\wins", "12"),
            (r"Record\WAR3\0\wins", "3"),
            (r"friend\count", "2"),
        ]);
        let p = plan(&[a], &opts(None));
        assert_eq!(p.accounts.len(), 1);
        let r = &p.accounts[0];
        assert_eq!((r.name.as_str(), r.created_at), ("Raynor", 1_136_073_600));
        assert_eq!(r.attrs.get(&AttrKey::new(r"profile\location")).map(String::as_str), Some("Mar Sara"));
        assert!(!r.attrs.contains_key(&AttrKey::new(r"profile\sex")), "empty values are not written");
        assert_eq!(r.attrs.get(&AttrKey::new(r"Record\SEXP\0\wins")).map(String::as_str), Some("12"));
        assert_eq!(r.attrs.get(&AttrKey::new(r"Record\WAR3\0\wins")).map(String::as_str), Some("3"), "no WarCraft III account: its records stay here");
        assert_eq!(r.attrs.get(&AttrKey::new(r"System\LastLogin")).map(String::as_str), Some("1136080000"));
        assert_eq!(r.ban, Some(("Locked on the PvPGN server".to_string(), None)));
        assert!(!r.attrs.keys().any(|k| k.as_str().contains("email")), "e-mail is not carried over");
        assert_eq!((p.admins.as_slice(), p.emails_left, p.friends_left), (&["Raynor".to_string()][..], 1, 1));
    }

    #[test]
    fn warcraft_three_accounts_need_an_srp_order_and_take_their_records() {
        let a = account(&[
            (r"BNET\acct\username", "Grom"),
            (r"BNET\acct\passhash1", "0123456789abcdef0011223344556677deadbeef"),
            (r"BNET\acct\salt", "01"),
            (r"BNET\acct\verifier", "0203"),
            (r"Record\W3XP\0\wins", "40"),
            (r"Record\STAR\0\wins", "1"),
        ]);
        let without = plan(std::slice::from_ref(&a), &opts(None));
        assert_eq!((without.accounts.len(), without.srp_left), (1, 1));
        let with = plan(&[a], &opts(Some(SrpOrder::Reversed)));
        assert_eq!(with.accounts.len(), 2);
        let w3 = &with.accounts[1];
        assert_eq!(w3.name, "Grom@bncc");
        let Credential::Srp { salt, verifier } = &w3.credential else { panic!("SRP") };
        assert_eq!((salt[0], verifier[0], verifier[1]), (0x01, 0x03, 0x02), "big-endian digits reversed into little-endian bytes");
        assert!(w3.attrs.contains_key(&AttrKey::new(r"Record\W3XP\0\wins")));
        assert!(!with.accounts[0].attrs.contains_key(&AttrKey::new(r"Record\W3XP\0\wins")));
    }

    #[test]
    fn bad_and_repeated_names_and_passwordless_accounts_are_skipped() {
        let hash = (r"BNET\acct\passhash1", "0123456789abcdef0011223344556677deadbeef");
        let accounts = [
            account(&[(r"BNET\acct\username", "x"), hash]),
            account(&[(r"BNET\acct\username", "Kerrigan"), hash]),
            account(&[(r"BNET\acct\username", "KERRIGAN"), hash]),
            account(&[(r"BNET\acct\username", "Nopass")]),
        ];
        let p = plan(&accounts, &opts(None));
        assert_eq!(p.accounts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Kerrigan"]);
        assert_eq!(p.skipped.len(), 3, "{:?}", p.skipped);
    }
}
