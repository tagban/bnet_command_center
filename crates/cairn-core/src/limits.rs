//! Admission control and flood control.
//!
//! Both are pure data structures driven by an explicit clock, so their behaviour is
//! deterministic and testable without sleeping. Nothing here touches a socket.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

use crate::channel::AccountId;
use crate::policy::{ConnLimits, FloodPenalty, FloodPolicy};

/// Connection classes, which have independent limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnClass {
    /// A game client on the BNCS protocol.
    Game,
    /// A bot on the telnet/chat gateway.
    Gateway,
    /// A BNFTP file transfer.
    Bnftp,
}

/// Why a connection was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Too many concurrent connections from this address.
    PerIp {
        /// The configured ceiling.
        limit: u32,
    },
    /// Too many concurrent connections for this account.
    PerAccount {
        /// The configured ceiling.
        limit: u32,
    },
    /// The server-wide ceiling for this class is reached.
    Global {
        /// The configured ceiling.
        limit: u32,
    },
}

/// Tracks concurrent connections per address, per account, and in total.
///
/// One instance per connection class.
#[derive(Debug, Default)]
pub struct AdmissionTable {
    per_ip: HashMap<IpAddr, u32>,
    per_account: HashMap<AccountId, u32>,
    total: u32,
    allowlist: Vec<IpAddr>,
}

impl AdmissionTable {
    /// Empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Addresses exempt from the per-IP ceiling.
    ///
    /// A warnet operator's known bot hosts belong here; the per-account ceiling still
    /// applies to them, which is the bound that actually matters.
    pub fn set_allowlist(&mut self, ips: Vec<IpAddr>) {
        self.allowlist = ips;
    }

    /// Current connections from an address.
    #[must_use]
    pub fn ip_count(&self, ip: IpAddr) -> u32 {
        self.per_ip.get(&ip).copied().unwrap_or(0)
    }

    /// Current connections for an account.
    #[must_use]
    pub fn account_count(&self, account: AccountId) -> u32 {
        self.per_account.get(&account).copied().unwrap_or(0)
    }

    /// Total connections of this class.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.total
    }

    /// Admit a connection from `ip`, or explain why not.
    ///
    /// Checked at accept time, before a single byte is read, so an address spraying
    /// connections costs us one `accept` and one hash lookup each.
    ///
    /// # Errors
    ///
    /// [`Rejection`] naming the ceiling that was hit.
    pub fn admit_ip(&mut self, ip: IpAddr, limits: ConnLimits) -> Result<(), Rejection> {
        if self.total >= limits.global {
            return Err(Rejection::Global {
                limit: limits.global,
            });
        }
        if !self.allowlist.contains(&ip) {
            let n = self.ip_count(ip);
            if n >= limits.per_ip {
                return Err(Rejection::PerIp {
                    limit: limits.per_ip,
                });
            }
        }
        *self.per_ip.entry(ip).or_insert(0) += 1;
        self.total += 1;
        Ok(())
    }

    /// Release a connection previously admitted from `ip`.
    pub fn release_ip(&mut self, ip: IpAddr) {
        if let Some(n) = self.per_ip.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_ip.remove(&ip);
            }
        }
        self.total = self.total.saturating_sub(1);
    }

    /// Bind an authenticated account to a connection.
    ///
    /// Separate from [`Self::admit_ip`] because the account is unknown at accept time.
    ///
    /// # Errors
    ///
    /// [`Rejection::PerAccount`] when the account already holds its maximum.
    pub fn bind_account(
        &mut self,
        account: AccountId,
        limits: ConnLimits,
    ) -> Result<(), Rejection> {
        if self.account_count(account) >= limits.per_account {
            return Err(Rejection::PerAccount {
                limit: limits.per_account,
            });
        }
        *self.per_account.entry(account).or_insert(0) += 1;
        Ok(())
    }

    /// Release an account binding.
    pub fn release_account(&mut self, account: AccountId) {
        if let Some(n) = self.per_account.get_mut(&account) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_account.remove(&account);
            }
        }
    }

    /// Number of distinct addresses currently tracked.
    ///
    /// Exposed because unbounded growth here would be a slow leak; the invariant is that
    /// it returns to zero when every connection closes.
    #[must_use]
    pub fn tracked_addresses(&self) -> usize {
        self.per_ip.len()
    }
}

