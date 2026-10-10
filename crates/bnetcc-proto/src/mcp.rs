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

use crate::buf::{Reader, RecvBuf, Writer};
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
    /// A dead hardcore character cannot create games (BNETDocs).
    pub const DEAD_HARDCORE: u32 = 0x6E;
}

/// The status that ends an `MCP_GAMELIST` reply ([`game_status::END`]). Each game is its own
/// `MCP_GAMELIST` packet; this one, with no name, tells the client the list is complete.
pub const GAMELIST_END_TOKEN: u32 = game_status::END;

/// The `i32` status of an `MCP_GAMELIST` or `MCP_GAMEINFO` reply, the one field the client reads
/// before deciding what the rest is (1.14d `Game.exe`: the list handler `0x0044B2D0`, the details
/// handler `0x0044ACA0`). Any other value is a game, and the client does nothing more with it;
/// Command Center sends the game's flags ([`game_flags`]).
pub mod game_status {
    /// "No information": no such game. The list handler drops the list's panel on it, so a list
    /// never uses it.
    pub const NONE: u32 = 0xFFFF_FFFF;
    /// The end of a game list, which the client then shows.
    pub const END: u32 = 0xFFFF_FFFE;
}

/// A game's flags, as in `MCP_CREATEGAME` and a listed game's status: `0x04`, the difficulty in
/// bits 12–13 (as `MCP_CREATEGAME` sends it, 1.14d `0x004456D0`), hardcore `0x800` (what the
/// client puts in `MCP_GAMELIST` for a hardcore character, `0x00445020`), expansion `0x100000`
/// (the client's own game record, `0x0044585F`) and ladder `0x200000` (the realm convention behind
/// BNETDocs' `0x00300004` for an open expansion ladder game; the client does not read it).
#[must_use]
pub fn game_flags(difficulty: u8, hardcore: bool, expansion: bool, ladder: bool) -> u32 {
    let mut flags = 0x04 | (u32::from(difficulty.min(2)) << 12);
    if hardcore {
        flags |= 0x800;
    }
    if expansion {
        flags |= 0x10_0000;
    }
    if ladder {
        flags |= 0x20_0000;
    }
    flags
}

/// Longest game name the client keeps (`MCP_CREATEGAME` allows 15; the list and details copy it
/// into 16-byte buffers).
pub const GAME_NAME_MAX: usize = 15;
/// Longest game description the client keeps (31 at creation; the details copy it into a 32-byte
/// buffer with no bound, so a longer one would overrun into the classes).
pub const GAME_DESCRIPTION_MAX: usize = 31;
/// Character names in `MCP_GAMEINFO` go into 16-byte slots, again with no bound.
const CHARACTER_NAME_MAX: usize = 15;
/// `MCP_GAMEINFO` holds sixteen characters.
pub const GAMEINFO_CHARACTERS: usize = 16;

fn clipped(s: &[u8], max: usize) -> &[u8] {
    let s = s.split(|&b| b == 0).next().unwrap_or_default();
    &s[..s.len().min(max)]
}

/// One game of an `MCP_GAMELIST` reply: `u16 request id, u32 token, u8 players, u32 status,
/// cstr name, cstr description` (1.14d `0x0044B2D0`). The packet must end with the description:
/// the client adds the game only when nothing follows it, and otherwise just shows the list so far.
/// It keeps one entry per token (the low 16 bits), so every game needs its own.
#[must_use]
pub fn gamelist_entry(request_id: u16, token: u32, players: u8, status: u32, name: &[u8], description: &[u8]) -> Vec<u8> {
    let (name, description) = (clipped(name, GAME_NAME_MAX), clipped(description, GAME_DESCRIPTION_MAX));
    let mut w = Writer::with_capacity(13 + name.len() + description.len());
    w.u16(request_id).u32(token).u8(players).u32(status).cstr(name).cstr(description);
    w.finish()
}

/// The packet that ends an `MCP_GAMELIST` reply: `u16 request id, u32 0, u8 0, u32`
/// [`game_status::END`] and nothing more — the client stops reading at the status.
#[must_use]
pub fn gamelist_end(request_id: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(11);
    w.u16(request_id).u32(0).u8(0).u32(game_status::END);
    w.finish()
}

/// One character in an `MCP_GAMEINFO` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameInfoCharacter<'a> {
    /// Class, 0 Amazon to 6 Assassin (the client writes "Level n Paladin" and so on).
    pub class: u8,
    /// Character level.
    pub level: u8,
    /// Name.
    pub name: &'a [u8],
}

/// What an `MCP_GAMEINFO` reply describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameInfo<'a> {
    /// The game's flags ([`game_flags`]).
    pub status: u32,
    /// Seconds since it was created ("Elapsed Time: h:mm:ss").
    pub uptime_secs: u32,
    /// The creator's level, the middle of the level range.
    pub creator_level: u8,
    /// The level difference allowed: 0 shows "Level n", 1–99 "Level n to m", and 100 or more
    /// (`0xFF` for none) no line at all.
    pub level_difference: u8,
    /// The player limit; 1–7 show "Up to n Players", 0 or 8 no line.
    pub max_players: u8,
    /// The description, shown first when not empty.
    pub description: &'a [u8],
    /// The characters in it.
    pub characters: &'a [GameInfoCharacter<'a>],
}

