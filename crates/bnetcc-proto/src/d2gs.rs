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
//! read from the operator's `Game.exe` at run time (`d2_data::engine`) and handed to
//! [`EngineTables::new`].

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

/// Server-to-client opcodes used so far.
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
    /// Release a room the client loaded (6 bytes).
    pub const UNLOAD_ROOM: u8 = 0x08;
    /// A warp tile unit (11 bytes).
    pub const ASSIGN_WARP: u8 = 0x09;
    /// Forget a unit (6 bytes).
    pub const REMOVE_UNIT: u8 = 0x0A;
    /// Add a little gold: `[gain u8]` (2 bytes).
    pub const ADD_GOLD_BYTE: u8 = 0x19;
    /// An item made, moved or placed where no unit owns it (variable; size at `+2`).
    pub const ITEM_ACTION_WORLD: u8 = 0x9C;
    /// An item action naming the unit that owns the item (variable; size at `+2`).
    pub const ITEM_ACTION_OWNED: u8 = 0x9D;
    /// An item spell waits for its target: `[icon u8][item guid u32][skill u16]` (8 bytes; builder
    /// `0x0053D220`) — an identify scroll turns the cursor to pick what it identifies.
    pub const ITEM_SPELL_READY: u8 = 0x3F;
    /// A trade with a vendor settled: `[kind u8][result u8][u32][item guid u32][gold u32]` (15
    /// bytes; builder `0x0053D740`, client `0x004B6390`).
    pub const NPC_TRANSACTION: u8 = 0x2A;
    /// An object's mode changed (12 bytes).
    pub const OBJECT_STATE: u8 = 0x0E;
    /// Which unit is the client's own player (6 bytes).
    pub const OWN_UNIT: u8 = 0x0B;
    /// A reaction of a player unit — hit, death — (13 bytes).
    pub const PLAYER_REACTION: u8 = 0x0D;
    /// Experience gained, in a byte (2 bytes).
    pub const ADD_EXPERIENCE_BYTE: u8 = 0x1A;
    /// Experience gained, in a word (3 bytes).
    pub const ADD_EXPERIENCE_WORD: u8 = 0x1B;
    /// Experience total (5 bytes).
    pub const SET_EXPERIENCE: u8 = 0x1C;
    /// A monster walks to a spot (16 bytes).
    pub const MONSTER_WALK: u8 = 0x67;
    /// A reaction of a monster — hit, death — (12 bytes).
    pub const MONSTER_REACTION: u8 = 0x69;
    /// A monster attacks a unit (16 bytes).
    pub const MONSTER_ATTACK: u8 = 0x6C;
    /// A unit's life in 128ths (7 bytes).
    pub const UNIT_LIFE: u8 = 0xAB;
    /// What an NPC has to say about quests, before its dialog opens (40 bytes).
    pub const NPC_QUEST_MESSAGES: u8 = 0x27;
    /// Quest flags: the player's own (type 6) or an NPC's quest dialog update (103 bytes).
    pub const QUEST_FLAGS: u8 = 0x28;
    /// Open the waypoint menu (21 bytes).
    pub const WAYPOINT_MENU: u8 = 0x63;
    /// Open or close a panel (2 bytes).
    pub const UI_ACTION: u8 = 0x77;
    /// The game's quest flags (97 bytes).
    pub const GAME_QUEST_FLAGS: u8 = 0x29;
    /// Whether each quest is available in this game (38 bytes).
    pub const QUEST_STATES: u8 = 0x5E;
    /// A unit's selected skill on one mouse button (13 bytes).
    pub const SELECT_SKILL: u8 = 0x23;
    /// One of the player's own stats, value in a byte (3 bytes).
    pub const SET_STAT_BYTE: u8 = 0x1D;
    /// One of the player's own stats, value in a word (4 bytes).
    pub const SET_STAT_WORD: u8 = 0x1E;
    /// One of the player's own stats, value in a dword (6 bytes).
    pub const SET_STAT_DWORD: u8 = 0x1F;
    /// The player's life, mana, stamina and position, bit-packed (13 bytes).
    pub const LIFE_AND_POSITION: u8 = 0x95;
    /// Place a unit (11 bytes).
    pub const REASSIGN_PLAYER: u8 = 0x15;
    /// The act's time of day (10 bytes).
    pub const ACT_ENVIRONMENT: u8 = 0x53;
    /// An object: its class, position and mode (14 bytes).
    pub const ASSIGN_OBJECT: u8 = 0x51;
    /// A player unit (26 bytes).
    pub const ASSIGN_PLAYER: u8 = 0x59;
    /// A monster standing where it is, with its life (10 bytes).
    pub const MONSTER_STANDING: u8 = 0x6D;
    /// A unit's active states, bit-packed (variable; total length at `+6`).
    pub const UNIT_STATES: u8 = 0xAA;
    /// A monster or NPC: class, position, life, then a bit-packed look (variable; total length
    /// at `+12`).
    pub const ASSIGN_MONSTER: u8 = 0xAC;
    /// Sent after the player is placed; the engine fills only the opcode (5 bytes).
    pub const PLAYER_PLACED: u8 = 0x7E;
    /// Reply to the client's ping (33 bytes, all zero past the opcode).
    pub const PONG: u8 = 0x8F;
    /// Connection terminated.
    pub const TERMINATED: u8 = 0xB0;
    /// Join refused, with a reason (5 bytes).
    pub const JOIN_FAILED: u8 = 0xB4;
}