/// What to do with a message that hit the flood budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodVerdict {
    /// Within budget.
    Allow,
    /// Over budget; apply the policy's penalty.
    Exceeded(FloodPenalty),
}

/// A token bucket over a caller-supplied millisecond clock.
///
/// Capacity is the burst allowance; the refill rate is the sustained rate. Using an
/// explicit clock rather than `Instant::now()` keeps this testable without sleeping and
/// keeps the crate free of a time dependency.
#[derive(Debug, Clone)]
pub struct FloodTracker {
    /// Thousandths of a token, to avoid floating point.
    millitokens: u64,
    capacity_millitokens: u64,
    per_10s: u64,
    last_ms: u64,
    penalty: FloodPenalty,
    /// Total messages suppressed, for metrics.
    suppressed: u64,
}

impl FloodTracker {
    /// New tracker, starting with a full burst allowance.
    #[must_use]
    pub const fn new(policy: FloodPolicy, now_ms: u64) -> Self {
        let capacity = (policy.burst as u64) * 1000;
        Self {
            millitokens: capacity,
            capacity_millitokens: capacity,
            per_10s: policy.messages_per_10s as u64,
            last_ms: now_ms,
            penalty: policy.penalty,
            suppressed: 0,
        }
    }

    /// Messages suppressed so far.
    #[must_use]
    pub const fn suppressed(&self) -> u64 {
        self.suppressed
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        if elapsed == 0 {
            return;
        }
        self.last_ms = now_ms;
        // millitokens per ms = per_10s * 1000 / 10_000 = per_10s / 10
        let gained = elapsed.saturating_mul(self.per_10s) / 10;
        self.millitokens = (self.millitokens + gained).min(self.capacity_millitokens);
    }

    /// Account for one message.
    pub fn check(&mut self, now_ms: u64) -> FloodVerdict {
        self.refill(now_ms);
        if self.millitokens >= 1000 {
            self.millitokens -= 1000;
            FloodVerdict::Allow
        } else {
            self.suppressed += 1;
            FloodVerdict::Exceeded(self.penalty)
        }
    }
}

/// A hashed CD key, as it arrives in `SID_AUTH_CHECK`.
///
/// We store the 20-byte hash the client sends, never a raw key. There is no reason for
/// a server to hold a usable CD key, and every reason not to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(pub [u8; 20]);

/// The outcome of trying to claim a CD key for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyVerdict {
    /// The key is now claimed by this session.
    Ok,
    /// Another live session holds it — `SID_AUTH_CHECK` result `0x201`.
    InUse {
        /// The account currently holding it, for the result string.
        by: AccountId,
    },
    /// The key is banned — `SID_AUTH_CHECK` result `0x202`.
    Banned,
}

/// One live session per CD key.
///
/// **This is the real economic gate on bot fleets.** Addresses are cheap to rent; keys
/// are not. A per-IP limit can be sidestepped by anyone with a handful of proxies, but
/// running twenty simultaneous bots genuinely requires twenty keys if the server
/// enforces uniqueness — which is exactly why real Battle.net answers `SID_AUTH_CHECK`
/// with `0x201` "CD key in use" and names the holder.
///
/// The cooldown reproduces a real Battle.net quirk that clients and bots already expect:
/// a key stays marked in use for a short window after a session ends, so reconnecting
/// too fast reports "key in use" rather than succeeding. Replicating it means bots
/// written against real Battle.net behave identically here.
#[derive(Debug)]
pub struct KeyRegistry {
    active: HashMap<KeyId, AccountId>,
    /// Recently released keys: holder and release time.
    cooling: HashMap<KeyId, (AccountId, u64)>,
    banned: BTreeSet<KeyId>,
    cooldown_ms: u64,
}

impl KeyRegistry {
    /// New registry. A cooldown of 500 ms matches observed Battle.net behaviour.
    #[must_use]
    pub fn new(cooldown_ms: u64) -> Self {
        Self {
            active: HashMap::new(),
            cooling: HashMap::new(),
            banned: BTreeSet::new(),
            cooldown_ms,
        }
    }

    /// Keys currently held by a live session.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Ban a key network-wide.
    pub fn ban(&mut self, key: KeyId) {
        self.banned.insert(key);
        self.active.remove(&key);
    }

