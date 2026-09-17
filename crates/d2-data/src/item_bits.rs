//! An item's bits, as `Game.exe` writes them into `0x9C`/`0x9D` packets and `.d2s` item lists
//! (`0x006312B0` → `0x0062AF80` simple, `0x0062FFF0` full) and reads them back (`0x0062E430` →
//! `0x0062A970`, `0x0062CBE0`). Values are packed least significant bit first.
//!
//! Common head: flags 32 (a save prefixes `JM`), version 10, mode 3, then x and y 16 each on the
//! ground (modes 3 and 5) or body location 4, column 4, row 4 and page + 1 3, then the code 32.
//! A simple item (`compactsave`, flag `0x200000`) ends there bar gold's amount and a save's realm
//! bit. A full item goes on: socketed items 3, the item's seed 32 (saves only), item level 7,
//! quality 4, a picture index behind a present bit, an auto-affix id behind a present bit, the
//! quality's ids (gated on identified in packets), a runeword id, a personalised name, a save's
//! realm bit, defence and durability for armour and weapons, a stack's quantity, the socket count,
//! and — in a save, or a packet for an identified item — the stat lists, each ended by id `0x1FF`:
//! the item's own, then any set bonus lists a 5-bit mask names, then a runeword's. A stat is id 9,
//! then its parameter and its value in the widths `ItemStatCost.txt` gives; the minimum-damage
//! stats and enhanced damage carry their partners' values straight after them.

use crate::item_stats::ItemStats;
use crate::items::{Code, Items};

/// Item flags.
pub mod flags {
    /// Identified.
    pub const IDENTIFIED: u32 = 0x10;
    /// On the packet taking out an item used up (`0x00561E70`, `0x0055E000`).
    pub const USED: u32 = 0x20;
    /// Has sockets: the socket count is written.
    pub const SOCKETED: u32 = 0x800;
    /// Slid down a belt column to close the gap an item taken out of it left (`0x0055EDC0`); the
    /// client plays the belt's sound for it (`0x004C50F1`). The engine never clears it again.
    pub const SLID_IN_BELT: u32 = 0x400;
    /// Just placed in the world: the client plays the fall.
    pub const DROPPED: u32 = 0x2000;
    /// An ear.
    pub const EAR: u32 = 0x1_0000;
    /// A starting item.
    pub const STARTER: u32 = 0x2_0000;
    /// Runtime only; cleared before writing.
    pub const INIT: u32 = 0x8_0000;
    /// Simple (`compactsave`).
    pub const COMPACT: u32 = 0x20_0000;
    /// Ethereal.
    pub const ETHEREAL: u32 = 0x40_0000;
    /// On every item written.
    pub const WRITTEN: u32 = 0x80_0000;
    /// Personalised: a name follows.
    pub const PERSONALIZED: u32 = 0x100_0000;
    /// A runeword: its id and a stat list follow.
    pub const RUNEWORD: u32 = 0x400_0000;
}

/// Where an item is (the mode and the location fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Mode 0: in a grid. `page` 0 is the inventory, 3 the cube, 4 the stash.
    Stored {
        /// Column.
        col: u8,
        /// Row.
        row: u8,
        /// Page.
        page: u8,
    },
    /// Mode 1: worn at a body location (1 head … 12 left hand switch).
    Equipped {
        /// `BodyLocs.txt` row.
        body: u8,
    },
    /// Mode 2: a belt slot (its column; row 0).
    Belt {
        /// Slot.
        slot: u8,
    },
    /// Mode 3: on the ground.
    Ground {
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
    },
    /// Mode 4: on the cursor, keeping the fields of where it came from: the body location it was
    /// worn at, or the grid spot and page it was lifted from (`0x0053D010` writes the old page).
    Cursor {
        /// Body location.
        body: u8,
        /// Column.
        col: u8,
        /// Row.
        row: u8,
        /// Page, `None` when it came from no grid.
        page: Option<u8>,
    },
    /// Mode 5: falling to the ground.
    Dropping {
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
    },
    /// Mode 6: in another item's socket.
    Socketed,
}

impl Location {
    /// The engine's item mode.
    #[must_use]
    pub fn mode(&self) -> u8 {
        match self {
            Self::Stored { .. } => 0,
            Self::Equipped { .. } => 1,
            Self::Belt { .. } => 2,
            Self::Ground { .. } => 3,
            Self::Cursor { .. } => 4,
            Self::Dropping { .. } => 5,
            Self::Socketed => 6,
        }
    }
}

