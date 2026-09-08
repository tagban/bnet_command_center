//! BNCS framing — the `0xFF | id | len:u16le` protocol spoken on TCP 6112.
//!
//! `len` **includes** the four-byte header, so a zero-payload message is `FF 02 04 00`.

use crate::buf::{Reader, RecvBuf};
use crate::error::{ProtoError, Result};

/// The BNCS header size in bytes.
pub const HEADER_LEN: usize = 4;

/// The BNCS magic byte.
pub const MAGIC: u8 = 0xFF;

/// Default ceiling on a single frame.
///
/// The length field is a `u16`, so 65535 is the protocol maximum. We cap far lower by
/// default: no legitimate classic-client packet approaches it, and a smaller ceiling
/// bounds the memory an unauthenticated peer can make us buffer.
pub const DEFAULT_MAX_FRAME: usize = 8192;

/// One decoded BNCS message: an identifier and its body, header stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Message identifier (`SID_*`).
    pub id: u8,
    /// Message body, excluding the 4-byte header.
    pub body: Vec<u8>,
}

impl Frame {
    /// Build a frame from an id and body.
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

    /// A checked reader over this frame's body.
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

/// Attempt to decode one frame from `src`.
///
/// Returns `Ok(None)` when more bytes are needed. On `Ok(Some(_))` the frame's bytes
/// have been consumed from `src`.
///
/// Callers must **loop until this returns `None`** after every socket read. Processing
/// one frame per readiness event is PvPGN's `sd_tcpinput()` mistake: it has no loop, so
/// it handles at most one packet per epoll wakeup regardless of how much data is already
/// buffered in the kernel.
///
/// # Errors
///
/// [`ProtoError::BadMagic`], [`ProtoError::ShortFrame`] or [`ProtoError::FrameTooLarge`]
/// for a frame that can never be valid. All three are fatal to the connection and to
/// nothing else.
pub fn decode_frame(src: &mut RecvBuf, max_frame: usize) -> Result<Option<Frame>> {
    let buf = src.as_slice();
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    if buf[0] != MAGIC {
        return Err(ProtoError::BadMagic(buf[0]));
    }
    let id = buf[1];
    let len = u16::from_le_bytes([buf[2], buf[3]]) as usize;

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
    dst.push(MAGIC);
    dst.push(frame.id);
    dst.extend_from_slice(&(len as u16).to_le_bytes());
    dst.extend_from_slice(&frame.body);
    Ok(())
}

/// The first byte a client sends on a fresh TCP connection to 6112.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolSelector {
    /// `0x01` — BNCS binary. Also used for the MCP/realm connection.
    Game,
    /// `0x02` — BNFTP file transfer.
    Bnftp,
    /// `0x03`, `0x43`, `0x63` — telnet/chat gateway.
    Chat,
}

impl ProtocolSelector {
    /// Classify a selector byte.
    ///
    /// Unknown selectors return `None`; the caller must close without responding.
    /// BNETDocs documents `0x04` (MCP→BNCS) and Atlas uses `0x80` for its unimplemented
    /// IPC draft, but neither is something a *client* should ever send, so accepting
    /// them on a client socket would be a mistake.
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Game),
            0x02 => Some(Self::Bnftp),
            0x03 | 0x43 | 0x63 => Some(Self::Chat),
            _ => None,
        }
    }
}

/// `SID_*` message identifiers.
///
/// Only those Cairn handles or deliberately ignores are named. The full index is at
/// <https://bnetdocs.org/packet/index>.
pub mod sid {
    /// Client stops advertising a game. Zero payload.
    ///
    /// Every `Battle.snp` client sends this on logoff even when not in a game, and
    /// StarCraft 1.16.1 may send it *before* login completes. Treat as a no-op, never
    /// as a protocol violation.
    pub const STOPADV: u8 = 0x02;
    /// Client requests the game list.
    pub const GETADVLISTEX: u8 = 0x09;
    /// Client enters chat.
    pub const ENTERCHAT: u8 = 0x0A;
    /// Client requests the channel list.
    pub const GETCHANNELLIST: u8 = 0x0B;
    /// Client joins a channel.
    pub const JOINCHANNEL: u8 = 0x0C;
    /// Client sends chat text or a slash command.
    pub const CHATCOMMAND: u8 = 0x0E;
    /// Server emits a chat event.
    pub const CHATEVENT: u8 = 0x0F;
    /// Server warns of flood detection before disconnecting.
    pub const FLOODDETECTED: u8 = 0x13;
    /// Client advertises a game.
    pub const STARTADVEX3: u8 = 0x1C;
    /// Client left a game.
    pub const LEAVEGAME: u8 = 0x1F;
    /// Client joined a game.
    pub const NOTIFYJOIN: u8 = 0x22;
    /// Keepalive, either direction.
    pub const PING: u8 = 0x25;
    /// Legacy logon (Diablo I, Warcraft II BNE, shareware StarCraft).
    pub const LOGONRESPONSE: u8 = 0x29;
    /// Account creation, second generation.
    pub const CREATEACCOUNT2: u8 = 0x3D;
    /// Modern-status logon used by StarCraft, Brood War and Diablo II.
    pub const LOGONRESPONSE2: u8 = 0x3A;
    /// Realm logon challenge (Diablo II closed).
    pub const LOGONREALMEX: u8 = 0x3E;
    /// Realm list query (Diablo II closed).
    pub const QUERYREALMS2: u8 = 0x40;
    /// WarCraft III profile, ladder and icon multiplexer.
    pub const WARCRAFTGENERAL: u8 = 0x44;
    /// WarCraft III announces its game hosting port.
    pub const NETGAMEPORT: u8 = 0x45;
    /// Version and platform negotiation.
    pub const AUTH_INFO: u8 = 0x50;
    /// Version check and CD-key verdict.
    pub const AUTH_CHECK: u8 = 0x51;
    /// SRP account creation (WarCraft III).
    pub const AUTH_ACCOUNTCREATE: u8 = 0x52;
    /// SRP logon step 1 (WarCraft III).
    pub const AUTH_ACCOUNTLOGON: u8 = 0x53;
    /// SRP logon step 2 (WarCraft III).
    pub const AUTH_ACCOUNTLOGONPROOF: u8 = 0x54;
}