    /// Lift a key ban.
    pub fn unban(&mut self, key: KeyId) -> bool {
        self.banned.remove(&key)
    }

    /// Try to claim a key for a session.
    pub fn claim(&mut self, key: KeyId, account: AccountId, now_ms: u64) -> KeyVerdict {
        if self.banned.contains(&key) {
            return KeyVerdict::Banned;
        }
        if let Some(&holder) = self.active.get(&key) {
            return KeyVerdict::InUse { by: holder };
        }
        // Expire stale cooldown entries lazily, so the map tracks live churn rather
        // than growing for the lifetime of the process.
        let cooldown = self.cooldown_ms;
        self.cooling
            .retain(|_, (_, at)| now_ms.saturating_sub(*at) < cooldown);
        if let Some(&(holder, at)) = self.cooling.get(&key) {
            if now_ms.saturating_sub(at) < cooldown {
                return KeyVerdict::InUse { by: holder };
            }
        }
        self.cooling.remove(&key);
        self.active.insert(key, account);
        KeyVerdict::Ok
    }

    /// Release a key when its session ends, starting the cooldown.
    pub fn release(&mut self, key: KeyId, now_ms: u64) {
        if let Some(holder) = self.active.remove(&key) {
            self.cooling.insert(key, (holder, now_ms));
        }
    }

    /// Entries currently held in the cooldown map, for leak checking.
    #[must_use]
    pub fn cooling_count(&self) -> usize {
        self.cooling.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Policy, ServerMode};
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    #[test]
    fn gateway_defaults_to_one_connection_per_ip_when_gaming() {
        let limits = Policy::for_mode(ServerMode::Gaming).gateway_limits;
        let mut t = AdmissionTable::new();
        assert!(t.admit_ip(ip(1), limits).is_ok());
        assert_eq!(
            t.admit_ip(ip(1), limits),
            Err(Rejection::PerIp { limit: 1 })
        );
        // A different address is unaffected.
        assert!(t.admit_ip(ip(2), limits).is_ok());
    }

    #[test]
    fn a_bot_fleet_costs_one_address_per_bot_even_on_a_warnet() {
        // The entry cost, stated as a test: twenty bots means twenty addresses. Warnet
        // mode does not relax this — it only raises the global ceiling.
        let limits = Policy::for_mode(ServerMode::Warnet).gateway_limits;
        let mut t = AdmissionTable::new();
        assert!(t.admit_ip(ip(1), limits).is_ok());
        assert_eq!(
            t.admit_ip(ip(1), limits),
            Err(Rejection::PerIp { limit: 1 }),
            "a second bot from the same address must be refused"
        );
        // Twenty bots from twenty addresses is fine.
        for i in 2..=21u8 {
            assert!(t.admit_ip(ip(i), limits).is_ok(), "bot {i} refused");
        }
        assert_eq!(t.total(), 21);
    }

    #[test]
    fn the_allowlist_is_the_operators_deliberate_exception() {
        // The escape hatch exists, but it is an explicit act by the server owner for a
        // named host — never a mode default that quietly makes every fleet free.
        let limits = Policy::for_mode(ServerMode::Warnet).gateway_limits;
        let mut t = AdmissionTable::new();
        assert!(t.admit_ip(ip(9), limits).is_ok());
        assert!(t.admit_ip(ip(9), limits).is_err());

        let mut t = AdmissionTable::new();
        t.set_allowlist(vec![ip(9)]);
        for _ in 0..20 {
            assert!(t.admit_ip(ip(9), limits).is_ok());
        }
    }

    #[test]
    fn releasing_frees_a_slot_and_leaks_nothing() {
        let limits = Policy::for_mode(ServerMode::Gaming).gateway_limits;
        let mut t = AdmissionTable::new();
        t.admit_ip(ip(1), limits).unwrap();
        assert_eq!(t.tracked_addresses(), 1);
        t.release_ip(ip(1));
        assert_eq!(t.ip_count(ip(1)), 0);
        assert_eq!(t.total(), 0);
        assert_eq!(
            t.tracked_addresses(),
            0,
            "the address entry must be reclaimed, not left at zero"
        );
        assert!(t.admit_ip(ip(1), limits).is_ok());
    }

