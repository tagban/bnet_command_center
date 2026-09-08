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
    /// # Why one gateway connection per IP, in every mode
    ///
    /// This looks like an anti-abuse limit and is not. **It is the entry cost, and it is
    /// deliberate in warnet mode above all.**
    ///
    /// One bot per address means a twenty-bot fleet costs twenty CD keys and twenty
    /// addresses. That expense is the point: a large fleet is a visible demonstration
    /// that someone actually acquired the keys and the infrastructure. Raising the cap so
    /// that one box can hold twenty bots makes fleets free, and a fleet that costs
    /// nothing displays nothing. The limit is what the achievement is *made of*.
    ///
    /// So warnet mode does **not** relax `per_ip`. What it raises is `global`, because a
    /// warnet legitimately carries far more gateway connections in total — they simply
    /// have to arrive from distinct addresses.
    ///
    /// The stronger half of the gate is elsewhere: CD-key session uniqueness (see
    /// [`crate::limits::KeyRegistry`]). Addresses are cheap to rent; keys are not.
    /// Enforcing one live session per key is what actually makes a fleet expensive, and
    /// it is why real Battle.net answers `SID_AUTH_CHECK` with `0x201` "key in use".
    ///
    /// An operator who wants to exempt their own bot host can still do it explicitly via
    /// the allowlist on [`crate::limits::AdmissionTable`]. That is a deliberate, visible
    /// act by the server owner, not a default that quietly devalues everyone's fleet.
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
                // per_ip stays at 1: the cost of a fleet is the point. Only `global`
                // rises, because a warnet carries more bots in total — from more
                // addresses, not more per address.
                gateway_limits: ConnLimits {
                    per_ip: 1,
                    per_account: 1,
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
                    per_ip: 1,
                    per_account: 1,
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
    fn one_gateway_connection_per_ip_in_every_mode() {
        // This is the entry cost for a bot fleet and it must never be relaxed by a mode
        // default. Twenty bots should mean twenty keys and twenty addresses; that
        // expense is what makes a large fleet worth showing off. An operator can exempt
        // a specific host through the allowlist — deliberately, and only their own.
        for mode in [ServerMode::Gaming, ServerMode::Warnet, ServerMode::Both] {
            let p = Policy::for_mode(mode);
            assert_eq!(
                p.gateway_limits.per_ip, 1,
                "{mode:?} must keep one gateway connection per address"
            );
            assert_eq!(
                p.gateway_limits.per_account, 1,
                "{mode:?} must keep one gateway connection per account"
            );
        }
    }

    #[test]
    fn warnet_raises_only_the_global_gateway_ceiling() {
        // A warnet carries far more bots in total — from more addresses, not more per
        // address. `global` is the only gateway limit that may move.
        let gaming = Policy::for_mode(ServerMode::Gaming);
        let warnet = Policy::for_mode(ServerMode::Warnet);
        assert!(
            warnet.gateway_limits.global > gaming.gateway_limits.global,
            "a warnet needs headroom for many bots in total"
        );
        assert_eq!(warnet.gateway_limits.per_ip, gaming.gateway_limits.per_ip);
        assert_eq!(
            warnet.gateway_limits.per_account,
            gaming.gateway_limits.per_account
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
        // A node with less capacity than the hub allows caps itself.
        node.gateway_limits.global = 128;
        node.game_limits.per_ip = 2;
        let effective = hub.narrowed_by(node);
        assert_eq!(effective.gateway_limits.global, 128);
        assert_eq!(effective.game_limits.per_ip, 2);
        // And per_ip on the gateway is already 1 everywhere, so it cannot move at all.
        assert_eq!(effective.gateway_limits.per_ip, 1);
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
