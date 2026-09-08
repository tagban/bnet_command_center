//! Server policy — the single place operator-configurable behaviour lives.
//!
//! Handlers ask the policy; they never branch on the mode directly. That is what keeps
//! "warnet" a configuration concern rather than a second code path that rots.

/// What this server is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerMode {
    /// Normal Battle.net: games, realms, ladder.
    #[default]
    Gaming,
    /// Channel warring: chat and bots only, no game hosting or listing.
    Warnet,
    /// Both, decided per channel and per connection class.
    Both,
}

impl ServerMode {
    /// Parse from a config string.
    ///
    /// # Errors
    ///
    /// Returns the offending string if it is not a recognised mode.
    pub fn parse(s: &str) -> Result<Self, &str> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gaming" => Ok(Self::Gaming),
            "warnet" => Ok(Self::Warnet),
            "both" => Ok(Self::Both),
            _ => Err(s),
        }
    }

    /// Config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gaming => "gaming",
            Self::Warnet => "warnet",
            Self::Both => "both",
        }
    }
}

/// Whether a subsystem is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Available.
    Allow,
    /// Unavailable. The handler must answer with the protocol's own documented
    /// "unavailable" status rather than dropping the packet — a client that gets no
    /// response hangs, and the user blames the server.
    Refuse,
}

impl Gate {
    /// True when the subsystem is available.
    #[must_use]
    pub const fn allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// How channel events are ordered across a federation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatOrdering {
    /// Echo locally at once, forward to the hub for cross-node fanout.
    ///
    /// Sub-millisecond local latency; global order may differ slightly between nodes.
    LocalFirst,
    /// Every event round-trips the hub sequencer before anyone sees it.
    ///
    /// Identical ordering on every node, at the cost of one hub RTT. This is what makes
    /// a federated channel war have exactly one answer to "who got operator first".
    HubSerialized,
}

/// Connection admission limits, per connection class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnLimits {
    /// Concurrent connections permitted from one IP address.
    pub per_ip: u32,
    /// Concurrent connections permitted for one account.
    pub per_account: u32,
    /// Concurrent connections of this class server-wide.
    pub global: u32,
}

/// Flood control parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloodPolicy {
    /// Sustained rate, messages per ten seconds.
    pub messages_per_10s: u32,
    /// Burst allowance above the sustained rate.
    pub burst: u32,
    /// What happens when the budget is exhausted.
    pub penalty: FloodPenalty,
}

/// What to do with a message that exceeds the flood budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodPenalty {
    /// Discard the message and tell the sender.
    ///
    /// Real Battle.net discards silently, which is maddening to debug. We always emit
    /// `EID_ERROR` to the sender and count it.
    DropMessage,
    /// Mute the sender for a period.
    Mute {
        /// Mute duration in seconds.
        seconds: u32,
    },
    /// Send `SID_FLOODDETECTED` and close the connection.
    ///
    /// Never appropriate for the chat gateway: a dropped bot reconnect-storms, which is
    /// worse for everyone than a dropped message.
    Disconnect,
}

/// Resolved server policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// What this server is for.
    pub mode: ServerMode,
    /// `SID_STARTADVEX3` — hosting a game.
    pub game_hosting: Gate,
    /// `SID_GETADVLISTEX` — listing games.
    pub game_listing: Gate,
    /// `SID_QUERYREALMS2` / `SID_LOGONREALMEX` — Diablo II realms.
    pub realms: Gate,
    /// Whether the WarCraft III route listener binds.
    pub w3_routing: Gate,
    /// Channel event ordering.
    pub chat_ordering: ChatOrdering,
    /// Limits for game clients.
    pub game_limits: ConnLimits,
    /// Limits for chat-gateway (bot) connections.
    pub gateway_limits: ConnLimits,
    /// Flood control for game clients.
    pub game_flood: FloodPolicy,
    /// Flood control for chat-gateway connections.
    pub gateway_flood: FloodPolicy,
}

