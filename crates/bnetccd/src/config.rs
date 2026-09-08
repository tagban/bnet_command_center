//! Node configuration.
//!
//! One typed document with `serde` defaults, rather than PvPGN's ~200 flat keys spread
//! across `bnetd.conf` plus fifteen side files. Every field has a default, so a minimal
//! config is a handful of lines and an operator only writes what they want to change.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

#[cfg(test)]
use bnetcc_core::limits::ClientClass;
use bnetcc_core::policy::{ClientLimits, ConnLimitsPatch, Policy, ServerMode};
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "Command Center".into(),
            mode: "gaming".into(),
            motd: "Welcome to Command Center.".into(),
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
}

/// Listener addresses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenConfig {
    /// BNCS and chat gateway (both are demultiplexed by the first byte).
    pub bncs: SocketAddr,
    /// Admin, health and Prometheus metrics.
    pub admin: SocketAddr,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            bncs: "0.0.0.0:6112".parse().expect("valid default"),
            admin: "127.0.0.1:6115".parse().expect("valid default"),
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
