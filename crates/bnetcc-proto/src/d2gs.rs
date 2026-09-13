//! The Diablo II game-server (D2GS) wire, 1.14d.
//!
//! A closed-realm client leaves the realm after `MCP_JOINGAME` and dials the game server on
//! port [`GAME_PORT`]. That connection has its own framing, different in each direction:
//!
//! ```text
//! client -> server   raw packets back to back, each sized by opcode (client_packet_len)
//! server -> client   AF 01 raw, then Huffman-compressed frames:
//!                    [n+1] body            when n+1 < 0xF0
//!                    [hi|F0, lo] body      otherwise, where (hi,lo) = n+2
//!                    n = compressed length; the length counts its own header
//! ```
//!
//! Everything here was read from the retail 1.14d `Game.exe` — see
//! `docs/D2GS-114D-WIRE.md`, which cites the engine address behind each rule. The engine's
//! own tables (Huffman code lengths, packet sizes) are **not** in this repository: they are
//! read from the operator's `Game.exe` at run time by [`EngineTables::from_game_exe`].

use crate::buf::Writer;
use crate::error::{ProtoError, Result};

/// The TCP port a client dials for a game. Fixed by the client.
pub const GAME_PORT: u16 = 4000;

/// The version field a 1.14d client puts in `GAMELOGON`; the engine refuses anything else.
pub const VERSION_114D: u32 = 0x0E;

/// Largest uncompressed buffer the engine will send in one frame (`SendPacketToClient`).
pub const MAX_FRAME_PAYLOAD: usize = 0x204;

/// How much the engine queues per client before starting a new frame (`QueueClientPacket`).
pub const QUEUE_CHUNK: usize = 0x200;

/// The greeting a game server sends raw on connect. A non-zero second byte switches the
/// client to compressed frames from its next read; `AF 00` would keep it on raw packets,
/// which only the engine's in-process single-player path uses.
pub const GREETING: [u8; 2] = [0xAF, 0x01];

/// Server-to-client opcodes used by the join.
pub mod sc {
    /// "Loading" — queued right after [`GAME_FLAGS`].
    pub const LOADING: u8 = 0x00;
    /// Game flags (8 bytes).
    pub const GAME_FLAGS: u8 = 0x01;
    /// The character is loaded; the client may enter the game.
    pub const LOAD_SUCCESS: u8 = 0x02;
    /// Load an act (12 bytes).
    pub const LOAD_ACT: u8 = 0x03;
    /// Act loading finished.
    pub const LOAD_COMPLETE: u8 = 0x04;
    /// The game is being left.
    pub const UNLOAD_COMPLETE: u8 = 0x05;
    /// Game exit.
    pub const GAME_EXIT: u8 = 0x06;
    /// Load a room on the client (6 bytes).
    pub const LOAD_ROOM: u8 = 0x07;
    /// Which unit is the client's own player (6 bytes).
    pub const OWN_UNIT: u8 = 0x0B;
    /// A unit's selected skill on one mouse button (13 bytes).
    pub const SELECT_SKILL: u8 = 0x23;
    /// Place a unit (11 bytes).
    pub const REASSIGN_PLAYER: u8 = 0x15;
    /// The act's time of day (10 bytes).
    pub const ACT_ENVIRONMENT: u8 = 0x53;
    /// A player unit (26 bytes).
    pub const ASSIGN_PLAYER: u8 = 0x59;
    /// Sent after the player is placed; the engine fills only the opcode (5 bytes).
    pub const PLAYER_PLACED: u8 = 0x7E;
    /// Reply to the client's ping (33 bytes, all zero past the opcode).
    pub const PONG: u8 = 0x8F;
    /// Connection terminated.
    pub const TERMINATED: u8 = 0xB0;
    /// Join refused, with a reason (5 bytes).
    pub const JOIN_FAILED: u8 = 0xB4;
}

/// Client-to-server opcodes used by the join.
pub mod cs {
    /// Leave the game.
    pub const LEAVE_GAME: u8 = 0x69;
    /// Log on to a game (37 bytes).
    pub const GAME_LOGON: u8 = 0x68;
    /// Enter the game once loaded (1 byte).
    pub const ENTER_GAME: u8 = 0x6B;
    /// Keep-alive ping (13 bytes).
    pub const PING: u8 = 0x6D;
}