/// An item's quality and the ids that go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// 1: low quality, with its `LowQualityItems.txt` row.
    Inferior(u8),
    /// 2.
    Normal,
    /// 3: superior, with its `QualityItems.txt` row.
    Superior(u8),
    /// 4: a `MagicPrefix.txt` and `MagicSuffix.txt` id each (row + 1; 0 for none).
    Magic {
        /// Prefix.
        prefix: u16,
        /// Suffix.
        suffix: u16,
    },
    /// 5: a `SetItems.txt` row.
    Set(u16),
    /// 6: a rare's two name parts and up to three affix ids of each kind.
    Rare(RareIds),
    /// 7: a `UniqueItems.txt` row.
    Unique(u16),
    /// 8: crafted, laid out as a rare.
    Crafted(RareIds),
    /// 9: tempered, two name parts.
    Tempered(u8, u8),
}

/// A rare or crafted item's ids.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RareIds {
    /// `RarePrefix.txt` and `RareSuffix.txt` name ids.
    pub names: (u8, u8),
    /// Magic prefix ids (0 for none).
    pub prefixes: [u16; 3],
    /// Magic suffix ids (0 for none).
    pub suffixes: [u16; 3],
}

impl Quality {
    /// The engine's quality number.
    #[must_use]
    pub fn number(&self) -> u8 {
        match self {
            Self::Inferior(_) => 1,
            Self::Normal => 2,
            Self::Superior(_) => 3,
            Self::Magic { .. } => 4,
            Self::Set(_) => 5,
            Self::Rare(_) => 6,
            Self::Unique(_) => 7,
            Self::Crafted(_) => 8,
            Self::Tempered(..) => 9,
        }
    }
}

/// One stat on an item: its `ItemStatCost.txt` id, parameter and value as the engine keeps it
/// (shifted by `ValShift`, e.g. life in 256ths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ItemStat {
    /// Id.
    pub id: u16,
    /// Parameter (a skill, a class, a monster type); 0 for most.
    pub param: u16,
    /// Value.
    pub value: i32,
}

/// Everything written about an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Flags ([`flags`]).
    pub flags: u32,
    /// The item version: 101 in an expansion game, 2 in a classic one.
    pub version: u16,
    /// Where it is.
    pub location: Location,
    /// Its code, space-padded.
    pub code: Code,
    /// Items in its sockets.
    pub socketed: u8,
    /// Its seed (written in saves only).
    pub seed: u32,
    /// Item level, 1–99.
    pub level: u8,
    /// Quality and ids.
    pub quality: Quality,
    /// Inventory picture index, for types that pick one (written whenever the type does).
    pub picture: Option<u8>,
    /// An `AutoMagic.txt` affix id (0 for none).
    pub auto_affix: u16,
    /// A normal charm's affix id, a normal body part's monster: the extra fields types 13 and 40
    /// carry in normal quality.
    pub type_extra: u16,
    /// A tome's scroll kind (0 town portal, 1 identify).
    pub book: u8,
    /// Runeword id.
    pub runeword: u16,
    /// A personalised name.
    pub name: String,
    /// Realm data (saves only): two words, when present.
    pub realm: Option<(u32, u32)>,
    /// Base defence (armour).
    pub defense: i32,
    /// Maximum durability, 0 for indestructible.
    pub max_durability: i32,
    /// Durability left.
    pub durability: i32,
    /// Gold in a pile.
    pub gold: u32,
    /// A stack's quantity.
    pub quantity: u16,
    /// Sockets (with [`flags::SOCKETED`]).
    pub sockets: u8,
    /// The item's own stats.
    pub stats: Vec<ItemStat>,
    /// Set bonus lists, by how many set pieces are worn (2–6); empty lists are not written.
    pub set_stats: [Vec<ItemStat>; 5],
    /// A runeword's stats.
    pub runeword_stats: Vec<ItemStat>,
}

impl Item {
    /// A normal, identified item of `code` at `location`, with nothing else set.
    #[must_use]
    pub fn new(code: Code, version: u16, level: u8, location: Location) -> Self {
        Self {
            flags: flags::IDENTIFIED,
            version,
            location,
            code,
            socketed: 0,
            seed: 0,
            level,
            quality: Quality::Normal,
            picture: None,
            auto_affix: 0,
            type_extra: 0,
            book: 0,
            runeword: 0,
            name: String::new(),
            realm: None,
            defense: 0,
            max_durability: 0,
            durability: 0,
            gold: 0,
            quantity: 0,
            sockets: 0,
            stats: Vec::new(),
            set_stats: Default::default(),
            runeword_stats: Vec::new(),
        }
    }

