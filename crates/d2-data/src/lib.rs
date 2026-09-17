//! Diablo II game rules, read from the operator's install.
//!
//! Every number here comes from the excel tables in the operator's MPQs at run time
//! (`diablo2.data_dir`); this repository ships none of them. Functions that turn table rows into
//! game state reproduce a 1.14d engine routine and cite its address.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use d2_formats::animdata::AnimData;
use d2_formats::excel::Table;
use d2_formats::mpq::{self, ArchiveSet, DATA_ARCHIVES};

pub mod affixes;
pub mod appearance;
pub mod character;
pub mod engine;
pub mod item_bits;
pub mod item_stats;
pub mod items;
pub mod levels;
pub mod lvlsub;
pub mod missiles;
pub mod monlvl;
pub mod monsters;
pub mod panels;
pub mod presets;
pub mod skills;
pub mod stat;
pub mod strings;
pub mod tiles;
pub mod trade;
pub mod treasure;

use items::{Code, Items};
use levels::Levels;
use lvlsub::LvlSubs;
use monlvl::MonLvls;
use monsters::Monsters;
use presets::{LvlPrests, MonPresets, Objects, Shrines};
use strings::Strings;
use tiles::{LvlMazes, LvlTypes, LvlWarps};

/// Classes in `charstats.txt` order, which is the engine's class id.
pub const CLASSES: [&str; 7] = ["Amazon", "Sorceress", "Necromancer", "Paladin", "Barbarian", "Druid", "Assassin"];

