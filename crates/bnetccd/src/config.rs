//! Node configuration.
//!
//! One typed document with `serde` defaults, rather than PvPGN's ~200 flat keys spread
//! across `bnetd.conf` plus fifteen side files. Every field has a default, so a minimal
//! config is a handful of lines and an operator only writes what they want to change.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

#[cfg(test)]
use bnetcc_core::limits::ClientClass;
use bnetcc_core::policy::{ClientLimits, ConnLimitsPatch, Policy, ServerMode};
use bnetcc_proto::chat::{normalize_channel_name, user_flags};
use bnetcc_proto::FourCc;
use serde::Deserialize;

/// Top-level node configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Server identity and mode.
    pub server: ServerConfig,
    /// Listener addresses.
    pub listen: ListenConfig,
    /// Resource ceilings.
    pub limits: LimitsConfig,
    /// Federation link to the hub.
    pub federation: FederationConfig,
    /// Account persistence.
    pub storage: StorageConfig,
    /// Client version restriction.
    pub versions: VersionsConfig,
    /// BNFTP file serving.
    pub files: FilesConfig,
    /// Server administrators.
    pub admins: AdminsConfig,
    /// Channel behaviour.
    pub channels: ChannelsConfig,
    /// Optional read-only status UI.
    pub status: StatusConfig,
    /// Optional Discord webhook updates.
    pub discord: DiscordConfig,
    /// Optional stats push to an external website.
    pub stats_push: StatsPushConfig,
    /// Optional PvPGN-compatible server tracking (advertise us / host a list).
    pub tracker: TrackerConfig,
    /// The Diablo II closed realm (private characters).
    pub diablo2: Diablo2Config,
}

/// The Diablo II closed realm: the "Battle.net" button's character select, with characters
/// stored on this server. It rides the BNCS port — the realm connection is told apart from a
/// login by the byte after the protocol selector — so it needs no extra forwarded port.
/// Game creation needs a Diablo II game server, which is not part of this yet: the realm
/// answers "server down" to create/join. See `docs/DIABLO2.md`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Diablo2Config {
    /// Offer the realm. Never offered in warnet mode.
    pub realm: bool,
    /// The realm's description in `SID_QUERYREALMS2`. Its *name* is `server.realm`.
    pub description: String,
    /// Host or IPv4 address a client dials for the realm. Empty (the default) hands each
    /// client the address it reached this server on, which is right on a LAN; set your
    /// public address or hostname when players connect from the internet through NAT.
    pub address: String,
    /// Characters per account, `1..=18`.
    pub max_characters: u32,
    /// The operator's Diablo II 1.14d install folder. `Game.exe` is read from it at startup
    /// for the engine's wire tables; nothing from it is copied into this server's files.
    pub data_dir: String,
    /// **Experimental test.** Let clients create and join games against the game-server
    /// handshake test on port 4000 (`crate::d2gs`): a joining client gets Blizzard's join
    /// sequence and stands in the Rogue Encampment with no stats, NPCs or actions. Needs `data_dir`.
    pub game_server_probe: bool,
    /// File name, in `[files] dir`, of the map from a realm character's statstring equipment
    /// bytes to the items they show (`crate::d2_equipment`), built from `data_dir` at startup for
    /// chat bots to fetch over BNFTP. Needs both directories; empty disables it.
    pub equipment_file: String,
    /// File name, in `[files] dir`, of the character pack (`crate::d2_characters`): the
    /// character-select animations as layers with their manifest, built from `data_dir` at
    /// startup for chat bots to fetch over BNFTP. Needs both directories; empty disables it.
    pub character_pack: String,
}

impl Default for Diablo2Config {
    fn default() -> Self {
        Self {
            realm: true,
            description: "Diablo II closed realm".into(),
            address: String::new(),
            max_characters: 18,
            data_dir: String::new(),
            game_server_probe: false,
            equipment_file: "d2-equipment.json".into(),
            character_pack: "d2-characters.zip".into(),
        }
    }
}

/// PvPGN-compatible server tracking (see `crate::tracker`). Both halves optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrackerConfig {
    /// Trackers to beacon this server to, `host` or `host:port` (default port 6114). Empty =
    /// don't advertise. e.g. `["tracker.pvpgn.org", "track.muleslow.net"]`.
    pub advertise_to: Vec<String>,
    /// Description/URL/contact sent in our beacon (description defaults to the server name).
    pub description: String,
    pub url: String,
    pub contact_name: String,
    pub contact_email: String,
    /// Seconds between beacons (minimum 30; default 300).
    pub advertise_interval_secs: u64,
    /// UDP address to receive other servers' beacons on, e.g. `"0.0.0.0:6114"`. Empty = off.
    pub host_listen: String,
    /// HTTP address to serve the public server-list page/JSON on, e.g. `"0.0.0.0:8080"`.
    /// Empty = off. (Port 80 needs root; bind a high port and forward at the router.)
    pub list_listen: String,
    /// Drop a listed server after this many seconds without a beacon (default 600).
    pub prune_after_secs: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            advertise_to: Vec::new(),
            description: String::new(),
            url: String::new(),
            contact_name: String::new(),
            contact_email: String::new(),
            advertise_interval_secs: 300,
            host_listen: String::new(),
            list_listen: String::new(),
            prune_after_secs: 600,
        }
    }
}

