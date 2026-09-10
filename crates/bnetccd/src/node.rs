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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bnetcc_core::ads::AdRotation;
use bnetcc_core::channel::{AccountId, Channel, ChannelClass, JoinDenial};
use bnetcc_core::limits::{AdmissionTable, ClientClass, KeyId, KeyRegistry, KeyVerdict, Rejection};
use bnetcc_core::policy::Policy;
use bnetcc_proto::bncs::{encode_frame, Frame};
use bnetcc_proto::chat::normalize_channel_name;
use tokio::sync::mpsc;

/// Bytes ready for the wire, shared by every recipient of a broadcast.
pub type Wire = Arc<Vec<u8>>;

/// The write side of one connection.
#[derive(Debug, Clone)]
pub struct Outbound {
    tx: mpsc::Sender<Wire>,
}

impl Outbound {
    /// Wrap a sender.
    #[must_use]
    pub const fn new(tx: mpsc::Sender<Wire>) -> Self {
        Self { tx }
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
    /// Subscribers per channel, keyed by normalised channel name.
    subscribers: HashMap<Vec<u8>, Vec<(AccountId, Outbound)>>,
    /// Advertised games, keyed by lowercased game name. A node is a directory of the
    /// games its clients host; the games themselves are peer-to-peer (`SID_STARTADVEX3`
    /// to advertise, `SID_GETADVLISTEX` to discover). See [`GameAd`].
    games: HashMap<Vec<u8>, GameAd>,
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
    inner: Mutex<Inner>,
    /// One table for every client class. Counts are keyed by `(class, address)`, so a
    /// Brood War client and a chat-gateway bot from the same address are accounted
    /// separately — which is the entire point of per-type limits.
    admission: Mutex<AdmissionTable>,
    connections: AtomicU64,
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
            inner: Mutex::new(Inner::default()),
            admission: Mutex::new(admission),
            connections: AtomicU64::new(0),
            storage,
            // 500ms matches observed real-Battle.net behaviour — see KeyRegistry's docs.
            key_registry: Mutex::new(KeyRegistry::new(500)),
            key_holder_names: Mutex::new(HashMap::new()),
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
            self.connections.fetch_add(1, Ordering::Relaxed);
        }
        r
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
        // `connection_count` reads an atomic, not the node lock, so taking it inside the
        // lock below is safe.
        let connections = self.connection_count();
        let inner = self.inner.lock().expect("node lock");
        let mut channels: Vec<crate::status::ChannelInfo> = inner
            .channels
            .values()
            .map(|c| {
                let users: Vec<String> = c.members().iter().map(|m| m.name.clone()).collect();
                crate::status::ChannelInfo {
                    name: c.display().to_string(),
                    user_count: users.len(),
                    users,
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
                elapsed_secs: now.saturating_duration_since(g.created).as_secs(),
            })
            .collect();
        games.sort_by(|a, b| a.name.cmp(&b.name));
        crate::status::Snapshot {
            server_name: self.name.clone(),
            uptime_secs: now.saturating_duration_since(self.started).as_secs(),
            connections,
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
            .push((account, out));
        Ok((joined, existing))
    }

    /// Leave a channel, destroying it if it is now empty and not persistent.
    pub fn leave_channel(&self, key: &[u8], account: AccountId) -> Option<AccountId> {
        let mut inner = self.inner.lock().expect("node lock");
        let channel = inner.channels.get_mut(key)?;
        let outcome = channel.leave(account);
        if let Some(subs) = inner.subscribers.get_mut(key) {
            subs.retain(|(id, _)| *id != account);
        }
        if outcome.should_destroy {
            inner.channels.remove(key);
            inner.subscribers.remove(key);
        }
        outcome.new_operator
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
    pub fn broadcast(&self, key: &[u8], wire: &Wire, exclude: Option<AccountId>) -> Vec<AccountId> {
        let inner = self.inner.lock().expect("node lock");
        let Some(subs) = inner.subscribers.get(key) else {
            return Vec::new();
        };
        let mut stalled = Vec::new();
        for (id, out) in subs {
            if Some(*id) == exclude {
                continue;
            }
            if !out.send(wire) {
                stalled.push(*id);
            }
        }
        stalled
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
    fn a_broadcast_reaches_everyone_except_the_excluded_sender() {
        let n = node();
        let (o1, mut r1) = outbound(4);
        let (o2, mut r2) = outbound(4);
        n.join_channel(b"chat", 1, "a", 0, Vec::new(), o1).unwrap();
        n.join_channel(b"chat", 2, "b", 0, Vec::new(), o2).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        let stalled = n.broadcast(b"chat", &wire, Some(1));
        assert!(stalled.is_empty());
        assert!(r1.try_recv().is_err(), "sender must not receive their own talk");
        assert!(r2.try_recv().is_ok());
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
            stalled = n.broadcast(b"chat", &wire, None);
        }
        assert_eq!(stalled, vec![1], "the slow subscriber must be named");
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
        n.leave_channel(b"temp", 1);
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
