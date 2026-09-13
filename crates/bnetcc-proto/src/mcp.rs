//! MCP framing — the Diablo II realm protocol.
//!
//! **The framing is not BNCS**, and confusing the two is the single most common bug in
//! Diablo II realm implementations:
//!
//! ```text
//! BNCS   FF | id:u8 | len:u16le      magic first, length third
//! MCP         len:u16le | id:u8      NO magic byte, length FIRST
//! ```
//!
//! Both lengths include their own header. Both connections open with protocol byte
//! `0x01`, which is part of why the two get mixed up: the realm connection looks exactly
//! like a game connection until the first frame arrives.
//!
//! The realm runs **inside `bnetccd`** rather than as a separate daemon — see
//! `docs/ARCHITECTURE.md` §11 — so this is a gateway module's codec, not a wire protocol
//! between our own processes.

use crate::buf::{Reader, RecvBuf};
use crate::error::{ProtoError, Result};

/// The MCP header size: `u16` length plus `u8` id.
pub const HEADER_LEN: usize = 3;

/// Default ceiling on one MCP frame.
///
/// Character lists are the largest thing this protocol carries — `MCP_CHARLIST2` returns
/// a name and a 34-byte statstring per character — so a few kilobytes is generous.
pub const DEFAULT_MAX_FRAME: usize = 8192;

/// One decoded MCP message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Message identifier (`MCP_*`).
    pub id: u8,
    /// Body, excluding the 3-byte header.
    pub body: Vec<u8>,
}

impl Frame {
    /// Build a frame.
    #[must_use]
    pub const fn new(id: u8, body: Vec<u8>) -> Self {
        Self { id, body }
    }

    /// A frame with an empty body.
    #[must_use]
    pub const fn empty(id: u8) -> Self {
        Self {
            id,
            body: Vec::new(),
        }
    }

    /// A checked reader over the body.
    #[must_use]
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(&self.body)
    }

    /// Total wire size including the header.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        HEADER_LEN + self.body.len()
    }
}

/// Decode one MCP frame, or `Ok(None)` if more bytes are needed.
///
/// As with BNCS, callers must **loop until this returns `None`** after every socket read.
///
/// # Errors
///
/// [`ProtoError::ShortFrame`] or [`ProtoError::FrameTooLarge`] for a frame that can never
/// be valid.
pub fn decode_frame(src: &mut RecvBuf, max_frame: usize) -> Result<Option<Frame>> {
    let buf = src.as_slice();
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    // Length first. There is no magic byte to sanity-check against, which is exactly why
    // the length bound below is the only thing standing between us and a bad allocation.
    let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    let id = buf[2];

    if len < HEADER_LEN {
        return Err(ProtoError::ShortFrame {
            len,
            header: HEADER_LEN,
        });
    }
    if len > max_frame {
        return Err(ProtoError::FrameTooLarge { len, max: max_frame });
    }
    if buf.len() < len {
        return Ok(None);
    }

    let body = buf[HEADER_LEN..len].to_vec();
    src.consume(len);
    Ok(Some(Frame { id, body }))
}

/// Append a frame's wire bytes to `dst`.
///
/// # Errors
///
/// [`ProtoError::FrameTooLarge`] if the body will not fit the `u16` length field.
pub fn encode_frame(frame: &Frame, dst: &mut Vec<u8>) -> Result<()> {
    let len = frame.wire_len();
    if len > u16::MAX as usize {
        return Err(ProtoError::FrameTooLarge {
            len,
            max: u16::MAX as usize,
        });
    }
    dst.reserve(len);
    dst.extend_from_slice(&(len as u16).to_le_bytes());
    dst.push(frame.id);
    dst.extend_from_slice(&frame.body);
    Ok(())
}