/// Push the public status JSON to an external URL on an interval (see `crate::stats_push`).
/// Disabled unless `url` is set. Outbound-only, so it needs no forwarded port.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatsPushConfig {
    /// The `https://` endpoint on your site that receives the stats POST. Empty disables it.
    pub url: String,
    /// Seconds between pushes (minimum 5).
    pub interval_secs: u64,
    /// Optional bearer token; sent as `Authorization: Bearer <token>` if set. A secret.
    pub token: String,
    /// Whether the pushed JSON includes the online-usernames list.
    pub include_users: bool,
}

impl Default for StatsPushConfig {
    fn default() -> Self {
        Self { url: String::new(), interval_secs: 60, token: String::new(), include_users: false }
    }
}

/// Discord webhook updates (see `crate::discord`). Disabled unless `webhook_url` is set.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiscordConfig {
    /// Discord channel webhook URL. Empty (the default) disables all Discord posting. The
    /// operator creates the webhook in Discord and pastes the URL here; it is a secret.
    pub webhook_url: String,
    /// Minutes between periodic status posts.
    pub status_interval_mins: u64,
    /// Window, in hours, for the "games played per client" figure in the status post.
    pub games_window_hours: u64,
    /// Post the periodic status summary.
    pub post_status: bool,
    /// Post server start/stop/restart events.
    pub post_events: bool,
    /// Post milestones (e.g. a new peak-connections record).
    pub post_milestones: bool,
    /// Post a line to a **separate** webhook whenever any client advertises a game (host,
    /// game name, product). Empty (the default) disables game announcements even if the main
    /// `webhook_url` is set, so games can go to their own channel. A secret, like `webhook_url`.
    pub games_webhook_url: String,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        // When a `[discord]` section is present the operator opts in per field, but the
        // sensible baseline (used for any field they omit) is "post everything, every 30
        // minutes, over a 6-hour games window". `webhook_url` empty is what keeps it off.
        Self {
            webhook_url: String::new(),
            status_interval_mins: 30,
            games_window_hours: 6,
            post_status: true,
            post_events: true,
            post_milestones: true,
            games_webhook_url: String::new(),
        }
    }
}

/// The admin panel (`crate::status`) and the public status endpoint (`crate::public_status`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusConfig {
    /// Address for the password-gated HTTPS admin panel, e.g. `"127.0.0.1:6114"`. Empty (the
    /// default) disables it. Loopback-only unless remote access is enabled from its Settings
    /// page — it can change the server, so keep it off the open internet.
    pub listen: String,
    /// Address for the read-only public status page + JSON feed, e.g. `"0.0.0.0:6116"`. Empty
    /// (the default) disables it. Plain HTTP, unauthenticated, safe to expose publicly — it
    /// shows only aggregate stats (and usernames only if `public_show_users` is on).
    pub public_listen: String,
    /// Whether the public status feed lists the online usernames. Off by default: counts,
    /// uptime and channel/game totals are always shown, but who is online is opt-in.
    pub public_show_users: bool,
}

/// Channel behaviour.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChannelsConfig {
    /// Whether the first arrival to a private (user-created) channel is granted operator.
    /// Op/Clan channels op only their named account and `Public *` channels never auto-op,
    /// regardless of this; it only governs the private case. Off by default.
    pub auto_op_private: bool,
    /// Global default for telnet/chat-gateway channel access: `"public"` (gateway users
    /// may enter public channels only — the default), `"none"` (no channels), or `"all"`.
    /// A per-channel `telnet` setting in `[[channels.defined]]` overrides this.
    pub telnet_access: String,
    /// Maximum users in a **private** (user-created) channel. **`0` means unlimited.**
    pub private_max: usize,
    /// Maximum users in a **`Public *`** channel. **`0` means unlimited.**
    pub public_max: usize,
    /// Maximum users in an **`Op`/`Clan`** channel. **`0` means unlimited.**
    pub clan_max: usize,
    /// Pre-defined channels with fixed properties, applied when the channel is created. A
    /// `max_users` set on a defined channel overrides the per-category default above (and
    /// `0` there is unlimited too).
    pub defined: Vec<ChannelDef>,
    /// Per-product default channel: product FourCC (e.g. `STAR`, `W2BN`) → channel name a
    /// client of that product lands in **only when it joins without naming a channel**. A
    /// client that requests a specific channel is honoured as-is.
    pub default_channel: BTreeMap<String, String>,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            auto_op_private: false,
            telnet_access: "public".to_string(),
            // Uncapped by default — operators set a cap deliberately if they want one.
            private_max: 0,
            public_max: 0,
            clan_max: 0,
            defined: Vec::new(),
            default_channel: BTreeMap::new(),
        }
    }
}

/// A pre-defined channel and its fixed properties. Unset optional fields fall back to the
/// name-convention default for that channel.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChannelDef {
    /// Display name (matched case-insensitively, normalised like any channel name).
    pub name: String,
    /// Whether it appears in `SID_GETCHANNELLIST`. Defaults to true.
    pub listed: Option<bool>,
    /// Public semantics: a public channel never auto-ops. Defaults from the name
    /// convention (`Public *` → true).
    pub public: Option<bool>,
    /// Whether the channel survives becoming empty (does not vanish). Default false.
    pub persist: Option<bool>,
    /// Maximum users; falls back to the server default.
    pub max_users: Option<usize>,
    /// Message shown to a user on entry (a per-channel topic/greeting).
    pub topic: Option<String>,
    /// Telnet/chat-gateway access override: `"allow"`, `"deny"`, or unset (inherit the
    /// global `telnet_access`). This is how "Open Tech Support" is opened to telnet while
    /// other non-public channels stay closed.
    pub telnet: Option<String>,
    /// Only game clients may enter (not the telnet/chat gateway). Default false.
    pub game_only: Option<bool>,
    /// Restrict entry to users carrying a chat flag: `"admin"`, `"speaker"`, or unset
    /// (anyone). Ties into flag-limited channels.
    pub min_flag: Option<String>,
    /// Restrict entry to specific game products by FourCC (e.g. `["W2BN"]` so only Warcraft
    /// II BNE clients may join). Unset or empty = open to every product. Matched
    /// case-insensitively.
    pub products: Option<Vec<String>>,
}