    /// Whether it is identified.
    #[must_use]
    pub fn identified(&self) -> bool {
        self.flags & flags::IDENTIFIED != 0
    }
}

/// Where the bits go: a packet or a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A `0x9C`/`0x9D` packet.
    Network,
    /// A `.d2s` item list: `JM` first, the seed, realm data, and every list whatever the flags.
    Save,
}

/// What could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// The bits ran out.
    Truncated,
    /// A save item without `JM`.
    NoMarker,
    /// A code the tables do not have.
    UnknownCode(Code),
    /// A stat id the table does not have.
    UnknownStat(u16),
    /// An ear, which is not modelled.
    Ear,
}

struct Writer {
    bytes: Vec<u8>,
    bit: usize,
}

impl Writer {
    fn put(&mut self, value: u32, bits: u32) {
        for i in 0..bits {
            if self.bit % 8 == 0 {
                self.bytes.push(0);
            }
            if i < 32 && value >> i & 1 != 0 {
                *self.bytes.last_mut().expect("pushed") |= 1 << (self.bit % 8);
            }
            self.bit += 1;
        }
    }

    /// `value` clamped into `bits` as the engine clamps (unsigned).
    fn clamped(&mut self, value: u32, bits: u32) {
        let max = if bits >= 32 { u32::MAX } else { (1 << bits) - 1 };
        self.put(value.min(max), bits);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    bit: usize,
}

impl Reader<'_> {
    fn get(&mut self, bits: u32) -> Result<u32, ReadError> {
        let mut value = 0u32;
        for i in 0..bits {
            let byte = *self.bytes.get(self.bit / 8).ok_or(ReadError::Truncated)?;
            if i < 32 && byte >> (self.bit % 8) & 1 != 0 {
                value |= 1 << i;
            }
            self.bit += 1;
        }
        Ok(value)
    }
}

/// The widths the rules give an item.
struct Kind {
    compact: bool,
    armor: bool,
    weapon: bool,
    gold: bool,
    stackable: bool,
    book: bool,
    charm: bool,
    body_part: bool,
    /// The type picks among inventory pictures (`VarInvGfx`, `0x0062E8D0`).
    pictures: bool,
}

fn kind(items: &Items, code: &Code) -> Option<Kind> {
    let class = items.class_of(code)?;
    let def = items.get(class)?;
    let is = |t: &str| items.is(class, t);
    Some(Kind {
        compact: def.compact,
        armor: is("armo"),
        weapon: is("weap"),
        gold: is("gold"),
        stackable: def.stackable,
        book: is("scro") || is("book"),
        charm: is("char"),
        body_part: is("body") && !is("play"),
        pictures: items.types().get(def.item_type).is_some_and(|t| t.var_inv_gfx > 0),
    })
}

/// Stats whose partners' values follow them, as `0x0062FFF0` writes them.
const GROUPS: [(u16, &[u16]); 6] = [(17, &[18]), (48, &[49]), (50, &[51]), (52, &[53]), (54, &[55, 56]), (57, &[58, 59])];

fn write_value(w: &mut Writer, stats: &ItemStats, id: u16, value: i32) {
    let Some(cost) = stats.get(id) else { return };
    w.clamped(((value >> cost.val_shift) + cost.save_add) as u32, u32::from(cost.save_bits));
}

fn write_list(w: &mut Writer, stats: &ItemStats, list: &[ItemStat]) {
    let mut sorted = list.to_vec();
    sorted.sort();
    let mut written: Vec<u16> = Vec::new();
    for s in &sorted {
        let Some(cost) = stats.get(s.id) else { continue };
        if cost.save_bits == 0 || s.value >> cost.val_shift == 0 || written.contains(&s.id) {
            continue;
        }
        w.put(u32::from(s.id), 9);
        if let Some((_, partners)) = GROUPS.iter().find(|(lead, _)| *lead == s.id) {
            write_value(w, stats, s.id, s.value);
            for &p in *partners {
                let v = sorted.iter().find(|o| o.id == p).map_or(0, |o| o.value);
                write_value(w, stats, p, v);
                written.push(p);
            }
            continue;
        }
        if cost.param_bits > 0 {
            w.clamped(u32::from(s.param), u32::from(cost.param_bits));
        }
        write_value(w, stats, s.id, s.value);
    }
    w.put(0x1FF, 9);
}