impl Policy {
    /// The default policy for a mode.
    ///
    /// The connection limits are where the two modes genuinely disagree. A gaming server
    /// wants one gateway connection per IP, because a gateway connection is a bot and one
    /// bot per address is a sane anti-abuse default. A warnet wants the opposite, because
    /// operators running fleets of bots from one box *is the product* — a hard 1-per-IP
    /// cap makes a warnet unusable. `per_account` is the more meaningful control there:
    /// it bounds how much presence one identity can project regardless of how many
    /// addresses they have.
    #[must_use]
    pub const fn for_mode(mode: ServerMode) -> Self {
        match mode {
            ServerMode::Gaming => Self {
                mode,
                game_hosting: Gate::Allow,
                game_listing: Gate::Allow,
                realms: Gate::Allow,
                w3_routing: Gate::Allow,
                chat_ordering: ChatOrdering::LocalFirst,
                game_limits: ConnLimits {
                    per_ip: 8,
                    per_account: 2,
                    global: 4096,
                },
                gateway_limits: ConnLimits {
                    per_ip: 1,
                    per_account: 1,
                    global: 256,
                },
                game_flood: FloodPolicy {
                    messages_per_10s: 12,
                    burst: 5,
                    penalty: FloodPenalty::Mute { seconds: 30 },
                },
                gateway_flood: FloodPolicy {
                    messages_per_10s: 60,
                    burst: 20,
                    penalty: FloodPenalty::DropMessage,
                },
            },
            ServerMode::Warnet => Self {
                mode,
                game_hosting: Gate::Refuse,
                game_listing: Gate::Refuse,
                realms: Gate::Refuse,
                w3_routing: Gate::Refuse,
                chat_ordering: ChatOrdering::HubSerialized,
                game_limits: ConnLimits {
                    per_ip: 8,
                    per_account: 2,
                    global: 4096,
                },
                gateway_limits: ConnLimits {
                    per_ip: 16,
                    per_account: 4,
                    global: 2048,
                },
                game_flood: FloodPolicy {
                    messages_per_10s: 12,
                    burst: 5,
                    penalty: FloodPenalty::Mute { seconds: 30 },
                },
                gateway_flood: FloodPolicy {
                    messages_per_10s: 200,
                    burst: 60,
                    penalty: FloodPenalty::DropMessage,
                },
            },
            ServerMode::Both => Self {
                mode,
                game_hosting: Gate::Allow,
                game_listing: Gate::Allow,
                realms: Gate::Allow,
                w3_routing: Gate::Allow,
                chat_ordering: ChatOrdering::LocalFirst,
                game_limits: ConnLimits {
                    per_ip: 8,
                    per_account: 2,
                    global: 4096,
                },
                gateway_limits: ConnLimits {
                    per_ip: 4,
                    per_account: 2,
                    global: 1024,
                },
                game_flood: FloodPolicy {
                    messages_per_10s: 12,
                    burst: 5,
                    penalty: FloodPenalty::Mute { seconds: 30 },
                },
                gateway_flood: FloodPolicy {
                    messages_per_10s: 120,
                    burst: 40,
                    penalty: FloodPenalty::DropMessage,
                },
            },
        }
    }

