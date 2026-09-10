//! Shared node state.
//!
//! Channels and subscribers live behind a `std::sync::Mutex` with **no `await` inside any
//! critical section** — every lock is taken, mutated, and released without yielding. That
//! is deliberate: a `tokio::sync::Mutex` invites holding a lock across an await point,
//! which is how a chat server acquires a global stall.
//!
//! Accounts are not in that lock — they live behind the storage actor (`crate::storage`),
//! reached over a channel, since persistence is exactly the kind of `await`-shaped work
//! the rule above exists to keep out.
//!
//! Fanout encodes each frame **once** and hands every subscriber an `Arc` of the same
//! bytes. A 200-user channel therefore costs one encode and 200 pointer clones, not 200
//! encodes. Channel fanout, not connection count, is the real scaling limit at this size.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bnetcc_core::ads::AdRotation;
use bnetcc_core::channel::{AccountId, Channel, ChannelClass, JoinDenial, OpError};
use bnetcc_core::limits::{AdmissionTable, ClientClass, KeyId, KeyRegistry, KeyVerdict, Rejection};
use bnetcc_core::policy::Policy;
use bnetcc_proto::bncs::{encode_frame, Frame};
use bnetcc_proto::chat::normalize_channel_name;
use tokio::sync::{mpsc, Notify};

use crate::moderation::BanStore;

/// Bytes ready for the wire, shared by every recipient of a broadcast.
pub type Wire = Arc<Vec<u8>>;

/// A per-session personal ignore ("squelch") list: the base account names whose channel
/// chat this session does not want to see. Lives on [`Outbound`] so channel fan-out can
/// consult the *recipient's* list cheaply. The `any` flag keeps the common case (nobody
/// ignored) to a single relaxed load with no locking on the broadcast hot path.
#[derive(Debug, Default)]
pub struct IgnoreSet {
    any: std::sync::atomic::AtomicBool,
    names: Mutex<HashSet<String>>,
}

impl IgnoreSet {
    /// Add a name (stored lowercased). Returns whether it was newly added.
    pub fn add(&self, name: &str) -> bool {
        let mut g = self.names.lock().expect("ignore lock");
        let added = g.insert(name.to_ascii_lowercase());
        self.any.store(!g.is_empty(), Ordering::Relaxed);
        added
    }

    /// Remove a name. Returns whether it was present.
    pub fn remove(&self, name: &str) -> bool {
        let mut g = self.names.lock().expect("ignore lock");
        let removed = g.remove(&name.to_ascii_lowercase());
        self.any.store(!g.is_empty(), Ordering::Relaxed);
        removed
    }

    /// Whether `lowercased` (an already-lowercased base name) is ignored. Short-circuits on
    /// the empty case without locking.
    #[must_use]
    pub fn contains(&self, lowercased: &str) -> bool {
        if !self.any.load(Ordering::Relaxed) {
            return false;
        }
        self.names.lock().expect("ignore lock").contains(lowercased)
    }

    /// The ignored names, for the `/squelch` listing.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.names.lock().expect("ignore lock").iter().cloned().collect()
    }
}

/// The write side of one connection.
#[derive(Debug, Clone)]
pub struct Outbound {
    tx: mpsc::Sender<Wire>,
    /// This session's personal ignore list, shared across every clone of the handle (each
    /// channel subscription is a clone), so a `/squelch` takes effect everywhere at once.
    ignores: Arc<IgnoreSet>,
}

impl Outbound {
    /// Wrap a sender.
    #[must_use]
    pub fn new(tx: mpsc::Sender<Wire>) -> Self {
        Self { tx, ignores: Arc::new(IgnoreSet::default()) }
    }

    /// This session's personal ignore list.
    #[must_use]
    pub fn ignores(&self) -> &Arc<IgnoreSet> {
        &self.ignores
    }

    /// Queue pre-encoded bytes.
    ///
    /// Returns `false` when the queue is full, which means the peer is not reading.
    /// The caller must then close the connection: buffering without bound is how PvPGN
    /// crashed until it added a hard cap in 2014, and silently dropping frames would
    /// desynchronise a client's view of a channel.
    #[must_use]
    pub fn send(&self, wire: &Wire) -> bool {
        self.tx.try_send(Arc::clone(wire)).is_ok()
    }

    /// Encode and queue one frame.
    #[must_use]
    pub fn send_frame(&self, frame: &Frame) -> bool {
        let mut buf = Vec::with_capacity(frame.wire_len());
        if encode_frame(frame, &mut buf).is_err() {
            return false;
        }
        self.send(&Arc::new(buf))
    }
}

/// The outcome of an operator moderation action (kick / ban / designate / unban).
pub enum ModResult {
    /// The actor does not hold operator in the channel.
    NotOperator,
    /// No such channel, or the target is not present.
    NotFound,
    /// The actor targeted themselves.
    CannotTargetSelf,
    /// Success. For kick/ban, `target_out` is the removed session's fan-out handle (so the
    /// caller can notify it); `new_operator` is any succession the removal triggered.
    Ok {
        target_out: Option<Outbound>,
        new_operator: Option<AccountId>,
    },
}

/// Remove a subscriber by display name from a channel's fan-out list, returning its handle.
fn pull_subscriber(inner: &mut Inner, key: &[u8], name: &str) -> Option<Outbound> {
    let subs = inner.subscribers.get_mut(key)?;
    let pos = subs.iter().position(|(n, _)| n.eq_ignore_ascii_case(name))?;
    Some(subs.remove(pos).1)
}

/// The outcome of trying to claim a CD key at `SID_AUTH_CHECK` time.
#[derive(Debug, Clone)]
pub enum KeyClaim {
    /// Nobody else holds this key right now.
    Ok,
    /// Another live session holds it. `holder` is empty if that session has not logged
    /// in yet.
    InUse {
        /// Best-known account name of the current holder.
        holder: String,
    },
    /// The key is banned network-wide.
    Banned,
}

/// A registered account.
///
/// A write-through view of whatever `bnetcc_storage::model::Account` the storage actor
/// holds (see `crate::storage`) — persisted if `storage.path` is configured, in-memory
/// and gone on restart otherwise. **Configure `storage.path` before relying on this** —
/// an unset one is exactly Atlas's situation, where every account evaporates on restart,
/// and it is the main reason Atlas appears more stable than it is.
#[derive(Debug, Clone)]
pub struct Account {
    /// Stable identifier.
    pub id: AccountId,
    /// Display name, as registered.
    pub name: String,
    /// `XSHA1(lowercase(password))`.
    ///
    /// Password-equivalent. In a federation this never leaves the hub; see
    /// `docs/FEDERATION.md` §4.
    pub password_hash: [u8; 20],
}