fn read_value(r: &mut Reader, stats: &ItemStats, id: u16) -> Result<i32, ReadError> {
    let cost = stats.get(id).ok_or(ReadError::UnknownStat(id))?;
    let raw = r.get(u32::from(cost.save_bits))? as i32;
    Ok((raw - cost.save_add) << cost.val_shift)
}

fn read_list(r: &mut Reader, stats: &ItemStats) -> Result<Vec<ItemStat>, ReadError> {
    let mut list = Vec::new();
    loop {
        let id = r.get(9)? as u16;
        if id == 0x1FF {
            return Ok(list);
        }
        if let Some((_, partners)) = GROUPS.iter().find(|(lead, _)| *lead == id) {
            list.push(ItemStat { id, param: 0, value: read_value(r, stats, id)? });
            for &p in *partners {
                list.push(ItemStat { id: p, param: 0, value: read_value(r, stats, p)? });
            }
            continue;
        }
        let cost = stats.get(id).ok_or(ReadError::UnknownStat(id))?;
        let param = if cost.param_bits > 0 { r.get(u32::from(cost.param_bits))? as u16 } else { 0 };
        list.push(ItemStat { id, param, value: read_value(r, stats, id)? });
    }
}

/// An item's bits: whole bytes, the last padded with zeros.
///
/// Stats the tables do not know, or whose value is 0 once shifted, are left out, as the engine
/// leaves them out.
#[must_use]
pub fn write(item: &Item, items: &Items, stats: &ItemStats, target: Target) -> Vec<u8> {
    let save = target == Target::Save;
    let mut w = Writer { bytes: Vec::new(), bit: 0 };
    let kind = kind(items, &item.code);
    let compact = kind.as_ref().is_some_and(|k| k.compact);
    if save {
        w.put(0x4D4A, 16);
    }
    let mut item_flags = (item.flags & !flags::INIT) | flags::WRITTEN;
    if compact {
        item_flags |= flags::COMPACT;
    } else {
        item_flags &= !flags::COMPACT;
    }
    if !save && item_flags & flags::IDENTIFIED == 0 {
        item_flags &= !flags::SOCKETED;
    }
    w.put(item_flags, 32);
    w.clamped(u32::from(item.version), 10);
    w.put(u32::from(item.location.mode()), 3);
    match item.location {
        Location::Ground { x, y } | Location::Dropping { x, y } => {
            w.put(u32::from(x), 16);
            w.put(u32::from(y), 16);
        }
        other => {
            let (body, col, row, page) = match other {
                Location::Stored { col, row, page } => (0, col, row, page + 1),
                Location::Equipped { body } => (body, 0, 0, 0),
                Location::Belt { slot } => (0, slot, 0, 0),
                Location::Cursor { body, col, row, page } => (body, col, row, page.map_or(0, |p| p + 1)),
                _ => (0, 0, 0, 0),
            };
            w.clamped(u32::from(body), 4);
            w.clamped(u32::from(col), 4);
            w.clamped(u32::from(row), 4);
            w.clamped(u32::from(page), 3);
        }
    }
    w.put(u32::from_le_bytes(item.code), 32);
    let Some(kind) = kind else { return w.bytes };
    if compact {
        if kind.gold {
            let big = item.gold > 0xFFF;
            w.put(u32::from(big), 1);
            w.put(item.gold, if big { 32 } else { 12 });
        }
        if save {
            write_realm(&mut w, item.realm);
        }
        return w.bytes;
    }
    let identified = save || item.identified();
    w.clamped(u32::from(item.socketed), 3);
    if save {
        w.put(item.seed, 32);
    }
    w.clamped(u32::from(item.level.clamp(1, 99)), 7);
    w.put(u32::from(item.quality.number()), 4);
    w.put(u32::from(kind.pictures), 1);
    if kind.pictures {
        w.clamped(u32::from(item.picture.unwrap_or(0)), 3);
    }
    w.put(u32::from(item.auto_affix != 0), 1);
    if item.auto_affix != 0 {
        w.clamped(u32::from(item.auto_affix), 11);
    }
    match item.quality {
        Quality::Inferior(id) | Quality::Superior(id) => w.clamped(u32::from(id), 3),
        Quality::Normal => {
            if kind.charm && identified {
                w.put(u32::from(item.type_extra != 0), 1);
                w.clamped(u32::from(item.type_extra), 11);
            }
            if kind.body_part {
                w.clamped(u32::from(item.type_extra), 10);
            }
            if kind.book {
                w.clamped(u32::from(item.book), 5);
            }
        }
        Quality::Magic { prefix, suffix } => {
            if identified {
                w.clamped(u32::from(prefix), 11);
                w.clamped(u32::from(suffix), 11);
            }
        }
        Quality::Set(id) | Quality::Unique(id) => {
            if identified {
                w.clamped(u32::from(id), 12);
            }
        }
        Quality::Rare(ids) | Quality::Crafted(ids) => {
            if identified {
                w.put(u32::from(ids.names.0), 8);
                w.put(u32::from(ids.names.1), 8);
            }
            for i in 0..3 {
                for id in [ids.prefixes[i], ids.suffixes[i]] {
                    w.put(u32::from(id != 0), 1);
                    if id != 0 {
                        w.clamped(u32::from(id), 11);
                    }
                }
            }
        }
        Quality::Tempered(a, b) => {
            if identified {
                w.put(u32::from(a), 8);
                w.put(u32::from(b), 8);
            }
        }
    }
    if item_flags & flags::RUNEWORD != 0 {
        w.put(u32::from(item.runeword), 16);
    }
    if item_flags & flags::PERSONALIZED != 0 {
        for c in item.name.bytes().chain(std::iter::once(0)) {
            w.clamped(u32::from(c), 7);
        }
    }
    if save {
        write_realm(&mut w, item.realm);
    }
    if kind.armor {
        write_value(&mut w, stats, 31, item.defense);
    }
    if kind.armor || kind.weapon {
        write_value(&mut w, stats, 73, item.max_durability);
        if item.max_durability != 0 {
            write_value(&mut w, stats, 72, item.durability);
        }
    } else if kind.gold {
        let big = item.gold > 0xFFF;
        w.put(u32::from(big), 1);
        w.put(item.gold, if big { 32 } else { 12 });
    }
    if kind.stackable {
        w.clamped(u32::from(item.quantity), 9);
    }
    if item_flags & flags::SOCKETED != 0 {
        if let Some(cost) = stats.get(194) {
            w.clamped(u32::from(item.sockets), u32::from(cost.save_bits));
        }
    }
    if !identified {
        return w.bytes;
    }
    if let Quality::Set(_) = item.quality {
        let mask = item.set_stats.iter().enumerate().filter(|(_, l)| !l.is_empty()).fold(0, |m, (i, _)| m | 1 << i);
        w.put(mask, 5);
    }
    write_list(&mut w, stats, &item.stats);
    if let Quality::Set(_) = item.quality {
        for list in item.set_stats.iter().filter(|l| !l.is_empty()) {
            write_list(&mut w, stats, list);
        }
    }
    if item_flags & flags::RUNEWORD != 0 {
        write_list(&mut w, stats, &item.runeword_stats);
    }
    w.bytes
}