/// `SID_LOGONRESPONSE2` (0x3A) server status codes.
pub mod logon_status {
    /// Logon succeeded.
    pub const SUCCESS: u32 = 0x00;
    /// No such account.
    pub const NO_SUCH_ACCOUNT: u32 = 0x01;
    /// Wrong password.
    pub const WRONG_PASSWORD: u32 = 0x02;
    /// Account data corrupted (Diablo II only).
    pub const ACCOUNT_CORRUPTED: u32 = 0x03;
    /// Account closed; a reason string follows.
    pub const ACCOUNT_CLOSED: u32 = 0x06;
}

/// `SID_AUTH_CHECK` (0x51) server result codes.
///
/// **Unknown result codes are treated as success by the client**, so never invent one.
pub mod auth_check_status {
    /// Passed the version and key challenge.
    pub const PASSED: u32 = 0x000;
    /// Old game version; the additional-information string is a patch MPQ filename.
    pub const OLD_VERSION: u32 = 0x100;
    /// Invalid version.
    pub const INVALID_VERSION: u32 = 0x101;
    /// Game version must be downgraded; additional information is a patch filename.
    pub const MUST_DOWNGRADE: u32 = 0x102;
    /// Invalid CD key.
    pub const INVALID_KEY: u32 = 0x200;
    /// **CD key is already in use by a live session.**
    ///
    /// The additional-information string carries the holder's username. This is the
    /// response that makes a bot fleet cost one key per bot — see
    /// `cairn_core::limits::KeyRegistry`.
    pub const KEY_IN_USE: u32 = 0x201;
    /// Banned CD key.
    pub const KEY_BANNED: u32 = 0x202;
    /// The key is for a different product.
    pub const WRONG_PRODUCT: u32 = 0x203;
    /// OR this into a `0x2xx` result to indicate the **second** CD key rather than the
    /// first (Warcraft III sends two).
    pub const SECOND_KEY: u32 = 0x010;
}