/// Global default for telnet/chat-gateway channel access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TelnetAccess {
    /// Gateway users may not enter any channel by default.
    None,
    /// Gateway users may enter public channels only (the default).
    #[default]
    Public,
    /// Gateway users may enter any channel by default.
    All,
}

/// A pre-defined channel with every property resolved to a concrete value.
#[derive(Debug, Clone)]
pub struct ResolvedChannel {
    /// Display name as configured.
    pub display: String,
    /// Appears in the channel list.
    pub listed: bool,
    /// Public semantics (never auto-ops).
    pub public: bool,
    /// Survives becoming empty.
    pub persist: bool,
    /// Max users, or `None` for the server default.
    pub max_users: Option<usize>,
    /// Per-entry topic/greeting.
    pub topic: Option<String>,
    /// Telnet override: `Some(true)` allow, `Some(false)` deny, `None` inherit global.
    /// Read by [`ChannelRules::telnet_may_join`]; not yet consulted on a live path because
    /// the chat gateway cannot join channels yet (docs/ROADMAP.md).
    #[allow(dead_code)]
    pub telnet: Option<bool>,
    /// Only game clients may enter. Enforced once the gateway's channel-join command
    /// exists — until then no telnet client can reach any channel, so nothing to reject.
    #[allow(dead_code)]
    pub game_only: bool,
    /// Required chat-flag bitmask for entry; 0 = open to anyone.
    pub min_flag: u32,
    /// Allowed product FourCCs (upper-case), empty = open to every product. A joining client
    /// whose product is not listed is refused.
    pub products: Vec<String>,
}

/// Resolved, validated channel rules: the global telnet default plus the defined channels
/// keyed by normalised name.
#[derive(Debug, Clone, Default)]
pub struct ChannelRules {
    /// Global telnet access default when a channel has no explicit `telnet` setting. Read
    /// by [`ChannelRules::telnet_may_join`]; see the note there about the enforcement gap.
    #[allow(dead_code)]
    pub telnet_access: TelnetAccess,
    by_key: HashMap<Vec<u8>, ResolvedChannel>,
    /// Product FourCC (upper-case) → default channel name, for clients that join without
    /// naming one.
    default_channel: HashMap<String, String>,
}

impl ChannelRules {
    /// The rule for a channel by its normalised key, if one is defined.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&ResolvedChannel> {
        self.by_key.get(key)
    }

    /// The default channel for a product (its FourCC as a string), if one is configured.
    #[must_use]
    pub fn default_channel(&self, product: &str) -> Option<&str> {
        self.default_channel.get(&product.to_ascii_uppercase()).map(String::as_str)
    }

    /// Every defined channel that should appear in the channel list.
    pub fn listed(&self) -> impl Iterator<Item = &ResolvedChannel> {
        self.by_key.values().filter(|c| c.listed)
    }

    /// Whether a telnet/chat-gateway user may enter this channel, given its display name.
    /// Uses the per-channel override if defined, else the global default (public-only).
    ///
    /// Not yet called on a live path: the chat gateway is a stub that cannot join channels.
    /// It becomes the enforcement point the moment the gateway parses `/join`
    /// (docs/ROADMAP.md); tested in `channel_rules` unit tests meanwhile.
    #[allow(dead_code)]
    #[must_use]
    pub fn telnet_may_join(&self, display: &str) -> bool {
        let key = normalize_channel_name(display.as_bytes());
        if let Some(rule) = self.by_key.get(&key) {
            if let Some(explicit) = rule.telnet {
                return explicit;
            }
            // No per-channel override: fall through to the global default using the
            // resolved public flag.
            return match self.telnet_access {
                TelnetAccess::None => false,
                TelnetAccess::Public => rule.public,
                TelnetAccess::All => true,
            };
        }
        match self.telnet_access {
            TelnetAccess::None => false,
            TelnetAccess::Public => {
                bnetcc_core::channel::classify_name(display) == bnetcc_core::channel::NameKind::Public
            }
            TelnetAccess::All => true,
        }
    }
}

/// Server administrators — accounts granted the Battle.net Administrator (sysop) role.
///
/// A listed account carries the sysop flags in chat: the Battle.net Administrator flag
/// and the Blizzard-representative tag (the icon that marks staff). Matching is
/// case-insensitive. Kept in config, never hardcoded, so who is staff is an operator
/// decision.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdminsConfig {
    /// Account names to treat as sysops.
    pub accounts: Vec<String>,
}

impl AdminsConfig {
    /// Whether `name` is a configured administrator (case-insensitive).
    #[must_use]
    pub fn is_admin(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.accounts.iter().any(|a| a.to_ascii_lowercase() == lower)
    }
}

