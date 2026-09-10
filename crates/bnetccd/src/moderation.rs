//! Staff moderation state: name-tag bans, time-boxed IP bans, and server-wide chat mutes.
//!
//! These are the "Blizzard rep" tools — actions only a configured sysop (see
//! [`crate::config::AdminsConfig`]) can take. They are deliberately blunt:
//!
//! * a **tag ban** refuses any account whose name contains a banned substring (e.g. a clan
//!   prefix like `BNU-`), at logon and by disconnecting anyone already online;
//! * an **IP ban** refuses a whole address for a number of hours, at the accept door, which
//!   inherently catches every session and alt on that address;
//! * a **mute** silences an account's chat across the entire server without disconnecting it.
//!
//! All three persist to a small JSON file so they survive a restart — an attacker who just
//! waits for the next reboot is not banned in any useful sense. One `Mutex` guards the whole
//! (small) set; mutators serialise and write while holding it, so a concurrent read never
//! observes a half-applied change and two racing writers cannot lose an update. Reads treat
//! expired entries as absent without rewriting the file; expired rows are physically dropped
//! the next time a mutator prunes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Expiry sentinel meaning "never expires".
pub const NEVER: u64 = u64::MAX;

/// The on-disk shape. IP and account keys are strings because JSON object keys must be.
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    /// Lowercased name substrings; an account whose lowercased name contains any of these is
    /// refused at logon and disconnected if already online.
    #[serde(default)]
    tags: Vec<String>,
    /// IP address (its `Display` form) -> expiry in epoch-millis (`NEVER` = permanent).
    #[serde(default)]
    ip_bans: HashMap<String, u64>,
    /// Lowercased account name -> mute expiry in epoch-millis (`NEVER` = permanent).
    #[serde(default)]
    mutes: HashMap<String, u64>,
}

/// A point-in-time view of the active bans, for the `/bans` listing.
pub struct BanSnapshot {
    /// Banned name substrings (as entered).
    pub tags: Vec<String>,
    /// `(ip, expiry_ms)` for each live IP ban.
    pub ip_bans: Vec<(String, u64)>,
    /// `(account, expiry_ms)` for each live mute.
    pub mutes: Vec<(String, u64)>,
}

/// Staff-set bans and mutes, persisted to `path` (when set) so they survive a restart.
///
/// The three `has_*` flags let the hot checks — `is_muted` on every chat line, `is_ip_banned`
/// on every accepted connection — return without taking the lock while the corresponding set
/// is empty, which is the state on any server that is not actively moderating. A flag can be
/// `true` with only expired rows left; the check then takes the lock and correctly finds
/// nothing live, and the next mutation prunes and clears the flag.
pub struct BanStore {
    path: Option<PathBuf>,
    inner: Mutex<Persisted>,
    has_tags: AtomicBool,
    has_ip_bans: AtomicBool,
    has_mutes: AtomicBool,
}