/// An `MCP_GAMEINFO` reply (1.14d `0x0044ACA0`, shown by `0x00442EB0`): `u16 request id,
/// u32 status, u32 uptime, u8 creator level, u8 level difference, u8 max players, u8 characters,
/// u8[16] classes, u8[16] levels, cstr description`, then a `cstr` name per character.
#[must_use]
pub fn gameinfo_reply(request_id: u16, info: &GameInfo<'_>) -> Vec<u8> {
    let characters = &info.characters[..info.characters.len().min(GAMEINFO_CHARACTERS)];
    let mut classes = [0u8; GAMEINFO_CHARACTERS];
    let mut levels = [0u8; GAMEINFO_CHARACTERS];
    for (i, c) in characters.iter().enumerate() {
        classes[i] = c.class;
        levels[i] = c.level;
    }
    let description = clipped(info.description, GAME_DESCRIPTION_MAX);
    let mut w = Writer::with_capacity(47 + description.len() + characters.len() * 16);
    w.u16(request_id)
        .u32(info.status)
        .u32(info.uptime_secs)
        .u8(info.creator_level)
        .u8(info.level_difference)
        .u8(info.max_players)
        .u8(characters.len() as u8)
        .bytes(&classes)
        .bytes(&levels)
        .cstr(description);
    for c in characters {
        w.cstr(clipped(c.name, CHARACTER_NAME_MAX));
    }
    w.finish()
}

/// The `MCP_GAMEINFO` reply for a game that is gone: status [`game_status::NONE`] and the fields
/// the client reads before it (it fills the details panel from them all the same), with no level
/// restriction and no player limit so the panel shows nothing but a zero elapsed time.
#[must_use]
pub fn gameinfo_none(request_id: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(14);
    w.u16(request_id).u32(game_status::NONE).u32(0).u8(0).u8(0xFF).u8(0).u8(0);
    w.finish()
}

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
    /// A dead hardcore character cannot join a game (BNETDocs).
    pub const DEAD_HARDCORE: u32 = 0x6E;
    /// A softcore character cannot join a hardcore game. This and the codes below are the ones
    /// 1.14d's join handler (`0x00441500`) knows; their meanings are BNETDocs'.
    pub const NOT_HARDCORE: u32 = 0x71;
    /// The character cannot play Nightmare yet.
    pub const NO_NIGHTMARE: u32 = 0x73;
    /// The character cannot play Hell yet.
    pub const NO_HELL: u32 = 0x74;
    /// A classic character cannot join an expansion game.
    pub const NOT_EXPANSION: u32 = 0x78;
    /// An expansion character cannot join a classic game.
    pub const NOT_CLASSIC: u32 = 0x79;
    /// A non-ladder character cannot join a ladder game.
    pub const NOT_LADDER: u32 = 0x7D;
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
    fn a_listed_game_ends_with_its_description() {
        let body = gamelist_entry(0x0102, 7, 3, game_flags(1, false, true, true), b"cows", b"moo");
        #[rustfmt::skip]
        let expected = [
            0x02, 0x01,             // request id
            7, 0, 0, 0,             // token
            3,                      // players
            0x04, 0x10, 0x30, 0x00, // 0x04 | Nightmare << 12 | expansion | ladder
            b'c', b'o', b'w', b's', 0,
            b'm', b'o', b'o', 0,
        ];
        assert_eq!(body, expected);
        // The client adds the game only if the packet ends right after the description.
        let mut r = Reader::new(&body);
        r.bytes(11).unwrap();
        r.cstr(64).unwrap();
        r.cstr(64).unwrap();
        assert!(r.rest().is_empty());

        assert_eq!(gamelist_end(9), [9, 0, 0, 0, 0, 0, 0, 0xFE, 0xFF, 0xFF, 0xFF]);
        let long = gamelist_entry(1, 1, 0, 4, b"0123456789abcdefgh", &[b'd'; 40]);
        assert_eq!(long.len(), 11 + 16 + 32, "a name of 15 and a description of 31 at most");
    }

    #[test]
    fn game_flags_follow_the_create_request() {
        assert_eq!(game_flags(0, false, false, false), 0x04);
        assert_eq!(game_flags(2, true, false, false), 0x2804);
        assert_eq!(game_flags(0, false, true, true), 0x0030_0004, "BNETDocs' open expansion ladder game");
        for d in 0..3 {
            let f = game_flags(d, true, true, true);
            assert_ne!(f, game_status::NONE);
            assert_ne!(f, game_status::END);
            assert_eq!((f >> 12) & 3, u32::from(d));
        }
    }

    #[test]
    fn game_info_lays_out_its_characters_in_three_columns() {
        let characters =
            [GameInfoCharacter { class: 3, level: 31, name: b"Tyrael" }, GameInfoCharacter { class: 6, level: 28, name: b"Natalya" }];
        let body = gameinfo_reply(
            5,
            &GameInfo {
                status: game_flags(2, false, true, false),
                uptime_secs: 3725,
                creator_level: 30,
                level_difference: 5,
                max_players: 4,
                description: b"baal",
                characters: &characters,
            },
        );
        assert_eq!(&body[..2], &[5, 0], "request id");
        assert_eq!(u32::from_le_bytes(body[2..6].try_into().unwrap()), 0x0010_2004);
        assert_eq!(u32::from_le_bytes(body[6..10].try_into().unwrap()), 3725, "1:02:05");
        assert_eq!(&body[10..14], &[30, 5, 4, 2], "creator level, level difference, max players, characters");
        assert_eq!(&body[14..17], &[3, 6, 0], "classes");
        assert_eq!(&body[30..33], &[31, 28, 0], "levels");
        assert_eq!(&body[46..], b"baal\0Tyrael\0Natalya\0");

        let none = gameinfo_none(6);
        assert_eq!(none, [6, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0xFF, 0, 0]);
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