/// `0xB4` reasons, as the engine's open-game check returns them.
pub mod join_failed {
    /// No such game, or the logon did not check out.
    pub const GENERIC: u32 = 0x06;
    /// The game already holds eight players.
    pub const GAME_FULL: u32 = 0x0F;
    /// The client's version field is not [`super::VERSION_114D`].
    pub const WRONG_VERSION: u32 = 0x10;
}

// --- engine tables ---------------------------------------------------------------------

/// Number of client-to-server opcodes the engine sizes (0x00..=0x70).
pub const CLIENT_OPCODES: usize = 0x71;
/// Number of server-to-client opcodes the engine sizes (0x00..=0xB4).
pub const SERVER_OPCODES: usize = 0xB5;

/// The engine data the wire needs: the Huffman code lengths and both packet-size tables.
#[derive(Debug, Clone)]
pub struct EngineTables {
    /// The canonical code built from the engine's 256 code lengths.
    pub huffman: Huffman,
    /// Client-to-server sizes by opcode: `>0` fixed, `-1` variable, `0` invalid.
    pub client_sizes: [i32; CLIENT_OPCODES],
    /// Server-to-client sizes by opcode, same convention.
    pub server_sizes: [i32; SERVER_OPCODES],
}

/// Where the tables live in `Game.exe` 1.14.3.71.
mod image {
    pub const HUFFMAN_CODE_LENGTHS: u32 = 0x0070_76C0;
    pub const CLIENT_SIZES: u32 = 0x0073_0DC0;
    pub const SERVER_SIZES: u32 = 0x0073_0AE8;
    /// `VS_FIXEDFILEINFO` 1.14.3.71: `dwFileVersionMS`, `dwFileVersionLS`.
    pub const FILE_VERSION: (u32, u32) = (0x0001_000E, 0x0003_0047);
}

impl EngineTables {
    /// Assemble tables, checking the code lengths form a usable code.
    ///
    /// # Errors
    ///
    /// [`ProtoError::InvalidValue`] if the code lengths are not a complete prefix code.
    pub fn new(
        code_lengths: &[u8; 256],
        client_sizes: [i32; CLIENT_OPCODES],
        server_sizes: [i32; SERVER_OPCODES],
    ) -> Result<Self> {
        Ok(Self { huffman: Huffman::new(code_lengths)?, client_sizes, server_sizes })
    }

    /// Read the tables out of a retail 1.14d `Game.exe` image (the whole file).
    ///
    /// Refuses any other build: the addresses are 1.14.3.71's. As a second guard, a few
    /// sizes every client relies on are checked after reading.
    ///
    /// # Errors
    ///
    /// [`ProtoError::InvalidValue`] naming what did not match.
    pub fn from_game_exe(file: &[u8]) -> Result<Self> {
        let invalid = |field: &'static str, value: String| ProtoError::InvalidValue { field, value };
        let pe = Pe::parse(file).ok_or_else(|| invalid("Game.exe", "not a PE image".into()))?;
        match fixed_file_version(file) {
            Some(v) if v == image::FILE_VERSION => {}
            Some((ms, ls)) => {
                return Err(invalid(
                    "Game.exe version",
                    format!("{}.{}.{}.{} (need 1.14.3.71)", ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF),
                ))
            }
            None => return Err(invalid("Game.exe version", "no version resource".into())),
        }

        let lengths: [u8; 256] = pe
            .bytes(image::HUFFMAN_CODE_LENGTHS, 256)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("Game.exe", "Huffman table out of range".into()))?;
        let mut client_sizes = [0i32; CLIENT_OPCODES];
        pe.i32s(image::CLIENT_SIZES, &mut client_sizes)
            .ok_or_else(|| invalid("Game.exe", "client size table out of range".into()))?;
        let mut server_sizes = [0i32; SERVER_OPCODES];
        pe.i32s(image::SERVER_SIZES, &mut server_sizes)
            .ok_or_else(|| invalid("Game.exe", "server size table out of range".into()))?;

        let expected = [
            (client_sizes[usize::from(cs::GAME_LOGON)], 37),
            (client_sizes[usize::from(cs::ENTER_GAME)], 1),
            (client_sizes[usize::from(cs::PING)], 13),
            (server_sizes[usize::from(sc::GAME_FLAGS)], 8),
            (server_sizes[usize::from(sc::LOAD_ACT)], 12),
            (server_sizes[usize::from(sc::ASSIGN_PLAYER)], 26),
        ];
        if expected.iter().any(|(got, want)| got != want) {
            return Err(invalid("Game.exe", "packet size tables do not match 1.14d".into()));
        }
        Self::new(&lengths, client_sizes, server_sizes)
    }
}