fn write_realm(w: &mut Writer, realm: Option<(u32, u32)>) {
    match realm {
        Some((a, b)) => {
            w.put(1, 1);
            w.put(a, 32);
            w.put(b, 32);
            w.put(0, 32);
        }
        None => w.put(0, 1),
    }
}

/// Read an item from the front of `bytes`: the item and the whole bytes it took.
///
/// # Errors
///
/// [`ReadError`] if the bits run out, name what the tables lack, or are an ear.
pub fn read(bytes: &[u8], items: &Items, stats: &ItemStats, target: Target) -> Result<(Item, usize), ReadError> {
    let save = target == Target::Save;
    let mut r = Reader { bytes, bit: 0 };
    if save && r.get(16)? != 0x4D4A {
        return Err(ReadError::NoMarker);
    }
    let item_flags = r.get(32)?;
    if item_flags & flags::EAR != 0 {
        return Err(ReadError::Ear);
    }
    let version = r.get(10)? as u16;
    let mode = r.get(3)?;
    let location = if mode == 3 || mode == 5 {
        let (x, y) = (r.get(16)? as u16, r.get(16)? as u16);
        if mode == 3 {
            Location::Ground { x, y }
        } else {
            Location::Dropping { x, y }
        }
    } else {
        let (body, col, row, page) = (r.get(4)? as u8, r.get(4)? as u8, r.get(4)? as u8, r.get(3)? as u8);
        match mode {
            0 => Location::Stored { col, row, page: page.saturating_sub(1) },
            1 => Location::Equipped { body },
            2 => Location::Belt { slot: col },
            4 => Location::Cursor { body, col, row, page: page.checked_sub(1) },
            _ => Location::Socketed,
        }
    };
    let code = r.get(32)?.to_le_bytes();
    let kind = kind(items, &code).ok_or(ReadError::UnknownCode(code))?;
    let mut item = Item::new(code, version, 1, location);
    item.flags = item_flags;
    if item_flags & flags::COMPACT != 0 {
        item.quality = Quality::Normal;
        if kind.gold {
            let big = r.get(1)? != 0;
            item.gold = r.get(if big { 32 } else { 12 })?;
        }
        if save {
            item.realm = read_realm(&mut r)?;
        }
        return Ok((item, r.bit.div_ceil(8)));
    }
    let identified = save || item_flags & flags::IDENTIFIED != 0;
    item.socketed = r.get(3)? as u8;
    if save {
        item.seed = r.get(32)?;
    }
    item.level = r.get(7)?.max(1) as u8;
    let quality = r.get(4)?;
    if r.get(1)? != 0 {
        item.picture = Some(r.get(3)? as u8);
    }
    if r.get(1)? != 0 {
        item.auto_affix = r.get(11)? as u16;
    }
    item.quality = match quality {
        1 => Quality::Inferior(r.get(3)? as u8),
        3 => Quality::Superior(r.get(3)? as u8),
        4 if identified => Quality::Magic { prefix: r.get(11)? as u16, suffix: r.get(11)? as u16 },
        4 => Quality::Magic { prefix: 0, suffix: 0 },
        5 | 7 => {
            let id = if identified { r.get(12)? as u16 } else { 0 };
            if quality == 5 {
                Quality::Set(id)
            } else {
                Quality::Unique(id)
            }
        }
        6 | 8 => {
            let mut ids = RareIds::default();
            if identified {
                ids.names = (r.get(8)? as u8, r.get(8)? as u8);
            }
            for i in 0..3 {
                for slot in [&mut ids.prefixes[i], &mut ids.suffixes[i]] {
                    if r.get(1)? != 0 {
                        *slot = r.get(11)? as u16;
                    }
                }
            }
            if quality == 6 {
                Quality::Rare(ids)
            } else {
                Quality::Crafted(ids)
            }
        }
        9 if identified => Quality::Tempered(r.get(8)? as u8, r.get(8)? as u8),
        9 => Quality::Tempered(0, 0),
        _ => {
            if kind.charm && identified && r.get(1)? != 0 {
                item.type_extra = r.get(11)? as u16;
            }
            if kind.body_part {
                item.type_extra = r.get(10)? as u16;
            }
            if kind.book {
                item.book = r.get(5)? as u8;
            }
            Quality::Normal
        }
    };
    if item_flags & flags::RUNEWORD != 0 {
        item.runeword = r.get(16)? as u16;
    }
    if item_flags & flags::PERSONALIZED != 0 {
        loop {
            let c = r.get(7)? as u8;
            if c == 0 {
                break;
            }
            item.name.push(char::from(c));
        }
    }
    if save {
        item.realm = read_realm(&mut r)?;
    }
    if kind.armor {
        item.defense = read_value(&mut r, stats, 31)?;
    }
    if kind.armor || kind.weapon {
        item.max_durability = read_value(&mut r, stats, 73)?;
        if item.max_durability != 0 {
            item.durability = read_value(&mut r, stats, 72)?;
        }
    } else if kind.gold {
        let big = r.get(1)? != 0;
        item.gold = r.get(if big { 32 } else { 12 })?;
    }
    if kind.stackable {
        item.quantity = r.get(9)? as u16;
    }
    if item_flags & flags::SOCKETED != 0 {
        let cost = stats.get(194).ok_or(ReadError::UnknownStat(194))?;
        item.sockets = r.get(u32::from(cost.save_bits))? as u8;
    }
    if !identified {
        return Ok((item, r.bit.div_ceil(8)));
    }
    let set_mask = if let Quality::Set(_) = item.quality { r.get(5)? } else { 0 };
    item.stats = read_list(&mut r, stats)?;
    for (i, list) in item.set_stats.iter_mut().enumerate() {
        if set_mask >> i & 1 != 0 {
            *list = read_list(&mut r, stats)?;
        }
    }
    if item_flags & flags::RUNEWORD != 0 {
        item.runeword_stats = read_list(&mut r, stats)?;
    }
    Ok((item, r.bit.div_ceil(8)))
}

