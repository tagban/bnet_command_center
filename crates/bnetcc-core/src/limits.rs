//! Admission control and flood control.
//!
//! Both are pure data structures driven by an explicit clock, so their behaviour is
//! deterministic and testable without sleeping. Nothing here touches a socket.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

use bnetcc_proto::FourCc;

use crate::channel::AccountId;
use crate::policy::{ConnLimits, FloodPenalty, FloodPolicy};

/// What kind of client a connection is, for limit purposes.
///
/// # Why this is two-stage
///
/// The protocol selector byte tells us *game vs gateway vs BNFTP* the instant a
/// connection is accepted, but **not which game** — the product code only arrives in
/// `SID_AUTH_INFO`, several packets later. So a game connection starts as
/// [`Self::GamePending`] and is promoted to [`Self::Game`] via
/// [`AdmissionTable::reclassify`] once the product is known.
///
/// The pending limits exist only to bound a connect flood in that window and should be
/// at least as loose as any product's, so that reclassification never rejects a client
/// the product itself would have allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClientClass {
    /// Telnet / chat gateway (`0x03`, `0x43`, `0x63`). No CD-key step exists here.
    Gateway,
    /// BNFTP file transfer (`0x02`).
    Bnftp,
    /// A game connection (`0x01`) whose product is not yet known.
    GamePending,
    /// A game client, identified by product code.
    Game(FourCc),
}

impl ClientClass {
    /// A short label for logs and metrics.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Gateway => "gateway".into(),
            Self::Bnftp => "bnftp".into(),
            Self::GamePending => "game:pending".into(),
            Self::Game(p) => format!("game:{p}"),
        }
    }
}

/// Why a connection was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Too many concurrent connections of this class from this address.
    PerIp {
        /// The configured ceiling.
        limit: u32,
    },
    /// Too many concurrent connections of this class for this account.
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

/// Tracks concurrent connections per client class, address, and account.
///
/// One instance per node. Counts are keyed by `(class, address)` so that a Brood War
/// client and a chat-gateway bot from the same address are accounted separately — which
/// is the whole point of per-type limits.
#[derive(Debug, Default)]
pub struct AdmissionTable {
    per_ip: HashMap<(ClientClass, IpAddr), u32>,
    per_account: HashMap<(ClientClass, AccountId), u32>,
    per_class: HashMap<ClientClass, u32>,
    total: u32,
    allowlist: Vec<IpAddr>,
}

impl AdmissionTable {
    /// Empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Addresses exempt from **per-IP** ceilings.
    ///
    /// An operator's own bot host belongs here. Global and per-account ceilings still
    /// apply, and so does the CD-key registry — the operator can waive the address cost,
    /// not the key cost.
    pub fn set_allowlist(&mut self, ips: Vec<IpAddr>) {
        self.allowlist = ips;
    }

    /// Current connections of a class from an address.
    #[must_use]
    pub fn ip_count(&self, class: ClientClass, ip: IpAddr) -> u32 {
        self.per_ip.get(&(class, ip)).copied().unwrap_or(0)
    }

    /// Current connections of a class for an account.
    #[must_use]
    pub fn account_count(&self, class: ClientClass, account: AccountId) -> u32 {
        self.per_account
            .get(&(class, account))
            .copied()
            .unwrap_or(0)
    }

    /// Current connections of a class, server-wide.
    #[must_use]
    pub fn class_count(&self, class: ClientClass) -> u32 {
        self.per_class.get(&class).copied().unwrap_or(0)
    }