/// Just enough of a PE32 image to read initialised data by virtual address.
struct Pe<'a> {
    file: &'a [u8],
    base: u32,
    /// `(virtual address, raw size, raw offset)` per section.
    sections: Vec<(u32, u32, u32)>,
}

impl<'a> Pe<'a> {
    fn parse(file: &'a [u8]) -> Option<Self> {
        let u16_at = |o: usize| file.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| file.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if file.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(0x3C)? as usize;
        if file.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let count = u16_at(pe + 6)? as usize;
        let optional = u16_at(pe + 20)? as usize;
        let base = u32_at(pe + 24 + 28)?;
        let first = pe + 24 + optional;
        let sections = (0..count)
            .map(|i| {
                let s = first + 40 * i;
                Some((u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { file, base, sections })
    }

    fn bytes(&self, va: u32, len: usize) -> Option<&'a [u8]> {
        let rva = va.checked_sub(self.base)?;
        self.sections.iter().find_map(|&(start, raw_size, raw_offset)| {
            let within = rva.checked_sub(start)?;
            (within as usize + len <= raw_size as usize)
                .then(|| self.file.get(raw_offset as usize + within as usize..)?.get(..len))
                .flatten()
        })
    }

    fn i32s(&self, va: u32, out: &mut [i32]) -> Option<()> {
        let bytes = self.bytes(va, out.len() * 4)?;
        for (v, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
            *v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        Some(())
    }
}

/// `dwFileVersionMS`/`LS` from the image's `VS_FIXEDFILEINFO`, found by its signature.
fn fixed_file_version(file: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 4] = 0xFEEF_04BD_u32.to_le_bytes();
    let at = file.windows(4).position(|w| w == SIGNATURE)?;
    let b = file.get(at + 8..at + 16)?;
    Some((u32::from_le_bytes([b[0], b[1], b[2], b[3]]), u32::from_le_bytes([b[4], b[5], b[6], b[7]])))
}

// --- Huffman ---------------------------------------------------------------------------

/// The wire's static Huffman code over byte values.
///
/// Canonical, assigned from the **longest** length down (engine `0x0040ADB0`): symbols are
/// ordered by descending code length, ties by symbol, the first gets code 0 and each next
/// `(previous + 1) >> (previous length - this length)`. Bits are packed MSB first
/// (`0x0040B1B0`). The engine keeps codes in a byte; with its stock table no code exceeds
/// 255, so the wider type here changes nothing.
#[derive(Debug, Clone)]
pub struct Huffman {
    lengths: [u8; 256],
    codes: [u16; 256],
}

impl Huffman {
    /// Build the code from 256 code lengths.
    ///
    /// # Errors
    ///
    /// [`ProtoError::InvalidValue`] if a length is outside `1..=15` or the lengths are not a
    /// complete prefix code (which every table the engine accepts is).
    pub fn new(lengths: &[u8; 256]) -> Result<Self> {
        if let Some(&bad) = lengths.iter().find(|&&l| !(1..=15).contains(&l)) {
            return Err(ProtoError::InvalidValue { field: "Huffman code length", value: bad.to_string() });
        }
        let kraft: u32 = lengths.iter().map(|&l| 1u32 << (15 - l)).sum();
        if kraft != 1 << 15 {
            return Err(ProtoError::InvalidValue {
                field: "Huffman code lengths",
                value: "not a complete prefix code".into(),
            });
        }
        let mut order: Vec<u8> = (0..=255).collect();
        order.sort_by_key(|&s| std::cmp::Reverse(lengths[usize::from(s)])); // stable: ties by symbol
        let mut codes = [0u16; 256];
        for pair in order.windows(2) {
            let (prev, next) = (usize::from(pair[0]), usize::from(pair[1]));
            codes[next] = (codes[prev] + 1) >> (lengths[prev] - lengths[next]);
        }
        Ok(Self { lengths: *lengths, codes })
    }

    /// Append the compressed form of `src` to `out`.
    pub fn compress(&self, src: &[u8], out: &mut Vec<u8>) {
        let mut acc: u32 = 0;
        let mut bits: u32 = 0;
        for &b in src {
            let len = u32::from(self.lengths[usize::from(b)]);
            acc = (acc << len) | u32::from(self.codes[usize::from(b)]);
            bits += len;
            while bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
            acc &= (1 << bits) - 1;
        }
        if bits > 0 {
            out.push((acc << (8 - bits)) as u8);
        }
    }

    /// Append the decompressed form of `src` to `out`. Trailing padding bits that do not
    /// complete a code are ignored, as they are by the client.
    pub fn decompress(&self, src: &[u8], out: &mut Vec<u8>) {
        // (length, code) -> symbol, searched by length as bits arrive. Only tests and
        // diagnostics decode; the server never does.
        let mut by_length: [Vec<(u16, u8)>; 16] = Default::default();
        for s in 0..=255u8 {
            by_length[usize::from(self.lengths[usize::from(s)])].push((self.codes[usize::from(s)], s));
        }
        for bucket in &mut by_length {
            bucket.sort_unstable();
        }
        let (mut code, mut len) = (0u16, 0usize);
        for byte in src {
            for i in (0..8).rev() {
                code = (code << 1) | u16::from((byte >> i) & 1);
                len += 1;
                if let Ok(at) = by_length[len].binary_search_by_key(&code, |&(c, _)| c) {
                    out.push(by_length[len][at].1);
                    code = 0;
                    len = 0;
                } else if len == 15 {
                    return; // no code this long: not a stream this table produced
                }
            }
        }
    }
}

// --- server-to-client frames ---------------------------------------------------------

/// Append one frame to `out`: `payload` compressed, behind the engine's length header.
///
/// # Errors
///
/// [`ProtoError::FrameTooLarge`] if `payload` exceeds [`MAX_FRAME_PAYLOAD`].
pub fn encode_frame(huffman: &Huffman, payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(ProtoError::FrameTooLarge { len: payload.len(), max: MAX_FRAME_PAYLOAD });
    }
    let mut body = Vec::with_capacity(payload.len() + payload.len() / 2);
    huffman.compress(payload, &mut body);
    if body.len() + 1 < 0xF0 {
        out.push((body.len() + 1) as u8);
    } else {
        let total = body.len() + 2;
        out.push(((total >> 8) as u8) | 0xF0);
        out.push(total as u8);
    }
    out.extend_from_slice(&body);
    Ok(())
}

/// Split one frame off a server-to-client byte stream: `(compressed body, bytes consumed)`,
/// or `None` until the whole frame is present.
#[must_use]
pub fn decode_frame(buf: &[u8]) -> Option<(&[u8], usize)> {
    let (header, total) = match *buf.first()? {
        b if b < 0xF0 => (1, usize::from(b)),
        b => (2, (usize::from(b & 0x0F) << 8) | usize::from(*buf.get(1)?)),
    };
    (total >= header && buf.len() >= total).then(|| (&buf[header..total], total))
}

/// A client's outbound packet queue, chunked the way the engine queues it: a packet that
/// would take the current chunk past [`QUEUE_CHUNK`] starts a new one, and each chunk goes
/// out as one compressed frame.
#[derive(Debug, Default)]
pub struct Outbox {
    chunks: Vec<Vec<u8>>,
}

impl Outbox {
    /// Queue one packet.
    pub fn push(&mut self, packet: &[u8]) {
        match self.chunks.last_mut() {
            Some(chunk) if chunk.len() + packet.len() <= QUEUE_CHUNK => chunk.extend_from_slice(packet),
            _ => self.chunks.push(packet.to_vec()),
        }
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The queued packets, uncompressed and in order — for logging.
    pub fn pending(&self) -> impl Iterator<Item = &[u8]> {
        self.chunks.iter().map(Vec::as_slice)
    }

    /// Frame everything queued into `out` and empty the queue.
    ///
    /// # Errors
    ///
    /// [`ProtoError::FrameTooLarge`] if a single queued packet exceeded
    /// [`MAX_FRAME_PAYLOAD`]; the queue is emptied regardless.
    pub fn flush(&mut self, huffman: &Huffman, out: &mut Vec<u8>) -> Result<()> {
        let chunks = std::mem::take(&mut self.chunks);
        chunks.iter().try_for_each(|chunk| encode_frame(huffman, chunk, out))
    }
}

// --- client-to-server packets --------------------------------------------------------

/// How much of a client stream the next packet takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientPacketLen {
    /// The whole packet is `n` bytes, opcode included.
    Len(usize),
    /// More bytes are needed to tell.
    Incomplete,
    /// No such packet: the stream cannot be framed past this point.
    Invalid(u8),
}

/// Size the client packet at the front of `buf`, as the server's framer (`0x0052BC20`)
/// does: fixed sizes from the table, and four opcodes sized from their contents.
#[must_use]
pub fn client_packet_len(sizes: &[i32; CLIENT_OPCODES], buf: &[u8]) -> ClientPacketLen {
    use ClientPacketLen::{Incomplete, Invalid, Len};
    let have = |n: usize| if buf.len() >= n { Len(n) } else { Incomplete };
    let Some(&op) = buf.first() else {
        return Incomplete;
    };
    if op == 0xFF {
        return have(16);
    }
    let Some(&size) = sizes.get(usize::from(op)) else {
        return Invalid(op);
    };
    let len = match size {
        n if n > 0 => return have(n as usize),
        0 => return Invalid(op),
        _ => match op {
            // [op][u16][cstr][cstr][i8 n][n bytes]
            0x14 | 0x15 => {
                let cstr_end = |from: usize| buf.get(from..)?.iter().position(|&b| b == 0).map(|p| from + p);
                let Some(first) = cstr_end(3) else { return Incomplete };
                let Some(second) = cstr_end(first + 1) else { return Incomplete };
                let Some(&extra) = buf.get(second + 1) else { return Incomplete };
                let total = second as isize + 2 + isize::from(extra as i8);
                if total <= second as isize + 1 {
                    return Invalid(op);
                }
                total as usize
            }
            // [op][u16 n][n bytes], n above 0x1FD read as 0
            0x66 => match buf.get(1..3) {
                Some(b) => match u16::from_le_bytes([b[0], b[1]]) {
                    n if n > 0x1FD => 3,
                    n => 3 + usize::from(n),
                },
                None => return Incomplete,
            },
            // [op][u8 n][5 bytes][n bytes]; the engine waits for 6 bytes before reading n
            0x6C => match (buf.len() >= 6, buf.get(1)) {
                (true, Some(&n)) => 7 + usize::from(n),
                _ => return Incomplete,
            },
            _ => return Invalid(op),
        },
    };
    if len > MAX_FRAME_PAYLOAD {
        return Invalid(op);
    }
    have(len)
}

/// `0x68` GAMELOGON, as the connection dispatcher (`0x0053F100`) reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLogon {
    /// Game hash, from `MCP_JOINGAME`.
    pub game_hash: u32,
    /// Game id, from `MCP_JOINGAME` — the server's slot for the game.
    pub game_id: u16,
    /// Character class.
    pub class: u8,
    /// Client version field; [`VERSION_114D`] for 1.14d.
    pub version: u32,
    /// The byte at +20, passed through by the engine; purpose not yet read.
    pub unknown_20: u8,
    /// Character name.
    pub name: String,
}