/// `MCP_*` message identifiers.
pub mod msg {
    /// Realm handshake. Carries the sixteen `u32`s from `SID_LOGONREALMEX` verbatim.
    pub const STARTUP: u8 = 0x01;
    /// Create a character.
    pub const CHARCREATE: u8 = 0x02;
    /// Create a game.
    pub const CREATEGAME: u8 = 0x03;
    /// Join a game. On success the client drops the MCP connection and dials the game
    /// server directly.
    pub const JOINGAME: u8 = 0x04;
    /// List games.
    pub const GAMELIST: u8 = 0x05;
    /// Query one game.
    pub const GAMEINFO: u8 = 0x06;
    /// Select a character.
    pub const CHARLOGON: u8 = 0x07;
    /// Delete a character.
    pub const CHARDELETE: u8 = 0x0A;
    /// Ladder data.
    pub const REQUESTLADDERDATA: u8 = 0x11;
    /// Realm message of the day.
    pub const MOTD: u8 = 0x12;
    /// Cancel a pending game creation.
    pub const CANCELGAMECREATE: u8 = 0x13;
    /// Join the game creation queue.
    pub const CREATEQUEUE: u8 = 0x14;
    /// Character ranking.
    pub const CHARRANK: u8 = 0x16;
    /// Character list, first generation.
    pub const CHARLIST: u8 = 0x17;
    /// Character upgrade.
    pub const CHARUPGRADE: u8 = 0x18;
    /// Character list, second generation. What modern clients use.
    pub const CHARLIST2: u8 = 0x19;
}

/// `MCP_STARTUP` result codes.
pub mod startup_result {
    /// Accepted.
    pub const OK: u32 = 0x00;
    /// The realm could not identify the logon (unknown or stale `SID_LOGONREALMEX` data).
    /// The client shows "realm unavailable".
    pub const UNAVAILABLE: u32 = 0x0A;
}

/// `MCP_CHARCREATE` result codes.
pub mod char_create_result {
    /// Created.
    pub const OK: u32 = 0x00;
    /// The name is taken.
    pub const NAME_TAKEN: u32 = 0x14;
    /// The name, class or flags are not allowed.
    pub const INVALID: u32 = 0x15;
}

/// `MCP_CHARLOGON` result codes. Anything but these proceeds into the realm.
pub mod char_logon_result {
    /// Selected.
    pub const OK: u32 = 0x00;
    /// No such character. Returns the client to character select with its MCP connection
    /// intact.
    pub const NOT_FOUND: u32 = 0x46;
}

/// `MCP_CHARDELETE` result codes.
pub mod char_delete_result {
    /// Deleted.
    pub const OK: u32 = 0x00;
    /// No such character on this account.
    pub const NOT_FOUND: u32 = 0x49;
}

/// `MCP_CREATEGAME` result codes.
pub mod create_game_result {
    /// Created.
    pub const OK: u32 = 0x00;
    /// "Invalid Game Name".
    pub const INVALID_NAME: u32 = 0x1E;
    /// "Game Already Exists".
    pub const NAME_TAKEN: u32 = 0x1F;
    /// "Server Down" — no game server could take the game.
    pub const SERVERS_DOWN: u32 = 0x20;
}

/// The token that ends an `MCP_GAMELIST` reply. Each game is its own `MCP_GAMELIST`
/// packet; this one, with no name, tells the client the list is complete.
pub const GAMELIST_END_TOKEN: u32 = 0xFFFF_FFFE;

/// `MCP_JOINGAME` result codes.
pub mod join_result {
    /// Success. The client now disconnects from the realm and dials the game server.
    pub const OK: u32 = 0x00;
    /// Wrong password.
    pub const BAD_PASSWORD: u32 = 0x29;
    /// No such game.
    pub const NO_SUCH_GAME: u32 = 0x2A;
    /// Game is full.
    pub const FULL: u32 = 0x2B;
    /// Character does not meet the level requirement.
    pub const LEVEL_REQUIREMENT: u32 = 0x2C;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_of(bytes: &[u8]) -> RecvBuf {
        let mut b = RecvBuf::new();
        b.extend_from_slice(bytes);
        b
    }

