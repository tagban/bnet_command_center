//! Making the weapons, armour and other full items a treasure class drops, as the 1.14d server
//! makes them: the quality roll (`0x00558640`), the base values (`0x00557AB0`), the quality's own
//! routine with its fallbacks (`0x00557450`) — low quality `0x005C2D40`, superior `0x005C2970`,
//! magic `0x005565E0`, rare `0x005C1BF0`, set `0x005C25C0`, unique `0x005566B0` — then ethereal
//! (`0x00556CA0`), sockets (`0x00556B60`) and an automagic affix. A property becomes stats through
//! its `Properties.txt` functions (`0x0065FD70`, table `0x007462F8`).
//!
//! The engine keeps an item's base damage, block and speed in a stat list it never writes; they
//! are left out here, where only what goes into the item's bits is kept.

use std::collections::HashSet;

use d2_data::affixes::{Affix, Mod, RareName};
use d2_data::item_bits::{flags, Item, ItemStat, Location, Quality, RareIds};
use d2_data::item_stats::Odds;
use d2_data::items::{class_index, Code, ItemDef, ItemType};
use d2_data::treasure::QualityMods;
use d2_data::GameData;
use d2_drlg::rng::Seed;

/// Stats the property functions name by id.
mod stat {
    pub const MAXDAMAGE_PERCENT: u16 = 17;
    pub const MINDAMAGE_PERCENT: u16 = 18;
    pub const MINDAMAGE: u16 = 21;
    pub const MAXDAMAGE: u16 = 22;
    pub const SECONDARY_MINDAMAGE: u16 = 23;
    pub const SECONDARY_MAXDAMAGE: u16 = 24;
    pub const SINGLE_SKILL: u16 = 107;
    pub const INDESTRUCTIBLE: u16 = 152;
    pub const THROW_MINDAMAGE: u16 = 159;
    pub const THROW_MAXDAMAGE: u16 = 160;
    pub const NUM_SOCKETS: u16 = 194;
}

/// The Cow King's set, which drops only where the game asks for it (`0x005C25C0`).
const COW_KING_SET: i32 = 29;

/// The game an item is made in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Making {
    /// The item version: 100 and up in a Lord of Destruction game.
    pub version: u16,
    /// 0 normal, 1 nightmare, 2 hell.
    pub difficulty: u8,
    /// A ladder game: ladder-only uniques may drop.
    pub ladder: bool,
    /// The killer's magic find.
    pub magic_find: i32,
}

impl Making {
    fn expansion(&self) -> bool {
        self.version >= 100
    }
}

/// Which stat list a property fills.
#[derive(Debug, Clone, Copy)]
enum List {
    Own,
    Set(usize),
}

/// Which affix table a pick is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Prefix,
    Suffix,
    Auto(i32),
}

struct Maker<'a> {
    data: &'a GameData,
    making: Making,
    class: i32,
    def: &'a ItemDef,
    kind: &'a ItemType,
    rng: Seed,
    item: Item,
    /// Made for a vendor's stock: never ethereal (create flag 2, `0x00559CE0`).
    for_store: bool,
}

/// Make an item of `code` for a monster of `level`, with the quality mods its treasure classes gave,
/// rolling on `seed`; `made_uniques` holds the uniques the game has already made, and gains this
/// one's. `None` for a simple item (`compactsave`) or a code the tables do not have. The item is at
/// `Location::Ground { x: 0, y: 0 }` for the caller to place.
#[must_use]
pub fn make(data: &GameData, code: Code, level: i32, mods: QualityMods, making: Making, made_uniques: &mut HashSet<u16>, seed: u32) -> Option<Item> {
    let items = data.items();
    let class = items.class_of(&code)?;
    let def = items.get(class)?;
    let kind = items.types().get(def.item_type)?;
    if def.compact {
        return None;
    }
    let mut item = Item::new(code, making.version, level.clamp(1, 99) as u8, Location::Ground { x: 0, y: 0 });
    item.seed = seed;
    let mut m = Maker { data, making, class, def, kind, rng: Seed::new(seed, 666), item, for_store: false };
    m.base();
    let quality = m.roll_quality(mods);
    m.generate(quality, made_uniques)?;
    Some(m.item)
}

/// An item of `code` for a vendor's stock (`0x00576330` → `0x00559CE0`): item level `level`,
/// `quality` given (1 low, 2 normal, 3 superior, 4 magic) and its routine's fallbacks, sockets and
/// the automagic affix, never ethereal; identified. A simple item (`compactsave`) is made as it is.
/// `None` for a code the tables lack.
#[must_use]
pub fn make_for_store(data: &GameData, code: Code, level: i32, quality: u8, making: Making, seed: u32) -> Option<Item> {
    let items = data.items();
    let class = items.class_of(&code)?;
    let def = items.get(class)?;
    let kind = items.types().get(def.item_type)?;
    let mut item = Item::new(code, making.version, level.clamp(1, 99) as u8, Location::Ground { x: 0, y: 0 });
    item.seed = seed;
    if def.compact {
        return Some(item);
    }
    let mut m = Maker { data, making, class, def, kind, rng: Seed::new(seed, 666), item, for_store: true };
    m.base();
    m.generate(quality, &mut HashSet::new())?;
    m.item.flags |= flags::IDENTIFIED;
    Some(m.item)
}

/// A new character's starting item of `code` (`0x00534C70`): normal, identified, item level 1,
/// flagged a starter, its base defence rolled, whole durability and a full stack; the class's
/// start skill (`skill`) a point on it when given. `None` for a code the tables lack.
#[must_use]
pub fn starter(data: &GameData, code: Code, version: u16, skill: Option<i32>, seed: u32) -> Option<Item> {
    let items = data.items();
    let class = items.class_of(&code)?;
    let def = items.get(class)?;
    let kind = items.types().get(def.item_type)?;
    let mut item = Item::new(code, version, 1, Location::Ground { x: 0, y: 0 });
    item.seed = seed;
    item.flags |= flags::STARTER;
    if def.compact {
        return Some(item);
    }
    let making = Making { version, difficulty: 0, ladder: false, magic_find: 0 };
    let mut m = Maker { data, making, class, def, kind, rng: Seed::new(seed, 666), item, for_store: false };
    m.base();
    m.item.durability = m.item.max_durability;
    if def.stackable {
        m.item.quantity = def.stack.1.clamp(1, 511) as u16;
    }
    if let Some(skill) = skill.and_then(|s| u16::try_from(s).ok()) {
        m.put(List::Own, stat::SINGLE_SKILL, skill, 1, true);
    }
    Some(m.item)
}

