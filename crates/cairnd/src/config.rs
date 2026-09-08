//! Node configuration.
//!
//! One typed document with `serde` defaults, rather than PvPGN's ~200 flat keys spread
//! across `bnetd.conf` plus fifteen side files. Every field has a default, so a minimal
//! config is a handful of lines and an operator only writes what they want to change.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use cairn_core::policy::ServerMode;
use serde::Deserialize;

/// Top-level node configuration.
#[derive(Debug, Clone, Deserialize)]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            listen: ListenConfig::default(),
            limits: LimitsConfig::default(),
            federation: FederationConfig::default(),
        }
    }
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
            name: "Cairn".into(),
            mode: "gaming".into(),
            motd: "Welcome to Cairn.".into(),
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
    /// Addresses exempt from the per-IP gateway ceiling.
    pub gateway_allowlist: Vec<IpAddr>,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_connections: 0,
            max_frame_bytes: cairn_proto::bncs::DEFAULT_MAX_FRAME,
            max_line_bytes: cairn_proto::line::DEFAULT_MAX_LINE,
            outbound_queue: 64,
            handshake_timeout_secs: 30,
            idle_timeout_secs: 1200,
            gateway_allowlist: Vec::new(),
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
}