    #[test]
    fn the_framing_is_length_first_with_no_magic_byte() {
        // The distinction this whole module exists for. An MCP_STARTUP with no body is
        // `03 00 01`, where the equivalent BNCS frame would be `FF 01 04 00`.
        let mut wire = Vec::new();
        encode_frame(&Frame::empty(msg::STARTUP), &mut wire).unwrap();
        assert_eq!(wire, vec![0x03, 0x00, 0x01]);
        assert_ne!(wire[0], crate::bncs::MAGIC, "MCP has no magic byte");
    }

    #[test]
    fn a_frame_round_trips() {
        let frame = Frame::new(msg::CHARLOGON, b"Zealot\0".to_vec());
        let mut wire = Vec::new();
        encode_frame(&frame, &mut wire).unwrap();
        assert_eq!(wire.len(), HEADER_LEN + 7);
        let mut buf = buf_of(&wire);
        assert_eq!(decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap().unwrap(), frame);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_bncs_frame_fed_to_the_mcp_decoder_does_not_silently_succeed() {
        // `FF 02 04 00` is a valid zero-payload BNCS frame. Read as MCP it claims a
        // length of 0x02FF = 767, so it waits for bytes that will never come rather
        // than producing a bogus frame. Wiring the wrong decoder must not look like it
        // is working.
        let mut buf = buf_of(&[0xFF, 0x02, 0x04, 0x00]);
        assert_eq!(decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap(), None);
        assert_eq!(buf.len(), 4, "the bytes are retained, not consumed");
    }

    #[test]
    fn waits_for_a_partial_frame() {
        let frame = Frame::new(msg::CHARLIST2, vec![1, 2, 3, 4]);
        let mut wire = Vec::new();
        encode_frame(&frame, &mut wire).unwrap();
        for split in 1..wire.len() {
            let mut buf = buf_of(&wire[..split]);
            assert_eq!(
                decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap(),
                None,
                "should wait at {split} of {} bytes",
                wire.len()
            );
        }
    }

    #[test]
    fn rejects_a_length_below_the_header() {
        for len in 0u16..3 {
            let mut buf = RecvBuf::new();
            buf.extend_from_slice(&len.to_le_bytes());
            buf.extend_from_slice(&[msg::STARTUP]);
            assert!(
                matches!(
                    decode_frame(&mut buf, DEFAULT_MAX_FRAME),
                    Err(ProtoError::ShortFrame { header: 3, .. })
                ),
                "length {len} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_oversize_frames() {
        // With no magic byte, the length bound is the only thing preventing a bad
        // allocation from a hostile peer.
        let mut buf = RecvBuf::new();
        buf.extend_from_slice(&40000u16.to_le_bytes());
        buf.extend_from_slice(&[msg::STARTUP]);
        assert!(matches!(
            decode_frame(&mut buf, DEFAULT_MAX_FRAME),
            Err(ProtoError::FrameTooLarge { len: 40000, .. })
        ));
    }

    #[test]
    fn drains_every_frame_from_one_buffer() {
        let mut wire = Vec::new();
        for id in [msg::STARTUP, msg::CHARLIST2, msg::CHARLOGON, msg::MOTD] {
            encode_frame(&Frame::empty(id), &mut wire).unwrap();
        }
        let mut buf = buf_of(&wire);
        let mut ids = Vec::new();
        while let Some(f) = decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap() {
            ids.push(f.id);
        }
        assert_eq!(ids, vec![0x01, 0x19, 0x07, 0x12]);
    }

    #[test]
    fn decoder_never_panics_on_arbitrary_input() {
        let mut seed = 0xD2D2_0001u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..30_000 {
            let n = (next() % 96) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let mut buf = buf_of(&bytes);
            for _ in 0..8 {
                match decode_frame(&mut buf, DEFAULT_MAX_FRAME) {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
}
