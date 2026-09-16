//! What a Command Center node and its hub say to each other.
//!
//! The design this implements is `docs/FEDERATION.md`; the message set is its §8. Read that
//! first — the decisions worth arguing about are made there, not here. The two that shape
//! every type in this file:
//!
//! **A node never says which node it is.** The node's identity comes from the authenticated
//! connection it is speaking on, so no message carries a node id. A field can be forged by
//! whoever is on the other end; a client certificate cannot. If you find yourself adding a
//! `node` field to a message, the answer is somewhere else.
//!
//! **Nothing is ordered by time.** Channel events carry a hub-assigned sequence number and
//! are rendered in that order. A federation of volunteer-run machines will contain a clock
//! that is a minute out, and it must not matter.
//!
//! This crate is the vocabulary only: types, framing, and the rules that hold between them.
//! It opens no sockets and makes no policy decisions, so both ends can depend on it without
//! depending on each other.

pub mod frame;
pub mod link;

use serde::{Deserialize, Serialize};

pub use frame::{decode, encode, FrameError, MAX_FRAME};
pub use link::{Link, LinkError};

/// The protocol version a `Hello` announces. Bumped when a peer that does not understand a
/// change would behave wrongly — not merely when a field is added, since an added field is
/// ignored by an older peer rather than misread.
pub const PROTOCOL_VERSION: u32 = 1;

/// Ties a reply to the request that asked for it.
///
/// Every request/response pair carries one. Replies can arrive out of order — a hub answering
/// a slow account lookup and a fast ping will not answer in the order asked — so a node
/// matches on this rather than on arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Cookie(pub u64);

/// Where a channel event sits in the hub's single ordering of that channel.
///
/// The point of the whole design: when two users on two nodes race, exactly one of them is
/// first, and every node agrees which. See FEDERATION.md §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Sequence(pub u64);

/// What a node says on connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// [`PROTOCOL_VERSION`] as this node understands it.
    pub protocol: u32,
    /// The server software, for the hub's operator list — `Command Center`.
    pub software: String,
    /// Its version.
    pub version: String,
    /// Optional behaviours this node can take part in. A hub must work with a node that
    /// offers none of them.
    pub capabilities: Vec<String>,
}

/// What the hub answers, and the terms the node then operates under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The version the hub will speak, never higher than the node asked for.
    pub protocol: u32,
    /// The name the hub knows this node by, for logs and the operator list. Assigned by the
    /// hub from the connection's identity — not chosen by the node.
    pub node: String,
    /// Channels every node joins and no operator may remove.
    pub official_channels: Vec<String>,
    /// How often the node should ping when otherwise idle.
    pub heartbeat_secs: u64,
}

/// Why the hub turned a node away. Sent before the connection closes, so an operator sees a
/// reason rather than a silent drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The node speaks a version the hub cannot.
    Protocol { hub_speaks: u32 },
    /// The hub does not recognise this node's identity.
    Unknown,
    /// Recognised, but not allowed in at the moment — suspended, or the hub is draining.
    NotWelcome { reason: String },
}

/// One message in either direction.
///
/// Tagged by a `type` field, so a peer reading a message it does not know can say so
/// precisely instead of misinterpreting the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Node → hub, first thing on the connection.
    Hello(Hello),
    /// Hub → node, accepting it.
    Welcome(Welcome),
    /// Hub → node, declining it. The connection closes after this.
    Refused(Refusal),
    /// Either direction: is the link alive, and how slow is it? The round trip feeds the
    /// choice between ordering modes (FEDERATION.md §5).
    Ping { cookie: Cookie },
    /// The answer to exactly one [`Message::Ping`].
    Pong { cookie: Cookie },
    /// Node → hub: a user of a product whose login the node cannot verify itself wants in.
    /// The node holds nothing that could verify it, by design (FEDERATION.md §4).
    AuthVerify(AuthVerify),
    /// Hub → node: whether that login is good.
    AuthVerdict { cookie: Cookie, granted: bool },
    /// Node → hub: a user logged in, or left. Presence, and the evidence a later game
    /// result is measured against.
    SessionOpened { account: String, product: String },
    /// Node → hub.
    SessionClosed { account: String },
    /// Node → hub: a user of this node wants into a federated channel.
    ChannelJoin { cookie: Cookie, channel: String, account: String },
    /// Node → hub.
    ChannelLeave { channel: String, account: String },
    /// Either direction: something happened in a channel. From the hub it carries the
    /// sequence number that fixes its place; to the hub it does not yet have one.
    ChannelEvent(ChannelEvent),
    /// Hub → node: who is in a channel, as of a sequence number. Sent when a node joins and
    /// again after a reconnect, so the node can reconcile rather than guess.
    ChannelRoster { channel: String, members: Vec<String>, as_of: Sequence },
    /// Node → hub: a game is open here, and where to reach it.
    GameAdvertise(GameAd),
    /// Node → hub: it is not open any more.
    GameWithdraw { name: String },
    /// Node → hub.
    GameListQuery { cookie: Cookie, product: Option<String> },
    /// Hub → node: every node's games, with what the hub knows about reaching them.
    GameList { cookie: Cookie, games: Vec<GameAd> },
}