#[derive(Default)]
struct Inner {
    channels: HashMap<Vec<u8>, Channel>,
    /// Subscribers per channel, keyed by normalised channel name. Each entry is a
    /// `(display_name, Outbound)` pair: the display name (server-wide unique, with any
    /// `#N` suffix) is the per-session fanout key, so two sessions of one account each
    /// receive their own copy and self-exclusion is per-session.
    subscribers: HashMap<Vec<u8>, Vec<(String, Outbound)>>,
    /// Advertised games, keyed by lowercased game name. A node is a directory of the
    /// games its clients host; the games themselves are peer-to-peer (`SID_STARTADVEX3`
    /// to advertise, `SID_GETADVLISTEX` to discover). See [`GameAd`].
    games: HashMap<Vec<u8>, GameAd>,
    /// Per-channel queue of pending leave-notification frames (each a pre-encoded
    /// `EID_LEAVE`). Membership *state* is updated immediately when a user leaves; only the
    /// *notification* is deferred here so a burst of departures (a load test disconnecting
    /// thousands at once) coalesces into one batched write per subscriber instead of one
    /// per departure, which is what a bounded outbound queue can actually absorb. Flushed by
    /// [`Node::flush_pending`] on a timer and, to preserve ordering, before any immediate
    /// broadcast on the same channel.
    pending_leaves: HashMap<Vec<u8>, Vec<Vec<u8>>>,
}

/// Flush a channel's queued leave notifications: concatenate them into one buffer (BNCS
/// accepts back-to-back `SID_CHATEVENT` frames) and hand every subscriber the same `Arc`, so
/// N departures cost one queue slot per subscriber rather than N. Takes `&mut Inner` because
/// it both drains `pending_leaves` and reads `subscribers`.
fn flush_leaves_locked(inner: &mut Inner, key: &[u8]) {
    let Some(events) = inner.pending_leaves.remove(key) else {
        return;
    };
    if events.is_empty() {
        return;
    }
    let Some(subs) = inner.subscribers.get(key) else {
        return; // channel gone; nobody left to notify
    };
    let mut batch = Vec::with_capacity(events.iter().map(Vec::len).sum());
    for e in &events {
        batch.extend_from_slice(e);
    }
    let batch = Arc::new(batch);
    for (_name, out) in subs {
        let _ = out.send(&batch);
    }
}

/// A game advertised by a hosting client, tracked so other clients can discover it via
/// `SID_GETADVLISTEX`.
///
/// The wire fields (type, parameter, host address, statstring) are carried through mostly
/// opaquely — the server is a directory, not a referee — and echoed back in the game list.
#[derive(Debug, Clone)]
pub struct GameAd {
    /// Display name as advertised.
    pub name: Vec<u8>,
    /// Join password; empty for a public game.
    pub password: Vec<u8>,
    /// Map/game info blob the client renders. Opaque to the server.
    pub statstring: Vec<u8>,
    /// Game type (melee, ffa, ladder, …) — echoed, not interpreted.
    pub game_type: u16,
    /// Product-specific sub-type / parameter.
    pub parameter: u16,
    /// Host-supplied state/flags.
    pub state: u32,
    /// Host's advertised game port.
    pub port: u16,
    /// Host's address, as seen by the node.
    pub host_ip: std::net::Ipv4Addr,
    /// The hosting account.
    pub host: AccountId,
    /// When the ad was created, for the elapsed-time field.
    pub created: std::time::Instant,
}

/// A logged-in session, registered so staff moderation can reach it across channels — to
/// resolve its address for an IP ban, or to force it off for a tag ban.
struct SessionEntry {
    /// The peer address this session connected from.
    ip: IpAddr,
    /// Its write side, so a removal notice can be queued before it is cut.
    out: Outbound,
    /// A one-shot signal the read loop selects on; firing it makes the session close
    /// cleanly (running its normal cleanup), unlike aborting the task.
    kill: Arc<Notify>,
}

/// Everything one node shares between connections.
pub struct Node {
    /// Effective policy, already narrowed by the hub if federated.
    pub policy: Policy,
    /// Client version restriction. Off by default — see `crate::config::VersionsConfig`.
    pub version_policy: crate::config::VersionPolicy,
    /// Accounts granted the Battle.net Administrator (sysop) role.
    pub admins: crate::config::AdminsConfig,
    /// Whether the first arrival to a private channel is auto-opped (Op/Clan always are,
    /// `Public *` never are; this governs only the private case).
    pub auto_op_private: bool,
    /// Pre-defined channel rules (listed/public/persist/telnet/game-only/flag-gated).
    pub channel_rules: crate::config::ChannelRules,
    /// Whether to enforce one live session per CD key.
    pub cd_key_uniqueness: bool,
    /// Shared UDP socket bound to `:6112`, used to send the login-time `PKT_SERVERPING`
    /// that un-greys Create/Join on classic clients; `None` if the UDP bind failed.
    pub udp_socket: Option<Arc<tokio::net::UdpSocket>>,
    /// Server name shown to clients.
    pub name: String,
    /// Message of the day.
    pub motd: String,
    /// Per-category default channel size caps (`0` = unlimited).
    pub channel_caps: ChannelCaps,
    /// Configured advertisement banners. Empty means we never answer `SID_CHECKAD`,
    /// which leaves the client showing whatever it already has.
    pub ads: AdRotation,
    /// Directory of operator-supplied files served over BNFTP (version-check MPQ,
    /// `icons.bni`, `tos.txt`, ad images). `None` refuses every BNFTP request.
    pub files_dir: Option<std::path::PathBuf>,
    /// When this node started, for the status UI's uptime figure.
    started: std::time::Instant,
    /// Display names currently in use across the whole node (lowercased). A second login of
    /// an account already online is disambiguated with `#2`, `#3`… so both can coexist.
    active_names: Mutex<HashSet<String>>,
    inner: Mutex<Inner>,
    /// One table for every client class. Counts are keyed by `(class, address)`, so a
    /// Brood War client and a chat-gateway bot from the same address are accounted
    /// separately — which is the entire point of per-type limits.
    admission: Mutex<AdmissionTable>,
    connections: AtomicU64,
    /// Highest live connection count seen since startup, for the status UI.
    peak_connections: AtomicU64,
    /// Accounts live behind this actor, not the `Mutex<Inner>` above — persistence is a
    /// disk write, and a disk write must never happen inside a lock that channel fanout
    /// also takes. See `crate::storage`.
    storage: crate::storage::StorageHandle,
    /// One live session per CD key — see `KeyClaim` and `docs/WARNET.md`.
    key_registry: Mutex<KeyRegistry>,
    /// Best-known display name for whoever currently holds each claimed key, so a
    /// rejection can name the holder. Populated once a claiming session logs in
    /// (`record_key_holder_name`); empty until then, since `SID_AUTH_CHECK` happens
    /// before the account is known.
    key_holder_names: Mutex<HashMap<KeyId, String>>,
    /// Staff-set tag bans, IP bans, and mutes, persisted to disk. See [`BanStore`].
    pub bans: BanStore,
    /// Every logged-in session, keyed by lowercased display name, so staff moderation can
    /// reach a session in any channel (or none). Populated at logon, cleared on disconnect.
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

/// The config-derived settings a [`Node`] is built from, bundled so the constructor takes
/// a handful of arguments rather than a dozen positional ones. Resolved from
/// [`crate::config::Config`] in `main`, or built with defaults in tests.
pub struct NodeConfig {
    /// Effective policy, already narrowed by the hub if federated.
    pub policy: Policy,
    /// Server name shown to clients.
    pub name: String,
    /// Message of the day.
    pub motd: String,
    /// Addresses exempt from the one-gateway-connection-per-IP rule.
    pub gateway_allowlist: Vec<IpAddr>,
    /// Client version restriction.
    pub version_policy: crate::config::VersionPolicy,
    /// Directory of files served over BNFTP; `None` refuses every request.
    pub files_dir: Option<std::path::PathBuf>,
    /// Administrator accounts.
    pub admins: crate::config::AdminsConfig,
    /// Whether private channels auto-op their first arrival.
    pub auto_op_private: bool,
    /// Pre-defined channel rules (listed/public/persist/telnet/game-only/flag-gated).
    pub channel_rules: crate::config::ChannelRules,
    /// Whether to enforce one live session per CD key. Off lets a fleet share a key.
    pub cd_key_uniqueness: bool,
    /// Per-category default channel size caps (`0` = unlimited).
    pub channel_caps: ChannelCaps,
    /// Shared UDP socket bound to `:6112` for sending the login-time UDP ping; `None` if the
    /// bind failed (classic clients then keep the No-UDP flag and games stay greyed).
    pub udp_socket: Option<Arc<tokio::net::UdpSocket>>,
    /// Where to persist staff bans/mutes. `None` keeps them in memory only (tests).
    pub bans_path: Option<std::path::PathBuf>,
}

/// Default per-category channel size caps. `0` means unlimited for that category. A
/// per-channel `max_users` in `[[channels.defined]]` overrides the category default.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelCaps {
    /// Private (user-created) channels.
    pub private: usize,
    /// `Public *` channels.
    pub public: usize,
    /// `Op`/`Clan` channels.
    pub clan: usize,
}

