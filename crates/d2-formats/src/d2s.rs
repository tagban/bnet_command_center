//! `.d2s` character saves, as 1.14d writes them (version `0x60`).
//!
//! A played save is a 335-byte header followed by fixed sections: quests (`Woo!`, 298 bytes),
//! waypoints (`WS`, 81), NPC introductions (`w4`, 51), the attribute bit list (`gf`, varying),
//! skills (`if`, 32) and the items (`JM` to the end of the file). Sections whose fields are not
//! modelled are carried as bytes, so an unchanged save writes back byte for byte.
//!
//! Ported from libd2 `packages/formats/src/d2s.zig` and `packages/save/src/{sections,attributes}.zig`
//! (MIT, © 2026 jaenster); the checksum is `Game.exe`'s `CalculateChecksum` (`0x00411130`) as libd2
//! documents it.

use std::fmt;

/// Header length.
pub const HEADER_LEN: usize = 0x14F;
/// `0xAA55AA55`.
pub const SIGNATURE: u32 = 0xAA55_AA55;
/// 1.14d's save version.
pub const VERSION: u32 = 0x60;
const QUEST_LEN: usize = 298;
const WAYPOINT_LEN: usize = 81;
const NPC_LEN: usize = 51;
const SKILL_LEN: usize = 32;
/// Bits each saved stat takes (`ItemStatCost.txt` `CSvBits`), ids 0–15.
const STAT_BITS: [u32; 16] = [10, 10, 10, 10, 10, 8, 21, 21, 21, 21, 21, 21, 7, 32, 25, 25];
const STAT_END: u32 = 0x1FF;
/// Item flag: saved and sent without quality, stats or sockets (`compactsave`).
pub const ITEM_COMPACT: u32 = 0x0020_0000;
/// Item flag: an ear, which carries a name instead of a code.
const ITEM_EAR: u32 = 0x0001_0000;

/// Why a save could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Shorter than its sections.
    Truncated,
    /// Not `0xAA55AA55`, or not version `0x60`.
    NotASave,
    /// A section marker is not where it belongs.
    BadMarker(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str(".d2s: truncated"),
            Self::NotASave => f.write_str(".d2s: not a 1.14d save"),
            Self::BadMarker(m) => write!(f, ".d2s: no {m} section"),
        }
    }
}

impl std::error::Error for Error {}

/// A 1.14d character save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Save {
    /// The header's 335 bytes, as read (size and checksum are refreshed on write).
    pub header: Vec<u8>,
    /// `Woo!`: the six bytes after the marker, then 96 bytes per difficulty.
    pub quests: Vec<u8>,
    /// `WS`: 24 bytes per difficulty — `02 01`, then the waypoint bits.
    pub waypoints: [[u8; 24]; 3],
    /// The byte after the waypoint blocks (1 in played saves).
    pub waypoint_tail: u8,
    /// `w4`'s body.
    pub npcs: Vec<u8>,
    /// `gf`: stat ids 0–15 and their raw values (life, mana and stamina in 256ths), in id order.
    pub stats: Vec<(u16, u32)>,
    /// `if`: 30 skill levels.
    pub skills: [u8; 30],
    /// From the player's `JM` to the end of the file.
    pub items: Vec<u8>,
}

/// A simple item — `compactsave`, no quality, stats or sockets — as `0x0062AF80` writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimpleItem {
    /// Its flags (`0x10` identified, [`ITEM_COMPACT`], `0x800000` on every item written).
    pub flags: u32,
    /// The game's item version: 101 in an expansion game, 2 in a classic one (`0x00530930`).
    pub version: u16,
    /// Where it is: 0 stored in a grid, 1 equipped, 2 in the belt.
    pub mode: u8,
    /// Body location when equipped.
    pub body: u8,
    /// Grid column, or the belt slot.
    pub col: u8,
    /// Grid row (0 in the belt).
    pub row: u8,
    /// The grid page plus one: 0 none (the belt), 1 the inventory, 4 the cube, 5 the stash.
    pub page: u8,
    /// Its code, space-padded.
    pub code: [u8; 4],
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The save checksum over `data` with its checksum field taken as zero.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    data.iter().enumerate().fold(0u32, |sum, (i, &b)| {
        let b = if (0x0C..0x10).contains(&i) { 0 } else { u32::from(b) };
        b.wrapping_add(sum >> 31).wrapping_add(sum.wrapping_mul(2))
    })
}