    /// Narrow this policy by another.
    ///
    /// A node may restrict what the hub permits but never widen it, so a hub in warnet
    /// mode cannot have a node quietly re-enabling game hosting. Every gate is combined
    /// with logical AND and every limit takes the minimum.
    #[must_use]
    pub fn narrowed_by(self, node: Self) -> Self {
        const fn and(a: Gate, b: Gate) -> Gate {
            if a.allowed() && b.allowed() {
                Gate::Allow
            } else {
                Gate::Refuse
            }
        }
        const fn min_limits(a: ConnLimits, b: ConnLimits) -> ConnLimits {
            ConnLimits {
                per_ip: if a.per_ip < b.per_ip { a.per_ip } else { b.per_ip },
                per_account: if a.per_account < b.per_account {
                    a.per_account
                } else {
                    b.per_account
                },
                global: if a.global < b.global { a.global } else { b.global },
            }
        }
        Self {
            mode: self.mode,
            game_hosting: and(self.game_hosting, node.game_hosting),
            game_listing: and(self.game_listing, node.game_listing),
            realms: and(self.realms, node.realms),
            w3_routing: and(self.w3_routing, node.w3_routing),
            // Ordering is the hub's call: a node cannot opt out of serialization and
            // still claim its channel events are network-ordered.
            chat_ordering: self.chat_ordering,
            game_limits: min_limits(self.game_limits, node.game_limits),
            gateway_limits: min_limits(self.gateway_limits, node.gateway_limits),
            game_flood: self.game_flood,
            gateway_flood: self.gateway_flood,
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::for_mode(ServerMode::Gaming)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnet_gates_every_game_subsystem() {
        let p = Policy::for_mode(ServerMode::Warnet);
        assert!(!p.game_hosting.allowed());
        assert!(!p.game_listing.allowed());
        assert!(!p.realms.allowed());
        assert!(!p.w3_routing.allowed());
    }

    #[test]
    fn gaming_allows_every_game_subsystem() {
        let p = Policy::for_mode(ServerMode::Gaming);
        assert!(p.game_hosting.allowed());
        assert!(p.game_listing.allowed());
        assert!(p.realms.allowed());
    }

    #[test]
    fn warnet_serializes_chat_and_gaming_does_not() {
        assert_eq!(
            Policy::for_mode(ServerMode::Warnet).chat_ordering,
            ChatOrdering::HubSerialized
        );
        assert_eq!(
            Policy::for_mode(ServerMode::Gaming).chat_ordering,
            ChatOrdering::LocalFirst
        );
    }

    #[test]
    fn warnet_relaxes_gateway_limits_but_keeps_a_per_account_bound() {
        let gaming = Policy::for_mode(ServerMode::Gaming);
        let warnet = Policy::for_mode(ServerMode::Warnet);
        assert_eq!(gaming.gateway_limits.per_ip, 1, "one bot per IP by default");
        assert!(
            warnet.gateway_limits.per_ip > gaming.gateway_limits.per_ip,
            "a 1-per-IP cap makes a warnet unusable"
        );
        assert!(
            warnet.gateway_limits.per_account <= 4,
            "per-account is the meaningful bound in warnet mode"
        );
    }

    #[test]
    fn gateway_flood_never_disconnects() {
        // A disconnected bot reconnect-storms, which is worse than a dropped message.
        for mode in [ServerMode::Gaming, ServerMode::Warnet, ServerMode::Both] {
            assert_ne!(
                Policy::for_mode(mode).gateway_flood.penalty,
                FloodPenalty::Disconnect,
                "{mode:?} must not disconnect gateway clients for pace alone"
            );
        }
    }

    #[test]
    fn a_node_can_narrow_but_never_widen() {
        let hub = Policy::for_mode(ServerMode::Warnet);
        let permissive_node = Policy::for_mode(ServerMode::Gaming);
        let effective = hub.narrowed_by(permissive_node);
        assert!(
            !effective.game_hosting.allowed(),
            "a node must not re-enable what the hub disabled"
        );
        assert_eq!(effective.chat_ordering, ChatOrdering::HubSerialized);
    }

    #[test]
    fn narrowing_takes_the_tighter_limits() {
        let hub = Policy::for_mode(ServerMode::Warnet);
        let mut node = Policy::for_mode(ServerMode::Warnet);
        node.gateway_limits.per_ip = 2;
        assert_eq!(hub.narrowed_by(node).gateway_limits.per_ip, 2);
    }

    #[test]
    fn mode_parses_and_round_trips() {
        for m in [ServerMode::Gaming, ServerMode::Warnet, ServerMode::Both] {
            assert_eq!(ServerMode::parse(m.as_str()), Ok(m));
        }
        assert_eq!(ServerMode::parse("  WARNET "), Ok(ServerMode::Warnet));
        assert!(ServerMode::parse("wharnet").is_err());
    }
}
