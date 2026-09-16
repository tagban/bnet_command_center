//! One federation connection, and the greeting that opens it.
//!
//! Deliberately generic over the stream. The design calls for mTLS, where a node's identity
//! comes from its client certificate rather than from anything it says — but none of the
//! reading, writing or handshaking below cares which it is, so that decision can be made
//! where it belongs (in the hub's setup) and tested here over a pipe. The one thing this
//! layer insists on is that the caller hand it a name for the peer that the peer did not
//! choose: [`accept`] takes the node name as an argument rather than reading it from
//! `Hello`, so there is no path where a node names itself.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{decode, encode, FrameError, Hello, Message, Refusal, Welcome, MAX_FRAME, PROTOCOL_VERSION};

/// What can end a connection.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("the connection closed")]
    Closed,
    #[error("input/output: {0}")]
    Io(#[from] std::io::Error),
    #[error("the peer is outside the protocol: {0}")]
    Protocol(#[from] FrameError),
    /// The peer said something that is legal to write but not allowed here — a node whose
    /// first word is not a greeting, or a hub answering a greeting with a channel event.
    #[error("unexpected {0}")]
    OutOfTurn(&'static str),
    /// The hub declined this node. Carries its reason so an operator sees one.
    #[error("refused by the hub: {0:?}")]
    Refused(Refusal),
}

/// A framed connection to the other end.
pub struct Link<S> {
    stream: S,
    /// Bytes read but not yet a whole message.
    pending: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Link<S> {
    /// Wrap a stream that is already connected (and, in production, already authenticated).
    pub fn new(stream: S) -> Self {
        Self { stream, pending: Vec::new() }
    }

    /// Write one message.
    pub async fn send(&mut self, message: &Message) -> Result<(), LinkError> {
        self.stream.write_all(&encode(message)).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Read the next message, waiting for the rest of it to arrive if need be.
    pub async fn recv(&mut self) -> Result<Message, LinkError> {
        loop {
            if let Some((message, used)) = decode(&self.pending)? {
                self.pending.drain(..used);
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(LinkError::Closed);
            }
            // A peer could otherwise dribble bytes forever without completing a frame.
            if self.pending.len() + n > MAX_FRAME + 4 {
                return Err(LinkError::Protocol(FrameError::TooLarge(self.pending.len() + n)));
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }

    /// Open the connection from the node's side: greet, and take the hub's terms.
    ///
    /// A refusal comes back as an error carrying the hub's reason, because an operator whose
    /// node will not join needs to be told why and not left reading logs.
    pub async fn greet(&mut self, hello: Hello) -> Result<Welcome, LinkError> {
        self.send(&Message::Hello(hello)).await?;
        match self.recv().await? {
            Message::Welcome(welcome) => Ok(welcome),
            Message::Refused(why) => Err(LinkError::Refused(why)),
            _ => Err(LinkError::OutOfTurn("answer to a greeting")),
        }
    }

    /// Open the connection from the hub's side.
    ///
    /// `node` is who the hub has decided this is, from the authenticated connection. It is an
    /// argument and not a field of the greeting on purpose: see the module note.
    pub async fn accept(&mut self, node: &str, official_channels: Vec<String>, heartbeat_secs: u64) -> Result<Hello, LinkError> {
        let Message::Hello(hello) = self.recv().await? else {
            return Err(LinkError::OutOfTurn("first message; a node greets before anything else"));
        };
        if hello.protocol > PROTOCOL_VERSION {
            // Speak down to what we know rather than turning a newer node away: every message
            // it sends that we understand still works, and ones we do not are ignored.
            self.send(&Message::Welcome(Welcome {
                protocol: PROTOCOL_VERSION,
                node: node.to_string(),
                official_channels,
                heartbeat_secs,
            }))
            .await?;
            return Ok(hello);
        }
        if hello.protocol < 1 {
            let refusal = Refusal::Protocol { hub_speaks: PROTOCOL_VERSION };
            self.send(&Message::Refused(refusal.clone())).await?;
            return Err(LinkError::Refused(refusal));
        }
        self.send(&Message::Welcome(Welcome {
            protocol: hello.protocol.min(PROTOCOL_VERSION),
            node: node.to_string(),
            official_channels,
            heartbeat_secs,
        }))
        .await?;
        Ok(hello)
    }

    /// Turn a node away with a reason, then let the connection close.
    pub async fn refuse(&mut self, why: Refusal) -> Result<(), LinkError> {
        self.send(&Message::Refused(why)).await
    }
}

/// How long to wait before dialling the hub again, given how many tries have failed.
///
/// One second doubling to a minute, as FEDERATION.md §7 specifies. The caller adds jitter;
/// without it, every node that was connected to a hub when it restarted comes back at the
/// same instant.
#[must_use]
pub fn retry_delay_secs(attempt: u32) -> u64 {
    const CEILING: u64 = 60;
    1u64.checked_shl(attempt.min(6)).unwrap_or(CEILING).min(CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cookie;

    fn hello(protocol: u32) -> Hello {
        Hello { protocol, software: "Command Center".into(), version: "0.2.3".into(), capabilities: Vec::new() }
    }

    #[tokio::test]
    async fn a_node_greets_and_is_let_in() {
        let (node_side, hub_side) = tokio::io::duplex(4096);
        let hub = tokio::spawn(async move {
            let mut link = Link::new(hub_side);
            // The hub knows who this is before a word is exchanged.
            let greeting = link.accept("node-eu", vec!["Blizzard Tech Support".into()], 30).await.unwrap();
            link.send(&Message::Ping { cookie: Cookie(1) }).await.unwrap();
            greeting
        });
        let mut link = Link::new(node_side);
        let welcome = link.greet(hello(PROTOCOL_VERSION)).await.expect("let in");
        assert_eq!(welcome.node, "node-eu", "the hub says who we are; we did not");
        assert_eq!(welcome.official_channels, ["Blizzard Tech Support"]);
        assert_eq!(link.recv().await.unwrap(), Message::Ping { cookie: Cookie(1) });
        assert_eq!(hub.await.unwrap().software, "Command Center");
    }

    #[tokio::test]
    async fn a_node_that_is_turned_away_is_told_why() {
        let (node_side, hub_side) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut link = Link::new(hub_side);
            let _ = link.recv().await;
            let _ = link.refuse(Refusal::NotWelcome { reason: "suspended".into() }).await;
        });
        let mut link = Link::new(node_side);
        let refused = link.greet(hello(PROTOCOL_VERSION)).await.expect_err("turned away");
        match refused {
            LinkError::Refused(Refusal::NotWelcome { reason }) => assert_eq!(reason, "suspended"),
            other => panic!("wanted a reason, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_node_must_greet_before_anything_else() {
        let (node_side, hub_side) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut link = Link::new(node_side);
            let _ = link.send(&Message::SessionOpened { account: "Zealot".into(), product: "STAR".into() }).await;
        });
        let mut link = Link::new(hub_side);
        assert!(matches!(link.accept("node-eu", Vec::new(), 30).await, Err(LinkError::OutOfTurn(_))));
    }

    #[tokio::test]
    async fn a_newer_node_is_spoken_to_in_the_version_we_know() {
        let (node_side, hub_side) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut link = Link::new(hub_side);
            let _ = link.accept("node-eu", Vec::new(), 30).await;
        });
        let mut link = Link::new(node_side);
        let welcome = link.greet(hello(PROTOCOL_VERSION + 5)).await.expect("not turned away for being newer");
        assert_eq!(welcome.protocol, PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn a_message_split_across_reads_still_arrives() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let message = Message::ChannelEvent(crate::ChannelEvent {
            channel: "Blizzard Tech Support".into(),
            account: "Zealot".into(),
            what: crate::ChannelWhat::Talk { text: "x".repeat(300) },
            sequence: Some(crate::Sequence(9)),
        });
        let sent = message.clone();
        tokio::spawn(async move {
            // A small pipe forces the frame across several reads.
            let _ = writer.write_all(&encode(&sent)).await;
        });
        let mut link = Link::new(reader);
        assert_eq!(link.recv().await.unwrap(), message);
    }

    #[test]
    fn dialling_backs_off_and_then_stops_growing() {
        assert_eq!(retry_delay_secs(0), 1);
        assert_eq!(retry_delay_secs(3), 8);
        assert_eq!(retry_delay_secs(6), 60, "a minute is the ceiling");
        assert_eq!(retry_delay_secs(1000), 60, "and it stays there");
    }
}