/// `SID_STARTADVEX3` (0x1C) server status codes.
pub mod advertise_status {
    /// Game created.
    pub const OK: u32 = 0x00;
    /// A game by that name already exists.
    pub const NAME_TAKEN: u32 = 0x01;
    /// The selected game type is currently unavailable.
    ///
    /// This is what warnet mode returns for every hosting attempt. Using the game's own
    /// documented code produces a real in-client message; silently dropping the packet
    /// makes the client hang and the user blame the server.
    pub const TYPE_UNAVAILABLE: u32 = 0x02;
    /// An error occurred while creating the game.
    pub const ERROR: u32 = 0x03;
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
    fn decodes_a_zero_payload_frame() {
        let mut buf = buf_of(&[0xFF, 0x02, 0x04, 0x00]);
        let f = decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap().unwrap();
        assert_eq!(f.id, sid::STOPADV);
        assert!(f.body.is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn waits_for_a_partial_header() {
        let mut buf = buf_of(&[0xFF, 0x02]);
        assert_eq!(decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap(), None);
        assert_eq!(buf.len(), 2, "partial data must be retained");
    }

    #[test]
    fn waits_for_a_partial_body() {
        let mut buf = buf_of(&[0xFF, 0x0E, 0x08, 0x00, b'h', b'i']);
        assert_eq!(decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap(), None);
        assert_eq!(buf.len(), 6);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = buf_of(&[0x00, 0x02, 0x04, 0x00]);
        assert!(matches!(
            decode_frame(&mut buf, DEFAULT_MAX_FRAME),
            Err(ProtoError::BadMagic(0x00))
        ));
    }

    #[test]
    fn rejects_length_below_the_header() {
        // A length under 4 would underflow the body slice. This is precisely the shape
        // of bug behind repeated "crash on malformed packet" fixes in prior servers.
        for len in 0u16..4 {
            let mut buf = RecvBuf::new();
            buf.extend_from_slice(&[0xFF, 0x0E]);
            buf.extend_from_slice(&len.to_le_bytes());
            assert!(
                matches!(
                    decode_frame(&mut buf, DEFAULT_MAX_FRAME),
                    Err(ProtoError::ShortFrame { header: 4, .. })
                ),
                "length {len} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_oversize_frames() {
        let mut buf = buf_of(&[0xFF, 0x0E, 0x00, 0x40]); // 0x4000 = 16384
        assert!(matches!(
            decode_frame(&mut buf, DEFAULT_MAX_FRAME),
            Err(ProtoError::FrameTooLarge { len: 16384, .. })
        ));
    }

    #[test]
    fn drains_every_frame_from_one_buffer() {
        // The property PvPGN lacks: N frames arriving together must cost one drain
        // loop, not N readiness events.
        let mut wire = Vec::new();
        for id in [0x02u8, 0x25, 0x0B, 0x0E] {
            encode_frame(&Frame::empty(id), &mut wire).unwrap();
        }
        let mut buf = buf_of(&wire);
        let mut ids = Vec::new();
        while let Some(f) = decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap() {
            ids.push(f.id);
        }
        assert_eq!(ids, vec![0x02, 0x25, 0x0B, 0x0E]);
        assert!(buf.is_empty());
    }

    #[test]
    fn handles_a_frame_split_across_reads() {
        let frame = Frame::new(sid::CHATCOMMAND, b"hello there\0".to_vec());
        let mut wire = Vec::new();
        encode_frame(&frame, &mut wire).unwrap();

        let mut buf = RecvBuf::new();
        for chunk in wire.chunks(3) {
            buf.extend_from_slice(chunk);
        }
        assert_eq!(
            decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap().unwrap(),
            frame
        );
    }

    #[test]
    fn roundtrips_a_body() {
        let frame = Frame::new(sid::CHATEVENT, b"hello world".to_vec());
        let mut wire = Vec::new();
        encode_frame(&frame, &mut wire).unwrap();
        assert_eq!(wire.len(), HEADER_LEN + 11);
        assert_eq!(&wire[..4], &[0xFF, 0x0F, 15, 0], "length includes the header");
        let mut buf = buf_of(&wire);
        assert_eq!(
            decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap().unwrap(),
            frame
        );
    }

    #[test]
    fn protocol_selector_classifies_known_bytes() {
        assert_eq!(ProtocolSelector::from_byte(0x01), Some(ProtocolSelector::Game));
        assert_eq!(ProtocolSelector::from_byte(0x02), Some(ProtocolSelector::Bnftp));
        for b in [0x03, 0x43, 0x63] {
            assert_eq!(ProtocolSelector::from_byte(b), Some(ProtocolSelector::Chat));
        }
        // Interserver selectors must not be accepted from a client socket.
        assert_eq!(ProtocolSelector::from_byte(0x04), None);
        assert_eq!(ProtocolSelector::from_byte(0x80), None);
        assert_eq!(ProtocolSelector::from_byte(0x00), None);
    }

    /// A cheap in-tree stand-in for `cargo fuzz`: no byte sequence may panic the
    /// decoder, and every call must terminate.
    #[test]
    fn decoder_never_panics_on_arbitrary_input() {
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..50_000 {
            let n = (next() % 96) as usize;
            let mut buf = RecvBuf::with_capacity(n);
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            buf.extend_from_slice(&bytes);
            // Drain until it stops making progress; must not loop forever or panic.
            for _ in 0..8 {
                match decode_frame(&mut buf, DEFAULT_MAX_FRAME) {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }

    /// Frames whose magic and length are valid but whose bodies are garbage must still
    /// decode cleanly at the framing layer — misparsing is the *handler's* job to reject.
    #[test]
    fn well_framed_garbage_decodes_without_panic() {
        let mut seed = 0xC0FF_EE00u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let body_len = (next() % 300) as usize;
            let body: Vec<u8> = (0..body_len).map(|_| (next() & 0xFF) as u8).collect();
            let frame = Frame::new((next() & 0xFF) as u8, body);
            let mut wire = Vec::new();
            encode_frame(&frame, &mut wire).unwrap();
            let mut buf = RecvBuf::new();
            buf.extend_from_slice(&wire);
            let decoded = decode_frame(&mut buf, DEFAULT_MAX_FRAME).unwrap().unwrap();
            assert_eq!(decoded, frame);
            // And every reader operation on that garbage body must return, not panic.
            let mut r = decoded.reader();
            let _ = r.u32();
            let _ = r.cstr(255);
            let _ = r.u64();
        }
    }
}
