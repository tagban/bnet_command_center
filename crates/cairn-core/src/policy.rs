//! Server policy — the single place operator-configurable behaviour lives.
//!
//! Handlers ask the policy; they never branch on the mode directly. That is what keeps
//! "warnet" a configuration concern rather than a second code path that rots.

use std::collections::BTreeMap;

use cairn_proto::FourCc;

use crate::limits::ClientClass;

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

/// Connection admission limits for one client class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnLimits {
    /// Concurrent connections permitted from one IP address.
    pub per_ip: u32,
    /// Concurrent connections permitted for one account.
    pub per_account: u32,
    /// Concurrent connections of this class server-wide.
    pub global: u32,
}

impl ConnLimits {
    /// The tighter of two limit sets, field by field.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        Self {
            per_ip: if self.per_ip < other.per_ip {
                self.per_ip
            } else {
                other.per_ip
            },
            per_account: if self.per_account < other.per_account {
                self.per_account
            } else {
                other.per_account
            },
            global: if self.global < other.global {
                self.global
            } else {
                other.global
            },
        }
    }
}

/// A partial override of [`ConnLimits`].
///
/// Config supplies only the fields an operator wants to change; the rest fall through to
/// the class default. This is what makes `per_ip = 2` for WarCraft III a one-line change
/// rather than a full triple that silently resets `global`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConnLimitsPatch {
    /// Override for concurrent connections per address.
    pub per_ip: Option<u32>,
    /// Override for concurrent connections per account.
    pub per_account: Option<u32>,
    /// Override for the class-wide ceiling.
    pub global: Option<u32>,
}

impl ConnLimitsPatch {
    /// Apply this patch over a base.
    #[must_use]
    pub const fn apply(self, base: ConnLimits) -> ConnLimits {
        ConnLimits {
            per_ip: match self.per_ip {
                Some(v) => v,
                None => base.per_ip,
            },
            per_account: match self.per_account {
                Some(v) => v,
                None => base.per_account,
            },
            global: match self.global {
                Some(v) => v,
                None => base.global,
            },
        }
    }
}

/// Per-client-type connection limits.
///
/// # Why this is per type rather than one number
///
/// The two bot paths cost their operator completely different things, and a single limit
/// cannot price both:
///
/// - **The telnet/chat gateway has no CD-key step at all.** A gateway bot needs an account
///   and nothing else, so the address is the *only* cost available to charge — and it is
///   charged, at one per address, in every mode. Twenty gateway bots means twenty
///   addresses, and going and getting them is the visible part of the achievement.
/// - **Game clients already pay in CD keys.** They run the full
///   `SID_AUTH_INFO`/`SID_AUTH_CHECK` handshake, so the real gate is one live session per
///   key ([`crate::limits::KeyRegistry`]). Keys cost far more than addresses, so the
///   per-IP number there is deliberately loose: households, LAN cafés and shared NATs
///   carry several legitimate players and there is no reason to break them.
///
/// Beyond that split, each **product** gets its own entry, because they are not
/// interchangeable either — an operator may happily allow eight Brood War clients from a
/// café while allowing only two WarCraft III, whose keys are scarcer. Anything without an
/// explicit entry falls back to [`Self::game_default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLimits {
    /// Telnet / chat gateway (protocol bytes `0x03`, `0x43`, `0x63`).
    pub gateway: ConnLimits,
    /// BNFTP file transfer (protocol byte `0x02`).
    pub bnftp: ConnLimits,
    /// A game connection whose product is not yet known.
    ///
    /// Applies between `accept` and `SID_AUTH_INFO`. Should be at least as loose as the
    /// most permissive product, since it is refined the moment the product arrives — its
    /// job is to bound a connect flood, not to be the real limit.
    pub game_pending: ConnLimits,
    /// Fallback for any game product without an explicit entry.
    pub game_default: ConnLimits,
    /// Per-product overrides, keyed by four-character product code.
    pub products: BTreeMap<FourCc, ConnLimits>,
}

impl ClientLimits {
    /// The limits that apply to a client class.
    #[must_use]
    pub fn for_class(&self, class: ClientClass) -> ConnLimits {
        match class {
            ClientClass::Gateway => self.gateway,
            ClientClass::Bnftp => self.bnftp,
            ClientClass::GamePending => self.game_pending,
            ClientClass::Game(product) => self
                .products
                .get(&product)
                .copied()
                .unwrap_or(self.game_default),
        }
    }

    /// Set or replace a product's limits.
    pub fn set_product(&mut self, product: FourCc, limits: ConnLimits) {
        self.products.insert(product, limits);
    }