/// Why the game data could not be loaded.
#[derive(Debug)]
pub enum Error {
    /// The archives could not be read.
    Mpq(mpq::Error),
    /// A table the rules need is not in the install.
    MissingTable(&'static str),
    /// A table is there but not in the shape expected.
    BadTable {
        /// Which table.
        table: &'static str,
        /// What was wrong.
        problem: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mpq(e) => write!(f, "{e}"),
            Self::MissingTable(t) => write!(f, "{t} is not in the install"),
            Self::BadTable { table, problem } => write!(f, "{table}: {problem}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mpq::Error> for Error {
    fn from(e: mpq::Error) -> Self {
        Self::Mpq(e)
    }
}

/// A class's starting attributes: the `charstats.txt` columns the engine copies into a new
/// character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassStats {
    /// `str`.
    pub strength: u8,
    /// `dex`.
    pub dexterity: u8,
    /// `int` — the engine's energy.
    pub energy: u8,
    /// `vit`.
    pub vitality: u8,
    /// `stamina`.
    pub stamina: u8,
    /// `hpadd`: life on top of vitality at level 1.
    pub life_bonus: u8,
    /// `ToHitFactor`: attack rating the class adds.
    pub to_hit_factor: i32,
    /// `RunDrain`: how fast running spends stamina.
    pub run_drain: i32,
    /// `LifePerLevel`, `StaminaPerLevel`, `ManaPerLevel`: quarters of a point per level.
    pub per_level: (i32, i32, i32),
    /// `StatPerLevel`: stat points per level.
    pub stat_per_level: i32,
    /// `LifePerVitality`, `StaminaPerVitality`, `ManaPerMagic`: quarters of a point per point
    /// of vitality or energy.
    pub per_point: (i32, i32, i32),
    /// `WalkVelocity`, `RunVelocity`.
    pub velocity: (i32, i32),
}

/// An item a new character starts with: `charstats.txt` `item1`…`item10` with their `loc` and
/// `count` (read by `0x00534F10`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartItem {
    /// `itemN`: the item's code.
    pub code: Code,
    /// `itemNloc`: the body location it is worn at (`rarm`, `larm`), when it is worn.
    pub location: Option<String>,
    /// `itemNcount`: how many.
    pub count: u8,
}

/// The rules loaded so far.
#[derive(Debug, Clone)]
pub struct GameData {
    classes: [ClassStats; 7],
    /// Each class's starting items, in column order.
    start_items: [Vec<StartItem>; 7],
    /// Each class's `StartSkill`: the skill its first starting item carries a point of.
    start_skills: [Option<String>; 7],
    /// `experience.txt` by level (row `"0"` first), one column per class.
    experience: Vec<[u32; 7]>,
    levels: Levels,
    lvl_prests: LvlPrests,
    lvl_subs: LvlSubs,
    lvl_types: LvlTypes,
    lvl_warps: LvlWarps,
    lvl_mazes: LvlMazes,
    mon_presets: MonPresets,
    monsters: Monsters,
    monster_levels: MonLvls,
    anim_data: AnimData,
    objects: Objects,
    shrines: Shrines,
    panels: panels::Panels,
    items: Items,
    item_stats: item_stats::ItemStats,
    item_ratios: item_stats::ItemRatios,
    affixes: affixes::Affixes,
    skills: skills::Skills,
    missiles: missiles::Missiles,
    treasure: treasure::TreasureClasses,
    /// `Npc.txt`: vendors' price multipliers.
    npc_trades: trade::NpcTrades,
    /// `Books.txt`.
    books: Vec<trade::Book>,
    /// `ArmType.txt`'s tokens, by body armour weight.
    armor_types: Vec<Code>,
    /// The install's archives, kept open for map files; `None` when built from tables.
    archives: Option<Arc<ArchiveSet>>,
}

impl GameData {
    /// Load from an install directory holding the MPQs.
    ///
    /// # Errors
    ///
    /// [`Error`] if the archives or the tables are missing or malformed.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let archives = ArchiveSet::open(dir, &DATA_ARCHIVES)?;
        let read = |name: &'static str| -> Result<Table, Error> {
            let bytes = archives
                .read(&format!("data\\global\\excel\\{name}"))?
                .ok_or(Error::MissingTable(name))?;
            Ok(Table::parse(&bytes))
        };
        let mut data = Self::from_tables(&read("charstats.txt")?, &read("experience.txt")?)?;
        data.levels = Levels::from_table(&read("levels.txt")?)?;
        data.lvl_prests = LvlPrests::from_table(&read("lvlprest.txt")?)?;
        data.lvl_subs = LvlSubs::from_table(&read("lvlsub.txt")?)?;
        data.lvl_types = LvlTypes::from_table(&read("lvltypes.txt")?)?;
        data.lvl_warps = LvlWarps::from_table(&read("lvlwarp.txt")?)?;
        data.lvl_mazes = LvlMazes::from_table(&read("lvlmaze.txt")?)?;
        let monstats = read("monstats.txt")?;
        data.mon_presets =
            MonPresets::from_tables(&read("monpreset.txt")?, &monstats, &read("superuniques.txt")?, &read("monplace.txt")?)?;
        data.monsters = Monsters::from_tables(&monstats, &read("monstats2.txt")?)?;
        data.npc_trades = trade::NpcTrades::from_table(&read("npc.txt")?, &data.monsters);
        data.monster_levels = MonLvls::from_table(&read("monlvl.txt")?)?;
        let anim = archives.read("data\\global\\animdata.d2")?.ok_or(Error::MissingTable("animdata.d2"))?;
        data.anim_data = AnimData::parse(&anim).map_err(|e| Error::BadTable { table: "animdata.d2", problem: e.to_string() })?;
        data.objects = Objects::from_table(&read("objects.txt")?);
        data.shrines = Shrines::from_table(&read("shrines.txt")?);
        data.panels = panels::Panels::from_table(&read("inventory.txt")?);
        data.items = Items::from_tables(&read("itemtypes.txt")?, &read("weapons.txt")?, &read("armor.txt")?, &read("misc.txt")?)?;
        data.armor_types = read("armtype.txt")?.rows().filter_map(|r| r.get("Token").map(items::code)).collect();
        data.item_stats = item_stats::ItemStats::from_table(&read("itemstatcost.txt")?)?;
        data.item_ratios = item_stats::ItemRatios::from_table(&read("itemratio.txt")?);
        data.affixes = affixes::Affixes::from_tables(
            &read("magicprefix.txt")?,
            &read("magicsuffix.txt")?,
            &read("automagic.txt")?,
            &read("rareprefix.txt")?,
            &read("raresuffix.txt")?,
            &read("properties.txt")?,
            &read("qualityitems.txt")?,
            &read("lowqualityitems.txt")?,
            &read("uniqueitems.txt")?,
            &read("setitems.txt")?,
            &read("sets.txt")?,
        );
        data.skills = skills::Skills::from_table(&read("skills.txt")?);
        data.missiles = missiles::Missiles::from_table(&read("missiles.txt")?);
        data.books = trade::books_from_table(&read("books.txt")?);
        data.treasure = treasure::TreasureClasses::from_table(&read("treasureclassex.txt")?)?;
        data.treasure.add_item_classes(&data.items);
        data.archives = Some(Arc::new(archives));
        Ok(data)
    }

    /// Read a file from the install, e.g. `data\global\tiles\Act1\Town\TownN1.ds1`.
    /// `Ok(None)` if there is no such file or no install was loaded.
    ///
    /// # Errors
    ///
    /// [`Error::Mpq`] if the file is there but cannot be read.
    pub fn read_file(&self, member: &str) -> Result<Option<Vec<u8>>, Error> {
        match &self.archives {
            Some(a) => Ok(a.read(member)?),
            None => Ok(None),
        }
    }

    /// A language's string tables, e.g. `"eng"`; tables the install lacks are left out.
    ///
    /// # Errors
    ///
    /// [`Error::Mpq`] if a table is there but cannot be read, [`Error::BadTable`] if it does not
    /// parse.
    pub fn strings(&self, language: &str) -> Result<Strings, Error> {
        let mut tables = Vec::new();
        for name in strings::TABLES {
            if let Some(bytes) = self.read_file(&format!("data\\local\\lng\\{language}\\{name}"))? {
                let table = d2_formats::tbl::StringTable::parse(&bytes)
                    .ok_or(Error::BadTable { table: name, problem: "not a string table".into() })?;
                tables.push(table);
            }
        }
        Ok(Strings::from_tables(tables))
    }

    /// `Weapons.txt`, `Armor.txt` and `Misc.txt`, with `ItemTypes.txt`.
    #[must_use]
    pub fn items(&self) -> &Items {
        &self.items
    }

    /// `ItemStatCost.txt`.
    #[must_use]
    pub fn item_stats(&self) -> &item_stats::ItemStats {
        &self.item_stats
    }

    /// `ItemRatio.txt`.
    #[must_use]
    pub fn item_ratios(&self) -> &item_stats::ItemRatios {
        &self.item_ratios
    }

    /// The affix, property, unique and set tables.
    #[must_use]
    pub fn affixes(&self) -> &affixes::Affixes {
        &self.affixes
    }

    /// Replace the affix tables — for tests.
    pub fn set_affixes(&mut self, affixes: affixes::Affixes) {
        self.affixes = affixes;
    }

    /// `Skills.txt`.
    #[must_use]
    pub fn skills(&self) -> &skills::Skills {
        &self.skills
    }

    /// `Npc.txt`: what vendors charge and pay.
    #[must_use]
    pub fn npc_trades(&self) -> &trade::NpcTrades {
        &self.npc_trades
    }

    /// `Books.txt`, by row.
    #[must_use]
    pub fn books(&self) -> &[trade::Book] {
        &self.books
    }

    /// Replace the vendor and book tables — for tests.
    pub fn set_trade_tables(&mut self, npc_trades: trade::NpcTrades, books: Vec<trade::Book>) {
        self.npc_trades = npc_trades;
        self.books = books;
    }

    /// What a new character of `class` starts with.
    #[must_use]
    pub fn start_items(&self, class: u8) -> &[StartItem] {
        self.start_items.get(usize::from(class)).map_or(&[], Vec::as_slice)
    }

    /// The skill a new character's first item carries a point of (`StartSkill`), by id.
    #[must_use]
    pub fn start_skill(&self, class: u8) -> Option<i32> {
        let name = self.start_skills.get(usize::from(class))?.as_deref()?;
        (0..self.skills.len() as i32).find(|&id| self.skills.get(id).is_some_and(|s| s.name.eq_ignore_ascii_case(name)))
    }

    /// `Missiles.txt`.
    #[must_use]
    pub fn missiles(&self) -> &missiles::Missiles {
        &self.missiles
    }

    /// Replace the missile table — for tests.
    pub fn set_missiles(&mut self, missiles: missiles::Missiles) {
        self.missiles = missiles;
    }

    /// Replace the skills table — for tests.
    pub fn set_skills(&mut self, skills: skills::Skills) {
        self.skills = skills;
    }

    /// Replace the item stat and ratio tables — for tests.
    pub fn set_item_rules(&mut self, stats: item_stats::ItemStats, ratios: item_stats::ItemRatios) {
        self.item_stats = stats;
        self.item_ratios = ratios;
    }

    /// `ArmType.txt`'s tokens (`lit`, `med`, `hvy`), by body armour weight.
    #[must_use]
    pub fn armor_types(&self) -> &[Code] {
        &self.armor_types
    }

    /// Replace the item tables — for building rules from tables in tests.
    pub fn set_items(&mut self, items: Items, armor_types: Vec<Code>) {
        self.items = items;
        self.armor_types = armor_types;
    }

    /// `LvlPrest.txt`.
    #[must_use]
    pub fn lvl_prests(&self) -> &LvlPrests {
        &self.lvl_prests
    }

    /// `LvlSub.txt`.
    #[must_use]
    pub fn lvl_subs(&self) -> &LvlSubs {
        &self.lvl_subs
    }

    /// `LvlTypes.txt`.
    #[must_use]
    pub fn lvl_types(&self) -> &LvlTypes {
        &self.lvl_types
    }

    /// `LvlWarp.txt`.
    #[must_use]
    pub fn lvl_warps(&self) -> &LvlWarps {
        &self.lvl_warps
    }

    /// `LvlMaze.txt`.
    #[must_use]
    pub fn lvl_mazes(&self) -> &LvlMazes {
        &self.lvl_mazes
    }

    /// `MonPreset.txt`, resolved.
    #[must_use]
    pub fn mon_presets(&self) -> &MonPresets {
        &self.mon_presets
    }

    /// `objects.txt`.
    #[must_use]
    pub fn objects(&self) -> &Objects {
        &self.objects
    }

    /// `Shrines.txt`.
    #[must_use]
    pub fn shrines(&self) -> &Shrines {
        &self.shrines
    }

    /// `Inventory.txt`: the grid sizes of the stash, cube and backpack.
    #[must_use]
    pub fn panels(&self) -> &panels::Panels {
        &self.panels
    }

    /// Replace the shrine table — for building rules from tables in tests.
    pub fn set_shrines(&mut self, shrines: Shrines) {
        self.shrines = shrines;
    }

    /// `MonStats.txt` joined to `MonStats2.txt`.
    #[must_use]
    pub fn monsters(&self) -> &Monsters {
        &self.monsters
    }

    /// `MonLvl.txt`.
    #[must_use]
    pub fn monster_levels(&self) -> &MonLvls {
        &self.monster_levels
    }

    /// `TreasureClassEx.txt`.
    #[must_use]
    pub fn treasure(&self) -> &treasure::TreasureClasses {
        &self.treasure
    }

    /// `animdata.d2`: animation lengths and hit frames.
    #[must_use]
    pub fn anim_data(&self) -> &AnimData {
        &self.anim_data
    }

    /// Replace the monster level and animation tables — for building rules in tests.
    pub fn set_combat_tables(&mut self, monster_levels: MonLvls, anim_data: AnimData) {
        self.monster_levels = monster_levels;
        self.anim_data = anim_data;
    }

    /// Replace the treasure classes — for building rules from tables in tests.
    pub fn set_treasure(&mut self, treasure: treasure::TreasureClasses) {
        self.treasure = treasure;
    }

    /// Replace the map tables — for building rules from tables in tests.
    pub fn set_map_tables(&mut self, mon_presets: MonPresets, monsters: Monsters, objects: Objects) {
        self.mon_presets = mon_presets;
        self.monsters = monsters;
        self.objects = objects;
    }

    /// Build from already-parsed tables.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if a class row or column is missing or out of range.
    pub fn from_tables(charstats: &Table, experience: &Table) -> Result<Self, Error> {
        let bad = |table, problem: String| Error::BadTable { table, problem };
        let mut start_items: [Vec<StartItem>; 7] = Default::default();
        let mut start_skills: [Option<String>; 7] = Default::default();
        let mut classes = [ClassStats {
            strength: 0,
            dexterity: 0,
            energy: 0,
            vitality: 0,
            stamina: 0,
            life_bonus: 0,
            to_hit_factor: 0,
            run_drain: 0,
            per_level: (0, 0, 0),
            stat_per_level: 0,
            per_point: (0, 0, 0),
            velocity: (0, 0),
        }; 7];
        for (id, name) in CLASSES.iter().enumerate() {
            let row = charstats
                .rows()
                .find(|r| r.get("class").is_some_and(|c| c.eq_ignore_ascii_case(name)))
                .ok_or_else(|| bad("charstats.txt", format!("no {name} row")))?;
            let byte = |column: &str| -> Result<u8, Error> {
                row.int(column)
                    .and_then(|v| u8::try_from(v).ok())
                    .ok_or_else(|| bad("charstats.txt", format!("{name}.{column} missing or not a byte")))
            };
            classes[id] = ClassStats {
                strength: byte("str")?,
                dexterity: byte("dex")?,
                energy: byte("int")?,
                vitality: byte("vit")?,
                stamina: byte("stamina")?,
                life_bonus: byte("hpadd")?,
                to_hit_factor: row.int("ToHitFactor").unwrap_or(0) as i32,
                run_drain: row.int("RunDrain").unwrap_or(0) as i32,
                per_level: (
                    row.int("LifePerLevel").unwrap_or(0) as i32,
                    row.int("StaminaPerLevel").unwrap_or(0) as i32,
                    row.int("ManaPerLevel").unwrap_or(0) as i32,
                ),
                stat_per_level: row.int("StatPerLevel").unwrap_or(0) as i32,
                per_point: (
                    row.int("LifePerVitality").unwrap_or(0) as i32,
                    row.int("StaminaPerVitality").unwrap_or(0) as i32,
                    row.int("ManaPerMagic").unwrap_or(0) as i32,
                ),
                velocity: (row.int("WalkVelocity").unwrap_or(0) as i32, row.int("RunVelocity").unwrap_or(0) as i32),
            };
            start_items[id] = (1..=10)
                .filter_map(|n| {
                    let code = row.get(&format!("item{n}")).filter(|c| !c.is_empty() && *c != "0")?;
                    let count = u8::try_from(row.int(&format!("item{n}count")).unwrap_or(0)).ok().filter(|&c| c > 0)?;
                    let location = row.get(&format!("item{n}loc")).filter(|l| !l.is_empty()).map(str::to_string);
                    Some(StartItem { code: items::code(code), location, count })
                })
                .collect();
            start_skills[id] = row.get("StartSkill").filter(|s| !s.is_empty()).map(str::to_string);
        }

        let mut levels = Vec::new();
        for row in experience.rows() {
            let Some(level) = row.get("Level").and_then(|l| l.parse::<usize>().ok()) else {
                continue; // the MaxLvl row
            };
            if level != levels.len() {
                return Err(bad("experience.txt", format!("row {level} out of order")));
            }
            let mut per_class = [0u32; 7];
            for (id, name) in CLASSES.iter().enumerate() {
                per_class[id] = row
                    .int(name)
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| bad("experience.txt", format!("level {level} {name} missing")))?;
            }
            levels.push(per_class);
        }
        if levels.len() < 2 {
            return Err(bad("experience.txt", "fewer than two levels".into()));
        }
        Ok(Self {
            classes,
            start_items,
            start_skills,
            experience: levels,
            levels: Levels::default(),
            lvl_prests: LvlPrests::default(),
            lvl_subs: LvlSubs::default(),
            lvl_types: LvlTypes::default(),
            lvl_warps: LvlWarps::default(),
            lvl_mazes: LvlMazes::default(),
            mon_presets: MonPresets::default(),
            monsters: Monsters::default(),
            monster_levels: MonLvls::default(),
            anim_data: AnimData::default(),
            objects: Objects::default(),
            shrines: Shrines::default(),
            panels: panels::Panels::default(),
            items: Items::default(),
            item_stats: item_stats::ItemStats::default(),
            item_ratios: item_stats::ItemRatios::default(),
            affixes: affixes::Affixes::default(),
            skills: skills::Skills::default(),
            missiles: missiles::Missiles::default(),
            treasure: treasure::TreasureClasses::default(),
            npc_trades: trade::NpcTrades::default(),
            books: Vec::new(),
            armor_types: Vec::new(),
            archives: None,
        })
    }

    /// `Levels.txt` (empty when built with [`GameData::from_tables`]).
    #[must_use]
    pub fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Replace the level table — for building rules from tables in tests.
    pub fn set_levels(&mut self, levels: Levels) {
        self.levels = levels;
    }

    /// A class's starting attributes; `None` for a class id past the seven.
    #[must_use]
    pub fn class(&self, class: u8) -> Option<&ClassStats> {
        self.classes.get(usize::from(class))
    }

    /// Experience needed to leave `level`: `experience.txt`'s row for that level, as the
    /// engine's lookup `0x00611800` indexes it. `None` past the table.
    #[must_use]
    pub fn next_level_experience(&self, class: u8, level: usize) -> Option<u32> {
        let class = usize::from(class).min(6);
        self.experience.get(level).map(|row| row[class])
    }

    /// The stats a player joins with from its saved ones (`.d2s` ids 0–15, life, mana and
    /// stamina in 256ths), as `(stat, value)` in ascending stat order: those it has, with the
    /// experience bounds of its level and the rates a new character gets.
    #[must_use]
    pub fn saved_character_stats(&self, class: u8, saved: &[(u16, u32)]) -> Vec<(u8, u32)> {
        let get = |id: u8| saved.iter().find(|&&(s, _)| s == u16::from(id)).map_or(0, |&(_, v)| v);
        let level = get(stat::LEVEL).max(1);
        let mut stats: Vec<(u8, u32)> = (0..=stat::GOLD).filter(|&id| get(id) != 0 || id == stat::LEVEL).map(|id| (id, if id == stat::LEVEL { level } else { get(id) })).collect();
        let last = if level > 1 { self.next_level_experience(class, level as usize - 1).unwrap_or(0) } else { 0 };
        if last != 0 {
            stats.push((stat::LASTEXP, last));
        }
        stats.push((stat::NEXTEXP, self.next_level_experience(class, level as usize).unwrap_or(0)));
        stats.extend([(stat::VELOCITY_PERCENT, 100), (stat::ATTACK_RATE, 100), (stat::OTHER_ANIM_RATE, 100)]);
        stats
    }

    /// The stats a new character starts with, as `(stat, value)` in ascending stat order —
    /// what `0x005706D0` sets when it creates one. Life, mana and stamina are 1/256
    /// fixed-point, as the engine keeps them.
    #[must_use]
    pub fn new_character_stats(&self, class: u8) -> Option<Vec<(u8, u32)>> {
        let c = self.class(class)?;
        let life = (u32::from(c.vitality) + u32::from(c.life_bonus)) << 8;
        let mana = u32::from(c.energy) << 8;
        let stamina = u32::from(c.stamina) << 8;
        Some(vec![
            (stat::STRENGTH, u32::from(c.strength)),
            (stat::ENERGY, u32::from(c.energy)),
            (stat::DEXTERITY, u32::from(c.dexterity)),
            (stat::VITALITY, u32::from(c.vitality)),
            (stat::HITPOINTS, life),
            (stat::MAXHP, life),
            (stat::MANA, mana),
            (stat::MAXMANA, mana),
            (stat::STAMINA, stamina),
            (stat::MAXSTAMINA, stamina),
            (stat::LEVEL, 1),
            (stat::NEXTEXP, self.next_level_experience(class, 1).unwrap_or(0)),
            (stat::VELOCITY_PERCENT, 100),
            (stat::ATTACK_RATE, 100),
            (stat::OTHER_ANIM_RATE, 100),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up numbers in the tables' real shape — no Blizzard data in tests.
    fn tables() -> (Table, Table) {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\r\n");
        for (i, name) in CLASSES.iter().enumerate() {
            if i == 5 {
                cs.push_str("Expansion\r\n");
            }
            cs.push_str(&format!("{name}\t{}\t{}\t{}\t{}\t0\t{}\t{}\r\n", 10 + i, 20 + i, 30 + i, 40 + i, 50 + i, 7));
        }
        let mut exp = String::from("Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\tExpRatio\r\n");
        exp.push_str("MaxLvl\t99\t99\t99\t99\t99\t99\t99\t10\r\n");
        for level in 0..3u32 {
            let v = level * 1000;
            exp.push_str(&format!("{level}\t{v}\t{v}\t{v}\t{v}\t{v}\t{v}\t{v}\t1024\r\n"));
        }
        (Table::parse(cs.as_bytes()), Table::parse(exp.as_bytes()))
    }

    #[test]
    fn classes_are_in_engine_order_past_the_expansion_marker() {
        let (cs, exp) = tables();
        let data = GameData::from_tables(&cs, &exp).unwrap();
        assert_eq!(data.class(5).unwrap().strength, 15, "Druid is class 5");
        assert_eq!(data.next_level_experience(3, 1), Some(1000), "row \"1\", not the MaxLvl row");
    }

    #[test]
    fn a_new_character_gets_what_the_engine_sets() {
        let (cs, exp) = tables();
        let data = GameData::from_tables(&cs, &exp).unwrap();
        let stats = data.new_character_stats(3).unwrap();
        let get = |s: u8| stats.iter().find(|&&(id, _)| id == s).unwrap().1;
        assert_eq!(get(stat::HITPOINTS), (43 + 7) << 8, "(vit + hpadd) << 8");
        assert_eq!(get(stat::MAXMANA), 33 << 8, "energy << 8");
        assert_eq!(get(stat::MAXSTAMINA), 53 << 8);
        assert_eq!(get(stat::NEXTEXP), 1000);
        assert!(stats.windows(2).all(|w| w[0].0 < w[1].0), "ascending stat order");
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`), the tables load for all seven classes.
    #[test]
    fn with_a_real_install_the_rules_load() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let data = GameData::load(dir).expect("load");
        for class in 0..7 {
            let c = data.class(class).unwrap();
            assert!(c.vitality > 0 && c.stamina > 0, "{}: {c:?}", CLASSES[usize::from(class)]);
        }
        assert!(data.next_level_experience(0, 1).unwrap() > 0);
        let town = data.levels().get(1).expect("Rogue Encampment");
        assert_eq!((town.act, town.drlg_type, town.waypoint), (0, levels::DrlgType::Preset, Some(0)));
        assert_eq!(data.levels().get(2).unwrap().waypoint, None, "Blood Moor has none");
        assert_eq!(data.lvl_prests().for_level(1).unwrap().files.len(), 4, "TownN1/E1/S1/W1");
        assert!(matches!(data.mon_presets().get(0, 2), Some(presets::PresetMonster::Class { name, .. }) if name == "akara"));
        let chicken = data.monsters().get(149).expect("chicken");
        assert_eq!((chicken.id.as_str(), chicken.critter), ("chicken", true));
        let rogue = data.monsters().get(152).expect("rogue1");
        assert_eq!((rogue.critter, rogue.components[6]), (false, 2), "a town rogue carries one of two bows");
        let torch = data.objects().get(37).expect("objects.txt row 37");
        assert_eq!(torch.init_fn, 8);
        assert_eq!(data.objects().get(267).unwrap().operate_fn, 32, "the stash");
        assert!(data.monsters().get(148).unwrap().interact, "Akara talks");
        assert!(!data.monsters().get(152).unwrap().interact, "town rogues do not");
        assert!(data.read_file("data\\global\\tiles\\Act1\\Town\\TownN1.ds1").unwrap().is_some());
        let moor = data.levels().get(2).unwrap();
        assert_eq!((moor.vis[3], moor.warp[3], moor.warp[0]), (8, 0, -1), "the Den of Evil through warp 0; no open edges listed");
        let border = data.lvl_prests().by_def(4).expect("Wild Border 1");
        assert_eq!((border.size, border.file_count), ((8, 8), 3));
        let cliffs = data.lvl_subs().group(0);
        assert_eq!((cliffs.len(), cliffs[0].bord_type, cliffs[0].grid_size), (1, 1, 1));
        assert!(data.lvl_subs().group(4).len() == 2, "two waypoint pieces");
        assert_eq!(data.objects().get(2).unwrap().parm0, 3, "a shrine: boost or magic");
        assert_eq!(data.shrines().get(1).map(|s| s.effect_class), Some(4), "Refill is a boost");
        assert_eq!(data.shrines().of_class(1), (16..=22).collect::<Vec<_>>(), "the magic shrines");
        let moor = &data.levels().get(2).unwrap().monsters;
        assert_eq!((moor.types, moor.density[0], moor.normal.as_slice()), (3, 520, &["zombie1".to_string(), "fallen1".into(), "quillrat1".into()][..]));
        let shaman = data.monsters().get(data.monsters().class_named("fallenshaman1").unwrap()).unwrap().spawn;
        assert_eq!((shaman.party, shaman.minions[0]), ((2, 6), 19), "a shaman brings two to six fallen");
    }
}