impl GameLogon {
    /// Wire size, opcode included.
    pub const LEN: usize = 37;

    /// Parse a whole `0x68` packet.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Truncated`] for a short packet, [`ProtoError::UnexpectedPacket`] if the
    /// opcode is not `0x68`.
    pub fn parse(packet: &[u8]) -> Result<Self> {
        if packet.len() < Self::LEN {
            return Err(ProtoError::Truncated { needed: Self::LEN, available: packet.len() });
        }
        if packet[0] != cs::GAME_LOGON {
            return Err(ProtoError::UnexpectedPacket { id: packet[0], state: "game logon" });
        }
        let u32_at = |o: usize| u32::from_le_bytes([packet[o], packet[o + 1], packet[o + 2], packet[o + 3]]);
        let name = &packet[21..37];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        Ok(Self {
            game_hash: u32_at(1),
            game_id: u16::from_le_bytes([packet[5], packet[6]]),
            class: packet[7],
            version: u32_at(8),
            unknown_20: packet[20],
            name: String::from_utf8_lossy(name).into_owned(),
        })
    }
}

// --- server-to-client packets --------------------------------------------------------

/// Game-creation flags as the engine's arena keeps them (`game creation flags & 0x3179C7`):
/// `0x4`, difficulty `<< 12`, hardcore `0x800`, expansion `0x100000`, ladder `0x200000`. A
/// live 1.14d server sent `0x00100004` for a normal, expansion, non-ladder, softcore game.
#[must_use]
pub const fn game_flags(difficulty: u8, hardcore: bool, expansion: bool, ladder: bool) -> u32 {
    0x4 | ((difficulty as u32 & 3) << 12)
        | if hardcore { 0x800 } else { 0 }
        | if expansion { 0x10_0000 } else { 0 }
        | if ladder { 0x20_0000 } else { 0 }
}