fn read_realm(r: &mut Reader) -> Result<Option<(u32, u32)>, ReadError> {
    if r.get(1)? == 0 {
        return Ok(None);
    }
    let pair = (r.get(32)?, r.get(32)?);
    r.get(32)?;
    Ok(Some(pair))
}

/// A `.d2s` item list (`JM`, a count, the items): what it holds and the bytes it took. The count
/// leaves out items in sockets, which follow the item holding them.
///
/// # Errors
///
/// [`ReadError`] as [`read`], [`ReadError::NoMarker`] without the list's `JM`.
pub fn read_save_list(bytes: &[u8], items: &Items, stats: &ItemStats) -> Result<(Vec<Item>, usize), ReadError> {
    if bytes.get(..2) != Some(b"JM") {
        return Err(ReadError::NoMarker);
    }
    let count = u16::from_le_bytes([*bytes.get(2).ok_or(ReadError::Truncated)?, *bytes.get(3).ok_or(ReadError::Truncated)?]);
    let mut at = 4;
    let mut list = Vec::new();
    for _ in 0..count {
        let (item, used) = read(&bytes[at..], items, stats, Target::Save)?;
        at += used;
        let socketed = item.socketed;
        list.push(item);
        for _ in 0..socketed {
            let (inside, used) = read(&bytes[at..], items, stats, Target::Save)?;
            at += used;
            list.push(inside);
        }
    }
    Ok((list, at))
}