impl Save {
    /// A level-1 character nobody has played: header, empty quests and waypoints, the stats
    /// given (id, raw value), no skills and no items. `status` is the `.d2s` status byte (bit
    /// 0 is set for it); an expansion character gets the mercenary and golem item blocks.
    #[must_use]
    pub fn new(name: &str, class: u8, status: u8, created: u32, stats: &[(u16, u32)]) -> Self {
        let mut header = vec![0u8; HEADER_LEN];
        header[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        let n = name.len().min(15);
        header[0x14..0x14 + n].copy_from_slice(&name.as_bytes()[..n]);
        header[0x24] = status | 1;
        header[0x28] = class;
        header[0x29] = 0x10;
        header[0x2A] = 0x1E;
        header[0x2B] = 1;
        header[0x2C..0x30].copy_from_slice(&created.to_le_bytes());
        header[0x30..0x34].copy_from_slice(&created.to_le_bytes());
        for hotkey in header[0x38..0x78].chunks_mut(4) {
            hotkey.copy_from_slice(&0xFFFFu32.to_le_bytes());
        }
        header[0x88..0xA8].fill(0xFF);
        header[0xA8] = 0x80;
        let mut quests = vec![0u8; 6 + 3 * 96];
        quests[..6].copy_from_slice(&[0x06, 0x00, 0x00, 0x00, 0x2A, 0x01]);
        let mut waypoints = [[0u8; 24]; 3];
        for block in &mut waypoints {
            block[0] = 0x02;
            block[1] = 0x01;
        }
        let mut npcs = vec![0u8; NPC_LEN - 2];
        npcs[0] = 0x34;
        let mut items = b"JM\0\0JM\0\0".to_vec();
        if status & 0x20 != 0 {
            items.extend_from_slice(b"jfkf\0");
        }
        let mut stats = stats.to_vec();
        stats.sort_unstable_by_key(|&(id, _)| id);
        Self { header, quests, waypoints, waypoint_tail: 1, npcs, stats, skills: [0; 30], items }
    }

    /// Read a played save.
    ///
    /// # Errors
    ///
    /// [`Error`] if the bytes are not a whole 1.14d save.
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        if data.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        if u32_at(data, 0) != SIGNATURE || u32_at(data, 4) != VERSION {
            return Err(Error::NotASave);
        }
        let mut at = HEADER_LEN;
        let section = |at: usize, marker: &'static str, len: usize| -> Result<&[u8], Error> {
            let s = data.get(at..at + len).ok_or(Error::Truncated)?;
            if !s.starts_with(marker.as_bytes()) {
                return Err(Error::BadMarker(marker));
            }
            Ok(s)
        };
        let quests = section(at, "Woo!", QUEST_LEN)?[4..].to_vec();
        at += QUEST_LEN;
        let ws = section(at, "WS", WAYPOINT_LEN)?;
        let mut waypoints = [[0u8; 24]; 3];
        for (i, block) in waypoints.iter_mut().enumerate() {
            block.copy_from_slice(&ws[8 + i * 24..32 + i * 24]);
        }
        let waypoint_tail = ws[80];
        at += WAYPOINT_LEN;
        let npcs = section(at, "w4", NPC_LEN)?[2..].to_vec();
        at += NPC_LEN;
        if data.get(at..at + 2) != Some(b"gf") {
            return Err(Error::BadMarker("gf"));
        }
        let mut bits = BitReader { data: &data[at + 2..], bit: 0 };
        let mut stats = Vec::new();
        while let Some(id) = bits.read(9) {
            if id == STAT_END || id as usize >= STAT_BITS.len() {
                break;
            }
            let Some(value) = bits.read(STAT_BITS[id as usize]) else { break };
            stats.push((id as u16, value));
        }
        at += 2 + bits.bit.div_ceil(8);
        let skills: [u8; 30] = section(at, "if", SKILL_LEN)?[2..].try_into().map_err(|_| Error::Truncated)?;
        at += SKILL_LEN;
        let items = data.get(at..).filter(|i| i.starts_with(b"JM")).ok_or(Error::BadMarker("JM"))?.to_vec();
        Ok(Self { header: data[..HEADER_LEN].to_vec(), quests, waypoints, waypoint_tail, npcs, stats, skills, items })
    }

    /// The status byte (`0x24`): hardcore `0x04`, died `0x08`, expansion `0x20`, ladder `0x40`.
    #[must_use]
    pub fn status(&self) -> u8 {
        self.header[0x24]
    }

    /// Replace the status byte.
    pub fn set_status(&mut self, status: u8) {
        self.header[0x24] = status;
    }

    /// The whole file, with its size and checksum filled in.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.header.clone();
        out.resize(HEADER_LEN, 0);
        out.extend_from_slice(b"Woo!");
        out.extend_from_slice(&self.quests);
        out.resize(HEADER_LEN + QUEST_LEN, 0);
        out.extend_from_slice(b"WS");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0x50u16.to_le_bytes());
        for block in &self.waypoints {
            out.extend_from_slice(block);
        }
        out.push(self.waypoint_tail);
        out.extend_from_slice(b"w4");
        out.extend_from_slice(&self.npcs);
        out.resize(HEADER_LEN + QUEST_LEN + WAYPOINT_LEN + NPC_LEN, 0);
        out.extend_from_slice(b"gf");
        let mut bits = BitWriter::default();
        for &(id, value) in &self.stats {
            if let Some(&width) = STAT_BITS.get(usize::from(id)) {
                bits.write(u32::from(id), 9);
                bits.write(value, width);
            }
        }
        bits.write(STAT_END, 9);
        out.extend_from_slice(&bits.bytes);
        out.extend_from_slice(b"if");
        out.extend_from_slice(&self.skills);
        out.extend_from_slice(&self.items);
        let len = out.len() as u32;
        out[8..12].copy_from_slice(&len.to_le_bytes());
        let sum = checksum(&out);
        out[0x0C..0x10].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// The player's own items (the first `JM` list) when every one is simple, and where the list
    /// ends in [`Self::items`]; `None` when an item is not simple or the list does not read to a
    /// whole (a real item body is not modelled). Without the item tables it cannot know a quest
    /// item whose row has `questdiffcheck` carries two more bits after its code (`d2-data`'s
    /// `item_bits` does); it is for potions, gems and the like.
    #[must_use]
    pub fn simple_items(&self) -> Option<(Vec<SimpleItem>, usize)> {
        let data = &self.items;
        if data.get(..2) != Some(b"JM") {
            return None;
        }
        let count = usize::from(u16::from_le_bytes([*data.get(2)?, *data.get(3)?]));
        let mut at = 4;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            if data.get(at..at + 2) != Some(b"JM") {
                return None;
            }
            let mut bits = BitReader { data: &data[at..], bit: 16 };
            let flags = bits.read(32)?;
            if flags & ITEM_COMPACT == 0 || flags & ITEM_EAR != 0 {
                return None;
            }
            let version = bits.read(10)? as u16;
            let mode = bits.read(3)? as u8;
            if mode == 3 || mode == 5 {
                return None;
            }
            let (body, col, row, page) = (bits.read(4)? as u8, bits.read(4)? as u8, bits.read(4)? as u8, bits.read(3)? as u8);
            let code = bits.read(32)?.to_le_bytes();
            if &code == b"gld " {
                return None;
            }
            if bits.read(1)? != 0 {
                bits.read(32)?;
                bits.read(32)?;
                bits.read(32)?;
            }
            items.push(SimpleItem { flags, version, mode, body, col, row, page, code });
            at += bits.bit.div_ceil(8);
        }
        // The corpse's list follows.
        (data.get(at..at + 2) == Some(b"JM")).then_some((items, at))
    }

    /// Replace the player's own items with `items`, keeping what follows the list (the corpse,
    /// the mercenary, the golem). Does nothing when the list does not read ([`Self::simple_items`]).
    pub fn set_simple_items(&mut self, items: &[SimpleItem]) -> bool {
        let Some((_, end)) = self.simple_items() else { return false };
        let mut out = b"JM".to_vec();
        out.extend_from_slice(&(items.len() as u16).to_le_bytes());
        for item in items {
            let mut bits = BitWriter::default();
            bits.write(0x4D4A, 16);
            bits.write(item.flags | ITEM_COMPACT, 32);
            bits.write(u32::from(item.version.min(0x3FF)), 10);
            bits.write(u32::from(item.mode.min(7)), 3);
            bits.write(u32::from(item.body.min(15)), 4);
            bits.write(u32::from(item.col.min(15)), 4);
            bits.write(u32::from(item.row.min(15)), 4);
            bits.write(u32::from(item.page.min(7)), 3);
            bits.write(u32::from_le_bytes(item.code), 32);
            // No realm data.
            bits.write(0, 1);
            out.extend_from_slice(&bits.bytes);
        }
        out.extend_from_slice(&self.items[end..]);
        self.items = out;
        true
    }

    /// A stat's raw value, 0 when not saved.
    #[must_use]
    pub fn stat(&self, id: u16) -> u32 {
        self.stats.iter().find(|&&(s, _)| s == id).map_or(0, |&(_, v)| v)
    }

    /// Set a stat's raw value; a zero is left out, as the engine leaves it out.
    pub fn set_stat(&mut self, id: u16, value: u32) {
        self.stats.retain(|&(s, _)| s != id);
        if value != 0 && usize::from(id) < STAT_BITS.len() {
            self.stats.push((id, value));
            self.stats.sort_unstable_by_key(|&(s, _)| s);
        }
    }

    /// Character level, from the header.
    #[must_use]
    pub fn level(&self) -> u8 {
        self.header[0x2B]
    }

    /// Set the header's level and last-played time.
    pub fn set_level(&mut self, level: u8, played: u32) {
        self.header[0x2B] = level;
        self.header[0x30..0x34].copy_from_slice(&played.to_le_bytes());
    }

    /// Class, from the header.
    #[must_use]
    pub fn class(&self) -> u8 {
        self.header[0x28]
    }

    /// The name, from the header.
    #[must_use]
    pub fn name(&self) -> String {
        self.header[0x14..0x24].iter().take_while(|&&b| b != 0).map(|&b| char::from(b)).collect()
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl BitReader<'_> {
    fn read(&mut self, width: u32) -> Option<u32> {
        let mut value = 0u32;
        for i in 0..width {
            let byte = *self.data.get(self.bit / 8)?;
            if byte >> (self.bit % 8) & 1 != 0 {
                value |= 1 << i;
            }
            self.bit += 1;
        }
        Some(value)
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn write(&mut self, value: u32, width: u32) {
        for i in 0..width {
            if self.bit % 8 == 0 {
                self.bytes.push(0);
            }
            if value >> i & 1 != 0 {
                *self.bytes.last_mut().expect("pushed") |= 1 << (self.bit % 8);
            }
            self.bit += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_save_reads_back_and_writes_the_same_bytes() {
        let stats = [(0, 30), (2, 20), (3, 25), (1, 10), (6, 55 << 8), (7, 55 << 8), (12, 1)];
        let mut save = Save::new("Tester", 4, 0x20, 1_000, &stats);
        save.waypoints[0][2] = 0b101;
        save.set_stat(13, 1234);
        save.set_level(3, 2_000);
        let bytes = save.to_bytes();
        assert_eq!(u32_at(&bytes, 8) as usize, bytes.len(), "size stamped");
        assert_eq!(u32_at(&bytes, 0x0C), checksum(&bytes), "checksum stamped");
        assert_eq!(&bytes[HEADER_LEN..HEADER_LEN + 4], b"Woo!");
        assert_eq!(&bytes[765..767], b"gf", "attributes at the classic offset");
        let back = Save::parse(&bytes).unwrap();
        assert_eq!((&back.stats, back.waypoints, &back.items, back.skills), (&save.stats, save.waypoints, &save.items, save.skills));
        assert_eq!(back.to_bytes(), bytes);
        assert_eq!((back.name(), back.class(), back.level()), ("Tester".into(), 4, 3));
        assert_eq!(back.stat(1), 10, "stats sorted by id");
        assert_eq!(back.stat(13), 1234);
        assert!(back.items.ends_with(b"jfkf\0"), "expansion blocks");
        assert_eq!(Save::parse(&bytes[..700]), Err(Error::Truncated));
    }

    #[test]
    fn simple_items_are_fourteen_bytes_and_keep_the_lists_after_them() {
        let mut save = Save::new("Tester", 4, 0x20, 1_000, &[(0, 30)]);
        assert_eq!(save.simple_items(), Some((Vec::new(), 4)));
        let potion = SimpleItem { flags: 0x0080_0010, version: 101, mode: 2, body: 0, col: 3, row: 0, page: 0, code: *b"hp1 " };
        let gem = SimpleItem { flags: 0x00A0_0010, version: 101, mode: 0, body: 0, col: 9, row: 3, page: 1, code: *b"gcr " };
        assert!(save.set_simple_items(&[potion, gem]));
        assert_eq!(save.items.len(), 4 + 14 * 2 + 4 + 5, "two items, the corpse list and the expansion blocks");
        assert!(save.items.ends_with(b"JM\0\0jfkf\0"));
        let back = Save::parse(&save.to_bytes()).unwrap();
        let (items, _) = back.simple_items().unwrap();
        assert_eq!(items, [SimpleItem { flags: 0x00A0_0010, ..potion }, gem], "written compact");
        // The retail pickup's hp2 at column 9, row 3 of the inventory, as a save writes it.
        let mut hp2 = Save::new("Tester", 4, 0, 1_000, &[]);
        hp2.set_simple_items(&[SimpleItem { code: *b"hp2 ", ..gem }]);
        assert_eq!(hp2.items[4..18], [0x4A, 0x4D, 0x10, 0x00, 0xA0, 0x00, 0x65, 0x00, 0x72, 0x82, 0x06, 0x27, 0x03, 0x02]);
        // A full item stops the reading, and then nothing is replaced.
        let mut full = back.clone();
        full.items[4 + 4] &= !0x20;
        assert_eq!(full.simple_items(), None);
        assert!(!full.set_simple_items(&[]));
    }

    #[test]
    fn the_checksum_rolls_like_the_engine() {
        // sum = byte + carry of the high bit + sum × 2, skipping the checksum field.
        let mut data = vec![0u8; 20];
        data[0] = 1;
        data[1] = 2;
        data[0x0C] = 0xFF;
        let expected = (0..20).fold(0u32, |s, i| {
            let b = match i {
                0 => 1,
                1 => 2,
                _ => 0,
            };
            b + (s >> 31) + s.wrapping_mul(2)
        });
        assert_eq!(checksum(&data), expected);
    }

    /// With libd2's recorded save (`LIBD2_DIR`): it parses and writes back byte for byte.
    #[test]
    fn with_libd2_a_played_save_round_trips() {
        let Ok(libd2) = std::env::var("LIBD2_DIR") else { return };
        let path = std::path::Path::new(&libd2).join("packages/save/src/testdata/EpicSorc.d2s");
        let Ok(bytes) = std::fs::read(path) else { return };
        let save = Save::parse(&bytes).unwrap();
        assert_eq!(save.class(), 1);
        assert!(save.level() > 1);
        let ours = save.to_bytes();
        let diff: Vec<(usize, u8, u8)> = ours.iter().zip(&bytes).enumerate().filter(|(_, (a, b))| a != b).map(|(i, (a, b))| (i, *a, *b)).collect();
        assert_eq!(ours.len(), bytes.len());
        assert!(diff.is_empty(), "{diff:?}");
    }
}