    #[test]
    fn churn_does_not_grow_the_table() {
        let limits = Policy::for_mode(ServerMode::Gaming).gateway_limits;
        let mut t = AdmissionTable::new();
        for i in 0..5_000u32 {
            let addr = IpAddr::V4(Ipv4Addr::from(i.to_be_bytes()));
            t.admit_ip(addr, limits).unwrap();
            t.release_ip(addr);
        }
        assert_eq!(t.tracked_addresses(), 0);
        assert_eq!(t.total(), 0);
    }

    #[test]
    fn the_allowlist_exempts_per_ip_only() {
        let limits = Policy::for_mode(ServerMode::Gaming).gateway_limits;
        let mut t = AdmissionTable::new();
        t.set_allowlist(vec![ip(7)]);
        for _ in 0..50 {
            assert!(t.admit_ip(ip(7), limits).is_ok());
        }
        // The global ceiling still applies to allowlisted hosts.
        let tight = ConnLimits {
            per_ip: 1,
            per_account: 1,
            global: 50,
        };
        assert_eq!(t.admit_ip(ip(7), tight), Err(Rejection::Global { limit: 50 }));
    }

    #[test]
    fn per_account_limits_are_independent_of_address() {
        // The bound that actually matters in a warnet: one identity, many addresses.
        let limits = Policy::for_mode(ServerMode::Warnet).gateway_limits;
        let mut t = AdmissionTable::new();
        for i in 0..limits.per_account {
            t.admit_ip(ip(i as u8), limits).unwrap();
            assert!(t.bind_account(42, limits).is_ok());
        }
        assert_eq!(
            t.bind_account(42, limits),
            Err(Rejection::PerAccount {
                limit: limits.per_account
            })
        );
        t.release_account(42);
        assert!(t.bind_account(42, limits).is_ok());
    }

    #[test]
    fn global_ceiling_is_checked_first() {
        let limits = ConnLimits {
            per_ip: 100,
            per_account: 100,
            global: 2,
        };
        let mut t = AdmissionTable::new();
        t.admit_ip(ip(1), limits).unwrap();
        t.admit_ip(ip(2), limits).unwrap();
        assert_eq!(t.admit_ip(ip(3), limits), Err(Rejection::Global { limit: 2 }));
    }

    #[test]
    fn flood_bucket_allows_a_burst_then_throttles() {
        let policy = FloodPolicy {
            messages_per_10s: 10,
            burst: 5,
            penalty: FloodPenalty::DropMessage,
        };
        let mut f = FloodTracker::new(policy, 0);
        for i in 0..5 {
            assert_eq!(f.check(0), FloodVerdict::Allow, "burst message {i}");
        }
        assert_eq!(
            f.check(0),
            FloodVerdict::Exceeded(FloodPenalty::DropMessage)
        );
        assert_eq!(f.suppressed(), 1);
    }

    #[test]
    fn flood_bucket_refills_at_the_sustained_rate() {
        let policy = FloodPolicy {
            messages_per_10s: 10, // one per second
            burst: 5,
            penalty: FloodPenalty::DropMessage,
        };
        let mut f = FloodTracker::new(policy, 0);
        for _ in 0..5 {
            f.check(0);
        }
        assert!(matches!(f.check(0), FloodVerdict::Exceeded(_)));
        // One second later, exactly one token is back.
        assert_eq!(f.check(1000), FloodVerdict::Allow);
        assert!(matches!(f.check(1000), FloodVerdict::Exceeded(_)));
    }

    #[test]
    fn flood_bucket_never_exceeds_its_burst_capacity() {
        let policy = FloodPolicy {
            messages_per_10s: 100,
            burst: 3,
            penalty: FloodPenalty::DropMessage,
        };
        let mut f = FloodTracker::new(policy, 0);
        // An hour of silence must not bank an hour of messages.
        for i in 0..3 {
            assert_eq!(f.check(3_600_000), FloodVerdict::Allow, "message {i}");
        }
        assert!(matches!(f.check(3_600_000), FloodVerdict::Exceeded(_)));
    }