/// Client-to-server opcodes handled so far.
pub mod cs {
    /// Walk to a spot: `[x u16][y u16]` (5 bytes; engine handler `0x005497E0`).
    pub const WALK_TO_LOCATION: u8 = 0x01;
    /// Walk to a unit: `[type u32][guid u32]` (9 bytes).
    pub const WALK_TO_UNIT: u8 = 0x02;
    /// Run to a spot: `[x u16][y u16]` (5 bytes; engine handler `0x005498D0`).
    pub const RUN_TO_LOCATION: u8 = 0x03;
    /// Run to a unit: `[type u32][guid u32]` (9 bytes).
    pub const RUN_TO_UNIT: u8 = 0x04;
    /// Use the left skill on a unit: `[type u32][guid u32]` (9 bytes; engine handler
    /// `0x00549D80`). `0x07`, `0x09` and `0x0A` carry the same body (handlers `0x00549E00`,
    /// `0x00549EE0` and `0x00549F40` end in the same two routines).
    pub const LEFT_SKILL_ON_UNIT: u8 = 0x06;
    /// See [`LEFT_SKILL_ON_UNIT`].
    pub const LEFT_SKILL_ON_UNIT_HOLD: u8 = 0x07;
    /// See [`LEFT_SKILL_ON_UNIT`].
    pub const LEFT_SKILL_ON_UNIT_REPEAT: u8 = 0x09;
    /// See [`LEFT_SKILL_ON_UNIT`].
    pub const LEFT_SKILL_ON_UNIT_HOLD_REPEAT: u8 = 0x0A;
    /// Use the right skill on a unit; `0x0E`, `0x10` and `0x11` as the left skill's variants.
    pub const RIGHT_SKILL_ON_UNIT: u8 = 0x0D;
    /// See [`RIGHT_SKILL_ON_UNIT`].
    pub const RIGHT_SKILL_ON_UNIT_HOLD: u8 = 0x0E;
    /// See [`RIGHT_SKILL_ON_UNIT`].
    pub const RIGHT_SKILL_ON_UNIT_REPEAT: u8 = 0x10;
    /// See [`RIGHT_SKILL_ON_UNIT`].
    pub const RIGHT_SKILL_ON_UNIT_HOLD_REPEAT: u8 = 0x11;
    /// Interact with a unit: `[type u32][guid u32]` (9 bytes).
    pub const INTERACT: u8 = 0x13;
    /// Spend an attribute point: `[stat u16]` (3 bytes).
    pub const ADD_STAT_POINT: u8 = 0x3A;
    /// Leave the corpse and restart in town, after "You have died" (1 byte).
    pub const RESPAWN: u8 = 0x41;
    /// Pick an item up: `[unit type u32][item guid u32][to cursor u32]` (13 bytes; engine
    /// handler `0x0054AAD0`, then `0x00548B00` as for `0x13`).
    pub const PICK_UP_ITEM: u8 = 0x16;
    /// Start talking to an NPC whose menu opened: `[unit type u32][guid u32]` (9 bytes; engine
    /// handler `0x0054B930`).
    pub const NPC_TALK: u8 = 0x2F;
    /// Drop the cursor item: `[item guid u32]` (5 bytes; engine handler `0x0054AB40` →
    /// `0x00563C00`).
    pub const DROP_ITEM: u8 = 0x17;
    /// Put the cursor item into a grid: `[item guid u32][x u32][y u32][grid u32]` (17 bytes; grid 0
    /// the inventory, 3 the cube, 4 the stash; `0x0054ABB0` → `0x00560200`).
    pub const INSERT_ITEM: u8 = 0x18;
    /// Lift an item out of a grid onto the cursor: `[item guid u32]` (5 bytes; `0x0054ACD0` →
    /// `0x00560420`).
    pub const LIFT_ITEM: u8 = 0x19;
    /// Wear the cursor item: `[item guid u32][body location u32]` (9 bytes; `0x0054AD90` →
    /// `0x005606B0`).
    pub const EQUIP_ITEM: u8 = 0x1A;
    /// Take off a worn item onto the cursor: `[body location u16]` (3 bytes; `0x0054AEC0` →
    /// `0x00560CD0`).
    pub const UNEQUIP_ITEM: u8 = 0x1C;
    /// Wear the cursor item in place of a worn one, which goes to the cursor: `[item guid
    /// u32][body location u32]` (9 bytes; `0x0054AF50` → `0x00560F00`).
    pub const SWAP_EQUIPPED: u8 = 0x1D;
    /// Put the cursor item into a grid in place of the item there: `[cursor guid u32][grid item
    /// guid u32][x u32][y u32]` (17 bytes; `0x0054B0F0` → `0x00561B00`).
    pub const SWAP_GRID_ITEM: u8 = 0x1F;
    /// Identify an item with the scroll or tome made ready: `[item guid u32][scroll guid u32]` (9
    /// bytes; `0x0054B280` → `0x00561ED0`).
    pub const IDENTIFY_ITEM: u8 = 0x27;
    /// Put the cursor item in the belt: `[item guid u32][slot u32]` (9 bytes; `0x0054B3E0` →
    /// `0x0055E9B0`).
    pub const BELT_ITEM: u8 = 0x23;
    /// Lift a belt item onto the cursor: `[item guid u32]` (5 bytes; `0x0054B450` →
    /// `0x00562250`).
    pub const UNBELT_ITEM: u8 = 0x24;
    /// Put the cursor item in the belt in place of a belt item: `[cursor guid u32][belt item guid
    /// u32]` (9 bytes; `0x0054B4E0` → `0x0055EB30`).
    pub const SWAP_BELT_ITEM: u8 = 0x25;
    /// Use an item in the inventory: `[item guid u32][x u32][y u32]` (13 bytes; engine handler
    /// `0x0054B1E0` → `0x0055E170`).
    pub const USE_ITEM: u8 = 0x20;
    /// Use an item in the belt: `[item guid u32][u32][u32]` (13 bytes; engine handler
    /// `0x0054B560` → `0x00562390`).
    pub const USE_BELT_ITEM: u8 = 0x26;
    /// Close an NPC's menu: `[unit type u32][guid u32]` (9 bytes; engine handler `0x0054B9F0` →
    /// `0x00572F20`).
    pub const NPC_CANCEL: u8 = 0x30;
    /// Buy from a vendor: `[npc guid u32][item guid u32][u32: bit 31 fill, low word 0 trade or 2
    /// gamble][cost u32]` (17 bytes; engine handler `0x0054BAC0` → `0x00577F30`).
    pub const NPC_BUY: u8 = 0x32;
    /// Sell to a vendor: `[npc guid u32][item guid u32][item mode u16][u16][cost u32]` (17 bytes;
    /// engine handler `0x0054BB20` → `0x00579510`).
    pub const NPC_SELL: u8 = 0x33;
    /// Pick from an NPC's menu: `[action u32: 1 trade, 2 gamble, 3 hire][npc guid u32][u32]` (13
    /// bytes; engine handler `0x0054BCA0` → `0x00579D60`).
    pub const NPC_ACTION: u8 = 0x38;
    /// Travel by waypoint: `[waypoint guid u32][level u16][u16]` (9 bytes; engine handler
    /// `0x0054C5D0`).
    pub const WAYPOINT_TRAVEL: u8 = 0x49;
    /// Where the client has its player: `[x u16][y u16]` (5 bytes; engine handler
    /// `0x0054CD50` re-syncs the server's unit to it).
    pub const UPDATE_POSITION: u8 = 0x5F;
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

/// Quests the engine tracks (`0x00731888`) — one byte each in `0x5E`.
pub const QUESTS: usize = 37;
/// Bytes of quest flags per difficulty, as a `.d2s` and the game keep them.
pub const QUEST_FLAG_BYTES: usize = 96;

/// `0x5E`: one byte per quest, the quest object's `+9` (`0x00546270`; 1 for every quest when
/// the game allocates them, `0x00545D80`). The client copies it into its quest table and marks
/// the table loaded (`0x004B92B0`); entering a new area reads it and halts the client if it never
/// came (`0x004B92E0`, "failed at (96)").
#[must_use]
pub fn quest_states(states: &[u8; QUESTS]) -> Vec<u8> {
    let mut w = Writer::with_capacity(1 + QUESTS);
    w.u8(sc::QUEST_STATES).bytes(states);
    w.finish()
}

/// `0x28` type 6: `[6][u32 0][u8 0][flags 96]` — the player's quest flags for the game's
/// difficulty (builder `0x0053D670`, client `0x004B6DD0` copies them).
#[must_use]
pub fn player_quest_flags(flags: &[u8; QUEST_FLAG_BYTES]) -> Vec<u8> {
    let mut w = Writer::with_capacity(7 + QUEST_FLAG_BYTES);
    w.u8(sc::QUEST_FLAGS).u8(6).u32(0).u8(0).bytes(flags);
    w.finish()
}

/// `0x27`: `[unit type u8][guid u32][count u8][u8][8 × (message u16, flag u8, u8)]` — the quest
/// messages an NPC offers, sent first when a player talks to it (`0x00572C10`, list from
/// `0x00661480`). An NPC with nothing quest-related to say: a count and entries all zero.
#[must_use]
pub fn npc_no_quest_messages(unit_type: u8, guid: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(40);
    w.u8(sc::NPC_QUEST_MESSAGES).u8(unit_type).u32(guid).bytes(&[0; 34]);
    w.finish()
}

/// `0x28` type 1: `[1][npc guid u32][u8 0][flags 96]` — the player's quest flags, last of the
/// packets that open an NPC's dialog (`0x00572C10` → `0x0053D670`); the client looks the NPC
/// up by guid and opens its menu (`0x004B6DD0`).
#[must_use]
pub fn npc_dialog_quest_flags(npc_guid: u32, flags: &[u8; QUEST_FLAG_BYTES]) -> Vec<u8> {
    let mut w = Writer::with_capacity(7 + QUEST_FLAG_BYTES);
    w.u8(sc::QUEST_FLAGS).u8(1).u32(npc_guid).u8(0).bytes(flags);
    w.finish()
}

/// Bytes of a player's waypoint flags after their version word (`0x006610B0` copies 16).
pub const WAYPOINT_FLAG_BYTES: usize = 14;

/// `0x63`: `[waypoint guid u32][version u16 0x0102][flags 14]` — the waypoint menu, with the
/// waypoints the player has; `Levels.txt` `Waypoint` n is bit n of the flags, least significant
/// first (`0x00660EC0`, table `0x00746424`). Sent by the waypoint's `OperateFn` 23
/// (`0x00584E30`) once it is active.
#[must_use]
pub fn waypoint_menu(guid: u32, flags: &[u8; WAYPOINT_FLAG_BYTES]) -> Vec<u8> {
    let mut w = Writer::with_capacity(21);
    w.u8(sc::WAYPOINT_MENU).u32(guid).u16(0x0102).bytes(flags);
    w.finish()
}

/// `0x77`: `[action u8]` — open or close a panel; `0x10` opens the stash (`OperateFn` 32,
/// `0x00564CD0`).
#[must_use]
pub fn ui_action(action: u8) -> Vec<u8> {
    vec![sc::UI_ACTION, action]
}

/// `0x77` action: open the stash.
pub const UI_OPEN_STASH: u8 = 0x10;

/// `0x29`: `[flags 96]` — the game's quest flags (`0x00544520`, client `0x004B2620`).
#[must_use]
pub fn game_quest_flags(flags: &[u8; QUEST_FLAG_BYTES]) -> Vec<u8> {
    let mut w = Writer::with_capacity(1 + QUEST_FLAG_BYTES);
    w.u8(sc::GAME_QUEST_FLAGS).bytes(flags);
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

/// `0x08`: `[room tile x u16][room tile y u16][level u8]` — the client releases a room it was
/// told to load (builder `0x0053BC90`, sent by `0x0053A9B0` when a room drops out of the
/// player's near rooms, after its units' `0x0A`).
#[must_use]
pub fn unload_room(tile_x: u16, tile_y: u16, level: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(6);
    w.u8(sc::UNLOAD_ROOM).u16(tile_x).u16(tile_y).u8(level);
    w.finish()
}

/// `0x0A`: `[unit type u8][guid u32]` — the client forgets a unit (`0x00571600` →
/// `0x0053BDA0`; never sent for missiles).
#[must_use]
pub fn remove_unit(unit_type: u8, guid: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(6);
    w.u8(sc::REMOVE_UNIT).u8(unit_type).u32(guid);
    w.finish()
}

/// `0x09`: `[unit type u8 = 5][guid u32][class u8][x u16][y u16]` — a warp tile unit, which the
/// client makes clickable (`SendUnitToClient` `0x00571F90` builds it with `0x0053BCD0`; client
/// handler `0x0045CB90`). `class` is the `LvlWarp.txt` `Id`.
#[must_use]
pub fn assign_warp(guid: u32, class: u8, x: u16, y: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(11);
    w.u8(sc::ASSIGN_WARP).u8(5).u32(guid).u8(class).u16(x).u16(y);
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

/// `0x1D`/`0x1E`/`0x1F`: `[stat u8][value]`, the smallest of byte, word and dword the value
/// fits below its all-ones (builder `0x0053BE40`). The client sets the stat on its own player
/// (handler `0x0045D780`), so this must follow `0x0B`. Stat `0xFF` is refused by the engine.
#[must_use]
pub fn set_stat(stat: u8, value: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(6);
    match value {
        v if v < 0xFF => w.u8(sc::SET_STAT_BYTE).u8(stat).u8(v as u8),
        v if v < 0xFFFF => w.u8(sc::SET_STAT_WORD).u8(stat).u16(v as u16),
        v => w.u8(sc::SET_STAT_DWORD).u8(stat).u32(v),
    };
    w.finish()
}

/// Item actions. `0x9C` (client `0x0045EB10`) and `0x9D` (`0x0045EC70`, which names the owning
/// unit) take disjoint sets, and the client halts on an action its packet does not take.
pub mod item_action {
    /// `0x9C`: made on the ground (`0x004C25B0`).
    pub const ADD_TO_GROUND: u8 = 0x00;
    /// `0x9C`: from the ground onto the cursor (`0x004C2650`; the item must be in mode 4).
    pub const GROUND_TO_CURSOR: u8 = 0x01;
    /// `0x9C`: dropped from the cursor (`0x004C26F0`).
    pub const DROP_TO_GROUND: u8 = 0x02;
    /// `0x9D`: worn (`0x004C2E90`).
    pub const EQUIP: u8 = 0x06;
    /// `0x9D`: taken off onto the cursor (`0x004C3380`).
    pub const UNEQUIP: u8 = 0x08;
    /// `0x9D`: an item's bits again, for the client to rebuild it (`0x004C4C70`; after identifying).
    pub const UPDATE: u8 = 0x15;
    /// `0x9D`: a worn item and the cursor item trade places, one packet each: item flag `0x40` on
    /// the one worn, `0x80` on the one lifted (`0x004C3920`).
    pub const SWAP_BODY: u8 = 0x09;
    /// `0x9C`: the cursor item and a grid item trade places (`0x004C40D0`).
    pub const SWAP_IN_CONTAINER: u8 = 0x0D;
    /// `0x9C`: the cursor item and a belt item trade places (`0x004C45C0`).
    pub const SWAP_IN_BELT: u8 = 0x10;
    /// `0x9C`: into a grid — made there when the client has no such unit (`0x004C2AD0`).
    pub const PUT_IN_CONTAINER: u8 = 0x04;
    /// `0x9C`: into a belt slot (`0x004C4130`).
    pub const PUT_IN_BELT: u8 = 0x0E;
    /// `0x9C`: out of the belt (`0x004C42A0`).
    pub const REMOVE_FROM_BELT: u8 = 0x0F;
    /// `0x9D`: out of a grid (`0x004C2C80`).
    pub const REMOVE_FROM_CONTAINER: u8 = 0x05;
    /// `0x9C`: into the open trade window's stock, at the page and cell its bits give
    /// (`0x004C3C00`; per-frame unit flag 4, `0x0053EF30`).
    pub const ADD_TO_STORE: u8 = 0x0B;
    /// `0x9C`: out of the open trade window's stock (`0x004C3C00`; unit flag `0x10`).
    pub const REMOVE_FROM_STORE: u8 = 0x0C;
}

/// `0x2A` kinds and results, as the vendor routines send them.
pub mod transaction {
    /// Kind of a refusal.
    pub const REFUSED: u8 = 0;
    /// Kind of an item sold to the player (`0x00577830`).
    pub const BOUGHT: u8 = 4;
    /// Kind of an item the player sold (`0x00579510`).
    pub const SOLD: u8 = 3;
    /// Result: done.
    pub const OK: u8 = 0;
    /// Result of a sale: done.
    pub const SOLD_OK: u8 = 1;
    /// Result: no such item or vendor.
    pub const NO_ITEM: u8 = 7;
    /// Result: the trade cannot be made.
    pub const CANNOT: u8 = 9;
    /// Result: no room for what was bought.
    pub const NO_ROOM: u8 = 10;
    /// Result: the store is not open.
    pub const NOT_OPEN: u8 = 11;
    /// Result: not enough gold.
    pub const NO_GOLD: u8 = 12;
}

/// Item flags as the item bits carry them.
pub mod item_flags {
    /// Identified.
    pub const IDENTIFIED: u32 = 0x10;
    /// Set on the packet that takes out an item used up (`0x00561E70`, `0x0055E000`).
    pub const USED: u32 = 0x20;
    /// Just placed in the world: the client plays the fall and its sound.
    pub const DROPPED: u32 = 0x2000;
    /// Simple (`compactsave`): no quality, stats or sockets follow the code.
    pub const COMPACT: u32 = 0x0020_0000;
    /// On every item written (`0x006312B0`).
    pub const WRITTEN: u32 = 0x0080_0000;
    /// A simple identified item.
    pub const SIMPLE: u32 = IDENTIFIED | COMPACT | WRITTEN;
}

/// Where a simple item is, as its bits say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSpot {
    /// On the ground (mode 3) at world subtiles.
    Ground {
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
    },
    /// Stored in a grid (mode 0): column, row and the page (0 the inventory).
    Stored {
        /// Column.
        col: u8,
        /// Row.
        row: u8,
        /// Page: 0 the inventory, 3 the cube, 4 the stash.
        page: u8,
    },
    /// In a belt slot (mode 2).
    Belt {
        /// Slot, 0 at the bottom left.
        slot: u8,
    },
}

/// A simple item's bits (`0x0062AF80`, read back by `0x0062A970`): flags 32, version 10, mode 3,
/// then `x`/`y` 16 each on the ground or body location 4, column 4, row 4 and page + 1 3 in a
/// grid or belt (a belt slot is its column), the code 32, and for gold a one-bit width flag and the
/// amount in 12 or 32 bits.
fn simple_item_bits(flags: u32, version: u16, spot: ItemSpot, code: [u8; 4], gold: Option<u32>) -> BitWriter {
    let mut w = BitWriter::with_bytes(24);
    w.put(flags, 32).put(u32::from(version.min(0x3FF)), 10);
    match spot {
        ItemSpot::Ground { x, y } => {
            w.put(3, 3).put(u32::from(x), 16).put(u32::from(y), 16);
        }
        ItemSpot::Stored { col, row, page } => {
            w.put(0, 3).put(0, 4).put(u32::from(col.min(15)), 4).put(u32::from(row.min(15)), 4).put(u32::from(page.min(6)) + 1, 3);
        }
        ItemSpot::Belt { slot } => {
            w.put(2, 3).put(0, 4).put(u32::from(slot.min(15)), 4).put(0, 4).put(0, 3);
        }
    }
    w.put(u32::from_le_bytes(code), 32);
    if let Some(amount) = gold {
        let big = amount > 0xFFF;
        w.put(u32::from(big), 1).put(amount, if big { 32 } else { 12 });
    }
    w
}

/// `0x9C`: `[0x9C][action][size][category][guid u32]` and the item's bits. `category` is the
/// item's `Misc.txt` `component` (`0x00628660`; 16 for potions, scrolls and gems).
#[must_use]
pub fn item_world(action: u8, category: u8, guid: u32, flags: u32, version: u16, spot: ItemSpot, code: [u8; 4]) -> Vec<u8> {
    item_packet(sc::ITEM_ACTION_WORLD, action, category, guid, None, simple_item_bits(flags, version, spot, code, None))
}

/// `0x9D`: as `0x9C` with the owning player's `[unit type u8 0][guid u32]` after the item's guid
/// (`0x0053CEF0`).
#[must_use]
#[allow(clippy::too_many_arguments)] // the packet's own fields
pub fn item_owned(action: u8, category: u8, guid: u32, owner: u32, flags: u32, version: u16, spot: ItemSpot, code: [u8; 4]) -> Vec<u8> {
    item_packet(sc::ITEM_ACTION_OWNED, action, category, guid, Some(owner), simple_item_bits(flags, version, spot, code, None))
}

fn item_packet(opcode: u8, action: u8, category: u8, guid: u32, owner: Option<u32>, bits: BitWriter) -> Vec<u8> {
    item_packet_bytes(opcode, action, category, guid, owner, &bits.bytes[..bits.bytes_used()])
}

fn item_packet_bytes(opcode: u8, action: u8, category: u8, guid: u32, owner: Option<u32>, bits: &[u8]) -> Vec<u8> {
    let mut p = vec![opcode, action, 0, category];
    p.extend_from_slice(&guid.to_le_bytes());
    if let Some(owner) = owner {
        p.push(0);
        p.extend_from_slice(&owner.to_le_bytes());
    }
    p.extend_from_slice(bits);
    p[2] = p.len().min(255) as u8;
    p
}

/// `0x3F`: an item spell (`icon`, the `Books.txt` `SpellIcon`) waits for its target; `skill` is the
/// book's skill (`0x005BE130`).
#[must_use]
pub fn item_spell_ready(icon: u8, guid: u32, skill: u16) -> Vec<u8> {
    let mut p = vec![sc::ITEM_SPELL_READY, icon];
    p.extend_from_slice(&guid.to_le_bytes());
    p.extend_from_slice(&skill.to_le_bytes());
    p
}

/// `0x2A`: a trade settled — `kind` and `result` ([`transaction`]), the item (`u32::MAX` for none)
/// and the player's gold after it. The engine leaves bytes 3–6 as they were on its stack; they are
/// sent as 0.
#[must_use]
pub fn npc_transaction(kind: u8, result: u8, guid: u32, gold: u32) -> Vec<u8> {
    let mut p = vec![sc::NPC_TRANSACTION, kind, result, 0, 0, 0, 0];
    p.extend_from_slice(&guid.to_le_bytes());
    p.extend_from_slice(&gold.to_le_bytes());
    p
}

/// `0x9C` carrying an item's bits as written elsewhere (a full item's: quality, stats, sockets).
#[must_use]
pub fn item_world_bits(action: u8, category: u8, guid: u32, bits: &[u8]) -> Vec<u8> {
    item_packet_bytes(sc::ITEM_ACTION_WORLD, action, category, guid, None, bits)
}

/// `0x9D` carrying an item's bits as written elsewhere, for the player owning it.
#[must_use]
pub fn item_owned_bits(action: u8, category: u8, guid: u32, owner: u32, bits: &[u8]) -> Vec<u8> {
    item_packet_bytes(sc::ITEM_ACTION_OWNED, action, category, guid, Some(owner), bits)
}

/// `0x9C` action 0 for a gold pile at `(x, y)` (flags identified, simple and written, with
/// [`item_flags::DROPPED`] when `dropping`; version as the game's). `dropping` false leaves a
/// pile that is just there. The category is sent as 0; the client does not read it for action 0.
#[must_use]
pub fn ground_gold(guid: u32, x: u16, y: u16, amount: u32, dropping: bool, version: u16) -> Vec<u8> {
    let flags = item_flags::SIMPLE | if dropping { item_flags::DROPPED } else { 0 };
    let bits = simple_item_bits(flags, version, ItemSpot::Ground { x, y }, *b"gld ", Some(amount));
    item_packet(sc::ITEM_ACTION_WORLD, item_action::ADD_TO_GROUND, 0, guid, None, bits)
}

/// A player's gold (stat 14) going from `old` to `new`, as the engine tells its client
/// (`0x0053E9B0`): a gain of 1–254 is `0x19 [gain]`; anything else sets the stat outright.
#[must_use]
pub fn gold_update(old: u32, new: u32) -> Vec<u8> {
    match new.wrapping_sub(old) {
        gain @ 1..=254 => vec![sc::ADD_GOLD_BYTE, gain as u8],
        _ => set_stat(14, new),
    }
}

/// Fog's bit buffer (`0x00410EB0`): values packed least significant bit first.
struct BitWriter {
    bytes: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn with_bytes(len: usize) -> Self {
        Self { bytes: vec![0; len], bit: 0 }
    }

    /// Whole bytes touched, as `0x00410E90` counts them.
    fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }

    fn put(&mut self, value: u32, bits: u32) -> &mut Self {
        for i in 0..bits {
            if value >> i & 1 != 0 {
                self.bytes[self.bit / 8] |= 1 << (self.bit % 8);
            }
            self.bit += 1;
        }
        self
    }
}

/// `0x95`: `[life u15][mana u15][stamina u15][x u16][y u16][dx i8][dy i8]`, bit-packed (builder
/// `0x0053C320`), life/mana/stamina in whole points. The client sets its player's life, mana and
/// stamina and nudges its position to `(x + dx, y + dy)` (handler `0x0045DB20`); the engine's
/// first one, before the player is placed, has position zero.
#[must_use]
pub fn life_and_position(life: u16, mana: u16, stamina: u16, x: u16, y: u16, dx: i8, dy: i8) -> Vec<u8> {
    let mut w = BitWriter::with_bytes(13);
    w.put(u32::from(sc::LIFE_AND_POSITION), 8)
        .put(u32::from(life), 15)
        .put(u32::from(mana), 15)
        .put(u32::from(stamina), 15)
        .put(u32::from(x), 16)
        .put(u32::from(y), 16)
        .put(u32::from(dx as u8), 8)
        .put(u32::from(dy as u8), 8);
    w.bytes
}

/// `0x51`: `[unit type u8][guid u32][class u16][x u16][y u16][mode u8][interaction u8]` (builder
/// `0x0053BD10`, from `SendUnitToClient` with type 2 and the object data's byte `+4`).
#[must_use]
pub fn assign_object(guid: u32, class: u16, x: u16, y: u16, mode: u8, interaction: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(14);
    w.u8(sc::ASSIGN_OBJECT).u8(2).u32(guid).u16(class).u16(x).u16(y).u8(mode).u8(interaction);
    w.finish()
}

/// `0x0E`: `[unit type u8 = 2][guid u32][3][selectable u8][mode u32]`, an object's new mode
/// (`0x0053B470`, from `OBJECT_SendStateToClient` `0x00581A20` when the object is flagged for
/// update); `selectable` is the unit's flag bit 1.
#[must_use]
pub fn object_state(guid: u32, selectable: bool, mode: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(12);
    w.u8(sc::OBJECT_STATE).u8(2).u32(guid).u8(3).u8(selectable.into()).u32(mode);
    w.finish()
}

/// Bits a graphics component's value takes in `0xAC`: one below three variants, else enough for
/// `variants - 1` (`0x0053E2E0`, and the client's `0x0045F190` reads it the same way).
#[must_use]
pub fn component_bits(variants: u8) -> u32 {
    if variants < 3 {
        1
    } else {
        u8::BITS - (variants - 1).leading_zeros()
    }
}

/// `0xAC` for a plain monster or NPC (builder `0x0053E2E0`): `[guid u32][class u16][x u16][y u16]
/// [life u8][total length u8]`, then Fog bits — the unit mode in 4 (the engine sends modes 0, 8,
/// 9 and 12 as they are and anything else as 1), a flag and the 16 graphics components when any
/// is non-zero, each in [`component_bits`] of its class's variant count, then three clear flags:
/// no champion/unique block, no owner, no stats. `life` is 128ths of full (`0x80` at full).
#[must_use]
#[allow(clippy::too_many_arguments)] // the packet's own fields
pub fn assign_monster(guid: u32, class: u16, x: u16, y: u16, life: u8, mode: u8, components: &[u8; 16], variants: &[u8; 16]) -> Vec<u8> {
    let mut bits = BitWriter::with_bytes(0xF4);
    bits.put(u32::from(if matches!(mode, 0 | 8 | 9 | 12) { mode } else { 1 }), 4);
    if components.iter().any(|&c| c != 0) {
        bits.put(1, 1);
        for (&value, &count) in components.iter().zip(variants) {
            bits.put(u32::from(value), component_bits(count));
        }
    } else {
        bits.put(0, 1);
    }
    bits.put(0, 1).put(0, 1).put(0, 1);
    let body = &bits.bytes[..bits.bytes_used()];
    let mut w = Writer::with_capacity(13 + body.len());
    w.u8(sc::ASSIGN_MONSTER).u32(guid).u16(class).u16(x).u16(y).u8(life).u8((13 + body.len()) as u8).bytes(body);
    w.finish()
}

/// `0xAA` for a unit with no states set (`0x00570E30`): `[unit type u8][guid u32][total length
/// u8]` and the state list's terminator, `0xFF` in 8 bits. `SendUnitToClient` sends one after
/// every monster.
#[must_use]
pub fn no_unit_states(unit_type: u8, guid: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(8);
    w.u8(sc::UNIT_STATES).u8(unit_type).u32(guid).u8(8).u8(0xFF);
    w.finish()
}

/// The alignment state (`states.txt` 105) and its stat (`ItemStatCost.txt` 172, `Send Bits` 2).
pub const ALIGNMENT_STATE: u32 = 0x69;
const ALIGNMENT_STAT: u32 = 172;

/// `0xAA` for a unit whose only state is its alignment (`0x00570E30`): state 105, then its stat
/// list — stat 172 with the alignment in 2 bits, ended by `0x1FF` — or, for alignment 0, which
/// the stat list does not keep, a clear flag; then `0xFF`. The client reads a unit's alignment
/// only from this state (`0x006259B0`) and will not let two units of the same alignment attack
/// each other (`0x00650D70`). The engine gives a player 2 (`0x005348C0`) and a monster its
/// `MonStats.txt` `Align`: 1 → 2, 2 → 1, else 0 (`0x005B2A00` → `0x005543B0`).
#[must_use]
pub fn alignment_state(unit_type: u8, guid: u32, alignment: u8) -> Vec<u8> {
    let mut bits = BitWriter::with_bytes(8);
    bits.put(ALIGNMENT_STATE, 8);
    if alignment == 0 {
        bits.put(0, 1);
    } else {
        bits.put(1, 1).put(ALIGNMENT_STAT, 9).put(u32::from(alignment.min(3)), 2).put(0x1FF, 9);
    }
    bits.put(0xFF, 8);
    let body = &bits.bytes[..bits.bytes_used()];
    let mut w = Writer::with_capacity(7 + body.len());
    w.u8(sc::UNIT_STATES).u8(unit_type).u32(guid).u8((7 + body.len()) as u8).bytes(body);
    w.finish()
}

/// `0x6D`: `[guid u32][x u16][y u16][life u8]` (builder `0x0053BB70`) — what `0x00597E20` sends
/// for a monster in its neutral mode, standing still.
#[must_use]
pub fn monster_standing(guid: u32, x: u16, y: u16, life: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(10);
    w.u8(sc::MONSTER_STANDING).u32(guid).u16(x).u16(y).u8(life);
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

/// `0xAB`: `[unit type u8][guid u32][life u8]` (builder `0x0053C150`) — a unit's life bar,
/// 128ths of full (client handler `0x0045F120` sets its life stat from it).
#[must_use]
pub fn unit_life(unit_type: u8, guid: u32, life: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(7);
    w.u8(sc::UNIT_LIFE).u8(unit_type).u32(guid).u8(life);
    w.finish()
}

/// `0x69`: `[guid u32][event u8][x u16][y u16][life u8][flag u8]` (builder `0x0053BA40`) — a
/// monster reacts: `event` is the unit event code (`0x06` get-hit, `0x08` dying, `0x09` dead),
/// `flag` `0x03` while alive and 0 on the dead frame, as a recorded retail kill carries them.
#[must_use]
pub fn monster_reaction(guid: u32, event: u8, x: u16, y: u16, life: u8, alive: bool) -> Vec<u8> {
    let mut w = Writer::with_capacity(12);
    w.u8(sc::MONSTER_REACTION).u32(guid).u8(event).u16(x).u16(y).u8(life).u8(if alive { 3 } else { 0 });
    w.finish()
}

/// `0x67`: `[guid u32][01][x u16][y u16][01][00][0D][velocity percent u16][05]` — a monster walks
/// to (`x`, `y`) at its class speed times `percent` / 100; the client finds its own way there
/// (handler `0x0045CDE0`) and sets the monster's stat 67 to `percent` (`0x004AFF60`). The other
/// fixed bytes are a recorded retail monster walk's (which carried 75).
#[must_use]
pub fn monster_walk(guid: u32, x: u16, y: u16, percent: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(16);
    w.u8(sc::MONSTER_WALK).u32(guid).u8(1).u16(x).u16(y).u8(1).u8(0).u8(0x0D).u16(percent).u8(5);
    w.finish()
}

/// `0x6C`: `[guid u32][10][00][target guid u32][00][x u16][y u16]` (builder `0x0053BAA0`) — a
/// monster standing at (`x`, `y`) swings at a unit; the client asserts its position (handler
/// `0x0045CFB0`). The fixed bytes are a recorded retail melee attack's.
#[must_use]
pub fn monster_attack(guid: u32, target: u32, x: u16, y: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(16);
    w.u8(sc::MONSTER_ATTACK).u32(guid).u8(0x10).u8(0).u32(target).u8(0).u16(x).u16(y);
    w.finish()
}

/// `0x0D`: `[unit type u8][guid u32][event u8][x u16][y u16][state u8][seed u8]` (builder
/// `0x0053B4B0`) — a player reacts (client handler `0x0045CCC0`): `0x06` get-hit, `0x13` a small
/// hit's sound, `0x08` dying (the client's own player also gets "You have died"), `0x09` the
/// corpse. A zero position leaves the client's idea of it alone (`0x004804E0`); the trailing
/// bytes are a recorded retail hit's (`03 60`) while alive and zero for death.
#[must_use]
pub fn player_reaction(unit_type: u8, guid: u32, event: u8, x: u16, y: u16) -> Vec<u8> {
    let alive = !matches!(event, 0x08 | 0x09);
    let mut w = Writer::with_capacity(13);
    w.u8(sc::PLAYER_REACTION).u8(unit_type).u32(guid).u8(event).u16(x).u16(y).u8(if alive { 3 } else { 0 }).u8(if alive { 0x60 } else { 0 });
    w.finish()
}

/// Experience from `old` to `new` the way `0x0053BDD0` sends it: the gain in a byte (`0x1A`)
/// below `0xFF`, in a word (`0x1B`) below `0xFFFF`, else the new total (`0x1C`).
#[must_use]
pub fn experience(old: u32, new: u32) -> Vec<u8> {
    let gain = new.wrapping_sub(old);
    let mut w = Writer::with_capacity(5);
    match gain {
        g if new >= old && g < 0xFF => w.u8(sc::ADD_EXPERIENCE_BYTE).u8(g as u8),
        g if new >= old && g < 0xFFFF => w.u8(sc::ADD_EXPERIENCE_WORD).u16(g as u16),
        _ => w.u8(sc::SET_EXPERIENCE).u32(new),
    };
    w.finish()
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

    #[test]
    fn gold_updates_add_small_gains_and_set_the_rest() {
        assert_eq!(gold_update(10, 14), [0x19, 4]);
        assert_eq!(gold_update(0, 254), [0x19, 254]);
        assert_eq!(gold_update(0, 255), set_stat(14, 255));
        assert_eq!(gold_update(100, 40), [0x1D, 14, 40], "a loss sets it");
        assert_eq!(gold_update(0, 70_000), [0x1F, 14, 0x70, 0x11, 0x01, 0x00]);
    }

    #[test]
    fn a_gold_pile_is_a_simple_ground_item() {
        let p = ground_gold(0x1234, 5000, 5700, 37, true, 101);
        assert_eq!(&p[..8], &[0x9C, 0, p.len() as u8, 0, 0x34, 0x12, 0, 0]);
        assert_eq!(p.len(), 8 + 16, "122 bits");
        // Flags 0x00A02010, then version 101 in the next 10 bits: the same leading bytes as a
        // retail ground-to-inventory item's `10 00 a0 00 65 00` apart from the drop bit.
        assert_eq!(&p[8..13], &[0x10, 0x20, 0xA0, 0x00, 0x65]);
        let bit = |i: usize| (p[8 + i / 8] >> (i % 8)) & 1;
        let field = |from: usize, len: usize| (0..len).map(|i| u32::from(bit(from + i)) << i).sum::<u32>();
        assert_eq!(field(42, 3), 3, "mode ground");
        assert_eq!((field(45, 16), field(61, 16)), (5000, 5700));
        assert_eq!(field(77, 32).to_le_bytes(), *b"gld ");
        assert_eq!((field(109, 1), field(110, 12)), (0, 37));
        let big = ground_gold(1, 0, 0, 5000, false, 101);
        assert_eq!(big.len(), 8 + 18, "142 bits");
        assert_eq!(big[9] & 0x20, 0, "no drop animation");
    }

    #[test]
    fn simple_items_match_the_retail_pickup() {
        // Retail, picking up a Light Healing Potion into the inventory's column 9, row 3:
        // `9c 04 14 10 2b0b3efa 1000a0006500728206270302`.
        let p = item_world(item_action::PUT_IN_CONTAINER, 16, 0xFA3E_0B2B, item_flags::SIMPLE, 101, ItemSpot::Stored { col: 9, row: 3, page: 0 }, *b"hp2 ");
        assert_eq!(p, [0x9C, 0x04, 0x14, 0x10, 0x2B, 0x0B, 0x3E, 0xFA, 0x10, 0x00, 0xA0, 0x00, 0x65, 0x00, 0x72, 0x82, 0x06, 0x27, 0x03, 0x02]);
        let bit = |p: &[u8], i: usize| (p[i / 8] >> (i % 8)) & 1;
        let field = |p: &[u8], from: usize, len: usize| (0..len).map(|i| u32::from(bit(p, from + i)) << i).sum::<u32>();
        let belt = item_world(item_action::PUT_IN_BELT, 16, 7, item_flags::SIMPLE, 2, ItemSpot::Belt { slot: 3 }, *b"rvs ");
        assert_eq!((belt[2] as usize, belt.len()), (20, 20));
        let body = &belt[8..];
        assert_eq!((field(body, 32, 10), field(body, 42, 3), field(body, 45, 4), field(body, 49, 4), field(body, 53, 4), field(body, 57, 3)), (2, 2, 0, 3, 0, 0), "classic version, belt mode, slot as the column");
        let owned = item_owned(item_action::REMOVE_FROM_CONTAINER, 16, 7, 1, item_flags::SIMPLE | item_flags::USED, 101, ItemSpot::Stored { col: 0, row: 1, page: 0 }, *b"mp1 ");
        assert_eq!(&owned[..13], &[0x9D, 0x05, 25, 0x10, 7, 0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(owned.len(), 25);
        assert_eq!(field(&owned[13..], 0, 32), 0x00A0_0030);
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
        assert_eq!(unload_room(1160, 888, 1), vec![0x08, 0x88, 0x04, 0x78, 0x03, 1]);
        assert_eq!(remove_unit(1, 5), vec![0x0A, 1, 5, 0, 0, 0]);
        assert_eq!(select_skill(0, 1, true, 0, u32::MAX).len(), 13);
        assert_eq!(set_stat(12, 1), vec![0x1D, 12, 1]);
        assert_eq!(set_stat(7, 55 << 8), vec![0x1E, 7, 0x00, 0x37]);
        assert_eq!(set_stat(13, 0xFFFF), vec![0x1F, 13, 0xFF, 0xFF, 0, 0], "0xFFFF itself needs the dword");
        assert_eq!(player_placed().len(), 5);
        assert_eq!(quest_states(&[1; QUESTS]).len(), 38);
        assert_eq!(&player_quest_flags(&[0; QUEST_FLAG_BYTES])[..7], &[0x28, 6, 0, 0, 0, 0, 0]);
        assert_eq!(player_quest_flags(&[0; QUEST_FLAG_BYTES]).len(), 103);
        assert_eq!(game_quest_flags(&[0; QUEST_FLAG_BYTES]).len(), 97);
        assert_eq!(npc_no_quest_messages(1, 5).len(), 40);
        assert_eq!(&npc_dialog_quest_flags(5, &[0; QUEST_FLAG_BYTES])[..7], &[0x28, 1, 5, 0, 0, 0, 0]);
        let mut known = [0u8; WAYPOINT_FLAG_BYTES];
        known[0] = 1;
        let menu = waypoint_menu(3, &known);
        assert_eq!((menu.len(), &menu[5..8]), (21, &[0x02, 0x01, 0x01][..]), "version 0x0102, then bit 0: the camp");
        assert_eq!(ui_action(UI_OPEN_STASH), vec![0x77, 0x10]);
        assert_eq!(pong().len(), 33);
        assert_eq!(join_failed_packet(join_failed::WRONG_VERSION), vec![0xB4, 0x10, 0, 0, 0]);
    }

    /// Read `bits` bits LSB-first starting at bit `from`.
    fn bits_at(p: &[u8], from: usize, bits: usize) -> u32 {
        (0..bits).fold(0, |v, i| v | u32::from(p[(from + i) / 8] >> ((from + i) % 8) & 1) << i)
    }

    #[test]
    fn units_are_packed_as_the_engine_builds_them() {
        assert_eq!(assign_object(3, 267, 5806, 4444, 0, 0), vec![0x51, 2, 3, 0, 0, 0, 0x0B, 0x01, 0xAE, 0x16, 0x5C, 0x11, 0, 0]);
        assert_eq!(object_state(5, true, 1), vec![0x0E, 2, 5, 0, 0, 0, 3, 1, 1, 0, 0, 0]);
        assert_eq!(no_unit_states(1, 9), vec![0xAA, 1, 9, 0, 0, 0, 8, 0xFF]);
        // As the retail server sends them: an evil monster, and a good player or town NPC.
        assert_eq!(alignment_state(1, 9, 0), vec![0xAA, 1, 9, 0, 0, 0, 0x0A, 0x69, 0xFE, 0x01]);
        assert_eq!(alignment_state(0, 9, 2), vec![0xAA, 0, 9, 0, 0, 0, 0x0C, 0x69, 0x59, 0xF9, 0xFF, 0x1F]);
        assert_eq!(monster_standing(9, 1, 2, 0x80), vec![0x6D, 9, 0, 0, 0, 1, 0, 2, 0, 0x80]);
        assert_eq!((component_bits(0), component_bits(2), component_bits(3), component_bits(4), component_bits(5)), (1, 1, 2, 2, 3));

        // Standing, no components: mode 1 then four clear flags — one byte.
        let plain = assign_monster(7, 148, 5872, 4421, 0x80, 1, &[0; 16], &[1; 16]);
        assert_eq!(plain, vec![0xAC, 7, 0, 0, 0, 148, 0, 0xF0, 0x16, 0x45, 0x11, 0x80, 14, 0x01]);
        assert_eq!(assign_monster(7, 148, 0, 0, 0x80, 2, &[0; 16], &[1; 16])[13], 0x01, "walking is sent as neutral");
        assert_eq!(assign_monster(7, 148, 0, 0, 0x80, 12, &[0; 16], &[1; 16])[13], 0x0C, "dead stays dead");

        // A rogue with the second bow: components flagged, 16 values at their widths.
        let mut components = [0u8; 16];
        components[6] = 1;
        let mut variants = [1u8; 16];
        variants[6] = 2;
        variants[8] = 5;
        let p = assign_monster(7, 152, 0, 0, 0x80, 1, &components, &variants);
        let body_bits: usize = 4 + 1 + 15 + 3 + 3;
        assert_eq!(usize::from(p[12]), p.len());
        assert_eq!(p.len(), 13 + body_bits.div_ceil(8));
        assert_eq!(bits_at(&p, 104, 4), 1);
        assert_eq!(bits_at(&p, 108, 1), 1, "components follow");
        assert_eq!(bits_at(&p, 109 + 6, 1), 1, "LH, one bit");
        assert_eq!(bits_at(&p, 109 + 8, 3), 0, "S1 takes three bits");
        assert_eq!(bits_at(&p, 109 + 18, 3), 0, "no type flags, owner or stats");
    }

    #[test]
    fn life_and_position_packs_least_significant_bit_first() {
        let p = life_and_position(55, 15, 89, 0, 0, 0, 0);
        assert_eq!((p.len(), p[0]), (13, 0x95));
        assert_eq!((bits_at(&p, 8, 15), bits_at(&p, 23, 15), bits_at(&p, 38, 15)), (55, 15, 89));
        let p = life_and_position(1, 2, 3, 5810, 4450, -1, 2);
        assert_eq!((bits_at(&p, 53, 16), bits_at(&p, 69, 16), bits_at(&p, 85, 8), bits_at(&p, 93, 8)), (5810, 4450, 0xFF, 2));
    }

    /// The fight packets reproduce a recorded retail Blood Moor fight (bnemu
    /// `docs/d2/re/combat.md`, MIT, used with permission).
    #[test]
    fn fight_packets_match_a_recorded_retail_fight() {
        assert_eq!(unit_life(1, 0xA4E8_2DF0, 0x20), [0xAB, 0x01, 0xF0, 0x2D, 0xE8, 0xA4, 0x20]);
        assert_eq!(
            monster_reaction(0xA4E8_2DF0, 0x06, 0x1247, 0x124D, 0x1F, true),
            [0x69, 0xF0, 0x2D, 0xE8, 0xA4, 0x06, 0x47, 0x12, 0x4D, 0x12, 0x1F, 0x03]
        );
        assert_eq!(
            monster_reaction(0xA4E8_2DF0, 0x09, 0x1247, 0x124D, 0x14, false),
            [0x69, 0xF0, 0x2D, 0xE8, 0xA4, 0x09, 0x47, 0x12, 0x4D, 0x12, 0x14, 0x00]
        );
        assert_eq!(
            monster_walk(0xDA93_90B7, 0x1282, 0x126F, 75),
            [0x67, 0xB7, 0x90, 0x93, 0xDA, 0x01, 0x82, 0x12, 0x6F, 0x12, 0x01, 0x00, 0x0D, 0x4B, 0x00, 0x05]
        );
        assert_eq!(
            monster_attack(0xDA93_90B7, 0x7F0B_15F3, 0x127E, 0x1272),
            [0x6C, 0xB7, 0x90, 0x93, 0xDA, 0x10, 0x00, 0xF3, 0x15, 0x0B, 0x7F, 0x00, 0x7E, 0x12, 0x72, 0x12]
        );
        assert_eq!(player_reaction(0, 1, 0x13, 0x1247, 0x124D), [0x0D, 0x00, 0x01, 0x00, 0x00, 0x00, 0x13, 0x47, 0x12, 0x4D, 0x12, 0x03, 0x60]);
        assert_eq!(&player_reaction(0, 1, 0x08, 0, 0)[11..], &[0, 0], "death frames carry no state");
        assert_eq!(experience(10, 30), [0x1A, 20]);
        assert_eq!(experience(10, 10 + 0xFF), [0x1B, 0xFF, 0x00]);
        assert_eq!(experience(0, 0x1_0000), [0x1C, 0x00, 0x00, 0x01, 0x00]);
    }
}
