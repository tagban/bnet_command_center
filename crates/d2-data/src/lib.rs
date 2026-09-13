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

use d2_formats::excel::Table;
use d2_formats::mpq::{self, ArchiveSet, DATA_ARCHIVES};

pub mod engine;
pub mod levels;
pub mod lvlsub;
pub mod monsters;
pub mod presets;
pub mod stat;

use levels::Levels;
use lvlsub::LvlSubs;
use monsters::Monsters;
use presets::{LvlPrests, MonPresets, Objects, Shrines};

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
}

/// The rules loaded so far.
#[derive(Debug, Clone)]
pub struct GameData {
    classes: [ClassStats; 7],
    /// `experience.txt` by level (row `"0"` first), one column per class.
    experience: Vec<[u32; 7]>,
    levels: Levels,
    lvl_prests: LvlPrests,
    lvl_subs: LvlSubs,
    mon_presets: MonPresets,
    monsters: Monsters,
    objects: Objects,
    shrines: Shrines,
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
        let monstats = read("monstats.txt")?;
        data.mon_presets =
            MonPresets::from_tables(&read("monpreset.txt")?, &monstats, &read("superuniques.txt")?, &read("monplace.txt")?)?;
        data.monsters = Monsters::from_tables(&monstats, &read("monstats2.txt")?)?;
        data.objects = Objects::from_table(&read("objects.txt")?);
        data.shrines = Shrines::from_table(&read("shrines.txt")?);
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

    /// Replace the shrine table — for building rules from tables in tests.
    pub fn set_shrines(&mut self, shrines: Shrines) {
        self.shrines = shrines;
    }

    /// `MonStats.txt` joined to `MonStats2.txt`.
    #[must_use]
    pub fn monsters(&self) -> &Monsters {
        &self.monsters
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
        let mut classes = [ClassStats { strength: 0, dexterity: 0, energy: 0, vitality: 0, stamina: 0, life_bonus: 0 }; 7];
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
            };
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
            experience: levels,
            levels: Levels::default(),
            lvl_prests: LvlPrests::default(),
            lvl_subs: LvlSubs::default(),
            mon_presets: MonPresets::default(),
            monsters: Monsters::default(),
            objects: Objects::default(),
            shrines: Shrines::default(),
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
    }
}