/// BNFTP file serving.
///
/// Classic clients fetch the version-check MPQ, `icons.bni`, `tos.txt` and ad images over
/// BNFTP. Real (non-BNLS) clients — Diablo I, War2 BNE, old Mac clients — *require* the
/// version-check MPQ here, or they hang at "Checking versions". Only operator-supplied
/// files from this directory are served; Command Center ships no Blizzard assets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesConfig {
    /// Directory of files to serve over BNFTP. Empty disables serving (every request is
    /// refused), which is why a real classic client cannot pass version checking until an
    /// operator points this at a directory holding the version-check MPQ.
    pub dir: String,
}

/// Account persistence.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageConfig {
    /// Path to the SQLite database file. Empty means in-memory: accounts do not survive
    /// a restart. Fine for a quick local test, never for a real node — see
    /// `bnetccd::node::Account`'s doc comment.
    pub path: String,
}

/// Client version restriction.
///
/// Off by default: a fresh node accepts any version byte, so nobody is locked out while an
/// operator is still learning what their community's clients report (the version byte and
/// platform of every connection are logged at `SID_AUTH_INFO` regardless). Turning
/// `restrict` on makes `allowed` an allowlist: a `(product, platform)` combination is
/// accepted only if it has an entry and the client's version byte is in it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VersionsConfig {
    /// When false (default) any version byte is accepted.
    pub restrict: bool,
    /// Product FourCC → platform FourCC → allowed version bytes. Consulted only when
    /// `restrict` is true. Products are codes like `SEXP`, `STAR`, `WAR3`; platforms are
    /// `IX86`, `PMAC`, `XMAC`. Version bytes are small integers (Brood War is `211`).
    pub allowed: BTreeMap<String, BTreeMap<String, Vec<u32>>>,
}

/// A resolved, validated form of [`VersionsConfig`] for fast per-connection lookup.
#[derive(Debug, Clone, Default)]
pub struct VersionPolicy {
    restrict: bool,
    allowed: BTreeMap<(FourCc, FourCc), Vec<u32>>,
}

impl VersionPolicy {
    /// Whether a client of `product` on `platform` reporting `version_byte` is allowed.
    ///
    /// Always true when restriction is off. When on, the `(product, platform)` pair must
    /// have an entry and the version byte must be listed in it.
    #[must_use]
    pub fn allows(&self, product: FourCc, platform: FourCc, version_byte: u32) -> bool {
        if !self.restrict {
            return true;
        }
        self.allowed
            .get(&(product, platform))
            .is_some_and(|bytes| bytes.contains(&version_byte))
    }
}

fn parse_fourcc(kind: &str, code: &str) -> Result<FourCc, String> {
    let bytes = code.as_bytes();
    if bytes.len() != 4 || !code.is_ascii() {
        return Err(format!(
            "versions.allowed: {kind} code {code:?} must be exactly four ASCII characters, \
             such as SEXP, IX86 or PMAC"
        ));
    }
    Ok(FourCc::from_ascii(&[bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Server identity and behaviour.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Name shown to clients.
    pub name: String,
    /// `gaming`, `warnet` or `both`.
    ///
    /// The hub can only narrow this, never widen it, so a node cannot re-enable game
    /// hosting on a warnet.
    pub mode: String,
    /// Message of the day, shown on first channel join.
    pub motd: String,
    /// Realm name for realm-scoped accounts. WarCraft III (SRP) accounts are a separate
    /// namespace from the X-SHA-1 products' accounts and are shown as `Name@<realm>`, the
    /// way real Battle.net shows WC3 users across gateways. See `docs/WARCRAFT3.md` §3.6.
    pub realm: String,
    /// WarCraft III logon method: `"nls"` (SRP, the modern default) or `"legacy"`.
    ///
    /// On the NLS path the `SID_AUTH_INFO` reply carries a 128-byte RSA signature that WC3
    /// verifies *after* the login proof; we cannot produce a valid one, so an unpatched
    /// client drops the instant it processes `SID_AUTH_ACCOUNTLOGONPROOF` (docs/LEGAL.md §3,
    /// docs/WARCRAFT3.md §3.7). `"legacy"` advertises logon type 0 and omits the signature,
    /// so a client driven through a StarCraft-hash loader logs in over `SID_LOGONRESPONSE2`
    /// against the **X-SHA-1** account namespace (plain `Name`, not `Name@<realm>`) — the
    /// same flow StarCraft uses. Does not change the NLS handlers, which stay available.
    pub wc3_logon: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "Command Center".into(),
            mode: "gaming".into(),
            motd: "Welcome to Command Center.".into(),
            realm: "bncc".into(),
            wc3_logon: "nls".into(),
        }
    }
}

impl ServerConfig {
    /// Parse the configured mode.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending value.
    pub fn parsed_mode(&self) -> Result<ServerMode, String> {
        ServerMode::parse(&self.mode)
            .map_err(|bad| format!("unknown server.mode {bad:?}: expected gaming, warnet or both"))
    }

    /// Whether WarCraft III should use the legacy X-SHA-1 logon instead of NLS/SRP.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending value.
    pub fn wc3_legacy_logon(&self) -> Result<bool, String> {
        match self.wc3_logon.trim().to_ascii_lowercase().as_str() {
            "nls" | "srp" => Ok(false),
            "legacy" | "xsha1" | "old" => Ok(true),
            other => {
                Err(format!("unknown server.wc3_logon {other:?}: expected \"nls\" or \"legacy\""))
            }
        }
    }
}

/// Listener addresses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenConfig {
    /// BNCS and chat gateway (both are demultiplexed by the first byte).
    pub bncs: SocketAddr,
    /// Admin, health and Prometheus metrics.
    pub admin: SocketAddr,
    /// Number of `SO_REUSEPORT` accept-loop shards for the BNCS listener. `0` or `1` is a
    /// single listener; higher values spread accept + connection-setup across cores and give
    /// each shard its own accept backlog, for very high connect rates. Capped at 64.
    pub accept_shards: usize,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            bncs: "0.0.0.0:6112".parse().expect("valid default"),
            admin: "127.0.0.1:6115".parse().expect("valid default"),
            accept_shards: 1,
        }
    }
}

