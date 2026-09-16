//! Framing: how one message is marked off from the next on the wire.
//!
//! A four-byte big-endian length, then that many bytes of JSON. That is the whole format.
//!
//! JSON, on a wire protocol, in a codebase that decodes Battle.net packets bit by bit, is a
//! deliberate choice and worth defending. The link carries tens of connections and a few
//! hundred messages a second at the busiest — nowhere near where encoding size matters — and
//! in exchange we get two things that do matter here. A federation is run by strangers on
//! machines we cannot attach a debugger to, and when a node misbehaves the operator can read
//! the bytes. And a field added in a later version is ignored by an older peer instead of
//! shifting every field after it, which is exactly the failure this project just spent an
//! evening diagnosing on a tracker list.

use crate::Message;

/// The largest message we will send or accept. Generous for a roster snapshot of a busy
/// channel, small enough that a confused or hostile peer cannot make us allocate freely.
pub const MAX_FRAME: usize = 1 << 20;

/// What can go wrong reading a frame.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    /// The length says more than [`MAX_FRAME`]. The connection should be dropped: a peer
    /// this far outside the protocol will not recover by being sent an error.
    #[error("frame of {0} bytes is over the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    /// The bytes inside the frame are not a message we understand.
    #[error("frame is not a message: {0}")]
    Malformed(String),
}

/// A message as it goes on the wire, length and all.
///
/// # Panics
///
/// Never in practice: the message set is plain data that always serialises. A message that
/// somehow did not would be a bug in this crate, not in the peer.
#[must_use]
pub fn encode(message: &Message) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("the message set always serialises");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Take one message off the front of `buf`, if a whole one has arrived.
///
/// Returns the message and how many bytes it used, so the caller can drop exactly that much
/// and call again. `Ok(None)` means the rest has not arrived yet — that is ordinary on a
/// stream, not an error.
pub fn decode(buf: &[u8]) -> Result<Option<(Message, usize)>, FrameError> {
    let Some(header) = buf.get(..4) else { return Ok(None) };
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let Some(body) = buf.get(4..4 + len) else { return Ok(None) };
    let message = serde_json::from_slice(body).map_err(|e| FrameError::Malformed(e.to_string()))?;
    Ok(Some((message, 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cookie, Hello};

    fn hello() -> Message {
        Message::Hello(Hello { protocol: crate::PROTOCOL_VERSION, software: "Command Center".into(), version: "0.2.3".into(), capabilities: vec!["chat".into()] })
    }

    #[test]
    fn a_message_survives_the_wire() {
        let bytes = encode(&hello());
        let (back, used) = decode(&bytes).expect("decodes").expect("a whole frame");
        assert_eq!(back, hello());
        assert_eq!(used, bytes.len(), "the caller is told to drop exactly this frame");
    }

    #[test]
    fn a_frame_that_has_not_all_arrived_is_not_an_error() {
        let bytes = encode(&hello());
        for cut in [0, 1, 3, 4, bytes.len() - 1] {
            assert_eq!(decode(&bytes[..cut]), Ok(None), "{cut} bytes in, we simply wait");
        }
    }

    #[test]
    fn messages_are_taken_one_at_a_time() {
        let mut stream = encode(&hello());
        stream.extend_from_slice(&encode(&Message::Ping { cookie: Cookie(7) }));
        let (first, used) = decode(&stream).unwrap().unwrap();
        assert_eq!(first, hello());
        let (second, _) = decode(&stream[used..]).unwrap().unwrap();
        assert_eq!(second, Message::Ping { cookie: Cookie(7) });
    }

    #[test]
    fn a_hostile_length_is_refused_before_anything_is_allocated() {
        let mut bytes = (MAX_FRAME as u32 + 1).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        assert_eq!(decode(&bytes), Err(FrameError::TooLarge(MAX_FRAME + 1)));
    }

    #[test]
    fn a_field_added_later_does_not_break_an_older_peer() {
        // The reason this protocol is self-describing: a peer that knows more than we do
        // must not shift every field that follows.
        let newer = br#"{"type":"ping","cookie":9,"measured_from":"a later version"}"#;
        let mut bytes = u32::try_from(newer.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(newer);
        let (message, _) = decode(&bytes).expect("an unknown field is ignored").unwrap();
        assert_eq!(message, Message::Ping { cookie: Cookie(9) });
    }

    #[test]
    fn nonsense_inside_a_frame_is_reported_not_guessed_at() {
        let mut bytes = 5u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"hello");
        assert!(matches!(decode(&bytes), Err(FrameError::Malformed(_))));
    }
}