impl Maker<'_> {
    fn is(&self, t: &str) -> bool {
        self.data.items().is(self.class, t)
    }

    /// `rand(n)`: 0 for `n` below 1.
    fn rand(&mut self, n: i32) -> i32 {
        self.rng.pick(n.max(0) as u32) as i32
    }

    /// A value between two bounds, either way round (`0x0065E9E0`).
    fn roll(&mut self, a: i32, b: i32) -> i32 {
        let (lo, hi) = (a.min(b), a.max(b));
        if lo < hi {
            lo + self.rand(hi - lo + 1)
        } else {
            lo
        }
    }

    /// The new low word of a step, as the routines that take `% n` of it use it.
    fn low(&mut self) -> u32 {
        self.rng.roll()
    }

    fn stat_value(&self, id: u16) -> i32 {
        self.item.stats.iter().filter(|s| s.id == id).map(|s| s.value).sum()
    }

    /// Worn down and repaired: not indestructible, with durability in its row (`0x00629930`).
    fn has_durability(&self) -> bool {
        !self.def.no_durability && self.def.durability != 0 && self.stat_value(stat::INDESTRUCTIBLE) < 1
    }

    fn quality(&self) -> u8 {
        self.item.quality.number()
    }

    /// The most sockets the item's type allows at its level, and its row (`0x0062BC20`).
    fn socket_cap(&self) -> i32 {
        let by_level = match self.item.level {
            ..=25 => self.kind.max_sockets[0],
            26..=40 => self.kind.max_sockets[1],
            _ => self.kind.max_sockets[2],
        };
        by_level.min(self.def.gem_sockets).max(0)
    }

    // --- base values -----------------------------------------------------------------------

    /// Defence, durability, a stack's quantity and the picture (`0x00557AB0`).
    fn base(&mut self) {
        let def = self.def;
        let quantity = |m: &mut Self| {
            let (min, max, _) = def.stack;
            (min + m.rand(max - min)).max(1) as u16
        };
        let durability = |m: &mut Self| {
            let dur = def.durability.clamp(0, 255);
            let current = (m.rand(dur >> 1) + (dur >> 1)).min(255);
            m.item.max_durability = dur;
            m.item.durability = current;
        };
        if self.is("armo") {
            durability(self);
            let (min, max) = def.defense;
            self.item.defense = min + self.rand(max - min + 1);
        } else if self.is("weap") {
            if def.stackable {
                self.item.quantity = quantity(self);
            }
            durability(self);
        } else if def.stackable {
            self.item.quantity = quantity(self);
        }
        if self.kind.var_inv_gfx > 0 {
            let n = self.kind.var_inv_gfx;
            self.item.picture = Some(self.rand(n) as u8);
        }
    }

    // --- quality -----------------------------------------------------------------------------

    /// One quality's test: its chance number, bettered by magic find over `divisor` when given,
    /// no worse than the floor, cut by the treasure class's mod (in 1024ths); an item of the
    /// quality when nothing is left or a roll under it falls below 128.
    fn quality_test(&mut self, odds: Odds, levels: i32, divisor: Option<i32>, cut: u32) -> bool {
        let base = odds.ratio - levels / odds.divisor.max(1);
        let mut chance = base * 128;
        if let Some(d) = divisor.filter(|d| *d != 0) {
            chance = base * 12800 / d;
        }
        if chance <= odds.min {
            chance = odds.min;
        }
        let left = chance - (cut as i32).saturating_mul(chance) / 1024;
        left <= 0 || self.rand(left) < 128
    }

    /// The quality a treasure class drop comes out (`0x00558640`), then the rules of the item's
    /// type (`0x00557450`).
    fn roll_quality(&mut self, mods: QualityMods) -> u8 {
        let (def, kind) = (self.def, self.kind);
        let mut quality = if kind.always_normal {
            2
        } else if def.unique_only || (kind.always_magic && def.quest) {
            7
        } else {
            self.roll_ratio(mods)
        };
        if kind.always_magic {
            if def.quest {
                quality = 7;
            } else if !(4..=9).contains(&quality) {
                quality = 4;
            }
        }
        if !kind.can_be_rare && quality == 6 {
            quality = 4;
        }
        if def.unique_only {
            quality = 7;
        }
        if kind.always_normal {
            quality = 2;
        }
        quality
    }

    fn roll_ratio(&mut self, mods: QualityMods) -> u8 {
        let items = self.data.items();
        let Some(ratio) = self.data.item_ratios().for_item(items.is_uber(self.class), self.kind.class.is_some()).copied() else { return 2 };
        let levels = i32::from(self.item.level) - self.def.level;
        let mf = self.making.magic_find;
        // Magic find over 10 counts for less the higher it goes (`0x00558610`).
        let diminished = |k: i32| if mf + 100 < 111 { mf + 100 } else { mf * k / (mf + k) + 100 };
        let boosted = mf != 0;
        if !(boosted && mf <= -100) {
            if self.quality_test(ratio.unique, levels, boosted.then(|| diminished(250)), mods.unique) {
                return 7;
            }
            if self.quality_test(ratio.set, levels, boosted.then(|| diminished(500)), mods.set) {
                return 5;
            }
            if self.kind.can_be_rare && self.quality_test(ratio.rare, levels, boosted.then(|| diminished(600)), mods.rare) {
                return 6;
            }
            if self.kind.always_magic {
                return 4;
            }
            if self.quality_test(ratio.magic, levels, boosted.then_some(mf + 100), mods.magic) {
                return 4;
            }
        }
        let superior = (ratio.superior.ratio - levels / ratio.superior.divisor.max(1)) * 128;
        if superior > 0 && self.rand(superior) >= 128 {
            let normal = (ratio.normal.ratio - levels / ratio.normal.divisor.max(1)) * 128;
            if normal < 1 || self.rand(normal) < 128 {
                return 2;
            }
            return 1;
        }
        3
    }

    /// The quality's routine and its fallbacks, then ethereal, sockets and the automagic affix.
    fn generate(&mut self, quality: u8, made_uniques: &mut HashSet<u16>) -> Option<()> {
        match quality {
            1 => {
                if !self.inferior() {
                    self.normal();
                }
            }
            2 => self.normal(),
            3 => {
                if !self.superior() {
                    self.normal();
                }
            }
            4 => self.magic_or_less(),
            5 => {
                if !self.set() {
                    self.fallback_durability(2);
                    self.magic_or_less();
                }
            }
            6 => self.rare_or_less(),
            7 => {
                if !self.unique(made_uniques) {
                    self.fallback_durability(3);
                    self.rare_or_less();
                }
            }
            _ => return None,
        }
        if self.making.expansion() {
            self.ethereal();
        }
        if (1..=3).contains(&self.quality()) {
            self.sockets();
        }
        if self.making.expansion() && matches!(self.quality(), 1 | 2 | 3 | 4 | 6 | 8 | 9) && self.def.auto_prefix != 0 {
            let id = self.pick_affix(Side::Auto(self.def.auto_prefix), false);
            if id != 0 {
                self.item.auto_affix = id;
                if let Some(affix) = self.data.affixes().auto.get(usize::from(id) - 1) {
                    self.apply_all(&affix.mods, List::Own);
                }
            }
        }
        Some(())
    }

    fn rare_or_less(&mut self) {
        self.reset(6);
        if !self.rare() {
            self.magic_or_less();
        }
    }

    fn magic_or_less(&mut self) {
        self.reset(4);
        if !self.magic() {
            self.reset(3);
            if !self.superior() {
                self.normal();
            }
        }
    }

    /// Forget the ids a failed quality left (`0x00557250`) and take `quality`.
    fn reset(&mut self, quality: u8) {
        self.item.quality = match quality {
            1 => Quality::Inferior(0),
            3 => Quality::Superior(0),
            4 => Quality::Magic { prefix: 0, suffix: 0 },
            6 => Quality::Rare(RareIds::default()),
            _ => Quality::Normal,
        };
    }

    /// A failed set or unique in a Lord of Destruction game is tougher for it (`0x00557450`).
    fn fallback_durability(&mut self, times: i32) {
        if self.has_durability() && self.making.expansion() {
            self.item.durability = (self.item.durability * times).min(255);
            self.item.max_durability = (self.item.max_durability * times).min(255);
        }
    }

    // --- the qualities -----------------------------------------------------------------------

    /// Normal (`0x00556E80`): a tome's or scroll's `Books.txt` row (`0x005C2540`: Town Portal
    /// 0, Identify 1), and the type's class skills.
    fn normal(&mut self) {
        self.item.quality = Quality::Normal;
        if self.is("book") || self.is("scro") {
            self.item.book = u8::from(matches!(&self.def.code, b"ibk " | b"isc "));
        }
        self.staff_mods();
    }

    /// Low quality (`0x005C2D40`): a name, a third of the durability and three quarters of the
    /// defence.
    fn inferior(&mut self) -> bool {
        let rows = self.data.affixes().low_quality as i32;
        if rows == 0 {
            return false;
        }
        let id = self.rand(rows);
        self.item.quality = Quality::Inferior(id as u8);
        if self.has_durability() {
            let max = (self.def.durability.clamp(0, 255) * 33 / 100).max(1);
            let current = (self.rand(max >> 1) + (max >> 1)).max(1);
            self.item.max_durability = max;
            self.item.durability = current;
        }
        if !self.is("weap") && self.is("armo") {
            self.item.defense = (self.item.defense * 75 / 100).max(1);
        }
        true
    }

    /// Superior (`0x005C2970`): a random `QualityItems.txt` row that suits the item, its mods set.
    fn superior(&mut self) -> bool {
        let rows = &self.data.affixes().superior;
        let count = if self.kind.throwable || self.def.no_durability { rows.len().min(4) } else { rows.len() };
        let mut tried = vec![false; count];
        while tried.iter().any(|t| !t) {
            let at = loop {
                let r = self.rand(count as i32) as usize;
                if !tried[r] {
                    break r;
                }
            };
            let row = &self.data.affixes().superior[at];
            if self.superior_applies(&row.applies) {
                self.item.quality = Quality::Superior(at as u8);
                let mods = row.mods.clone();
                for m in &mods {
                    self.apply(m, List::Own);
                }
                self.staff_mods();
                return true;
            }
            tried[at] = true;
        }
        false
    }

    /// Whether a `QualityItems.txt` row suits the item (`0x0065E7D0`).
    fn superior_applies(&self, applies: &[bool; 11]) -> bool {
        let code = self.kind.code.as_str();
        if applies[1] && self.is("weap") && !["staf", "bow", "xbow", "scep", "wand"].contains(&code) {
            return true;
        }
        if applies[0] && self.is("armo") && !["shld", "boot", "glov", "belt"].contains(&code) {
            return true;
        }
        match code {
            "shld" => applies[2],
            "scep" => applies[4],
            "wand" => applies[5],
            "staf" => applies[6],
            "bow" | "xbow" => applies[7],
            "boot" => applies[8],
            "glov" => applies[9],
            "belt" => applies[10],
            _ => false,
        }
    }

    /// Magic (`0x005565E0`): a prefix half the time, a suffix half the time or whenever there is
    /// no prefix; unidentified.
    fn magic(&mut self) -> bool {
        let prefix = self.pick_affix(Side::Prefix, false);
        if prefix != 0 {
            self.item.quality = Quality::Magic { prefix, suffix: 0 };
            self.apply_affix(Side::Prefix, prefix);
        }
        let suffix = self.pick_affix(Side::Suffix, prefix == 0);
        if suffix != 0 {
            self.item.quality = Quality::Magic { prefix, suffix };
            self.apply_affix(Side::Suffix, suffix);
        }
        if prefix == 0 && suffix == 0 {
            return false;
        }
        self.item.flags &= !flags::IDENTIFIED;
        self.staff_mods();
        true
    }

    /// Rare (`0x005C1BF0`): two name parts, then three to six affixes, each side at most three;
    /// unidentified.
    fn rare(&mut self) -> bool {
        let affixes = self.data.affixes();
        let first = self.pick_rare_name(&affixes.rare_prefixes, affixes.rare_suffixes.len());
        let second = self.pick_rare_name(&affixes.rare_suffixes, 0);
        if first == 0 || second == 0 {
            return false;
        }
        let mut ids = RareIds { names: (first as u8, second as u8), ..RareIds::default() };
        self.item.quality = Quality::Rare(ids);
        let count = [3, 4, 4, 5, 5, 5, 6, 6][(self.low() & 7) as usize];
        let (mut prefixes_done, mut suffixes_done) = (false, false);
        let (mut np, mut ns) = (0, 0);
        let mut n = 0;
        while n < count {
            let suffix = if prefixes_done {
                if suffixes_done {
                    break;
                }
                true
            } else {
                !suffixes_done && self.low() & 1 != 0
            };
            if suffix {
                let id = self.pick_affix(Side::Suffix, true);
                if id == 0 {
                    suffixes_done = true;
                    n -= 1;
                } else {
                    ids.suffixes[ns] = id;
                    ns += 1;
                    suffixes_done = ns > 2;
                }
            } else {
                let id = self.pick_affix(Side::Prefix, true);
                if id == 0 {
                    prefixes_done = true;
                    n -= 1;
                } else {
                    ids.prefixes[np] = id;
                    np += 1;
                    prefixes_done = np > 2;
                }
            }
            self.item.quality = Quality::Rare(ids);
            n += 1;
        }
        if np == 0 && ns == 0 {
            return false;
        }
        self.item.flags &= !flags::IDENTIFIED;
        for i in 0..3 {
            self.apply_affix(Side::Prefix, ids.prefixes[i]);
            self.apply_affix(Side::Suffix, ids.suffixes[i]);
        }
        self.staff_mods();
        true
    }

    /// Unique (`0x005566B0`): a row for the item's code, weighted by rarity, once a game unless it
    /// has no limit; unidentified.
    fn unique(&mut self, made: &mut HashSet<u16>) -> bool {
        let (level, expansion, ladder) = (i32::from(self.item.level), self.making.expansion(), self.making.ladder);
        let code = self.def.code;
        let candidates: Vec<(usize, i32)> = self
            .data
            .affixes()
            .uniques
            .iter()
            .enumerate()
            .filter(|(_, u)| (u.version < 100 || expansion) && u.enabled && u.code == code && (ladder || !u.ladder) && u.level <= level)
            .map(|(i, u)| (i, u.rarity.max(1)))
            .collect();
        let Some(id) = self.weighted_last_start(&candidates) else { return false };
        let row = &self.data.affixes().uniques[id];
        let id = id as u16;
        if !self.def.quest && made.contains(&id) {
            return false;
        }
        if !row.no_limit {
            made.insert(id);
        }
        self.item.quality = Quality::Unique(id);
        self.item.flags &= !flags::IDENTIFIED;
        let props = row.props.clone();
        self.apply_all(&props, List::Own);
        true
    }

    /// Set (`0x005C25C0`): a row for the item's code, weighted by rarity; its properties, and its
    /// bonuses into the lists for pieces worn; unidentified.
    fn set(&mut self) -> bool {
        let (level, expansion, code) = (i32::from(self.item.level), self.making.expansion(), self.def.code);
        let candidates: Vec<(usize, i32)> = self
            .data
            .affixes()
            .set_items
            .iter()
            .enumerate()
            .filter(|(_, s)| (s.version < 100 || expansion) && s.level <= level && s.code == code && s.set_row != COW_KING_SET)
            .map(|(i, s)| (i, s.rarity.max(1)))
            .collect();
        let total: i32 = candidates.iter().map(|c| c.1).sum();
        if total == 0 {
            return false;
        }
        let mut roll = self.rand(total);
        let Some(&(id, _)) = candidates.iter().find(|(_, w)| {
            if roll < *w {
                return true;
            }
            roll -= w;
            false
        }) else {
            return false;
        };
        let row = &self.data.affixes().set_items[id];
        self.item.quality = Quality::Set(id as u16);
        self.item.flags &= !flags::IDENTIFIED;
        let (props, bonuses, always) = (row.props.clone(), row.bonuses.clone(), row.add_func == 0);
        self.apply_all(&props, List::Own);
        for (pieces, list) in bonuses.iter().enumerate() {
            let target = if always { List::Own } else { List::Set(pieces) };
            self.apply_all(list, target);
        }
        true
    }

    /// The unique roll's pick: a roll under the total lands on the last candidate starting at or
    /// below it.
    fn weighted_last_start(&mut self, candidates: &[(usize, i32)]) -> Option<usize> {
        let total: i32 = candidates.iter().map(|c| c.1).sum();
        if candidates.is_empty() {
            return None;
        }
        let roll = self.rand(total);
        let mut start = 0;
        let mut pick = candidates[0].0;
        for &(id, weight) in candidates {
            if start > roll {
                break;
            }
            pick = id;
            start += weight;
        }
        Some(pick)
    }

    // --- after the quality --------------------------------------------------------------------

    /// Ethereal, one time in twenty, in a Lord of Destruction game (`0x00556CA0`).
    fn ethereal(&mut self) {
        if self.for_store || !(self.is("weap") || self.is("armo")) || !self.has_durability() || matches!(self.quality(), 1 | 5) || self.def.quest {
            return;
        }
        if self.rand(100) < 5 {
            self.make_ethereal();
            if self.has_durability() {
                self.item.max_durability = self.item.max_durability / 2 + 1;
                self.item.durability = self.item.max_durability;
            }
        }
    }

    /// The ethereal flag, and half as much defence again on armour (`0x0065E4D0`).
    fn make_ethereal(&mut self) {
        self.item.flags |= flags::ETHEREAL;
        if !self.is("weap") {
            self.item.defense = self.item.defense * 3 / 2;
        }
    }

    /// Sockets, one normal or superior item in three, no more than the difficulty allows
    /// (`0x00556B60`); never body armour in a classic game.
    fn sockets(&mut self) {
        if self.quality() < 2 || !self.def.has_inv || self.def.stackable {
            return;
        }
        let cap = self.socket_cap().min([3, 4, 6][usize::from(self.making.difficulty.min(2))]);
        if cap == 0 {
            return;
        }
        if (self.making.expansion() || !self.is("tors")) && self.rand(100) < 33 {
            self.item.flags |= flags::SOCKETED;
            self.item.sockets = (self.item.seed % cap as u32 + 1) as u8;
        }
    }

    /// Class skills for a type with staff mods (`0x005C1260` → `0x005C0F90`): up to three, their
    /// tier by item level.
    fn staff_mods(&mut self) {
        let Some(class) = self.kind.staff_mods else { return };
        let skills = self.data.skills();
        let Some(first) = skills.first_of(class) else { return };
        let r = self.low() % 100;
        let count = match r {
            91.. => 3,
            71..=90 => 2,
            31..=70 => 1,
            _ => return,
        };
        let level = i32::from(self.item.level);
        let tier = match level {
            37.. if self.making.expansion() => 5,
            25.. => 4,
            19..=24 => 3,
            12..=18 => 2,
            _ => 1,
        };
        let mut chosen = [-1; 3];
        for n in 0..count {
            let r = self.low() % 100;
            let mut t = match r {
                81.. => tier + 1,
                31..=80 => tier,
                11..=30 => tier - 1,
                _ => tier - 2,
            }
            .max(1);
            if self.quality() == 1 && t > 3 {
                t = 4;
            }
            let mut skill = 0;
            for _ in 0..6 {
                skill = (self.low() % 5) as i32 + (t - 1) * 5 + first;
                let fits = skills.get(skill).and_then(|s| s.item_type.as_deref()).map_or(true, |needs| self.is(needs));
                if fits && !chosen.contains(&skill) {
                    chosen[n] = skill;
                    break;
                }
            }
            let skill_level = if !self.making.expansion() || self.quality() != 1 {
                match self.low() % 100 {
                    90.. => 3,
                    60..=89 => 2,
                    _ => 1,
                }
            } else {
                1
            };
            self.put(List::Own, stat::SINGLE_SKILL, skill as u16, skill_level, true);
        }
    }

    // --- affixes -------------------------------------------------------------------------------

    /// The affix table a side picks from.
    fn table(&self, side: Side) -> &[Affix] {
        let a = self.data.affixes();
        match side {
            Side::Prefix => &a.prefixes,
            Side::Suffix => &a.suffixes,
            Side::Auto(_) => &a.auto,
        }
    }

    fn apply_affix(&mut self, side: Side, id: u16) {
        if id == 0 {
            return;
        }
        let data = self.data;
        let table: &[Affix] = match side {
            Side::Prefix => &data.affixes().prefixes,
            Side::Suffix => &data.affixes().suffixes,
            Side::Auto(_) => &data.affixes().auto,
        };
        if let Some(affix) = table.get(usize::from(id) - 1) {
            self.apply_all(&affix.mods, List::Own);
        }
    }

    /// Classic items that stack or are thrown take no affixes or rare names (`0x0065E620`).
    fn takes_affixes(&self) -> bool {
        self.making.expansion() || !(self.def.stackable || self.kind.throwable)
    }

    fn types_allow(&self, itypes: &[String], etypes: &[String]) -> bool {
        !etypes.iter().any(|t| self.is(t)) && itypes.iter().any(|t| self.is(t))
    }

    /// An affix id (row + 1), 0 for none (`0x005C1560`): without `force`, half the time none;
    /// otherwise a weighted pick among the rows that suit the item's affix level, type, class and
    /// quality and share no group with its affixes so far.
    fn pick_affix(&mut self, side: Side, force: bool) -> u16 {
        let coin = self.rng.step();
        if coin & 1 == 0 && !force {
            return 0;
        }
        let level = i32::from(self.item.level).max(self.def.level);
        let magic_level = self.def.magic_level;
        let affix_level = if magic_level == 0 {
            let half = self.def.level / 2;
            if level < 99 - half {
                level - half
            } else {
                2 * level - 99
            }
        } else {
            level + magic_level
        };
        let affix_level = if affix_level < 2 { 1 } else { affix_level.min(99) };
        let quality = self.quality();
        let item_class = self.kind.class;
        let used_groups: Vec<i32> = self.used_groups();
        let mut candidates: Vec<(usize, i32)> = Vec::new();
        for (i, row) in self.table(side).iter().enumerate() {
            if !row.spawnable || (row.version >= 100 && !self.making.expansion()) {
                continue;
            }
            if affix_level < row.level || (row.max_level != 0 && row.max_level < affix_level) {
                continue;
            }
            if !row.rare && matches!(quality, 2 | 3 | 6) {
                continue;
            }
            if !self.affix_allowed(row) {
                continue;
            }
            if let Side::Auto(group) = side {
                if row.group != group {
                    continue;
                }
            }
            if row.frequency == 0 {
                continue;
            }
            let row_class = row.class.as_deref().and_then(class_index);
            if row_class.is_some() && item_class.is_some() && row_class != item_class {
                continue;
            }
            if used_groups.contains(&row.group) {
                continue;
            }
            let weight = if magic_level == 0 { row.frequency } else { row.frequency * row.level };
            candidates.push((i, weight));
        }
        if candidates.is_empty() {
            return 0;
        }
        let total: i32 = candidates.iter().map(|c| c.1).sum();
        let mut roll = self.rand(total + 1);
        let mut pick = candidates[candidates.len() - 1].0;
        for &(i, w) in &candidates {
            roll -= w;
            if roll < 0 {
                pick = i;
                break;
            }
        }
        pick as u16 + 1
    }

    fn affix_allowed(&self, row: &Affix) -> bool {
        if !self.takes_affixes() {
            return false;
        }
        let sockets_ok = self.def.has_inv && self.socket_cap() != 0;
        if !sockets_ok {
            let first_stat = row.mods.first().and_then(|m| self.data.affixes().property(&m.code)).and_then(|f| f.first()).and_then(|f| f.stat.as_deref());
            if first_stat.is_some_and(|s| self.data.item_stats().id(s) == Some(stat::NUM_SOCKETS)) {
                return false;
            }
        }
        self.types_allow(&row.itypes, &row.etypes)
    }

    /// The groups of the prefixes and suffixes the item has (`0x005C1500`).
    fn used_groups(&self) -> Vec<i32> {
        let a = self.data.affixes();
        let (prefixes, suffixes): (Vec<u16>, Vec<u16>) = match self.item.quality {
            Quality::Magic { prefix, suffix } => (vec![prefix], vec![suffix]),
            Quality::Rare(ids) | Quality::Crafted(ids) => (ids.prefixes.to_vec(), ids.suffixes.to_vec()),
            _ => (Vec::new(), Vec::new()),
        };
        let group = |table: &[Affix], id: u16| (id != 0).then(|| table.get(usize::from(id) - 1).map(|r| r.group)).flatten();
        prefixes.iter().filter_map(|&id| group(&a.prefixes, id)).chain(suffixes.iter().filter_map(|&id| group(&a.suffixes, id))).collect()
    }

    /// A rare name part's id, counted from `offset`, uniform among the parts that suit the item
    /// (`0x005C1AB0`, `0x0065E710`); 0 for none.
    fn pick_rare_name(&mut self, names: &[RareName], offset: usize) -> u16 {
        if !self.takes_affixes() {
            return 0;
        }
        let expansion = self.making.expansion();
        let fits: Vec<usize> = names.iter().enumerate().filter(|(_, n)| (n.version < 100 || expansion) && self.types_allow(&n.itypes, &n.etypes)).map(|(i, _)| i).collect();
        if fits.is_empty() {
            return 0;
        }
        let at = self.rand(fits.len() as i32) as usize;
        (fits[at] + offset + 1) as u16
    }

    // --- properties -----------------------------------------------------------------------------

    fn apply_all(&mut self, mods: &[Mod], list: List) {
        for m in mods {
            self.apply(m, list);
        }
    }

    fn list(&mut self, list: List) -> &mut Vec<ItemStat> {
        match list {
            List::Own => &mut self.item.stats,
            List::Set(i) => &mut self.item.set_stats[i.min(4)],
        }
    }

    /// Add `value` (or set it) to stat `id` with `param`, shifted as the stat is kept
    /// (`0x0065EA50`). Returns the value, 0 when nothing was put.
    fn put(&mut self, list: List, id: u16, param: u16, value: i32, set: bool) -> i32 {
        if value == 0 {
            return 0;
        }
        let Some(cost) = self.data.item_stats().get(id) else { return 0 };
        let shifted = value << cost.val_shift;
        let stats = self.list(list);
        match stats.iter_mut().find(|s| s.id == id && s.param == param) {
            Some(s) if set => s.value = shifted,
            Some(s) => s.value += shifted,
            None => stats.push(ItemStat { id, param, value: shifted }),
        }
        value
    }

    /// One property's functions in turn, each after the first handed the first's value
    /// (`0x0065FD70`).
    fn apply(&mut self, m: &Mod, list: List) {
        let Some(funcs) = self.data.affixes().property(&m.code) else { return };
        let mut first = 0;
        for (i, f) in funcs.iter().enumerate() {
            let stat = f.stat.as_deref().and_then(|s| self.data.item_stats().id(s));
            let value = self.func(f.func, stat, f.set != 0, f.val, m, first, list);
            if i == 0 {
                first = value;
            }
        }
    }

    /// A skill's level for a proc or charges: from the item level when the mod gives none, scaled
    /// over the levels left when it gives a negative (`0x0065F470`).
    fn skill_level(&mut self, skill: i32, given: i32) -> i32 {
        let (req, max) = self.data.skills().get(skill).map_or((0, 0), |s| (s.req_level, s.max_level));
        let item_level = i32::from(self.item.level);
        if given > 0 {
            given
        } else if given == 0 {
            let level = (item_level - req) / 4 + 1;
            if max <= level.max(1) {
                max
            } else if level <= 1 {
                1
            } else {
                level
            }
        } else {
            let div = (-((99 - req).max(1) / given)).max(1);
            ((item_level - req) / div).max(1)
        }
    }

    /// One property function (`0x007462F8`): the value it put, for the functions after it.
    #[allow(clippy::too_many_arguments)] // a property function's own arguments
    fn func(&mut self, func: i32, stat: Option<u16>, set: bool, val: i32, m: &Mod, first: i32, list: List) -> i32 {
        let or_roll = |me: &mut Self| if first != 0 { first } else { me.roll(m.min, m.max) };
        let param = m.param.clamp(0, i32::from(u16::MAX)) as u16;
        match func {
            1 | 2 => {
                let v = self.roll(m.min, m.max);
                stat.map_or(0, |s| self.put(list, s, 0, v, set))
            }
            3 | 4 | 8 => {
                let v = or_roll(self);
                stat.map_or(0, |s| self.put(list, s, 0, v, set))
            }
            5 => {
                let v = or_roll(self);
                self.damage(list, v, false, set)
            }
            6 => {
                let v = or_roll(self);
                self.damage(list, v, true, set)
            }
            7 => {
                let v = or_roll(self);
                let top = self.def.damage.1.max(self.def.two_hand_damage.1);
                if self.is("weap") && top * v / 100 <= 0 {
                    return self.damage(list, 1, true, set);
                }
                self.put(list, stat::MINDAMAGE_PERCENT, 0, v, set);
                self.put(list, stat::MAXDAMAGE_PERCENT, 0, v, set);
                v
            }
            9 | 24 => {
                let v = or_roll(self);
                if func == 9 && self.data.skills().get(m.param).is_none() {
                    return 0;
                }
                stat.map_or(0, |s| self.put(list, s, param, v, set))
            }
            10 => {
                let v = or_roll(self);
                let tab = (m.param % 3 + (m.param / 3) * 8).clamp(0, i32::from(u16::MAX)) as u16;
                stat.map_or(0, |s| self.put(list, s, tab, v, set))
            }
            11 => {
                if self.data.skills().get(m.param).is_none() {
                    return 0;
                }
                let chance = if m.min < 1 { 5 } else { m.min };
                let level = self.skill_level(m.param, m.max);
                let p = (m.param * 64 + (level & 63)) as u16;
                stat.map_or(0, |s| self.put(list, s, p, chance, set))
            }
            12 => {
                let p = self.roll(m.min, m.max).clamp(0, i32::from(u16::MAX)) as u16;
                stat.map_or(0, |s| self.put(list, s, p, m.param, set))
            }
            13 => {
                let v = self.roll(m.min, m.max);
                let put = stat.map_or(0, |s| self.put(list, s, 0, v, set));
                if put != 0 && self.item.max_durability > 0 {
                    let percent = self.stat_value(75);
                    self.item.durability = self.item.max_durability + self.item.max_durability * percent / 100;
                }
                put
            }
            14 => {
                let (w, h) = self.def.inv_size;
                let cap = (i32::from(w) * i32::from(h)).min(6).min(self.socket_cap());
                let mut v = if first > 0 { first } else { self.roll(m.min, m.max) };
                if v < 1 {
                    v = m.param;
                }
                let count = if v.max(1) < cap {
                    v.max(1)
                } else if cap < 1 {
                    return 0;
                } else {
                    cap
                };
                self.item.flags |= flags::SOCKETED;
                self.item.sockets = count as u8;
                count
            }
            15 => {
                if stat == Some(stat::MINDAMAGE) {
                    return self.damage(list, m.min, false, set);
                }
                stat.map_or(0, |s| self.put(list, s, 0, m.min, set))
            }
            16 => {
                if stat == Some(stat::MAXDAMAGE) {
                    return self.damage(list, m.max, true, set);
                }
                stat.map_or(0, |s| self.put(list, s, 0, m.max, set))
            }
            17 => {
                let v = if m.param != 0 { m.param } else { self.roll(m.min, m.max) };
                if v == 0 {
                    return 0;
                }
                if stat == Some(stat::MAXDAMAGE) {
                    return self.damage(list, v, true, set);
                }
                stat.map_or(0, |s| self.put(list, s, 0, v, set))
            }
            18 => {
                let period = m.param.clamp(0, 3);
                let lo = (m.min + 256).clamp(0, 1023);
                let hi = (m.max + 256).clamp(0, 1023);
                stat.map_or(0, |s| self.put(list, s, 0, period + (hi * 1024 + lo) * 4, true))
            }
            19 => {
                if self.data.skills().get(m.param).is_none() {
                    return 0;
                }
                let level = self.skill_level(m.param, m.max);
                let mut charges = m.min;
                if charges == 0 {
                    charges = 5;
                } else if charges < 0 {
                    charges = -charges + (-charges * level) / 8;
                }
                let charges = charges.clamp(1, 255);
                let now = (self.rand(charges - charges / 8) + 1 + charges / 8) & 0xFF;
                let p = (m.param * 64 + (level & 63)) as u16;
                stat.map_or(0, |s| self.put(list, s, p, now + charges * 256, true))
            }
            20 => self.put(list, stat::INDESTRUCTIBLE, 0, 1, false),
            21 => {
                let v = self.roll(m.min, m.max);
                let p = val.clamp(0, i32::from(u16::MAX)) as u16;
                stat.map_or(0, |s| self.put(list, s, p, v, set))
            }
            22 => {
                let v = self.roll(m.min, m.max);
                stat.map_or(0, |s| self.put(list, s, param, v, set))
            }
            23 => {
                if self.item.flags & flags::ETHEREAL == 0 && self.has_durability() {
                    self.make_ethereal();
                    return 1;
                }
                0
            }
            36 => {
                let p = self.roll(m.min, m.max).clamp(0, i32::from(u16::MAX)) as u16;
                stat.map_or(0, |s| self.put(list, s, p, val, set))
            }
            _ => 0,
        }
    }

    /// Minimum (`0x0065ECF0`) or maximum (`0x0065EE80`) damage: onto the one-handed, two-handed
    /// and thrown stats the weapon has (every one on anything else), no lower than leaves it 1.
    fn damage(&mut self, list: List, value: i32, max: bool, set: bool) -> i32 {
        let def = self.def;
        let (one, two, thrown) =
            if max { (def.damage.1, def.two_hand_damage.1, def.missile_damage.1) } else { (def.damage.0, def.two_hand_damage.0, def.missile_damage.0) };
        let (one_stat, two_stat, thrown_stat) = if max {
            (stat::MAXDAMAGE, stat::SECONDARY_MAXDAMAGE, stat::THROW_MAXDAMAGE)
        } else {
            (stat::MINDAMAGE, stat::SECONDARY_MINDAMAGE, stat::THROW_MINDAMAGE)
        };
        let floor = |base: i32| if base != 0 && base + value < 1 { if max { -base } else { 1 - base } } else { value };
        let weapon = self.is("weap");
        if !weapon || one != 0 || two == 0 {
            self.put(list, one_stat, 0, floor(one), set);
        }
        if !weapon || two != 0 || one == 0 {
            self.put(list, two_stat, 0, floor(two), set);
        }
        if weapon && !self.kind.throwable {
            return value;
        }
        self.put(list, thrown_stat, 0, floor(thrown), set);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::affixes::Affixes;
    use d2_data::item_bits::{self, Target};
    use d2_data::item_stats::{ItemRatios, ItemStats};
    use d2_data::items::{code, Items};
    use d2_data::skills::Skills;
    use d2_formats::excel::Table;

    fn rules() -> GameData {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\tToHitFactor\tLifePerLevel\tStaminaPerLevel\tManaPerLevel\tLifePerVitality\tStaminaPerVitality\tManaPerMagic\tStatPerLevel\tRunDrain\r\n");
        for name in ["Amazon", "Sorceress", "Necromancer", "Paladin", "Barbarian", "Druid", "Assassin"] {
            cs.push_str(&format!("{name}\t30\t27\t10\t25\t0\t92\t30\t20\t8\t4\t4\t16\t4\t4\t5\t20\r\n"));
        }
        let exp = "Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\tExpRatio\r\nMaxLvl\t99\t99\t99\t99\t99\t99\t99\t10\r\n0\t0\t0\t0\t0\t0\t0\t0\t1024\r\n1\t500\t500\t500\t500\t500\t500\t500\t1024\r\n";
        let mut data = GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(exp.as_bytes())).unwrap();
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\tThrowable\tMagic\tRare\tNormal\tMaxSock1\tMaxSock25\tMaxSock40\tStaffMods\tClass\tVarInvGfx\r\n\
              Weapon\tweap\t\t\t0\t0\t1\t0\t0\t0\t0\t\t\t0\r\n\
              Axe\taxe\tweap\t\t0\t0\t1\t0\t4\t5\t6\t\t\t0\r\n\
              Armor\tarmo\t\t\t0\t0\t1\t0\t0\t0\t0\t\t\t0\r\n\
              Helm\thelm\tarmo\t\t0\t0\t1\t0\t2\t2\t2\t\t\t0\r\n\
              Wand\twand\tweap\t\t0\t0\t1\t0\t1\t2\t2\tnec\t\t0\r\n\
              Ring\tring\t\t\t0\t1\t1\t0\t0\t0\t0\t\t\t5\r\n",
        );
        let weapons = Table::parse(
            b"name\tcode\ttype\tlevel\tmindam\tmaxdam\tdurability\tgemsockets\thasinv\tinvwidth\tinvheight\r\n\
              Hand Axe\thax\taxe\t3\t3\t6\t28\t2\t1\t1\t3\r\n\
              Wand\twnd\twand\t2\t2\t4\t15\t1\t1\t1\t2\r\n",
        );
        let armor = Table::parse(b"name\tcode\ttype\tlevel\tminac\tmaxac\tdurability\tgemsockets\thasinv\tinvwidth\tinvheight\r\nCap\tcap\thelm\t1\t3\t5\t12\t2\t1\t2\t2\r\n");
        let misc = Table::parse(b"name\tcode\ttype\tlevel\tspawnable\r\nRing\trin\tring\t1\t1\r\n");
        data.set_items(Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap(), Vec::new());
        let stats = ItemStats::from_table(&Table::parse(
            b"Stat\tID\tSave Bits\tSave Add\tSave Param Bits\tValShift\r\n\
              maxdamage_percent\t17\t9\t0\t\t\r\nmindamage_percent\t18\t9\t0\t\t\r\n\
              mindamage\t21\t6\t0\t\t\r\nmaxdamage\t22\t7\t0\t\t\r\nsecondary_mindamage\t23\t6\t0\t\t\r\nsecondary_maxdamage\t24\t7\t0\t\t\r\n\
              armorclass\t31\t11\t10\t\t\r\nmaxhp\t7\t9\t32\t\t8\r\ndurability\t72\t8\t0\t\t\r\nmaxdurability\t73\t8\t0\t\t\r\n\
              item_singleskill\t107\t3\t0\t9\t\r\ntohit\t19\t10\t0\t\t\r\n\
              item_numsockets\t194\t4\t0\t\t\r\n",
        ))
        .unwrap();
        let ratios = ItemRatios::from_table(&Table::parse(
            b"Function\tVersion\tUber\tClass Specific\tUnique\tUniqueDivisor\tUniqueMin\tRare\tRareDivisor\tRareMin\tSet\tSetDivisor\tSetMin\tMagic\tMagicDivisor\tMagicMin\tHiQuality\tHiQualityDivisor\tNormal\tNormalDivisor\r\n\
              Item Ratio\t0\t0\t0\t400\t1\t6400\t100\t2\t3200\t160\t2\t5600\t34\t3\t192\t12\t8\t5\t2\r\n",
        ));
        data.set_item_rules(stats, ratios);
        let prefix = Table::parse(
            b"Name\tversion\tspawnable\trare\tlevel\tmaxlevel\tclassspecific\tfrequency\tgroup\tmod1code\tmod1param\tmod1min\tmod1max\titype1\titype2\tetype1\r\n\
              Jagged\t0\t1\t1\t1\t\t\t1\t1\tdmg%\t\t10\t20\tweap\t\t\r\n\
              Sturdy\t0\t1\t1\t1\t\t\t1\t2\tac\t\t1\t3\tarmo\t\t\r\n",
        );
        let suffix = Table::parse(
            b"Name\tversion\tspawnable\trare\tlevel\tmaxlevel\tclassspecific\tfrequency\tgroup\tmod1code\tmod1param\tmod1min\tmod1max\titype1\titype2\tetype1\r\n\
              of Maiming\t0\t1\t1\t1\t\t\t1\t3\tdmg-max\t\t1\t2\tweap\t\t\r\n\
              of Life\t0\t1\t1\t1\t\t\t1\t4\thp\t\t5\t10\tarmo\tweap\t\r\n",
        );
        let rare_names = Table::parse(b"name\tversion\titype1\r\nBeast\t0\tweap\r\nDoom\t0\tarmo\r\n");
        let props = Table::parse(
            b"code\tset1\tval1\tfunc1\tstat1\r\ndmg%\t\t\t7\t\r\nac\t\t\t1\tarmorclass\r\ndmg-max\t\t\t6\t\r\nhp\t\t\t1\tmaxhp\r\natt\t\t\t1\ttohit\r\n",
        );
        let quality = Table::parse(b"nummods\tmod1code\tmod1param\tmod1min\tmod1max\tarmor\tweapon\r\n1\tatt\t\t1\t3\t0\t1\r\n1\tac\t\t1\t2\t1\t0\r\n");
        let low = Table::parse(b"Name\r\nCrude\r\nCracked\r\n");
        let uniques = Table::parse(b"index\tversion\tenabled\trarity\tnolimit\tlvl\tlvl req\tcode\tprop1\tpar1\tmin1\tmax1\r\nThe Gnasher\t0\t1\t1\t\t5\t5\thax\tdmg%\t\t60\t70\r\n");
        let empty = Table::parse(b"Name\r\n");
        let set_items = Table::parse(b"index\tset\titem\trarity\tlvl\r\n");
        let sets = Table::parse(b"index\tversion\r\n");
        data.set_affixes(Affixes::from_tables(&prefix, &suffix, &empty, &rare_names, &rare_names, &props, &quality, &low, &uniques, &set_items, &sets));
        let mut skills = String::from("skill\tcharclass\titypea1\treqlevel\tmaxlvl\r\nAttack\t\t\t\t\r\n");
        for n in 0..10 {
            skills.push_str(&format!("Bone {n}\tnec\t\t{}\t20\r\n", 1 + n / 5 * 6));
        }
        data.set_skills(Skills::from_table(&Table::parse(skills.as_bytes())));
        data
    }

    const CLASSIC: Making = Making { version: 2, difficulty: 0, ladder: false, magic_find: 0 };

    fn many(data: &GameData, c: &str, level: i32, mods: QualityMods) -> Vec<Item> {
        let mut made = HashSet::new();
        (0..2000u32).filter_map(|s| make(data, code(c), level, mods, CLASSIC, &mut made, s.wrapping_mul(2_654_435_761))).collect()
    }

    #[test]
    fn armour_gets_defence_and_durability_from_its_row() {
        let data = rules();
        for cap in many(&data, "cap", 1, QualityMods::default()) {
            let low_quality = matches!(cap.quality, Quality::Inferior(_));
            if low_quality {
                assert!((1..=3).contains(&cap.defense), "three quarters, at least 1: {}", cap.defense);
                assert_eq!(cap.max_durability, 3, "a third of 12");
            } else {
                assert!((3..=5).contains(&cap.defense), "{}", cap.defense);
                assert_eq!(cap.max_durability, 12);
                assert!((6..=11).contains(&cap.durability), "half again at most: {}", cap.durability);
            }
        }
    }

    #[test]
    fn qualities_come_out_in_their_proportions_and_follow_their_rules() {
        let data = rules();
        let axes = many(&data, "hax", 10, QualityMods::default());
        let count = |f: &dyn Fn(&Quality) -> bool| axes.iter().filter(|i| f(&i.quality)).count();
        let (magic, rare, unique, superior, normal, inferior) = (
            count(&|q| matches!(q, Quality::Magic { .. })),
            count(&|q| matches!(q, Quality::Rare(_))),
            count(&|q| matches!(q, Quality::Unique(_))),
            count(&|q| matches!(q, Quality::Superior(_))),
            count(&|q| matches!(q, Quality::Normal)),
            count(&|q| matches!(q, Quality::Inferior(_))),
        );
        assert_eq!(unique, 1, "The Gnasher drops once a game");
        assert!(magic > 40 && rare > 5 && superior > 50 && normal > 700 && inferior > 700, "{magic} {rare} {unique} {superior} {normal} {inferior}");
        for axe in &axes {
            match axe.quality {
                Quality::Magic { prefix, suffix } => {
                    assert!(prefix != 0 || suffix != 0);
                    assert!(!axe.identified(), "magic items drop unidentified");
                    assert!(!axe.stats.is_empty());
                }
                Quality::Rare(ids) => {
                    assert_eq!(ids.names, (3, 1), "Beast: a prefix name counted after both suffix names, then a suffix name");
                    assert!(!axe.identified());
                }
                Quality::Superior(row) => assert_eq!(row, 0, "the weapon row"),
                _ => {}
            }
            if axe.flags & flags::SOCKETED != 0 {
                assert!((1..=2).contains(&axe.sockets), "gemsockets 2");
                assert!(matches!(axe.quality, Quality::Normal | Quality::Superior(_)));
            }
            let bytes = item_bits::write(axe, data.items(), data.item_stats(), Target::Save);
            let (back, _) = item_bits::read(&bytes, data.items(), data.item_stats(), Target::Save).unwrap();
            assert_eq!(back.quality, axe.quality);
        }
    }

    #[test]
    fn magic_mods_and_unique_mods_raise_their_odds() {
        let data = rules();
        let magic = QualityMods { magic: 1024, ..QualityMods::default() };
        let axes = many(&data, "hax", 10, magic);
        let magic_axes = axes.iter().filter(|a| matches!(a.quality, Quality::Magic { .. })).count();
        assert!(magic_axes > 1900, "a mod of 1024 leaves nothing to roll under, so every test reaching magic passes: {magic_axes}");
        let axes = many(&data, "hax", 10, QualityMods { unique: 1024, ..QualityMods::default() });
        let rares = axes.iter().filter(|a| matches!(a.quality, Quality::Rare(_))).count();
        assert!(rares > 1900, "past its one unique, a failed unique becomes rare: {rares}");
    }

    #[test]
    fn a_rare_axe_carries_its_affix_stats() {
        let data = rules();
        let axe = many(&data, "hax", 10, QualityMods::default()).into_iter().find(|a| matches!(a.quality, Quality::Rare(_))).unwrap();
        let Quality::Rare(ids) = axe.quality else { unreachable!() };
        assert!(ids.prefixes.iter().chain(&ids.suffixes).filter(|&&i| i != 0).count() >= 1);
        assert!(ids.prefixes.iter().all(|&p| p == 0 || p == 1), "only Jagged fits a weapon");
        assert!(ids.prefixes.iter().filter(|&&p| p == 1).count() <= 1, "one affix per group");
    }

    #[test]
    fn wands_roll_necromancer_skills() {
        let data = rules();
        let wands = many(&data, "wnd", 5, QualityMods::default());
        let rolled: Vec<&Item> = wands.iter().filter(|w| !matches!(w.quality, Quality::Inferior(_))).collect();
        let skilled = rolled.iter().filter(|w| w.stats.iter().any(|s| s.id == stat::SINGLE_SKILL)).count();
        assert!(skilled * 100 > rolled.len() * 60, "seven in ten past low quality have one: {skilled} of {}", rolled.len());
        for w in &wands {
            for s in w.stats.iter().filter(|s| s.id == stat::SINGLE_SKILL) {
                assert!((1..=10).contains(&s.param), "a first or second tier necromancer skill: {}", s.param);
                assert!((1..=3).contains(&s.value));
            }
        }
    }

    #[test]
    fn rings_are_always_magic_and_pick_a_picture() {
        let data = rules();
        for ring in many(&data, "rin", 10, QualityMods::default()).iter().take(50) {
            assert!(ring.picture.is_some_and(|p| p < 5));
            assert!(matches!(ring.quality, Quality::Magic { .. } | Quality::Rare(_) | Quality::Normal), "{:?}", ring.quality);
        }
    }

    #[test]
    fn damage_properties_land_on_the_stats_the_weapon_uses() {
        let data = rules();
        let class = data.items().class_of(&code("hax")).unwrap();
        let def = data.items().get(class).unwrap();
        let kind = data.items().types().get(def.item_type).unwrap();
        let item = Item::new(code("hax"), 2, 10, Location::Ground { x: 0, y: 0 });
        let mut m = Maker { data: &data, making: CLASSIC, class, def, kind, rng: Seed::new(1, 666), item, for_store: false };
        m.apply(&Mod { code: "dmg-max".into(), param: 0, min: 2, max: 2 }, List::Own);
        m.apply(&Mod { code: "dmg%".into(), param: 0, min: 10, max: 10 }, List::Own);
        assert_eq!(m.item.stats.iter().find(|s| s.id == 22).map(|s| s.value), Some(3), "10% of 6 is nothing: a point of maximum damage instead");
        m.item.stats.clear();
        m.apply(&Mod { code: "dmg-max".into(), param: 0, min: 2, max: 2 }, List::Own);
        m.apply(&Mod { code: "dmg%".into(), param: 0, min: 20, max: 20 }, List::Own);
        m.apply(&Mod { code: "hp".into(), param: 0, min: 4, max: 4 }, List::Own);
        let mut stats = m.item.stats.clone();
        stats.sort();
        assert_eq!(
            stats,
            [
                ItemStat { id: 7, param: 0, value: 4 << 8 },
                ItemStat { id: 17, param: 0, value: 20 },
                ItemStat { id: 18, param: 0, value: 20 },
                ItemStat { id: 22, param: 0, value: 2 },
            ],
            "a one-handed axe: maximum damage only, not the two-handed or thrown"
        );
    }

    #[test]
    fn with_a_real_install_drops_are_made_and_written() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else { return };
        let data = GameData::load(dir).unwrap();
        let mut made = HashSet::new();
        let mut qualities = [0usize; 10];
        let mut seed = 1u32;
        for c in ["hax", "lsd", "cap", "lrg", "rin", "amu", "wnd", "sst", "buc", "lgl", "vbl", "lbt", "sbw", "jav"] {
            for level in [3, 20, 45, 80] {
                for _ in 0..200 {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    for making in [CLASSIC, Making { version: 101, difficulty: 2, ladder: true, magic_find: 300 }] {
                        let Some(item) = make(&data, code(c), level, QualityMods::default(), making, &mut made, seed) else { panic!("{c} is made") };
                        qualities[usize::from(item.quality.number())] += 1;
                        for target in [Target::Save, Target::Network] {
                            let bytes = item_bits::write(&item, data.items(), data.item_stats(), target);
                            let (back, used) = item_bits::read(&bytes, data.items(), data.item_stats(), target).unwrap_or_else(|e| panic!("{c} {:?}: {e:?}", item.quality));
                            assert_eq!(used, bytes.len());
                            assert_eq!(back.quality.number(), item.quality.number());
                            if target == Target::Save {
                                let mut sorted = item.stats.iter().filter(|s| s.value >> data.item_stats().get(s.id).map_or(0, |c| c.val_shift) != 0).copied().collect::<Vec<_>>();
                                sorted.sort();
                                let mut read = back.stats.clone();
                                read.sort();
                                assert_eq!(read.len(), sorted.len(), "{c} {:?}", item.quality);
                            }
                        }
                    }
                }
            }
        }
        assert!(qualities[4] > 0 && qualities[6] > 0 && qualities[7] > 0 && qualities[5] > 0 && qualities[3] > 0 && qualities[1] > 0, "{qualities:?}");
    }
}