/// Resource ceilings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Hard ceiling on concurrent connections.
    ///
    /// `0` means "derive from `RLIMIT_NOFILE`", which is the right default: PvPGN ships
    /// a hard-coded 1000 and silently refuses connections past it, and operators who
    /// never found that knob concluded the software could not scale.
    pub max_connections: u32,
    /// Largest accepted BNCS frame.
    pub max_frame_bytes: usize,
    /// Largest accepted chat-gateway line.
    pub max_line_bytes: usize,
    /// Per-connection outbound queue depth, in frames.
    ///
    /// A full queue means the peer is not reading. The connection is dropped rather than
    /// buffered without bound — PvPGN had to add exactly this cap in 2014 after crashes
    /// from unbounded queue growth.
    pub outbound_queue: usize,
    /// Seconds from accept to authenticated before the connection is closed.
    pub handshake_timeout_secs: u64,
    /// Idle seconds after authentication before the connection is closed.
    pub idle_timeout_secs: u64,
    /// Addresses exempt from per-IP ceilings.
    ///
    /// Global and per-account ceilings still apply, and so does the CD-key registry —
    /// an operator can waive the address cost for their own host, not the key cost.
    pub gateway_allowlist: Vec<IpAddr>,

    /// Enforce one live session per CD key (`SID_AUTH_CHECK` result `0x201` "key in use").
    /// This is the economic gate on bot fleets (see `docs/WARNET.md`), so it defaults on.
    /// Turn it off to let several clients share a key — useful when testing with a fleet
    /// of your own bots on one key.
    pub cd_key_uniqueness: bool,

    /// Connection limits, per client type.
    pub clients: ClientLimitsConfig,
}

/// A partial override of one client type's connection limits.
///
/// Only the fields you set are changed; the rest keep the built-in default. So
/// `per_ip = 2` for WarCraft III is a one-line change that does not silently reset that
/// product's global ceiling.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsPatchConfig {
    /// Concurrent connections of this type from one address.
    pub per_ip: Option<u32>,
    /// Concurrent connections of this type for one account.
    pub per_account: Option<u32>,
    /// Concurrent connections of this type server-wide.
    pub global: Option<u32>,
}

impl LimitsPatchConfig {
    const fn to_patch(self) -> ConnLimitsPatch {
        ConnLimitsPatch {
            per_ip: self.per_ip,
            per_account: self.per_account,
            global: self.global,
        }
    }
}

/// Per-client-type connection limits.
///
/// Each client type is limited independently, because they cost their operator different
/// things. The telnet/chat gateway has no CD-key step, so the address is the only cost
/// available to charge and it stays at one per address. Game clients already pay in CD
/// keys, so their per-IP number is loose enough not to break households and LAN cafés,
/// and each product can be tuned separately — eight Brood War from a café but only two
/// WarCraft III, say.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClientLimitsConfig {
    /// Telnet / chat gateway.
    pub gateway: LimitsPatchConfig,
    /// BNFTP file transfer.
    pub bnftp: LimitsPatchConfig,
    /// Game connections whose product is not yet known (accept to `SID_AUTH_INFO`).
    pub game_pending: LimitsPatchConfig,
    /// Fallback for any game product without its own entry.
    pub game_default: LimitsPatchConfig,
    /// Per-product overrides, keyed by four-character product code (`SEXP`, `WAR3`, …).
    pub products: BTreeMap<String, LimitsPatchConfig>,
}

impl ClientLimitsConfig {
    /// Apply these overrides to a base set of limits.
    ///
    /// # Errors
    ///
    /// Returns a message naming any product key that is not exactly four ASCII
    /// characters — a typo here would otherwise create a product nothing ever matches.
    pub fn apply(&self, base: &mut ClientLimits) -> Result<(), String> {
        base.gateway = self.gateway.to_patch().apply(base.gateway);
        base.bnftp = self.bnftp.to_patch().apply(base.bnftp);
        base.game_pending = self.game_pending.to_patch().apply(base.game_pending);
        base.game_default = self.game_default.to_patch().apply(base.game_default);
        for (code, patch) in &self.products {
            let bytes = code.as_bytes();
            if bytes.len() != 4 || !code.is_ascii() {
                return Err(format!(
                    "limits.clients.products.{code:?}: product codes are exactly four \
                     ASCII characters, such as SEXP, D2XP or WAR3"
                ));
            }
            let fourcc = FourCc::from_ascii(&[bytes[0], bytes[1], bytes[2], bytes[3]]);
            base.patch_product(fourcc, patch.to_patch());
        }
        Ok(())
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_connections: 0,
            max_frame_bytes: bnetcc_proto::bncs::DEFAULT_MAX_FRAME,
            max_line_bytes: bnetcc_proto::line::DEFAULT_MAX_LINE,
            outbound_queue: 64,
            handshake_timeout_secs: 30,
            idle_timeout_secs: 1200,
            gateway_allowlist: Vec::new(),
            cd_key_uniqueness: true,
            clients: ClientLimitsConfig::default(),
        }
    }
}