/// `0x01`: `[difficulty u8][flags u32][expansion u8][ladder u8]` (builder `0x0053B340`).
#[must_use]
pub fn game_flags_packet(difficulty: u8, flags: u32, expansion: bool, ladder: bool) -> Vec<u8> {
    let mut w = Writer::with_capacity(8);
    w.u8(sc::GAME_FLAGS).u8(difficulty).u32(flags).u8(expansion.into()).u8(ladder.into());
    w.finish()
}

/// `0x03`: `[act u8][map seed u32][town area u16][u32]` (builder `0x0053B390`). The client
/// generates the act from the seed; the last field it stores beside the seed.
#[must_use]
pub fn load_act(act: u8, map_seed: u32, town_area: u16, paired: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(12);
    w.u8(sc::LOAD_ACT).u8(act).u32(map_seed).u16(town_area).u32(paired);
    w.finish()
}

/// `0x53`: `[period u32][ticks u32][eclipse u8]`. The client aborts unless `period` is
/// `0..=5` (engine `0x0061C240`); period 2 begins at angle 0, the start of the day.
#[must_use]
pub fn act_environment(period: u32, ticks: u32, eclipse: bool) -> Vec<u8> {
    let mut w = Writer::with_capacity(10);
    w.u8(sc::ACT_ENVIRONMENT).u32(period.min(5)).u32(ticks).u8(eclipse.into());
    w.finish()
}