    /// Apply a partial override to a product, over the current effective limits.
    pub fn patch_product(&mut self, product: FourCc, patch: ConnLimitsPatch) {
        let base = self.for_class(ClientClass::Game(product));
        self.products.insert(product, patch.apply(base));
    }

    /// The tighter of two sets, class by class.
    ///
    /// A product present in either set appears in the result; one present in only one set
    /// is compared against the other's `game_default`, so a node cannot widen a product
    /// the hub restricted simply by omitting it.
    #[must_use]
    pub fn narrowed_by(&self, other: &Self) -> Self {
        let mut products = BTreeMap::new();
        let keys: std::collections::BTreeSet<FourCc> = self
            .products
            .keys()
            .chain(other.products.keys())
            .copied()
            .collect();
        for k in keys {
            let mine = self
                .products
                .get(&k)
                .copied()
                .unwrap_or(self.game_default);
            let theirs = other
                .products
                .get(&k)
                .copied()
                .unwrap_or(other.game_default);
            products.insert(k, mine.min(theirs));
        }
        Self {
            gateway: self.gateway.min(other.gateway),
            bnftp: self.bnftp.min(other.bnftp),
            game_pending: self.game_pending.min(other.game_pending),
            game_default: self.game_default.min(other.game_default),
            products,
        }
    }
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Connection limits, per client type.
    pub clients: ClientLimits,
    /// Flood control for game clients.
    pub game_flood: FloodPolicy,
    /// Flood control for chat-gateway connections.
    pub gateway_flood: FloodPolicy,
}

impl Policy {
    /// The default policy for a mode.
    ///
    /// See [`ClientLimits`] for why the gateway and the game products are priced
    /// differently, and why warnet mode raises only the gateway's `global` ceiling.
    #[must_use]
    pub fn for_mode(mode: ServerMode) -> Self {
        let game_default = ConnLimits {
            per_ip: 8,
            per_account: 2,
            global: 4096,
        };
        // The gateway is keyless, so the address is the only cost available to charge.
        // One per address, in every mode.
        let gateway = ConnLimits {
            per_ip: 1,
            per_account: 1,
            global: match mode {
                ServerMode::Gaming => 256,
                ServerMode::Warnet => 2048,
                ServerMode::Both => 1024,
            },
        };
        let clients = ClientLimits {
            gateway,
            bnftp: ConnLimits {
                per_ip: 4,
                per_account: 4,
                global: 512,
            },
            // Deliberately at least as loose as any product: this only bounds a connect
            // flood in the window before SID_AUTH_INFO identifies the client.
            game_pending: ConnLimits {
                per_ip: 16,
                per_account: u32::MAX,
                global: 8192,
            },
            game_default,
            products: BTreeMap::new(),
        };

        let (game_hosting, game_listing, realms, w3_routing, chat_ordering) = match mode {
            ServerMode::Warnet => (
                Gate::Refuse,
                Gate::Refuse,
                Gate::Refuse,
                Gate::Refuse,
                ChatOrdering::HubSerialized,
            ),
            ServerMode::Gaming | ServerMode::Both => (
                Gate::Allow,
                Gate::Allow,
                Gate::Allow,
                Gate::Allow,
                ChatOrdering::LocalFirst,
            ),
        };

        Self {
            mode,
            game_hosting,
            game_listing,
            realms,
            w3_routing,
            chat_ordering,
            clients,
            game_flood: FloodPolicy {
                messages_per_10s: 12,
                burst: 5,
                penalty: FloodPenalty::Mute { seconds: 30 },
            },
            gateway_flood: FloodPolicy {
                messages_per_10s: match mode {
                    ServerMode::Gaming => 60,
                    ServerMode::Warnet => 200,
                    ServerMode::Both => 120,
                },
                burst: match mode {
                    ServerMode::Gaming => 20,
                    ServerMode::Warnet => 60,
                    ServerMode::Both => 40,
                },
                penalty: FloodPenalty::DropMessage,
            },
        }
    }