/// Federation link.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FederationConfig {
    /// Whether to connect to a hub at all. A standalone node is a valid deployment.
    pub enabled: bool,
    /// Hub address.
    pub hub: String,
    /// Path to this node's Ed25519 identity key.
    pub identity_key: String,
    /// Allow logins from cached credentials while the hub is unreachable.
    ///
    /// **This is a deliberate trade.** For SRP products (WarCraft III) it costs nothing:
    /// the node holds only a verifier, which cannot be used to impersonate the user.
    /// For XSHA-1 products it requires the hub to push a password-equivalent hash to
    /// this node, so enabling it means this node's operator holds credentials usable on
    /// every other node in the federation. Sessions established this way are flagged
    /// unverified and their game results are quarantined until the hub reconciles.
    pub offline_login: bool,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hub: String::new(),
            identity_key: "node.key".into(),
            offline_login: false,
        }
    }
}

impl Config {
    /// Load from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message on read or parse failure.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_toml(&text)
    }

    /// Parse from TOML text.
    ///
    /// # Errors
    ///
    /// Returns a human-readable parse or validation message.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let cfg: Self = toml::from_str(text).map_err(|e| format!("config parse error: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Check invariants that `serde` cannot express.
    ///
    /// # Errors
    ///
    /// Returns a message describing the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        self.server.parsed_mode()?;
        self.server.wc3_legacy_logon()?;
        self.policy()?;
        if self.limits.outbound_queue == 0 {
            return Err("limits.outbound_queue must be at least 1".into());
        }
        if self.limits.max_frame_bytes < 16 {
            return Err("limits.max_frame_bytes is implausibly small".into());
        }
        if self.federation.enabled && self.federation.hub.is_empty() {
            return Err("federation.enabled is true but federation.hub is empty".into());
        }
        if !(1..=18).contains(&self.diablo2.max_characters) {
            return Err("diablo2.max_characters must be between 1 and 18".into());
        }
        if self.diablo2.game_server_probe && self.diablo2.data_dir.trim().is_empty() {
            return Err("diablo2.game_server_probe needs diablo2.data_dir (the folder holding Game.exe)".into());
        }
        Ok(())
    }

    /// Build the effective policy: the mode's defaults with config overrides applied.
    ///
    /// # Errors
    ///
    /// A bad mode or a malformed product code.
    pub fn policy(&self) -> Result<Policy, String> {
        let mut policy = Policy::for_mode(self.server.parsed_mode()?);
        self.limits.clients.apply(&mut policy.clients)?;
        // The pending class only bounds a connect flood before SID_AUTH_INFO identifies
        // the client; if an operator tightened it below a product's limit it would
        // reject connections that product would have allowed, which is confusing and
        // always a mistake. Widen it rather than failing to start.
        let widest = policy
            .clients
            .products
            .values()
            .map(|l| l.per_ip)
            .chain(std::iter::once(policy.clients.game_default.per_ip))
            .max()
            .unwrap_or(0);
        if policy.clients.game_pending.per_ip < widest {
            policy.clients.game_pending.per_ip = widest;
        }
        Ok(policy)
    }

    /// Resolve the effective connection ceiling.
    ///
    /// When `max_connections` is 0 the ceiling is derived from the process file
    /// descriptor limit, reserving headroom for listeners, logs, storage handles and the
    /// federation link. The result is logged at startup so an operator never has to
    /// guess why connections are being refused.
    #[must_use]
    pub fn effective_max_connections(&self, fd_limit: u64) -> u32 {
        const RESERVED_FDS: u64 = 64;
        if self.limits.max_connections > 0 {
            return self.limits.max_connections;
        }
        u32::try_from(fd_limit.saturating_sub(RESERVED_FDS)).unwrap_or(u32::MAX)
    }

    /// Resolve and validate the version restriction policy.
    ///
    /// # Errors
    ///
    /// A product or platform key that is not exactly four ASCII characters — a typo would
    /// otherwise create a rule nothing ever matches, silently locking out the client the
    /// operator meant to allow. Also rejects `restrict = true` with no entries at all,
    /// which would refuse every client and is never what an operator intends.
    pub fn version_policy(&self) -> Result<VersionPolicy, String> {
        let mut allowed = BTreeMap::new();
        for (product, platforms) in &self.versions.allowed {
            let product_cc = parse_fourcc("product", product)?;
            for (platform, bytes) in platforms {
                let platform_cc = parse_fourcc("platform", platform)?;
                allowed.insert((product_cc, platform_cc), bytes.clone());
            }
        }
        if self.versions.restrict && allowed.is_empty() {
            return Err(
                "versions.restrict is true but versions.allowed is empty; this would \
                 refuse every client. List the products and platforms to allow, or set \
                 restrict = false."
                    .into(),
            );
        }
        Ok(VersionPolicy {
            restrict: self.versions.restrict,
            allowed,
        })
    }

    /// Resolve and validate the channel rules.
    ///
    /// # Errors
    ///
    /// A bad `telnet_access` value, a bad per-channel `telnet` / `min_flag` value, a blank
    /// channel name, or two definitions that normalise to the same channel.
    pub fn channel_rules(&self) -> Result<ChannelRules, String> {
        let telnet_access = match self.channels.telnet_access.to_ascii_lowercase().as_str() {
            "none" => TelnetAccess::None,
            "public" | "" => TelnetAccess::Public,
            "all" => TelnetAccess::All,
            other => {
                return Err(format!(
                    "channels.telnet_access must be \"public\", \"none\" or \"all\", not {other:?}"
                ))
            }
        };

        let mut by_key = HashMap::new();
        for def in &self.channels.defined {
            let display = def.name.trim();
            if display.is_empty() {
                return Err("a channels.defined entry has an empty name".into());
            }
            let key = normalize_channel_name(display.as_bytes());
            if key.is_empty() {
                return Err(format!("channel name {display:?} normalises to nothing"));
            }
            let public = def.public.unwrap_or_else(|| {
                bnetcc_core::channel::classify_name(display) == bnetcc_core::channel::NameKind::Public
            });
            let telnet = match def.telnet.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("") => None,
                Some("allow") => Some(true),
                Some("deny") => Some(false),
                Some(other) => {
                    return Err(format!(
                        "channel {display:?}: telnet must be \"allow\" or \"deny\", not {other:?}"
                    ))
                }
            };
            let min_flag = match def.min_flag.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("") | Some("none") => 0,
                Some("admin") => user_flags::ADMIN,
                Some("speaker") => user_flags::SPEAKER,
                Some("operator") => user_flags::OPERATOR,
                Some(other) => {
                    return Err(format!(
                        "channel {display:?}: min_flag must be \"admin\", \"speaker\" or \
                         \"operator\", not {other:?}"
                    ))
                }
            };
            let products: Vec<String> = def
                .products
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|p| p.trim().to_ascii_uppercase())
                .filter(|p| !p.is_empty())
                .collect();
            let resolved = ResolvedChannel {
                display: display.to_string(),
                listed: def.listed.unwrap_or(true),
                public,
                persist: def.persist.unwrap_or(false),
                max_users: def.max_users,
                topic: def.topic.clone().filter(|t| !t.is_empty()),
                telnet,
                game_only: def.game_only.unwrap_or(false),
                min_flag,
                products,
            };
            if by_key.insert(key, resolved).is_some() {
                return Err(format!("two channels.defined entries resolve to {display:?}"));
            }
        }

        let default_channel: HashMap<String, String> = self
            .channels
            .default_channel
            .iter()
            .filter(|(_, chan)| !chan.trim().is_empty())
            .map(|(product, chan)| (product.trim().to_ascii_uppercase(), chan.trim().to_string()))
            .collect();

        Ok(ChannelRules { telnet_access, by_key, default_channel })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_is_valid() {
        let cfg = Config::from_toml("").unwrap();
        assert_eq!(cfg.server.mode, "gaming");
        assert_eq!(cfg.listen.bncs.port(), 6112);
        assert!(!cfg.federation.enabled);
    }

    #[test]
    fn product_restriction_and_default_channel_resolve() {
        let cfg = Config::from_toml(
            r#"
            [[channels.defined]]
            name = "War2 BNE"
            products = ["w2bn"]

            [channels.default_channel]
            star = "StarCraft"
            "#,
        )
        .unwrap();
        let rules = cfg.channel_rules().unwrap();
        let key = bnetcc_proto::chat::normalize_channel_name(b"War2 BNE");
        assert_eq!(rules.get(&key).unwrap().products, vec!["W2BN".to_string()], "products upper-cased");
        assert_eq!(rules.default_channel("STAR"), Some("StarCraft"));
        assert_eq!(rules.default_channel("star"), Some("StarCraft"), "lookup is case-insensitive");
        assert_eq!(rules.default_channel("SEXP"), None);
    }

    #[test]
    fn defined_channels_resolve_and_apply_rules() {
        let cfg = Config::from_toml(
            r#"
            [channels]
            telnet_access = "public"

            [[channels.defined]]
            name = "Open Tech Support"
            telnet = "allow"
            persist = true
            topic = "Ask here"

            [[channels.defined]]
            name = "Staff"
            listed = false
            min_flag = "admin"

            [[channels.defined]]
            name = "War2 Ladder"
            game_only = true
            "#,
        )
        .unwrap();
        let rules = cfg.channel_rules().unwrap();

        // Per-channel telnet override lets telnet into a non-public channel...
        assert!(rules.telnet_may_join("Open Tech Support"));
        // ...while a normal private channel stays closed to telnet under the default.
        assert!(!rules.telnet_may_join("Someones Room"));
        // Public channels are reachable by the global default.
        assert!(rules.telnet_may_join("Public Chat"));

        let tech = rules
            .get(&normalize_channel_name(b"Open Tech Support"))
            .expect("defined");
        assert!(tech.persist);
        assert_eq!(tech.topic.as_deref(), Some("Ask here"));

        let staff = rules.get(&normalize_channel_name(b"Staff")).expect("defined");
        assert!(!staff.listed);
        assert_eq!(staff.min_flag, user_flags::ADMIN);

        let ladder = rules.get(&normalize_channel_name(b"War2 Ladder")).expect("defined");
        assert!(ladder.game_only);

        // Only "Open Tech Support" and "War2 Ladder" are listed (Staff is hidden).
        assert_eq!(rules.listed().count(), 2);
    }

    #[test]
    fn a_bad_channel_setting_is_rejected() {
        let cfg = Config::from_toml(
            "[[channels.defined]]\nname = \"X\"\ntelnet = \"maybe\"",
        )
        .unwrap();
        assert!(cfg.channel_rules().is_err());
    }

    #[test]
    fn a_minimal_warnet_config_parses() {
        let cfg = Config::from_toml(
            r#"
            [server]
            name = "Warzone"
            mode = "warnet"

            [federation]
            enabled = true
            hub = "hub.example.net:7112"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.parsed_mode().unwrap(), ServerMode::Warnet);
        assert_eq!(cfg.server.name, "Warzone");
        // Unset fields still carry their defaults.
        assert_eq!(cfg.limits.outbound_queue, 64);
    }

    #[test]
    fn an_unknown_mode_is_rejected_with_a_useful_message() {
        let err = Config::from_toml("[server]\nmode = \"wharnet\"").unwrap_err();
        assert!(err.contains("wharnet"), "message should name the bad value: {err}");
        assert!(err.contains("gaming"), "message should list the valid values");
    }

    #[test]
    fn a_typo_in_a_key_is_rejected_rather_than_ignored() {
        // Silently ignoring an unknown key is how operators end up with a setting they
        // believe is applied and is not.
        assert!(Config::from_toml("[server]\nmoed = \"warnet\"").is_err());
    }

    #[test]
    fn federation_without_a_hub_is_rejected() {
        let err = Config::from_toml("[federation]\nenabled = true").unwrap_err();
        assert!(err.contains("hub"));
    }

    #[test]
    fn a_zero_outbound_queue_is_rejected() {
        assert!(Config::from_toml("[limits]\noutbound_queue = 0").is_err());
    }

    #[test]
    fn max_connections_is_derived_from_the_fd_limit_by_default() {
        let cfg = Config::default();
        assert_eq!(cfg.effective_max_connections(1024), 960);
        assert_eq!(cfg.effective_max_connections(65_536), 65_472);
    }

    #[test]
    fn an_explicit_max_connections_wins() {
        let cfg = Config::from_toml("[limits]\nmax_connections = 2500").unwrap();
        assert_eq!(cfg.effective_max_connections(1_000_000), 2500);
    }

    #[test]
    fn a_tiny_fd_limit_does_not_underflow() {
        let cfg = Config::default();
        assert_eq!(cfg.effective_max_connections(10), 0);
    }

    #[test]
    fn client_limits_default_to_the_mode_defaults() {
        let p = Config::default().policy().unwrap();
        assert_eq!(p.clients.gateway.per_ip, 1);
        assert!(p.clients.game_default.per_ip > 1);
    }

    #[test]
    fn a_product_limit_can_be_set_from_config() {
        let cfg = Config::from_toml(
            r#"
            [limits.clients.products.WAR3]
            per_ip = 2
            "#,
        )
        .unwrap();
        let p = cfg.policy().unwrap();
        assert_eq!(
            p.clients
                .for_class(ClientClass::Game(bnetcc_proto::product::WAR3))
                .per_ip,
            2
        );
        // Other products are untouched.
        assert_eq!(
            p.clients
                .for_class(ClientClass::Game(bnetcc_proto::product::SEXP))
                .per_ip,
            p.clients.game_default.per_ip
        );
    }

    #[test]
    fn a_partial_product_override_keeps_the_global_ceiling() {
        let cfg = Config::from_toml("[limits.clients.products.D2XP]\nper_ip = 3").unwrap();
        let p = cfg.policy().unwrap();
        let d2 = p.clients.for_class(ClientClass::Game(bnetcc_proto::product::D2XP));
        assert_eq!(d2.per_ip, 3);
        assert_eq!(d2.global, p.clients.game_default.global);
    }

    #[test]
    fn the_gateway_limit_is_configurable_but_defaults_to_one() {
        assert_eq!(
            Config::default().policy().unwrap().clients.for_class(ClientClass::Gateway).per_ip,
            1
        );
        let cfg = Config::from_toml("[limits.clients.gateway]\nper_ip = 4").unwrap();
        assert_eq!(
            cfg.policy().unwrap().clients.for_class(ClientClass::Gateway).per_ip,
            4
        );
    }

    #[test]
    fn a_malformed_product_code_is_rejected_with_a_useful_message() {
        // A typo here would otherwise create a product nothing ever matches, and the
        // operator would never learn the setting had no effect.
        let err = Config::from_toml("[limits.clients.products.WARCRAFT3]\nper_ip = 2")
            .unwrap_err();
        assert!(err.contains("WARCRAFT3"), "{err}");
        assert!(err.contains("four"), "{err}");
    }

    #[test]
    fn pending_limits_are_widened_to_cover_the_loosest_product() {
        // The pending class only bounds a connect flood before SID_AUTH_INFO identifies
        // the client. If it were tighter than a product it would reject connections that
        // product allows, which is always a misconfiguration rather than an intent.
        let cfg = Config::from_toml(
            r#"
            [limits.clients.game_pending]
            per_ip = 2

            [limits.clients.products.SEXP]
            per_ip = 12
            "#,
        )
        .unwrap();
        let p = cfg.policy().unwrap();
        assert!(p.clients.game_pending.per_ip >= 12);
    }
}