impl ChannelCaps {
    /// The cap for a channel, chosen from its name category (see
    /// [`bnetcc_core::channel::classify_name`]). `Op` and `Clan` share the clan cap.
    #[must_use]
    pub fn for_name(&self, display: &str) -> usize {
        use bnetcc_core::channel::NameKind;
        match bnetcc_core::channel::classify_name(display) {
            NameKind::Public => self.public,
            NameKind::OpOrClan => self.clan,
            NameKind::Private => self.private,
        }
    }
}

impl Node {
    /// Build a node from its resolved configuration and a storage handle.
    #[must_use]
    pub fn new(cfg: NodeConfig, storage: crate::storage::StorageHandle) -> Self {
        let mut admission = AdmissionTable::new();
        admission.set_allowlist(cfg.gateway_allowlist);
        Self {
            policy: cfg.policy,
            version_policy: cfg.version_policy,
            admins: cfg.admins,
            auto_op_private: cfg.auto_op_private,
            channel_rules: cfg.channel_rules,
            cd_key_uniqueness: cfg.cd_key_uniqueness,
            udp_socket: cfg.udp_socket,
            name: cfg.name,
            motd: cfg.motd,
            channel_caps: cfg.channel_caps,
            ads: AdRotation::default(),
            files_dir: cfg.files_dir,
            started: std::time::Instant::now(),
            active_names: Mutex::new(HashSet::new()),
            inner: Mutex::new(Inner::default()),
            admission: Mutex::new(admission),
            connections: AtomicU64::new(0),
            peak_connections: AtomicU64::new(0),
            storage,
            // 500ms matches observed real-Battle.net behaviour — see KeyRegistry's docs.
            key_registry: Mutex::new(KeyRegistry::new(500)),
            key_holder_names: Mutex::new(HashMap::new()),
            bans: BanStore::load(cfg.bans_path),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Live connection count, for metrics.
    #[must_use]
    pub fn connection_count(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    /// Admit a connection of `class` from `ip`.
    ///
    /// # Errors
    ///
    /// The rejection reason, for logging and metrics.
    pub fn admit(&self, class: ClientClass, ip: IpAddr) -> Result<(), Rejection> {
        let limits = self.policy.clients.for_class(class);
        let r = self
            .admission
            .lock()
            .expect("admission lock")
            .admit(class, ip, limits);
        if r.is_ok() {
            let live = self.connections.fetch_add(1, Ordering::Relaxed) + 1;
            self.peak_connections.fetch_max(live, Ordering::Relaxed);
        }
        r
    }

    /// The highest live connection count seen since startup, for the status UI.
    #[must_use]
    pub fn peak_connections(&self) -> u64 {
        self.peak_connections.load(Ordering::Relaxed)
    }

    /// Release a connection of `class`.
    ///
    /// The caller must pass the class the connection currently holds, which may differ
    /// from the one it was admitted under if it was promoted — see [`Self::reclassify`].
    pub fn release(&self, class: ClientClass, ip: IpAddr) {
        self.admission
            .lock()
            .expect("admission lock")
            .release(class, ip);
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Promote a connection once `SID_AUTH_INFO` reveals its product.
    ///
    /// On rejection the connection stays in its original class, so the caller can close
    /// it without corrupting the counts.
    ///
    /// # Errors
    ///
    /// The rejection reason for the target class.
    pub fn reclassify(
        &self,
        ip: IpAddr,
        from: ClientClass,
        to: ClientClass,
    ) -> Result<(), Rejection> {
        let limits = self.policy.clients.for_class(to);
        self.admission
            .lock()
            .expect("admission lock")
            .reclassify(ip, from, to, limits)
    }

    /// Look up an account by name, case-insensitively.
    pub async fn account(&self, name: &str) -> Option<Account> {
        self.storage.account_by_name(name).await
    }

    /// Claim a server-wide-unique display name for a new session: `base` if it is free,
    /// otherwise `base#2`, `base#3`, … (the lowest free suffix). Lets several logins of one
    /// account coexist, each shown distinctly. Pair every claim with [`Self::release_name`].
    #[must_use]
    pub fn claim_name(&self, base: &str) -> String {
        let mut names = self.active_names.lock().expect("names lock");
        let mut candidate = base.to_string();
        let mut n = 1u32;
        // `HashSet::insert` returns false when the (lowercased) name is already present.
        while !names.insert(candidate.to_ascii_lowercase()) {
            n += 1;
            candidate = format!("{base}#{n}");
        }
        candidate
    }

    /// Release a display name claimed with [`Self::claim_name`], freeing it for reuse.
    pub fn release_name(&self, display: &str) {
        self.active_names
            .lock()
            .expect("names lock")
            .remove(&display.to_ascii_lowercase());
    }

    /// Record one finished-game outcome against an account's `Record\<product>\0\` counters.
    /// Returns the new counter value, or an error string for logging.
    pub async fn record_game(
        &self,
        account_id: bnetcc_core::AccountId,
        product: &str,
        outcome: crate::storage::GameOutcome,
    ) -> Result<u64, String> {
        self.storage.record_game(account_id, product, outcome).await
    }

    /// Read the given attribute keys for an account by name, filtered to what any peer may
    /// read (records, profile, non-secret system keys). Safe to return to a client.
    pub async fn read_readable_attrs(
        &self,
        account_name: &str,
        keys: Vec<bnetcc_storage::attr::AttrKey>,
    ) -> bnetcc_storage::attr::AttrMap {
        self.storage.read_readable_attrs(account_name, keys).await
    }

    /// Create an account.
    ///
    /// # Errors
    ///
    /// [`crate::storage::CreateAccountError::NameTaken`] if the name is already
    /// registered, or [`crate::storage::CreateAccountError::Invalid`] if it fails
    /// validation (see `bnetcc_storage::validate_account_name`).
    pub async fn create_account(
        &self,
        name: &str,
        password_hash: [u8; 20],
    ) -> Result<Account, crate::storage::CreateAccountError> {
        self.storage.create_account(name, password_hash).await
    }

    /// Try to claim a CD key for a session that has not logged in yet.
    ///
    /// `SID_AUTH_CHECK` happens before `SID_LOGONRESPONSE2`, so there is no account to
    /// record as the holder — that gets filled in later by
    /// [`Self::record_key_holder_name`] if this session goes on to log in. Until then a
    /// rejection names whichever account most recently logged in while holding the key,
    /// or an empty string if nobody has yet.
    pub fn claim_key(&self, key: KeyId, now_ms: u64) -> KeyClaim {
        let verdict = self
            .key_registry
            .lock()
            .expect("key registry lock")
            .claim(key, 0, now_ms);
        match verdict {
            KeyVerdict::Ok => KeyClaim::Ok,
            KeyVerdict::Banned => KeyClaim::Banned,
            KeyVerdict::InUse { .. } => KeyClaim::InUse {
                holder: self
                    .key_holder_names
                    .lock()
                    .expect("key holder names lock")
                    .get(&key)
                    .cloned()
                    .unwrap_or_default(),
            },
        }
    }

    /// Release a key a session claimed, whether or not it ever logged in.
    pub fn release_key(&self, key: KeyId, now_ms: u64) {
        self.key_registry.lock().expect("key registry lock").release(key, now_ms);
        self.key_holder_names.lock().expect("key holder names lock").remove(&key);
    }

    /// Record which account is holding a claimed key, once its session logs in.
    pub fn record_key_holder_name(&self, key: KeyId, name: String) {
        self.key_holder_names
            .lock()
            .expect("key holder names lock")
            .insert(key, name);
    }

    /// Names of all live channels, for `SID_GETCHANNELLIST`.
    #[must_use]
    pub fn channel_names(&self) -> Vec<String> {
        use std::collections::BTreeSet;
        // Listed pre-defined channels always appear, even when empty; live channels are
        // added on top. Dedupe by normalised name so a live instance of a defined channel
        // is not listed twice.
        let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut names = Vec::new();
        for rule in self.channel_rules.listed() {
            let key = normalize_channel_name(rule.display.as_bytes());
            if seen.insert(key) {
                names.push(rule.display.clone());
            }
        }
        let inner = self.inner.lock().expect("node lock");
        for c in inner.channels.values() {
            if seen.insert(c.name().to_vec()) {
                names.push(c.display().to_string());
            }
        }
        names
    }

    /// Display names of everyone currently in the channel keyed by `key` (already
    /// normalised), for the `/who` command. Empty if no such live channel exists.
    #[must_use]
    pub fn channel_occupant_names(&self, key: &[u8]) -> Vec<String> {
        let inner = self.inner.lock().expect("node lock");
        inner
            .channels
            .get(key)
            .map(|c| c.members().iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default()
    }

    /// A read-only snapshot of the node for the status UI (`crate::status`). Takes the node
    /// lock once and copies out owned, serialisable data.
    #[must_use]
    pub fn status_snapshot(&self) -> crate::status::Snapshot {
        let now = std::time::Instant::now();
        // These read atomics, not the node lock, so taking them inside the lock is safe.
        let connections = self.connection_count();
        let peak_connections = self.peak_connections();
        let inner = self.inner.lock().expect("node lock");
        let mut channels: Vec<crate::status::ChannelInfo> = inner
            .channels
            .values()
            .map(|c| {
                let users: Vec<String> = c.members().iter().map(|m| m.name.clone()).collect();
                // Resolve the operator's account to its display name for the UI.
                let operator = c.operator().and_then(|op| {
                    c.members().iter().find(|m| m.account == op).map(|m| m.name.clone())
                });
                crate::status::ChannelInfo {
                    name: c.display().to_string(),
                    user_count: users.len(),
                    users,
                    operator,
                }
            })
            .collect();
        channels.sort_by(|a, b| a.name.cmp(&b.name));
        let online_users = channels.iter().map(|c| c.user_count).sum();
        let mut games: Vec<crate::status::GameInfo> = inner
            .games
            .values()
            .map(|g| crate::status::GameInfo {
                name: String::from_utf8_lossy(&g.name).into_owned(),
                host_ip: g.host_ip.to_string(),
                port: g.port,
                game_type: g.game_type,
                has_password: !g.password.is_empty(),
                elapsed_secs: now.saturating_duration_since(g.created).as_secs(),
            })
            .collect();
        games.sort_by(|a, b| a.name.cmp(&b.name));
        crate::status::Snapshot {
            server_name: self.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: now.saturating_duration_since(self.started).as_secs(),
            connections,
            peak_connections,
            online_users,
            channels,
            games,
        }
    }

    /// Join a channel, creating it if it does not exist.
    ///
    /// Returns the roster as seen *before* this user arrived (for `EID_SHOWUSER`), plus
    /// the join outcome.
    ///
    /// # Errors
    ///
    /// [`JoinDenial`] when the channel is full, the user is banned, or already present.
    pub fn join_channel(
        &self,
        raw_name: &[u8],
        account: AccountId,
        display_name: &str,
        base_flags: u32,
        statstring: Vec<u8>,
        out: Outbound,
    ) -> Result<(JoinedChannel, Vec<ChannelOccupant>), JoinDenial> {
        let key = normalize_channel_name(raw_name);
        if key.is_empty() {
            return Err(JoinDenial::Full);
        }
        let display = String::from_utf8_lossy(raw_name).trim().to_string();

        // A defined channel may gate entry on a required flag (e.g. a staff channel).
        let rule = self.channel_rules.get(&key).cloned();
        if let Some(rule) = &rule {
            if rule.min_flag != 0 && base_flags & rule.min_flag == 0 {
                return Err(JoinDenial::Restricted);
            }
        }
        let topic = rule.as_ref().and_then(|r| r.topic.clone());

        let mut inner = self.inner.lock().expect("node lock");
        let channel_caps = self.channel_caps;
        let auto_op_private = self.auto_op_private;
        let channel = inner.channels.entry(key.clone()).or_insert_with(|| {
            // A defined channel's explicit max wins; otherwise the per-category default,
            // chosen from the channel's name (private/public/clan). `0` = unlimited.
            let max_users = rule
                .as_ref()
                .and_then(|r| r.max_users)
                .unwrap_or_else(|| channel_caps.for_name(&display));
            let mut c = Channel::new(key.clone(), display.clone(), ChannelClass::Local, max_users);
            // A defined channel that is marked public never auto-ops; otherwise apply the
            // name convention ("Op <name>"/"Clan <name>" op only <name>, private per config).
            let op_grant = if rule.as_ref().is_some_and(|r| r.public) {
                bnetcc_core::channel::OpGrant::None
            } else {
                bnetcc_core::channel::op_grant_for_name(&display, auto_op_private)
            };
            c.set_op_grant(op_grant);
            if let Some(r) = &rule {
                c.set_persist(r.persist);
            }
            c
        });

        let existing: Vec<ChannelOccupant> = channel
            .members()
            .iter()
            .map(|m| ChannelOccupant {
                name: m.name.clone(),
                flags: m.flags,
                statstring: m.statstring.clone(),
            })
            .collect();

        let outcome = channel.join(account, display_name.to_string(), base_flags, statstring)?;
        let joined = JoinedChannel {
            key: key.clone(),
            display: channel.display().to_string(),
            flags: channel.flags(),
            outcome,
            topic,
        };
        inner
            .subscribers
            .entry(key)
            .or_default()
            .push((display_name.to_string(), out));
        Ok((joined, existing))
    }

    /// Leave a channel, destroying it if it is now empty and not persistent. Keyed on the
    /// session's unique display name, so it removes exactly that session even when others
    /// share its account.
    pub fn leave_channel(&self, key: &[u8], display_name: &str) -> Option<AccountId> {
        let mut inner = self.inner.lock().expect("node lock");
        let channel = inner.channels.get_mut(key)?;
        let outcome = channel.leave(display_name);
        if let Some(subs) = inner.subscribers.get_mut(key) {
            subs.retain(|(name, _)| !name.eq_ignore_ascii_case(display_name));
        }
        if outcome.should_destroy {
            inner.channels.remove(key);
            inner.subscribers.remove(key);
            inner.pending_leaves.remove(key);
        }
        outcome.new_operator
    }

    /// Whether `display_name` is currently a member of the channel keyed by `key`. Used to
    /// re-check membership before a talk fans out, so a kicked session cannot keep chatting.
    #[must_use]
    pub fn is_member(&self, key: &[u8], display_name: &str) -> bool {
        let inner = self.inner.lock().expect("node lock");
        inner
            .channels
            .get(key)
            .is_some_and(|c| c.members().iter().any(|m| m.name.eq_ignore_ascii_case(display_name)))
    }

    /// Kick the session shown as `target_name` from the channel (operator action). Removes
    /// them from the roster and the fan-out, returning their outbound handle so the caller
    /// can notify them, plus any operator succession.
    pub fn channel_kick(&self, key: &[u8], actor: AccountId, target_name: &str) -> ModResult {
        let mut inner = self.inner.lock().expect("node lock");
        let outcome = {
            let Some(channel) = inner.channels.get_mut(key) else {
                return ModResult::NotFound;
            };
            match channel.kick_by_name(actor, target_name) {
                Ok(o) => o,
                Err(OpError::NotOperator) => return ModResult::NotOperator,
                Err(OpError::NotPresent) => return ModResult::NotFound,
                Err(OpError::CannotTargetSelf) => return ModResult::CannotTargetSelf,
            }
        };
        let target_out = pull_subscriber(&mut inner, key, target_name);
        if outcome.should_destroy {
            inner.channels.remove(key);
            inner.subscribers.remove(key);
            inner.pending_leaves.remove(key);
        }
        ModResult::Ok {
            target_out,
            new_operator: outcome.new_operator,
        }
    }

    /// Ban the account behind the session shown as `target_name` and remove that session.
    pub fn channel_ban(&self, key: &[u8], actor: AccountId, target_name: &str) -> ModResult {
        let mut inner = self.inner.lock().expect("node lock");
        let outcome = {
            let Some(channel) = inner.channels.get_mut(key) else {
                return ModResult::NotFound;
            };
            match channel.ban_by_name(actor, target_name) {
                Ok((_account, o)) => o,
                Err(OpError::NotOperator) => return ModResult::NotOperator,
                Err(OpError::NotPresent) => return ModResult::NotFound,
                Err(OpError::CannotTargetSelf) => return ModResult::CannotTargetSelf,
            }
        };
        let target_out = pull_subscriber(&mut inner, key, target_name);
        if outcome.should_destroy {
            inner.channels.remove(key);
            inner.subscribers.remove(key);
            inner.pending_leaves.remove(key);
        }
        ModResult::Ok {
            target_out,
            new_operator: outcome.new_operator,
        }
    }

    /// Nominate the session shown as `target_name` as the operator's heir.
    pub fn channel_designate(&self, key: &[u8], actor: AccountId, target_name: &str) -> ModResult {
        let mut inner = self.inner.lock().expect("node lock");
        let Some(channel) = inner.channels.get_mut(key) else {
            return ModResult::NotFound;
        };
        match channel.designate_by_name(actor, target_name) {
            Ok(_heir) => ModResult::Ok {
                target_out: None,
                new_operator: None,
            },
            Err(OpError::NotOperator) => ModResult::NotOperator,
            Err(OpError::NotPresent) => ModResult::NotFound,
            Err(OpError::CannotTargetSelf) => ModResult::CannotTargetSelf,
        }
    }

    /// Lift a channel ban on `target_account` (resolved by the caller from a name).
    pub fn channel_unban(&self, key: &[u8], actor: AccountId, target_account: AccountId) -> ModResult {
        let mut inner = self.inner.lock().expect("node lock");
        let Some(channel) = inner.channels.get_mut(key) else {
            return ModResult::NotFound;
        };
        match channel.unban(actor, target_account) {
            Ok(_was_banned) => ModResult::Ok {
                target_out: None,
                new_operator: None,
            },
            Err(_) => ModResult::NotOperator,
        }
    }

    /// Advertise (or re-advertise) a game. Keyed by lowercased name, so a host updating
    /// its own game replaces the prior entry. Returns whether the name was free — a
    /// different host trying to reuse a live name is refused (`false`).
    pub fn advertise_game(&self, ad: GameAd) -> bool {
        let key = ad.name.to_ascii_lowercase();
        let mut inner = self.inner.lock().expect("node lock");
        if let Some(existing) = inner.games.get(&key) {
            if existing.host != ad.host {
                return false; // name taken by another host's live game
            }
        }
        inner.games.insert(key, ad);
        true
    }

    /// Remove a game advertised by `account`, if any. Called on `SID_STOPADV`,
    /// `SID_LEAVEGAME` and disconnect, so a game never outlives its host.
    pub fn withdraw_game(&self, account: AccountId) {
        let mut inner = self.inner.lock().expect("node lock");
        inner.games.retain(|_, g| g.host != account);
    }

    /// Snapshot of the currently advertised games, for `SID_GETADVLISTEX`.
    #[must_use]
    pub fn games(&self) -> Vec<GameAd> {
        self.inner.lock().expect("node lock").games.values().cloned().collect()
    }

    /// Modification time of a served file, for `SID_GETFILETIME`. Returns `None` if there
    /// is no files directory, the name fails the traversal guard, or the file is absent.
    /// A brief blocking `metadata` call, acceptable for an infrequent handshake packet.
    #[must_use]
    pub fn file_mtime(&self, filename: &[u8]) -> Option<std::time::SystemTime> {
        let dir = self.files_dir.as_ref()?;
        let name = bnetcc_proto::bnftp::sanitize_filename(filename)?;
        std::fs::metadata(dir.join(name)).ok()?.modified().ok()
    }

    /// Send pre-encoded bytes to every subscriber of a channel, optionally excluding one.
    ///
    /// Encoding happens once in the caller; this clones an `Arc` per recipient. A
    /// subscriber whose queue is full is dropped from the fanout and returned, so the
    /// caller can close it — it must never block the other 199 users.
    ///
    /// `exclude` skips one session by its display name (real Battle.net does not echo your
    /// own talk back to you). `sender_base`, when set, is the sender's *base* account name:
    /// a recipient who has personally squelched that account is skipped, so `/squelch` hides
    /// all of that account's sessions. System events (join/leave/kick) pass `None`.
    pub fn broadcast(
        &self,
        key: &[u8],
        wire: &Wire,
        exclude: Option<&str>,
        sender_base: Option<&str>,
    ) -> Vec<String> {
        // Lowercase the sender once (not per recipient); the empty-ignore fast path in
        // `IgnoreSet::contains` means well-behaved channels never touch a lock here.
        let sender_lc = sender_base.map(str::to_ascii_lowercase);
        let mut inner = self.inner.lock().expect("node lock");
        // Ordering: any queued leaves for this channel must reach subscribers before this
        // (later) event, so a departure can never arrive after a same-name rejoin. Flushing
        // here also means chat activity propagates pending leaves promptly.
        flush_leaves_locked(&mut inner, key);
        let Some(subs) = inner.subscribers.get(key) else {
            return Vec::new();
        };
        let mut stalled = Vec::new();
        for (name, out) in subs {
            if exclude.is_some_and(|e| name.eq_ignore_ascii_case(e)) {
                continue;
            }
            if let Some(sl) = &sender_lc {
                if out.ignores().contains(sl) {
                    continue;
                }
            }
            if !out.send(wire) {
                stalled.push(name.clone());
            }
        }
        stalled
    }

    /// Queue a pre-encoded `EID_LEAVE` frame to be delivered to the channel's subscribers in
    /// the next coalesced flush, rather than broadcast immediately. The leaver has already
    /// been removed from the subscriber list, so the batch never reaches them and no
    /// exclusion is needed. A no-op if the channel has no remaining subscribers.
    pub fn enqueue_leave(&self, key: &[u8], frame: Vec<u8>) {
        let mut inner = self.inner.lock().expect("node lock");
        if inner.subscribers.contains_key(key) {
            inner.pending_leaves.entry(key.to_vec()).or_default().push(frame);
        }
    }

    /// Flush every channel's queued leave notifications. Called on a short timer so
    /// departures still propagate on an otherwise-idle channel; an active channel also
    /// flushes via [`Self::broadcast`].
    pub fn flush_pending(&self) {
        let mut inner = self.inner.lock().expect("node lock");
        if inner.pending_leaves.is_empty() {
            return;
        }
        let keys: Vec<Vec<u8>> = inner.pending_leaves.keys().cloned().collect();
        for key in keys {
            flush_leaves_locked(&mut inner, &key);
        }
    }

    /// Register a logged-in session so staff moderation can reach it later. Pair with
    /// [`Self::unregister_session`] on disconnect.
    pub fn register_session(&self, display_name: &str, ip: IpAddr, out: Outbound, kill: Arc<Notify>) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(display_name.to_ascii_lowercase(), SessionEntry { ip, out, kill });
    }

    /// Drop a session from the moderation registry.
    pub fn unregister_session(&self, display_name: &str) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(&display_name.to_ascii_lowercase());
    }

    /// The address a currently-online session connected from, by display name.
    #[must_use]
    pub fn session_ip(&self, display_name: &str) -> Option<IpAddr> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(&display_name.to_ascii_lowercase())
            .map(|e| e.ip)
    }

    /// Force off every session on `ip`, queueing `notice` to each first. Returns how many
    /// were signalled. The sessions close cleanly (normal cleanup runs); the queued notice
    /// flushes because the writer drains its queue before the socket shuts down.
    pub fn disconnect_ip(&self, ip: IpAddr, notice: &Wire) -> usize {
        let sessions = self.sessions.lock().expect("sessions lock");
        let mut n = 0;
        for entry in sessions.values() {
            if entry.ip == ip {
                let _ = entry.out.send(notice);
                entry.kill.notify_one();
                n += 1;
            }
        }
        n
    }

    /// Force off every session whose (lowercased) display name contains `substring_lower`,
    /// queueing `notice` to each. Returns how many were signalled.
    pub fn disconnect_name_matches(&self, substring_lower: &str, notice: &Wire) -> usize {
        let sessions = self.sessions.lock().expect("sessions lock");
        let mut n = 0;
        for (name, entry) in sessions.iter() {
            if name.contains(substring_lower) {
                let _ = entry.out.send(notice);
                entry.kill.notify_one();
                n += 1;
            }
        }
        n
    }

    /// Whether an account currently holds operator in a channel.
    #[must_use]
    pub fn is_operator(&self, key: &[u8], account: AccountId) -> bool {
        self.inner
            .lock()
            .expect("node lock")
            .channels
            .get(key)
            .is_some_and(|c| c.is_operator(account))
    }
}

/// The result of joining a channel.
#[derive(Debug, Clone)]
pub struct JoinedChannel {
    /// Normalised lookup key.
    pub key: Vec<u8>,
    /// Display name.
    pub display: String,
    /// Channel flags.
    pub flags: u32,
    /// Join outcome, including whether operator was granted.
    pub outcome: bnetcc_core::channel::JoinOutcome,
    /// Per-channel topic/greeting to show the joiner, if the channel defines one.
    pub topic: Option<String>,
}

/// A user already present in a channel when someone joins, for `EID_SHOWUSER`.
#[derive(Debug, Clone)]
pub struct ChannelOccupant {
    /// Display name.
    pub name: String,
    /// Chat flags.
    pub flags: u32,
    /// Statstring, echoed so the joining client can render this user.
    pub statstring: Vec<u8>,
}

/// An in-memory-backed node for tests. Exposed beyond this module so `session`'s own
/// tests can drive a real `Node` without a database.
#[cfg(test)]
pub(crate) fn test_node() -> Node {
    let storage = crate::storage::spawn(Box::new(bnetcc_storage::memory::MemoryStorage::new()));
    Node::new(
        NodeConfig {
            policy: Policy::for_mode(bnetcc_core::policy::ServerMode::Gaming),
            name: "Test".into(),
            motd: "motd".into(),
            gateway_allowlist: Vec::new(),
            version_policy: crate::config::VersionPolicy::default(),
            files_dir: None,
            admins: crate::config::AdminsConfig::default(),
            // Tests exercise the classic "first joiner of a normal channel is opped"
            // behaviour, which in production is the opt-in private-channel case.
            auto_op_private: true,
            channel_rules: crate::config::ChannelRules::default(),
            cd_key_uniqueness: true,
            // Tests that exercise the size cap construct channels with an explicit max via
            // bnetcc_core directly; the node default here is uncapped.
            channel_caps: ChannelCaps::default(),
            udp_socket: None,
            bans_path: None,
        },
        storage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnetcc_proto::chat::user_flags;

    fn node() -> Node {
        test_node()
    }

    fn outbound(cap: usize) -> (Outbound, mpsc::Receiver<Wire>) {
        let (tx, rx) = mpsc::channel(cap);
        (Outbound::new(tx), rx)
    }

    #[tokio::test]
    async fn accounts_are_case_insensitive_and_unique() {
        let n = node();
        let a = n.create_account("Zealot", [1u8; 20]).await.unwrap();
        assert_eq!(n.account("zealot").await.unwrap().id, a.id);
        assert_eq!(n.account("ZEALOT").await.unwrap().id, a.id);
        assert!(n.create_account("zEaLoT", [2u8; 20]).await.is_err());
    }

    #[test]
    fn first_joiner_of_a_channel_gets_operator() {
        let n = node();
        let (out, _rx) = outbound(4);
        // A private channel (test_node enables auto_op_private), so the first arrival is
        // opped. Op/Clan channels op only their named account — covered in bnetcc-core.
        let (joined, existing) = n
            .join_channel(b"Zealot's Hangout", 1, "Zealot", 0, Vec::new(), out)
            .unwrap();
        assert!(joined.outcome.granted_operator);
        assert_eq!(joined.outcome.flags & user_flags::OPERATOR, user_flags::OPERATOR);
        assert!(existing.is_empty());
        assert_eq!(joined.key, b"zealot's hangout");
    }

    #[test]
    fn channel_names_are_normalized_so_variants_share_one_channel() {
        let n = node();
        let (o1, _r1) = outbound(4);
        let (o2, _r2) = outbound(4);
        n.join_channel(b"Blizzard Tech", 1, "a", 0, Vec::new(), o1).unwrap();
        let (_, existing) = n.join_channel(b"  blizzard   TECH ", 2, "b", 0, Vec::new(), o2).unwrap();
        assert_eq!(existing.len(), 1, "second user must land in the same channel");
    }

    #[test]
    fn duplicate_display_names_get_a_numeric_suffix() {
        let n = node();
        assert_eq!(n.claim_name("Fish"), "Fish");
        assert_eq!(n.claim_name("Fish"), "Fish#2");
        // Collisions are case-insensitive; the caller's casing is preserved in the result.
        assert_eq!(n.claim_name("fish"), "fish#3");
        // Releasing a slot frees it for reuse (lowest free suffix wins).
        n.release_name("Fish#2");
        assert_eq!(n.claim_name("Fish"), "Fish#2");
    }

    #[test]
    fn two_sessions_of_one_account_coexist_with_distinct_names() {
        // The name registry hands duplicate logins distinct display names; channel
        // membership keys on that name, so both sessions of one account can be present.
        let n = node();
        let (o1, _r1) = outbound(4);
        let (o2, _r2) = outbound(4);
        n.join_channel(b"chat", 7, "Fish", 0, Vec::new(), o1).unwrap();
        let (_, existing) = n.join_channel(b"chat", 7, "Fish#2", 0, Vec::new(), o2).unwrap();
        assert_eq!(existing.len(), 1, "the second session sees the first already present");
        assert_eq!(
            n.channel_occupant_names(b"chat").len(),
            2,
            "both sessions of the account coexist in the channel"
        );
        // Leaving by one display name removes only that session.
        n.leave_channel(b"chat", "Fish");
        assert_eq!(n.channel_occupant_names(b"chat"), vec!["Fish#2".to_string()]);
    }

    #[test]
    fn a_broadcast_reaches_everyone_except_the_excluded_sender() {
        let n = node();
        let (o1, mut r1) = outbound(4);
        let (o2, mut r2) = outbound(4);
        n.join_channel(b"chat", 1, "a", 0, Vec::new(), o1).unwrap();
        n.join_channel(b"chat", 2, "b", 0, Vec::new(), o2).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        let stalled = n.broadcast(b"chat", &wire, Some("a"), None);
        assert!(stalled.is_empty());
        assert!(r1.try_recv().is_err(), "sender must not receive their own talk");
        assert!(r2.try_recv().is_ok());
    }

    #[test]
    fn a_squelcher_does_not_receive_the_squelched_accounts_talk() {
        let n = node();
        let (listener, mut lrx) = outbound(4);
        let (speaker, _srx) = outbound(4);
        // The listener squelches the speaker's base account name.
        listener.ignores().add("Bob");
        n.join_channel(b"chat", 1, "alice", 0, Vec::new(), listener).unwrap();
        n.join_channel(b"chat", 2, "Bob", 0, Vec::new(), speaker).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        // Bob (base "Bob") talks; Alice has squelched Bob, so she receives nothing.
        let stalled = n.broadcast(b"chat", &wire, Some("Bob"), Some("Bob"));
        assert!(stalled.is_empty());
        assert!(lrx.try_recv().is_err(), "squelched account's talk must be withheld");

        // A different speaker still reaches Alice.
        let stalled = n.broadcast(b"chat", &wire, Some("carol"), Some("carol"));
        assert!(stalled.is_empty());
        assert!(lrx.try_recv().is_ok());
    }

    #[test]
    fn moderation_registry_resolves_ip_and_disconnects_selectively() {
        let n = node();
        let (o1, mut r1) = outbound(4);
        let (o2, mut r2) = outbound(4);
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        n.register_session("Bob", ip1, o1, Arc::new(Notify::new()));
        n.register_session("BNU-Eve", ip2, o2, Arc::new(Notify::new()));

        // /ipban resolves an online user's address.
        assert_eq!(n.session_ip("bob"), Some(ip1));
        assert_eq!(n.session_ip("nobody"), None);

        let notice = Arc::new(vec![1u8, 2, 3]);
        // An IP ban disconnects only the sessions on that address, queueing them a notice.
        assert_eq!(n.disconnect_ip(ip1, &notice), 1);
        assert!(r1.try_recv().is_ok(), "the disconnected session gets the notice");
        assert!(r2.try_recv().is_err(), "the untouched session gets nothing");

        // A tag ban disconnects by name substring (case-insensitive).
        assert_eq!(n.disconnect_name_matches("bnu-", &notice), 1);
        assert!(r2.try_recv().is_ok());

        n.unregister_session("Bob");
        assert_eq!(n.session_ip("Bob"), None);
    }

    #[test]
    fn leaves_are_coalesced_into_one_batched_write() {
        let n = node();
        let (obs, mut r) = outbound(64);
        n.join_channel(b"chat", 1, "obs", 0, Vec::new(), obs).unwrap();
        // Two departures enqueue their leave frames; nothing is delivered yet.
        n.enqueue_leave(b"chat", vec![0xFF, 0x0F, 4, 0]);
        n.enqueue_leave(b"chat", vec![0xFF, 0x0F, 4, 0]);
        assert!(r.try_recv().is_err(), "leaves are deferred, not sent per-departure");
        // One flush delivers both, concatenated into a single frame (one queue slot).
        n.flush_pending();
        let batch = r.try_recv().expect("one batched frame");
        assert_eq!(batch.len(), 8, "two 4-byte leave frames in one buffer");
        assert!(r.try_recv().is_err(), "exactly one batched write, not one per leave");
    }

    #[test]
    fn a_broadcast_flushes_pending_leaves_before_its_own_event() {
        let n = node();
        let (obs, mut r) = outbound(64);
        n.join_channel(b"chat", 1, "obs", 0, Vec::new(), obs).unwrap();
        n.enqueue_leave(b"chat", vec![0xFF, 0x0F, 4, 0]);
        // An immediate event (a talk) must not jump ahead of an already-queued leave.
        let talk = Arc::new(vec![0xFFu8, 0x0F, 5, 0, 0]);
        n.broadcast(b"chat", &talk, None, None);
        assert_eq!(r.try_recv().expect("leave first").len(), 4);
        assert_eq!(r.try_recv().expect("then the talk").len(), 5);
    }

    #[test]
    fn a_stalled_subscriber_is_reported_and_does_not_block_others() {
        let n = node();
        let (slow, _slow_rx) = outbound(1); // fills immediately
        let (fast, mut fast_rx) = outbound(64);
        n.join_channel(b"chat", 1, "slow", 0, Vec::new(), slow).unwrap();
        n.join_channel(b"chat", 2, "fast", 0, Vec::new(), fast).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        let mut stalled = Vec::new();
        for _ in 0..8 {
            stalled = n.broadcast(b"chat", &wire, None, None);
        }
        assert_eq!(stalled, vec!["slow".to_string()], "the slow subscriber must be named");
        // The healthy subscriber still got every message.
        let mut got = 0;
        while fast_rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, 8);
    }

    #[test]
    fn an_empty_channel_is_destroyed_on_the_last_departure() {
        let n = node();
        let (out, _rx) = outbound(4);
        n.join_channel(b"temp", 1, "a", 0, Vec::new(), out).unwrap();
        assert_eq!(n.channel_names().len(), 1);
        n.leave_channel(b"temp", "a");
        assert!(n.channel_names().is_empty());
    }

    #[test]
    fn admission_counts_are_released() {
        let n = node();
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        n.admit(ClientClass::GamePending, ip).unwrap();
        assert_eq!(n.connection_count(), 1);
        n.release(ClientClass::GamePending, ip);
        assert_eq!(n.connection_count(), 0);
    }

    #[test]
    fn gateway_admission_honours_the_one_per_ip_default() {
        let n = node();
        let ip: IpAddr = "10.1.2.4".parse().unwrap();
        assert!(n.admit(ClientClass::Gateway, ip).is_ok());
        assert!(
            n.admit(ClientClass::Gateway, ip).is_err(),
            "the gateway is keyless, so the address is the only cost"
        );
        // A game client from the same address is a separate budget entirely.
        assert!(n.admit(ClientClass::GamePending, ip).is_ok());
    }

    #[test]
    fn a_promoted_connection_is_not_double_counted() {
        let n = node();
        let ip: IpAddr = "10.1.2.5".parse().unwrap();
        n.admit(ClientClass::GamePending, ip).unwrap();
        let target = ClientClass::Game(bnetcc_proto::product::SEXP);
        n.reclassify(ip, ClientClass::GamePending, target).unwrap();
        assert_eq!(n.connection_count(), 1);
        n.release(target, ip);
        assert_eq!(n.connection_count(), 0);
    }
}