/// A login the node is asking the hub to judge.
///
/// This is the whole of what the node sends and the whole of what it holds: the tokens and
/// the client's proof. The secret that proof is checked against never leaves the hub, which
/// is why a node operator cannot impersonate their own users elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthVerify {
    pub cookie: Cookie,
    pub account: String,
    pub client_token: u32,
    pub server_token: u32,
    /// The client's `XSHA1(clientToken ‖ serverToken ‖ h1)`, 20 bytes.
    pub proof: Vec<u8>,
}

/// Something that happened in a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub channel: String,
    pub account: String,
    pub what: ChannelWhat,
    /// Where this sits in the channel's order. Absent on the way to the hub — assigning it
    /// is the hub's entire job here — and always present on the way back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Sequence>,
}

/// The kinds of thing a channel event can be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum ChannelWhat {
    Talk { text: String },
    Emote { text: String },
    Joined,
    Left,
    /// Operator given or taken away.
    Operator { granted: bool },
    Kicked { by: String },
}

/// A game someone is hosting, as the list shows it.
///
/// The address is the host's, not the server's: Battle.net joins are peer to peer, which is
/// the reason a game on one node is joinable from another at all (FEDERATION.md §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameAd {
    pub name: String,
    pub product: String,
    pub host: String,
    pub statstring: String,
    /// Whether the hub could reach the host. `None` means it has not tried yet. A game that
    /// cannot be reached is still listed, but can be shown last instead of wasting the time
    /// of everyone who tries to join it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
}

impl Message {
    /// Whether this is the first thing a node may say. Anything else on a fresh connection
    /// is a protocol error, not something to answer.
    #[must_use]
    pub fn is_greeting(&self) -> bool {
        matches!(self, Message::Hello(_))
    }

    /// The cookie a reply must carry back, when this message expects one.
    #[must_use]
    pub fn cookie(&self) -> Option<Cookie> {
        match self {
            Message::Ping { cookie }
            | Message::Pong { cookie }
            | Message::AuthVerdict { cookie, .. }
            | Message::ChannelJoin { cookie, .. }
            | Message::GameListQuery { cookie, .. }
            | Message::GameList { cookie, .. } => Some(*cookie),
            Message::AuthVerify(a) => Some(a.cookie),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_can_be_matched_to_what_asked_for_it() {
        let asked = Message::GameListQuery { cookie: Cookie(4), product: Some("W2BN".into()) };
        let answered = Message::GameList { cookie: Cookie(4), games: Vec::new() };
        assert_eq!(asked.cookie(), answered.cookie());
        // Presence is told, not asked: nothing comes back, so there is nothing to match.
        assert_eq!(Message::SessionClosed { account: "Zealot".into() }.cookie(), None);
    }

    #[test]
    fn only_a_hello_opens_a_connection() {
        let hello = Message::Hello(Hello { protocol: PROTOCOL_VERSION, software: "Command Center".into(), version: "0.2.3".into(), capabilities: Vec::new() });
        assert!(hello.is_greeting());
        assert!(!Message::Ping { cookie: Cookie(1) }.is_greeting(), "a ping is not a greeting");
    }

    #[test]
    fn a_channel_event_carries_no_order_until_the_hub_gives_it_one() {
        let said = ChannelEvent { channel: "Blizzard Tech Support".into(), account: "Zealot".into(), what: ChannelWhat::Talk { text: "hello".into() }, sequence: None };
        let json = serde_json::to_string(&said).unwrap();
        assert!(!json.contains("sequence"), "nothing claims an order on the way to the hub");
        let placed = ChannelEvent { sequence: Some(Sequence(41)), ..said };
        let back: ChannelEvent = serde_json::from_str(&serde_json::to_string(&placed).unwrap()).unwrap();
        assert_eq!(back.sequence, Some(Sequence(41)));
    }

    #[test]
    fn no_message_lets_a_node_say_which_node_it_is() {
        // The hub takes that from the connection. If this ever fails, someone has added a
        // field that a dishonest node could fill in with a neighbour's name.
        let every = [
            Message::Hello(Hello { protocol: 1, software: "s".into(), version: "v".into(), capabilities: vec![] }),
            Message::SessionOpened { account: "Zealot".into(), product: "STAR".into() },
            Message::ChannelEvent(ChannelEvent { channel: "c".into(), account: "a".into(), what: ChannelWhat::Joined, sequence: None }),
            Message::GameAdvertise(GameAd { name: "g".into(), product: "STAR".into(), host: "1.2.3.4:6112".into(), statstring: "".into(), reachable: None }),
        ];
        for message in every {
            let json = serde_json::to_string(&message).unwrap();
            assert!(!json.contains("\"node\""), "a node's identity is never a payload field: {json}");
        }
    }
}