    /// Total live connections across every class.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.total
    }

    fn would_admit(
        &self,
        class: ClientClass,
        ip: IpAddr,
        limits: ConnLimits,
    ) -> Result<(), Rejection> {
        if self.class_count(class) >= limits.global {
            return Err(Rejection::Global {
                limit: limits.global,
            });
        }
        if !self.allowlist.contains(&ip) && self.ip_count(class, ip) >= limits.per_ip {
            return Err(Rejection::PerIp {
                limit: limits.per_ip,
            });
        }
        Ok(())
    }

    fn add(&mut self, class: ClientClass, ip: IpAddr) {
        *self.per_ip.entry((class, ip)).or_insert(0) += 1;
        *self.per_class.entry(class).or_insert(0) += 1;
        self.total += 1;
    }

    fn remove(&mut self, class: ClientClass, ip: IpAddr) {
        if let Some(n) = self.per_ip.get_mut(&(class, ip)) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_ip.remove(&(class, ip));
            }
        }
        if let Some(n) = self.per_class.get_mut(&class) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_class.remove(&class);
            }
        }
        self.total = self.total.saturating_sub(1);
    }

    /// Admit a connection of `class` from `ip`, or explain why not.
    ///
    /// Checked at accept time, before a byte is read, so an address spraying connections
    /// costs one `accept` and two hash lookups each.
    ///
    /// # Errors
    ///
    /// [`Rejection`] naming the ceiling that was hit.
    pub fn admit(
        &mut self,
        class: ClientClass,
        ip: IpAddr,
        limits: ConnLimits,
    ) -> Result<(), Rejection> {
        self.would_admit(class, ip, limits)?;
        self.add(class, ip);
        Ok(())
    }

    /// Release a connection previously admitted.
    pub fn release(&mut self, class: ClientClass, ip: IpAddr) {
        self.remove(class, ip);
    }

    /// Promote a connection from one class to another once more is known about it.
    ///
    /// Used when `SID_AUTH_INFO` reveals the product: the connection moves from
    /// [`ClientClass::GamePending`] to [`ClientClass::Game`] and is re-checked against
    /// that product's limits.
    ///
    /// On rejection the connection **stays in its original class** so the caller can
    /// close it cleanly without corrupting the counts.
    ///
    /// # Errors
    ///
    /// [`Rejection`] if the target class is already at a ceiling.
    pub fn reclassify(
        &mut self,
        ip: IpAddr,
        from: ClientClass,
        to: ClientClass,
        to_limits: ConnLimits,
    ) -> Result<(), Rejection> {
        if from == to {
            return Ok(());
        }
        self.would_admit(to, ip, to_limits)?;
        self.remove(from, ip);
        self.add(to, ip);
        Ok(())
    }

    /// Bind an authenticated account to a connection of this class.
    ///
    /// Separate from [`Self::admit`] because the account is unknown at accept time.
    ///
    /// # Errors
    ///
    /// [`Rejection::PerAccount`] when the account already holds its maximum.
    pub fn bind_account(
        &mut self,
        class: ClientClass,
        account: AccountId,
        limits: ConnLimits,
    ) -> Result<(), Rejection> {
        if self.account_count(class, account) >= limits.per_account {
            return Err(Rejection::PerAccount {
                limit: limits.per_account,
            });
        }
        *self.per_account.entry((class, account)).or_insert(0) += 1;
        Ok(())
    }

    /// Release an account binding.
    pub fn release_account(&mut self, class: ClientClass, account: AccountId) {
        if let Some(n) = self.per_account.get_mut(&(class, account)) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_account.remove(&(class, account));
            }
        }
    }

    /// Number of `(class, address)` pairs currently tracked.
    ///
    /// Unbounded growth here would be a slow leak; the invariant is that it returns to
    /// zero when every connection closes.
    #[must_use]
    pub fn tracked_entries(&self) -> usize {
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
/// **This is the real economic gate on game-client bot fleets.** Addresses are cheap to
/// rent; keys are not. A per-IP limit alone is sidestepped by anyone with a handful of
/// proxies, but running twenty simultaneous bots genuinely requires twenty keys if the
/// server enforces uniqueness — which is why real Battle.net answers `SID_AUTH_CHECK`
/// with `0x201` "CD key in use" and names the holder.
///
/// Note that this does **not** apply to the telnet/chat gateway, which has no CD-key step
/// at all. There the address is the only cost available to charge, which is why the
/// gateway is capped at one connection per address in every mode.
///
/// The cooldown reproduces a real Battle.net quirk that bots already expect: a key stays
/// marked in use for a short window after a session ends, so reconnecting too fast
/// reports "key in use" rather than succeeding.
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
    use bnetcc_proto::product;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    fn gateway_limits(mode: ServerMode) -> ConnLimits {
        Policy::for_mode(mode).clients.gateway
    }

    #[test]
    fn the_gateway_is_one_per_ip() {
        let limits = gateway_limits(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        assert!(t.admit(ClientClass::Gateway, ip(1), limits).is_ok());
        assert_eq!(
            t.admit(ClientClass::Gateway, ip(1), limits),
            Err(Rejection::PerIp { limit: 1 })
        );
        assert!(t.admit(ClientClass::Gateway, ip(2), limits).is_ok());
    }

    #[test]
    fn a_gateway_bot_fleet_costs_one_address_per_bot_even_on_a_warnet() {
        // The entry cost, stated as a test. The gateway is keyless, so the address is
        // the only cost available to charge — and warnet mode does not waive it.
        let limits = gateway_limits(ServerMode::Warnet);
        let mut t = AdmissionTable::new();
        assert!(t.admit(ClientClass::Gateway, ip(1), limits).is_ok());
        assert_eq!(
            t.admit(ClientClass::Gateway, ip(1), limits),
            Err(Rejection::PerIp { limit: 1 })
        );
        for i in 2..=21u8 {
            assert!(t.admit(ClientClass::Gateway, ip(i), limits).is_ok());
        }
        assert_eq!(t.class_count(ClientClass::Gateway), 21);
    }

    #[test]
    fn game_clients_are_not_subject_to_the_gateway_cap() {
        // A household or LAN café shares one address across several real players.
        let p = Policy::for_mode(ServerMode::Gaming);
        let game = p.clients.for_class(ClientClass::Game(product::SEXP));
        let mut t = AdmissionTable::new();
        for i in 0..game.per_ip {
            assert!(
                t.admit(ClientClass::Game(product::SEXP), ip(1), game).is_ok(),
                "player {i} from a shared address refused"
            );
        }
        // And a gateway bot from that same address is counted separately.
        assert!(t
            .admit(ClientClass::Gateway, ip(1), p.clients.gateway)
            .is_ok());
    }

    #[test]
    fn classes_are_counted_independently() {
        let p = Policy::for_mode(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        t.admit(ClientClass::Gateway, ip(1), p.clients.gateway).unwrap();
        t.admit(
            ClientClass::Game(product::SEXP),
            ip(1),
            p.clients.game_default,
        )
        .unwrap();
        t.admit(ClientClass::Bnftp, ip(1), p.clients.bnftp).unwrap();
        assert_eq!(t.ip_count(ClientClass::Gateway, ip(1)), 1);
        assert_eq!(t.ip_count(ClientClass::Game(product::SEXP), ip(1)), 1);
        assert_eq!(t.ip_count(ClientClass::Bnftp, ip(1)), 1);
        assert_eq!(t.total(), 3);
    }

    #[test]
    fn products_are_limited_separately_from_each_other() {
        // An operator may allow eight Brood War from a café but only two WarCraft III.
        let mut p = Policy::for_mode(ServerMode::Gaming);
        p.clients.set_product(
            product::WAR3,
            ConnLimits {
                per_ip: 2,
                per_account: 1,
                global: 512,
            },
        );
        let mut t = AdmissionTable::new();
        let w3 = p.clients.for_class(ClientClass::Game(product::WAR3));
        let bw = p.clients.for_class(ClientClass::Game(product::SEXP));

        t.admit(ClientClass::Game(product::WAR3), ip(1), w3).unwrap();
        t.admit(ClientClass::Game(product::WAR3), ip(1), w3).unwrap();
        assert_eq!(
            t.admit(ClientClass::Game(product::WAR3), ip(1), w3),
            Err(Rejection::PerIp { limit: 2 })
        );
        // Brood War from the same address is unaffected by WarCraft III's ceiling.
        for _ in 0..bw.per_ip {
            assert!(t.admit(ClientClass::Game(product::SEXP), ip(1), bw).is_ok());
        }
    }

    #[test]
    fn a_pending_connection_is_promoted_once_the_product_is_known() {
        let p = Policy::for_mode(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        t.admit(ClientClass::GamePending, ip(1), p.clients.game_pending)
            .unwrap();
        assert_eq!(t.class_count(ClientClass::GamePending), 1);

        let target = ClientClass::Game(product::SEXP);
        t.reclassify(
            ip(1),
            ClientClass::GamePending,
            target,
            p.clients.for_class(target),
        )
        .unwrap();
        assert_eq!(t.class_count(ClientClass::GamePending), 0);
        assert_eq!(t.class_count(target), 1);
        assert_eq!(t.total(), 1, "promotion must not double-count");
    }

    #[test]
    fn a_rejected_promotion_leaves_the_connection_in_its_original_class() {
        // Otherwise a refused reclassification corrupts the counts and slowly leaks.
        let mut p = Policy::for_mode(ServerMode::Gaming);
        p.clients.set_product(
            product::WAR3,
            ConnLimits {
                per_ip: 1,
                per_account: 1,
                global: 512,
            },
        );
        let w3 = ClientClass::Game(product::WAR3);
        let w3_limits = p.clients.for_class(w3);
        let mut t = AdmissionTable::new();

        t.admit(w3, ip(1), w3_limits).unwrap();
        t.admit(ClientClass::GamePending, ip(1), p.clients.game_pending)
            .unwrap();
        assert_eq!(
            t.reclassify(ip(1), ClientClass::GamePending, w3, w3_limits),
            Err(Rejection::PerIp { limit: 1 })
        );
        assert_eq!(t.class_count(ClientClass::GamePending), 1);
        assert_eq!(t.class_count(w3), 1);
        assert_eq!(t.total(), 2);

        // And releasing the pending connection still balances the books.
        t.release(ClientClass::GamePending, ip(1));
        t.release(w3, ip(1));
        assert_eq!(t.total(), 0);
        assert_eq!(t.tracked_entries(), 0);
    }

    #[test]
    fn reclassify_to_the_same_class_is_a_no_op() {
        let p = Policy::for_mode(ServerMode::Gaming);
        let c = ClientClass::Game(product::SEXP);
        let mut t = AdmissionTable::new();
        t.admit(c, ip(1), p.clients.game_default).unwrap();
        t.reclassify(ip(1), c, c, p.clients.game_default).unwrap();
        assert_eq!(t.total(), 1);
    }

    #[test]
    fn releasing_frees_a_slot_and_leaks_nothing() {
        let limits = gateway_limits(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        t.admit(ClientClass::Gateway, ip(1), limits).unwrap();
        assert_eq!(t.tracked_entries(), 1);
        t.release(ClientClass::Gateway, ip(1));
        assert_eq!(t.ip_count(ClientClass::Gateway, ip(1)), 0);
        assert_eq!(t.total(), 0);
        assert_eq!(t.tracked_entries(), 0, "entries must be reclaimed");
        assert!(t.admit(ClientClass::Gateway, ip(1), limits).is_ok());
    }

    #[test]
    fn churn_does_not_grow_the_table() {
        let limits = gateway_limits(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        for i in 0..5_000u32 {
            let addr = IpAddr::V4(Ipv4Addr::from(i.to_be_bytes()));
            t.admit(ClientClass::Gateway, addr, limits).unwrap();
            t.release(ClientClass::Gateway, addr);
        }
        assert_eq!(t.tracked_entries(), 0);
        assert_eq!(t.total(), 0);
        assert_eq!(t.class_count(ClientClass::Gateway), 0);
    }

    #[test]
    fn the_allowlist_is_the_operators_deliberate_exception() {
        let limits = gateway_limits(ServerMode::Warnet);
        let mut t = AdmissionTable::new();
        assert!(t.admit(ClientClass::Gateway, ip(9), limits).is_ok());
        assert!(t.admit(ClientClass::Gateway, ip(9), limits).is_err());

        let mut t = AdmissionTable::new();
        t.set_allowlist(vec![ip(9)]);
        for _ in 0..20 {
            assert!(t.admit(ClientClass::Gateway, ip(9), limits).is_ok());
        }
        // The class ceiling still applies to allowlisted hosts.
        let tight = ConnLimits {
            per_ip: 1,
            per_account: 1,
            global: 20,
        };
        assert_eq!(
            t.admit(ClientClass::Gateway, ip(9), tight),
            Err(Rejection::Global { limit: 20 })
        );
    }

    #[test]
    fn per_account_limits_are_independent_of_address_and_class() {
        let p = Policy::for_mode(ServerMode::Gaming);
        let mut t = AdmissionTable::new();
        assert!(t
            .bind_account(ClientClass::Gateway, 42, p.clients.gateway)
            .is_ok());
        assert_eq!(
            t.bind_account(ClientClass::Gateway, 42, p.clients.gateway),
            Err(Rejection::PerAccount { limit: 1 })
        );
        // The same account on a game client is a separate budget.
        assert!(t
            .bind_account(
                ClientClass::Game(product::SEXP),
                42,
                p.clients.game_default
            )
            .is_ok());
        t.release_account(ClientClass::Gateway, 42);
        assert!(t
            .bind_account(ClientClass::Gateway, 42, p.clients.gateway)
            .is_ok());
    }

    #[test]
    fn global_ceiling_is_checked_first() {
        let limits = ConnLimits {
            per_ip: 100,
            per_account: 100,
            global: 2,
        };
        let mut t = AdmissionTable::new();
        t.admit(ClientClass::Gateway, ip(1), limits).unwrap();
        t.admit(ClientClass::Gateway, ip(2), limits).unwrap();
        assert_eq!(
            t.admit(ClientClass::Gateway, ip(3), limits),
            Err(Rejection::Global { limit: 2 })
        );
    }

    #[test]
    fn class_labels_are_readable() {
        assert_eq!(ClientClass::Gateway.label(), "gateway");
        assert_eq!(ClientClass::Game(product::SEXP).label(), "game:SEXP");
        assert_eq!(ClientClass::GamePending.label(), "game:pending");
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
        assert_eq!(f.check(0), FloodVerdict::Exceeded(FloodPenalty::DropMessage));
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
        let policy = Policy::for_mode(ServerMode::Warnet).gateway_flood;
        let mut f = FloodTracker::new(policy, 0);
        let allowed = (0..200u64)
            .filter(|i| f.check(i * 50) == FloodVerdict::Allow)
            .count();
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
        // Time travels backwards (NTP step, or a node with a bad clock).
        assert!(matches!(f.check(0), FloodVerdict::Exceeded(_)));
    }

    fn key(n: u8) -> KeyId {
        KeyId([n; 20])
    }

    #[test]
    fn a_key_can_only_be_live_once() {
        let mut r = KeyRegistry::new(500);
        assert_eq!(r.claim(key(1), 100, 0), KeyVerdict::Ok);
        assert_eq!(r.claim(key(1), 200, 0), KeyVerdict::InUse { by: 100 });
        assert_eq!(r.claim(key(2), 200, 0), KeyVerdict::Ok);
        assert_eq!(r.active_count(), 2);
    }

    #[test]
    fn twenty_game_bots_needs_twenty_keys() {
        let mut r = KeyRegistry::new(500);
        for i in 1..=20u8 {
            assert_eq!(r.claim(key(i), u64::from(i), 0), KeyVerdict::Ok);
        }
        assert_eq!(r.active_count(), 20);
        for i in 1..=20u8 {
            assert!(matches!(r.claim(key(i), 999, 0), KeyVerdict::InUse { .. }));
        }
    }

    #[test]
    fn a_released_key_stays_in_use_through_the_cooldown() {
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