/// A `.d2s` item list of `list`, items in sockets after the item holding them.
#[must_use]
pub fn write_save_list(list: &[Item], items: &Items, stats: &ItemStats) -> Vec<u8> {
    let count = list.iter().filter(|i| i.location != Location::Socketed).count() as u16;
    let mut out = b"JM".to_vec();
    out.extend_from_slice(&count.to_le_bytes());
    for item in list {
        out.extend_from_slice(&write(item, items, stats, Target::Save));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::code;
    use d2_formats::excel::Table;

    /// Made-up tables in the real shapes, with the widths 1.14d's `ItemStatCost.txt` gives the
    /// stats the retail captures below carry.
    fn rules() -> (Items, ItemStats) {
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\tVarInvGfx\r\nNone\t\t\t\t0\r\nAny Armor\tarmo\t\t\t0\r\nHelm\thelm\tarmo\t\t0\r\nAny Shield\tshld\tarmo\t\t0\r\nShield\tshie\tshld\t\t0\r\n\
              Weapon\tweap\t\t\t0\r\nAxe\taxe\tweap\t\t0\r\nBook\tbook\t\t\t0\r\nKey\tkey\t\t\t0\r\nGold\tgold\t\t\t0\r\nRing\tring\t\t\t5\r\n",
        );
        let weapons = Table::parse(b"name\tcode\ttype\tcompactsave\tstackable\r\nLarge Axe\tlax\taxe\t0\t0\r\n");
        let armor = Table::parse(b"name\tcode\ttype\tcompactsave\tstackable\r\nCap\tcap\thelm\t0\t0\r\nBuckler\tbuc\tshie\t0\t0\r\n");
        let misc = Table::parse(
            b"name\tcode\ttype\tcompactsave\tstackable\r\nTown Portal Book\ttbk\tbook\t0\t1\r\nIdentify Book\tibk\tbook\t0\t1\r\nSkeleton Key\tkey\tkey\t0\t1\r\ngold\tgld\tgold\t1\t1\r\nRing\trin\tring\t0\t0\r\n",
        );
        let items = Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap();
        let stats = ItemStats::from_table(&Table::parse(
            b"Stat\tID\tSave Bits\tSave Add\tSave Param Bits\tValShift\r\n\
              strength\t0\t8\t32\t\t\r\nmaxmana\t9\t8\t32\t\t8\r\nitem_maxdamage_percent\t17\t9\t0\t\t\r\nitem_mindamage_percent\t18\t9\t0\t\t\r\n\
              tohit\t19\t10\t\t\t\r\narmorclass\t31\t11\t10\t\t\r\ndurability\t72\t9\t\t\t\r\nmaxdurability\t73\t8\t\t\t\r\n\
              item_lightradius\t89\t4\t4\t\t\r\nitem_addclassskills\t83\t3\t\t3\t\r\nitem_numsockets\t194\t4\t\t\t\r\n",
        ))
        .unwrap();
        (items, stats)
    }

    fn body(hex: &str) -> Vec<u8> {
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    /// Retail `0x9C` bodies from Charsi's and Akara's stores (bnemu's plaintext captures): each
    /// reads back to what the store holds and writes back byte for byte.
    #[test]
    fn retail_store_items_read_and_write_back_exactly() {
        let (items, stats) = rules();
        type Check = fn(&Item);
        let cases: [(&str, Check); 5] = [
            ("10208000650000321606070283d0000606ff01", |i| {
                assert_eq!((i.code, i.level, i.quality, i.defense, i.max_durability, i.durability), (code("cap"), 6, Quality::Normal, 3, 12, 12));
                assert!(i.stats.is_empty());
            }),
            ("10208000650000c41686070283e0e1e13f", |i| {
                assert_eq!((i.code, i.quality, i.max_durability, i.durability), (code("lax"), Quality::Normal, 30, 30));
            }),
            ("1020800065004822563706020311139b408081410294090f64a9ff", |i| {
                assert_eq!((i.code, i.location, i.level), (code("buc"), Location::Stored { col: 4, row: 2, page: 0 }, 6));
                assert_eq!((i.quality, i.defense, i.max_durability, i.durability), (Quality::Magic { prefix: 305, suffix: 310 }, 6, 12, 12));
                assert_eq!(
                    i.stats,
                    [ItemStat { id: 9, param: 0, value: 8 << 8 }, ItemStat { id: 19, param: 0, value: 15 }, ItemStat { id: 89, param: 0, value: 1 }]
                );
            }),
            ("102080006500004827b60602830004fc07", |i| {
                assert_eq!((i.code, i.book, i.quantity), (code("tbk"), 0, 2));
            }),
            ("102080006500b2b8569607028310e03f", |i| {
                assert_eq!((i.code, i.quantity), (code("key"), 1));
            }),
        ];
        for (hex, check) in cases {
            let bytes = body(hex);
            let (item, used) = read(&bytes, &items, &stats, Target::Network).unwrap_or_else(|e| panic!("{hex}: {e:?}"));
            assert_eq!(used, bytes.len(), "{hex}");
            check(&item);
            assert_eq!(write(&item, &items, &stats, Target::Network), bytes, "{hex}");
        }
    }

    #[test]
    fn saves_carry_the_seed_and_every_list_and_packets_hide_an_unidentified_items_stats() {
        let (items, stats) = rules();
        let mut ring = Item::new(code("rin"), 101, 30, Location::Equipped { body: 6 });
        ring.quality = Quality::Rare(RareIds { names: (12, 40), prefixes: [3, 0, 7], suffixes: [0, 9, 0] });
        ring.picture = Some(2);
        ring.seed = 0xDEAD_BEEF;
        ring.flags = flags::STARTER;
        ring.stats = vec![
            ItemStat { id: 0, param: 0, value: -5 },
            ItemStat { id: 83, param: 3, value: 2 },
            ItemStat { id: 18, param: 0, value: 40 },
            ItemStat { id: 17, param: 0, value: 40 },
        ];
        let saved = write(&ring, &items, &stats, Target::Save);
        let (back, used) = read(&saved, &items, &stats, Target::Save).unwrap();
        assert_eq!(used, saved.len());
        let mut expect = ring.clone();
        expect.flags |= flags::WRITTEN;
        expect.stats.sort();
        assert_eq!(back, expect, "strength -5 fits by its add; enhanced damage writes both halves");
        let sent = write(&ring, &items, &stats, Target::Network);
        let (seen, _) = read(&sent, &items, &stats, Target::Network).unwrap();
        let hidden = RareIds { names: (0, 0), prefixes: [3, 0, 7], suffixes: [0, 9, 0] };
        assert_eq!((seen.quality, seen.stats.len(), seen.seed), (Quality::Rare(hidden), 0, 0), "unidentified: no names, no stats");
        let mut plain = Item::new(code("gld"), 101, 1, Location::Ground { x: 5000, y: 5700 });
        plain.gold = 37;
        plain.flags |= flags::DROPPED;
        let gold = write(&plain, &items, &stats, Target::Network);
        assert_eq!(&gold[..5], &[0x10, 0x20, 0xA0, 0x00, 0x65], "a compact item keeps the simple layout");
        assert_eq!(read(&gold, &items, &stats, Target::Network).unwrap().0.gold, 37);
        let mut list = write_save_list(&[ring.clone(), Item::new(code("rin"), 101, 5, Location::Stored { col: 2, row: 1, page: 0 })], &items, &stats);
        list.extend_from_slice(b"JM\0\0");
        let (read_back, end) = read_save_list(&list, &items, &stats).unwrap();
        assert_eq!((read_back.len(), read_back[1].location, &list[end..]), (2, Location::Stored { col: 2, row: 1, page: 0 }, &b"JM\0\0"[..]), "the corpse list follows");
    }
}