impl BanStore {
    /// Load from `path`, or start empty when the file is absent, unreadable, or malformed
    /// (all logged, none fatal — a broken ban file must never stop the server from serving).
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let inner = match path.as_ref() {
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    warn!(path = %p.display(), error = %e, "ban file is malformed; starting empty");
                    Persisted::default()
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
                Err(e) => {
                    warn!(path = %p.display(), error = %e, "cannot read ban file; starting empty");
                    Persisted::default()
                }
            },
            None => Persisted::default(),
        };
        let has_tags = AtomicBool::new(!inner.tags.is_empty());
        let has_ip_bans = AtomicBool::new(!inner.ip_bans.is_empty());
        let has_mutes = AtomicBool::new(!inner.mutes.is_empty());
        Self { path, inner: Mutex::new(inner), has_tags, has_ip_bans, has_mutes }
    }

    /// Refresh the empty-set fast-path flags from `guard`. Called by mutators while the lock
    /// is held, so a reader that saw a flag `true` and took the lock sees a consistent map.
    fn refresh_flags(&self, guard: &Persisted) {
        self.has_tags.store(!guard.tags.is_empty(), Ordering::Relaxed);
        self.has_ip_bans.store(!guard.ip_bans.is_empty(), Ordering::Relaxed);
        self.has_mutes.store(!guard.mutes.is_empty(), Ordering::Relaxed);
    }

    /// Serialise and write the file while `guard` is still held, so the on-disk copy always
    /// matches an in-memory state no other writer can have moved past. Best-effort: a write
    /// failure is logged, never propagated.
    fn write(&self, guard: &Persisted) {
        let Some(path) = self.path.as_ref() else { return };
        match serde_json::to_vec_pretty(guard) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, &bytes) {
                    warn!(path = %path.display(), error = %e, "could not persist ban store");
                }
            }
            Err(e) => warn!(error = %e, "could not serialise ban store"),
        }
    }

    /// Drop expired IP bans and mutes from `guard`. Called by mutators so the file does not
    /// accumulate dead rows; reads skip expired entries without taking this path.
    fn prune(guard: &mut Persisted, now_ms: u64) {
        guard.ip_bans.retain(|_, &mut exp| exp > now_ms);
        guard.mutes.retain(|_, &mut exp| exp > now_ms);
    }

    // -- tag bans ----------------------------------------------------------------

    /// Add a name substring to the tag-ban list. Returns `false` if it was already present
    /// (case-insensitively). An empty substring is rejected (it would match everyone).
    pub fn add_tag(&self, substring: &str) -> bool {
        let sub = substring.trim().to_ascii_lowercase();
        if sub.is_empty() {
            return false;
        }
        let mut g = self.inner.lock().expect("ban lock");
        if g.tags.contains(&sub) {
            return false;
        }
        g.tags.push(sub);
        self.refresh_flags(&g);
        self.write(&g);
        true
    }

    /// Remove a tag substring. Returns whether it was present.
    pub fn remove_tag(&self, substring: &str) -> bool {
        let sub = substring.trim().to_ascii_lowercase();
        let mut g = self.inner.lock().expect("ban lock");
        let before = g.tags.len();
        g.tags.retain(|t| *t != sub);
        let removed = g.tags.len() != before;
        if removed {
            self.refresh_flags(&g);
            self.write(&g);
        }
        removed
    }

    /// Whether `name` contains any banned substring (case-insensitive).
    #[must_use]
    pub fn is_tag_banned(&self, name: &str) -> bool {
        if !self.has_tags.load(Ordering::Relaxed) {
            return false;
        }
        let lower = name.to_ascii_lowercase();
        let g = self.inner.lock().expect("ban lock");
        g.tags.iter().any(|t| lower.contains(t.as_str()))
    }

    // -- IP bans -----------------------------------------------------------------

    /// Ban `ip` until `expiry_ms` (epoch-millis; [`NEVER`] for permanent).
    pub fn ban_ip(&self, ip: IpAddr, expiry_ms: u64, now_ms: u64) {
        let mut g = self.inner.lock().expect("ban lock");
        Self::prune(&mut g, now_ms);
        g.ip_bans.insert(ip.to_string(), expiry_ms);
        self.refresh_flags(&g);
        self.write(&g);
    }

    /// Lift an IP ban. Returns whether one was present.
    pub fn unban_ip(&self, ip: IpAddr, now_ms: u64) -> bool {
        let mut g = self.inner.lock().expect("ban lock");
        Self::prune(&mut g, now_ms);
        let removed = g.ip_bans.remove(&ip.to_string()).is_some();
        self.refresh_flags(&g);
        self.write(&g);
        removed
    }

    /// Whether `ip` is currently banned. On an unmoderated server this is a single relaxed
    /// atomic load with no lock — it runs on every accepted connection.
    #[must_use]
    pub fn is_ip_banned(&self, ip: IpAddr, now_ms: u64) -> bool {
        if !self.has_ip_bans.load(Ordering::Relaxed) {
            return false;
        }
        let g = self.inner.lock().expect("ban lock");
        g.ip_bans.get(&ip.to_string()).is_some_and(|&exp| exp > now_ms)
    }

    // -- mutes -------------------------------------------------------------------

    /// Mute account `name` (server-wide) until `expiry_ms`.
    pub fn mute(&self, name: &str, expiry_ms: u64, now_ms: u64) {
        let mut g = self.inner.lock().expect("ban lock");
        Self::prune(&mut g, now_ms);
        g.mutes.insert(name.to_ascii_lowercase(), expiry_ms);
        self.refresh_flags(&g);
        self.write(&g);
    }

    /// Unmute account `name`. Returns whether it was muted.
    pub fn unmute(&self, name: &str, now_ms: u64) -> bool {
        let mut g = self.inner.lock().expect("ban lock");
        Self::prune(&mut g, now_ms);
        let removed = g.mutes.remove(&name.to_ascii_lowercase()).is_some();
        self.refresh_flags(&g);
        self.write(&g);
        removed
    }

    /// Whether account `name` is currently muted. On an unmuted server this is a single
    /// relaxed atomic load with no lock — it runs on every chat line.
    #[must_use]
    pub fn is_muted(&self, name: &str, now_ms: u64) -> bool {
        if !self.has_mutes.load(Ordering::Relaxed) {
            return false;
        }
        let g = self.inner.lock().expect("ban lock");
        g.mutes.get(&name.to_ascii_lowercase()).is_some_and(|&exp| exp > now_ms)
    }

    // -- listing -----------------------------------------------------------------

    /// A snapshot of every still-active ban, for the `/bans` command.
    #[must_use]
    pub fn snapshot(&self, now_ms: u64) -> BanSnapshot {
        let g = self.inner.lock().expect("ban lock");
        BanSnapshot {
            tags: g.tags.clone(),
            ip_bans: g
                .ip_bans
                .iter()
                .filter(|(_, &exp)| exp > now_ms)
                .map(|(ip, &exp)| (ip.clone(), exp))
                .collect(),
            mutes: g
                .mutes
                .iter()
                .filter(|(_, &exp)| exp > now_ms)
                .map(|(n, &exp)| (n.clone(), exp))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    #[test]
    fn tag_ban_matches_substring_case_insensitively() {
        let s = BanStore::load(None);
        assert!(s.add_tag("BNU-"));
        assert!(!s.add_tag("bnu-")); // duplicate, case-insensitive
        assert!(s.is_tag_banned("BNU-Bot"));
        assert!(s.is_tag_banned("xbnu-y"));
        assert!(!s.is_tag_banned("Clanless"));
        assert!(s.remove_tag("bnu-"));
        assert!(!s.is_tag_banned("BNU-Bot"));
    }

    #[test]
    fn empty_tag_is_rejected() {
        let s = BanStore::load(None);
        assert!(!s.add_tag("   "));
        assert!(!s.is_tag_banned("anyone"));
    }

    #[test]
    fn ip_ban_expires() {
        let s = BanStore::load(None);
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        s.ban_ip(ip, T0 + 1000, T0);
        assert!(s.is_ip_banned(ip, T0 + 500));
        assert!(!s.is_ip_banned(ip, T0 + 1000)); // exclusive: expiry reached
        assert!(!s.is_ip_banned(ip, T0 + 2000));
        s.ban_ip(ip, NEVER, T0);
        assert!(s.is_ip_banned(ip, T0 + 10_000_000));
        assert!(s.unban_ip(ip, T0));
        assert!(!s.is_ip_banned(ip, T0));
    }

    #[test]
    fn mute_is_per_account_and_expires() {
        let s = BanStore::load(None);
        s.mute("Flooder", T0 + 1000, T0);
        assert!(s.is_muted("flooder", T0 + 500));
        assert!(!s.is_muted("flooder", T0 + 2000));
        s.mute("Flooder", NEVER, T0);
        assert!(s.is_muted("Flooder", T0 + 10_000_000));
        assert!(s.unmute("FLOODER", T0));
        assert!(!s.is_muted("Flooder", T0));
    }

    #[test]
    fn survives_a_reload_from_disk() {
        let dir = std::env::temp_dir().join(format!("bnetccd-bans-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bans.json");
        {
            let s = BanStore::load(Some(path.clone()));
            s.add_tag("BNU-");
            s.ban_ip("1.2.3.4".parse().unwrap(), NEVER, T0);
            s.mute("Flooder", NEVER, T0);
        }
        let s = BanStore::load(Some(path.clone()));
        assert!(s.is_tag_banned("BNU-Bot"));
        assert!(s.is_ip_banned("1.2.3.4".parse().unwrap(), T0));
        assert!(s.is_muted("Flooder", T0));
        let snap = s.snapshot(T0);
        assert_eq!(snap.tags, vec!["bnu-".to_string()]);
        assert_eq!(snap.ip_bans.len(), 1);
        assert_eq!(snap.mutes.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_rows_are_pruned_on_mutation() {
        let s = BanStore::load(None);
        s.ban_ip("1.1.1.1".parse().unwrap(), T0 + 100, T0);
        // A later mutation past the expiry prunes the dead row.
        s.ban_ip("2.2.2.2".parse().unwrap(), NEVER, T0 + 200);
        let snap = s.snapshot(T0 + 200);
        assert_eq!(snap.ip_bans.len(), 1);
        assert_eq!(snap.ip_bans[0].0, "2.2.2.2");
    }
}