/// `0x0B`: `[unit type u8][guid u32]` (builder `0x00537930`). The client makes the unit it
/// already knows by that guid its own player (handler `0x0045CC50`), so the unit's `0x59` must
/// come first — and nothing that reads the player, such as `0x53`, may come before this.
#[must_use]
pub fn own_unit(unit_type: u8, guid: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(6);
    w.u8(sc::OWN_UNIT).u8(unit_type).u32(guid);
    w.finish()
}

/// `0x07`: `[room tile x u16][room tile y u16][level u8]` — load the room whose top-left tile
/// is `(x, y)` (builder `0x0053BC50`, sent by `PlacePlayerInAct` for the spawn room).
#[must_use]
pub fn load_room(tile_x: u16, tile_y: u16, level: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(6);
    w.u8(sc::LOAD_ROOM).u16(tile_x).u16(tile_y).u8(level);
    w.finish()
}

/// `0x23`: `[unit type u8][guid u32][right-hand u8][skill u16][item guid u32]` (builder
/// `0x0053C590`); item guid `0xFFFFFFFF` when no item grants the skill.
#[must_use]
pub fn select_skill(unit_type: u8, guid: u32, right_hand: bool, skill: u16, item_guid: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(13);
    w.u8(sc::SELECT_SKILL).u8(unit_type).u32(guid).u8(right_hand.into()).u16(skill).u32(item_guid);
    w.finish()
}

/// `0x59`: `[guid u32][class u8][name 16][x u16][y u16]` (builder `0x0053E8F0`).
#[must_use]
pub fn assign_player(guid: u32, class: u8, name: &str, x: u16, y: u16) -> Vec<u8> {
    let mut field = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(15);
    field[..n].copy_from_slice(&bytes[..n]);
    let mut w = Writer::with_capacity(26);
    w.u8(sc::ASSIGN_PLAYER).u32(guid).u8(class).bytes(&field).u16(x).u16(y);
    w.finish()
}

/// `0x15`: `[unit type u8][guid u32][x u16][y u16][flag u8]` (builder `0x0053BC10`).
#[must_use]
pub fn reassign_player(unit_type: u8, guid: u32, x: u16, y: u16, flag: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(11);
    w.u8(sc::REASSIGN_PLAYER).u8(unit_type).u32(guid).u16(x).u16(y).u8(flag);
    w.finish()
}

/// `0x7E`. The engine writes only the opcode (`0x0053DB70`); the rest was uninitialised
/// stack, so the client cannot depend on it — zeros here.
#[must_use]
pub fn player_placed() -> Vec<u8> {
    vec![sc::PLAYER_PLACED, 0, 0, 0, 0]
}