    #[test]
    fn a_warnet_bot_pace_is_within_budget() {
        // 20 messages/second sustained, which a war bot genuinely does.
        let policy = Policy::for_mode(ServerMode::Warnet).gateway_flood;
        let mut f = FloodTracker::new(policy, 0);
        let mut allowed = 0;
        for i in 0..200u64 {
            if f.check(i * 50) == FloodVerdict::Allow {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 200, "warnet policy must not throttle a normal bot");
    }

    #[test]
    fn the_same_pace_is_throttled_on_a_gaming_server() {
        let policy = Policy::for_mode(ServerMode::Gaming).game_flood;
        let mut f = FloodTracker::new(policy, 0);
        let allowed = (0..200u64)
            .filter(|i| f.check(i * 50) == FloodVerdict::Allow)
            .count();
        assert!(allowed < 200, "a human-tuned policy must throttle a bot pace");
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_panic_or_grant_tokens() {
        let policy = FloodPolicy {
            messages_per_10s: 10,
            burst: 2,
            penalty: FloodPenalty::DropMessage,
        };
        let mut f = FloodTracker::new(policy, 10_000);
        f.check(10_000);
        f.check(10_000);
        // Time travels backwards (NTP step, or a federated node with a bad clock).
        assert!(matches!(f.check(0), FloodVerdict::Exceeded(_)));
    }

    fn key(n: u8) -> KeyId {
        KeyId([n; 20])
    }

    #[test]
    fn a_key_can_only_be_live_once() {
        // The gate that actually makes a fleet expensive: addresses are cheap, keys are
        // not. Twenty simultaneous bots requires twenty keys.
        let mut r = KeyRegistry::new(500);
        assert_eq!(r.claim(key(1), 100, 0), KeyVerdict::Ok);
        assert_eq!(r.claim(key(1), 200, 0), KeyVerdict::InUse { by: 100 });
        // A different key is unaffected.
        assert_eq!(r.claim(key(2), 200, 0), KeyVerdict::Ok);
        assert_eq!(r.active_count(), 2);
    }

    #[test]
    fn twenty_bots_needs_twenty_keys() {
        let mut r = KeyRegistry::new(500);
        for i in 1..=20u8 {
            assert_eq!(r.claim(key(i), u64::from(i), 0), KeyVerdict::Ok);
        }
        assert_eq!(r.active_count(), 20);
        // Reusing any of them for a 21st bot fails.
        for i in 1..=20u8 {
            assert!(matches!(
                r.claim(key(i), 999, 0),
                KeyVerdict::InUse { .. }
            ));
        }
    }

    #[test]
    fn a_released_key_stays_in_use_through_the_cooldown() {
        // Real Battle.net reports "key in use" if you reconnect within ~500 ms. Bots are
        // written against that behaviour, so we reproduce it rather than being subtly
        // faster and confusing them.
        let mut r = KeyRegistry::new(500);
        r.claim(key(1), 100, 1_000);
        r.release(key(1), 1_000);
        assert_eq!(r.claim(key(1), 100, 1_100), KeyVerdict::InUse { by: 100 });
        assert_eq!(r.claim(key(1), 100, 1_499), KeyVerdict::InUse { by: 100 });
        assert_eq!(r.claim(key(1), 100, 1_500), KeyVerdict::Ok);
    }

    #[test]
    fn banned_keys_are_refused_even_when_free() {
        let mut r = KeyRegistry::new(500);
        r.ban(key(3));
        assert_eq!(r.claim(key(3), 1, 0), KeyVerdict::Banned);
        assert!(r.unban(key(3)));
        assert_eq!(r.claim(key(3), 1, 0), KeyVerdict::Ok);
    }

    #[test]
    fn banning_a_live_key_evicts_it() {
        let mut r = KeyRegistry::new(500);
        r.claim(key(4), 7, 0);
        assert_eq!(r.active_count(), 1);
        r.ban(key(4));
        assert_eq!(r.active_count(), 0);
        assert_eq!(r.claim(key(4), 7, 0), KeyVerdict::Banned);
    }

    #[test]
    fn cooldown_entries_do_not_accumulate() {
        // A long-running server churns through millions of sessions; the cooldown map
        // must track live churn, not the whole history.
        let mut r = KeyRegistry::new(500);
        for i in 0..2_000u64 {
            let k = KeyId([(i % 251) as u8; 20]);
            r.claim(k, i, i * 1_000);
            r.release(k, i * 1_000);
        }
        r.claim(KeyId([255; 20]), 0, 2_000_000);
        assert!(
            r.cooling_count() <= 2,
            "cooldown map holds {} entries",
            r.cooling_count()
        );
    }
}