    /// Narrow this policy by another.
    ///
    /// A node may restrict what the hub permits but never widen it, so a hub in warnet
    /// mode cannot have a node quietly re-enabling game hosting. Every gate is combined
    /// with logical AND and every limit takes the minimum.
    #[must_use]
    pub fn narrowed_by(&self, node: &Self) -> Self {
        const fn and(a: Gate, b: Gate) -> Gate {
            if a.allowed() && b.allowed() {
                Gate::Allow
            } else {
                Gate::Refuse
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
            clients: self.clients.narrowed_by(&node.clients),
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
    use cairn_proto::product;

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
    fn only_the_gateway_is_capped_at_one_per_ip() {
        // The gateway is keyless, so the address is the only cost. Game clients already
        // pay in CD keys and must not be capped at one per address — households, LAN
        // cafés and shared NATs carry several legitimate players.
        for mode in [ServerMode::Gaming, ServerMode::Warnet, ServerMode::Both] {
            let c = &Policy::for_mode(mode).clients;
            assert_eq!(
                c.gateway.per_ip, 1,
                "{mode:?}: gateway must stay at one per address"
            );
            assert!(
                c.game_default.per_ip > 1,
                "{mode:?}: game clients must not inherit the gateway cap"
            );
            assert!(c.bnftp.per_ip > 1, "{mode:?}: file transfer is not a bot path");
        }
    }

    #[test]
    fn warnet_raises_only_the_global_gateway_ceiling() {
        let gaming = Policy::for_mode(ServerMode::Gaming);
        let warnet = Policy::for_mode(ServerMode::Warnet);
        assert!(warnet.clients.gateway.global > gaming.clients.gateway.global);
        assert_eq!(warnet.clients.gateway.per_ip, gaming.clients.gateway.per_ip);
        assert_eq!(
            warnet.clients.gateway.per_account,
            gaming.clients.gateway.per_account
        );
    }

    #[test]
    fn unknown_products_fall_back_to_the_game_default() {
        let c = &Policy::for_mode(ServerMode::Gaming).clients;
        assert_eq!(
            c.for_class(ClientClass::Game(product::SEXP)),
            c.game_default
        );
        assert_eq!(
            c.for_class(ClientClass::Game(FourCc::from_ascii(b"XXXX"))),
            c.game_default
        );
    }

    #[test]
    fn a_product_override_applies_only_to_that_product() {
        let mut c = Policy::for_mode(ServerMode::Gaming).clients;
        c.set_product(
            product::WAR3,
            ConnLimits {
                per_ip: 2,
                per_account: 1,
                global: 512,
            },
        );
        assert_eq!(c.for_class(ClientClass::Game(product::WAR3)).per_ip, 2);
        assert_eq!(
            c.for_class(ClientClass::Game(product::SEXP)).per_ip,
            c.game_default.per_ip
        );
        assert_eq!(c.for_class(ClientClass::Gateway).per_ip, 1);
    }

    #[test]
    fn a_partial_override_keeps_the_other_fields() {
        // `per_ip = 2` for WarCraft III must not silently reset its global ceiling.
        let mut c = Policy::for_mode(ServerMode::Gaming).clients;
        let before = c.game_default;
        c.patch_product(
            product::W3XP,
            ConnLimitsPatch {
                per_ip: Some(2),
                ..ConnLimitsPatch::default()
            },
        );
        let after = c.for_class(ClientClass::Game(product::W3XP));
        assert_eq!(after.per_ip, 2);
        assert_eq!(after.per_account, before.per_account);
        assert_eq!(after.global, before.global);
    }

    #[test]
    fn pending_is_at_least_as_loose_as_any_product() {
        // It is refined the instant SID_AUTH_INFO arrives; its only job is to bound a
        // connect flood, so it must not reject a client the product would have allowed.
        let c = &Policy::for_mode(ServerMode::Gaming).clients;
        assert!(c.game_pending.per_ip >= c.game_default.per_ip);
        for limits in c.products.values() {
            assert!(c.game_pending.per_ip >= limits.per_ip);
        }
    }

    #[test]
    fn gateway_flood_never_disconnects() {
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
        let effective = hub.narrowed_by(&permissive_node);
        assert!(!effective.game_hosting.allowed());
        assert_eq!(effective.chat_ordering, ChatOrdering::HubSerialized);
    }

    #[test]
    fn narrowing_takes_the_tighter_limits_per_class() {
        let hub = Policy::for_mode(ServerMode::Warnet);
        let mut node = Policy::for_mode(ServerMode::Warnet);
        node.clients.gateway.global = 128;
        node.clients.game_default.per_ip = 2;
        let e = hub.narrowed_by(&node);
        assert_eq!(e.clients.gateway.global, 128);
        assert_eq!(e.clients.game_default.per_ip, 2);
        assert_eq!(e.clients.gateway.per_ip, 1);
    }

    #[test]
    fn a_node_cannot_widen_a_product_by_omitting_it() {
        // The hub restricts WarCraft III; the node simply has no entry for it. The node
        // must not thereby inherit its own looser default.
        let mut hub = Policy::for_mode(ServerMode::Gaming);
        hub.clients.patch_product(
            product::WAR3,
            ConnLimitsPatch {
                per_ip: Some(2),
                ..ConnLimitsPatch::default()
            },
        );
        let node = Policy::for_mode(ServerMode::Gaming);
        let e = hub.narrowed_by(&node);
        assert_eq!(e.clients.for_class(ClientClass::Game(product::WAR3)).per_ip, 2);
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