/// `0x8F`, the ping reply: 33 bytes, zero past the opcode (builder `0x0053E020`).
#[must_use]
pub fn pong() -> Vec<u8> {
    let mut p = vec![0u8; 33];
    p[0] = sc::PONG;
    p
}

/// `0xB4`: `[reason u32]` (`0x0053B260`).
#[must_use]
pub fn join_failed_packet(reason: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(5);
    w.u8(sc::JOIN_FAILED).u32(reason);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete prefix code that is not the engine's: one 1-bit symbol, one 8-bit, the
    /// rest 9-bit (1/2 + 1/256 + 254/512 = 1).
    fn test_lengths() -> [u8; 256] {
        let mut l = [9u8; 256];
        l[0] = 1;
        l[0xAF] = 8;
        l
    }

    fn sizes_for_tests() -> [i32; CLIENT_OPCODES] {
        let mut s = [0i32; CLIENT_OPCODES];
        s[0x14] = -1;
        s[0x15] = -1;
        s[0x66] = -1;
        s[0x6C] = -1;
        s[usize::from(cs::GAME_LOGON)] = 37;
        s[usize::from(cs::ENTER_GAME)] = 1;
        s[usize::from(cs::PING)] = 13;
        s
    }

    #[test]
    fn codes_are_assigned_longest_first_and_round_trip() {
        let h = Huffman::new(&test_lengths()).unwrap();
        assert_eq!(h.codes[1], 0, "the first of the longest codes is 0");
        assert_eq!(h.codes[0], 1, "the one short code is the top of the space: a single 1 bit");
        let all: Vec<u8> = (0..=255).chain([0, 0, 0, 0xAF, 7]).collect();
        let mut packed = Vec::new();
        h.compress(&all, &mut packed);
        let mut back = Vec::new();
        h.decompress(&packed, &mut back);
        assert_eq!(back, all);
    }

    #[test]
    fn incomplete_or_out_of_range_lengths_are_refused() {
        let mut l = test_lengths();
        l[5] = 10;
        assert!(Huffman::new(&l).is_err(), "a hole in the code space");
        l[5] = 0;
        assert!(Huffman::new(&l).is_err(), "zero length");
    }

    #[test]
    fn frame_headers_count_themselves() {
        let h = Huffman::new(&test_lengths()).unwrap();
        let mut out = Vec::new();
        encode_frame(&h, &[0; 16], &mut out).unwrap();
        // sixteen 1-bit symbols: two bytes of body
        assert_eq!(out, vec![3, 0xFF, 0xFF]);
        assert_eq!(decode_frame(&out), Some((&out[1..], 3)));

        let big: Vec<u8> = (0..0x200u32).map(|i| (i % 255 + 1) as u8).collect();
        out.clear();
        encode_frame(&h, &big, &mut out).unwrap();
        let total = (usize::from(out[0] & 0x0F) << 8) | usize::from(out[1]);
        assert_eq!(out[0] & 0xF0, 0xF0, "a long frame takes the two-byte header");
        assert_eq!(total, out.len(), "and it too counts itself");
        let (body, used) = decode_frame(&out).unwrap();
        assert_eq!(used, out.len());
        let mut back = Vec::new();
        h.decompress(body, &mut back);
        assert_eq!(back, big);

        assert!(encode_frame(&h, &[0; MAX_FRAME_PAYLOAD + 1], &mut out).is_err());
        assert_eq!(decode_frame(&[5, 1, 2]), None, "incomplete");
    }

    #[test]
    fn the_outbox_starts_a_new_frame_past_the_engines_chunk() {
        let mut o = Outbox::default();
        o.push(&[0x01; 0x1F0]);
        o.push(&[0x02; 0x10]);
        o.push(&[0x03; 1]);
        let pending: Vec<usize> = o.pending().map(<[u8]>::len).collect();
        assert_eq!(pending, vec![0x200, 1]);
        let h = Huffman::new(&test_lengths()).unwrap();
        let mut out = Vec::new();
        o.flush(&h, &mut out).unwrap();
        assert!(o.is_empty());
        let (_, first) = decode_frame(&out).unwrap();
        assert!(decode_frame(&out[first..]).is_some(), "two frames on the wire");
    }

    #[test]
    fn client_packets_are_sized_like_the_engine_does() {
        use ClientPacketLen::{Incomplete, Invalid, Len};
        let s = sizes_for_tests();
        assert_eq!(client_packet_len(&s, &[]), Incomplete);
        assert_eq!(client_packet_len(&s, &[0x6B]), Len(1));
        assert_eq!(client_packet_len(&s, &[0x68; 10]), Incomplete);
        assert_eq!(client_packet_len(&s, &[0x00]), Invalid(0x00));
        assert_eq!(client_packet_len(&s, &[0x71]), Invalid(0x71));
        assert_eq!(client_packet_len(&s, &[0xFF; 16]), Len(16));
        // chat: header, "hi", "", then a count of 2 extra bytes
        let chat = [0x15, 1, 0, b'h', b'i', 0, 0, 2, 9, 9];
        assert_eq!(client_packet_len(&s, &chat), Len(10));
        assert_eq!(client_packet_len(&s, &chat[..8]), Incomplete);
        assert_eq!(client_packet_len(&s, &[0x66, 4, 0, 1, 2, 3, 4]), Len(7));
        assert_eq!(client_packet_len(&s, &[0x66, 0xFF, 0xFF]), Len(3), "an oversized length reads as 0");
        assert_eq!(client_packet_len(&s, &[0x6C, 2, 0, 0, 0]), Incomplete, "waits for six bytes");
        assert_eq!(client_packet_len(&s, &[0x6C, 2, 0, 0, 0, 0, 0, 0, 0]), Len(9));
    }

    #[test]
    fn game_logon_fields_sit_where_the_dispatcher_reads_them() {
        let mut p = vec![0u8; GameLogon::LEN];
        p[0] = 0x68;
        p[1..5].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        p[5..7].copy_from_slice(&3u16.to_le_bytes());
        p[7] = 4;
        p[8..12].copy_from_slice(&VERSION_114D.to_le_bytes());
        p[20] = 0x99;
        p[21..27].copy_from_slice(b"TestBa");
        let l = GameLogon::parse(&p).unwrap();
        assert_eq!(
            l,
            GameLogon {
                game_hash: 0xDEAD_BEEF,
                game_id: 3,
                class: 4,
                version: 0x0E,
                unknown_20: 0x99,
                name: "TestBa".into()
            }
        );
        assert!(GameLogon::parse(&p[..36]).is_err());
    }

    #[test]
    fn packets_have_their_table_sizes() {
        assert_eq!(game_flags_packet(0, game_flags(0, false, true, false), true, false).len(), 8);
        assert_eq!(game_flags(0, false, true, false), 0x0010_0004, "the live capture's value");
        assert_eq!(load_act(0, 1, 1, 0).len(), 12);
        assert_eq!(act_environment(9, 0, false)[1], 5, "periods past 5 would abort the client");
        assert_eq!(act_environment(2, 0, false).len(), 10);
        assert_eq!(assign_player(1, 4, "AVeryLongNameIndeed", 1, 2).len(), 26);
        assert_eq!(reassign_player(0, 1, 2, 3, 1).len(), 11);
        assert_eq!(own_unit(0, 1), vec![0x0B, 0, 1, 0, 0, 0]);
        assert_eq!(load_room(1160, 888, 1), vec![0x07, 0x88, 0x04, 0x78, 0x03, 1]);
        assert_eq!(select_skill(0, 1, true, 0, u32::MAX).len(), 13);
        assert_eq!(player_placed().len(), 5);
        assert_eq!(pong().len(), 33);
        assert_eq!(join_failed_packet(join_failed::WRONG_VERSION), vec![0xB4, 0x10, 0, 0, 0]);
    }

    /// With the operator's real `Game.exe` (set `BNETCC_D2_GAME_EXE`), the tables load and the
    /// codec reproduces a frame captured off a live 1.14d server — plaintext GameFlags + 0x00
    /// in, `7a 09 a5 f0` out. The capture is from jaenster/libd2 (MIT).
    #[test]
    fn with_a_real_game_exe_the_codec_matches_a_live_server() {
        let Ok(path) = std::env::var("BNETCC_D2_GAME_EXE") else {
            return;
        };
        let file = std::fs::read(path).expect("read Game.exe");
        let tables = EngineTables::from_game_exe(&file).expect("1.14d tables");
        let plain = [0x01, 0x00, 0x04, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00];
        let mut out = Vec::new();
        encode_frame(&tables.huffman, &plain, &mut out).unwrap();
        assert_eq!(out, vec![0x05, 0x7A, 0x09, 0xA5, 0xF0]);
    }
}
