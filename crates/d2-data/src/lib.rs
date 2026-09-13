//! Diablo II game rules, read from the operator's install.
//!
//! Every number here comes from the excel tables in the operator's MPQs at run time
//! (`diablo2.data_dir`); this repository ships none of them. Functions that turn table rows into
//! game state reproduce a 1.14d engine routine and cite its address.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;
use std::path::Path;

use d2_formats::excel::Table;
use d2_formats::mpq::{self, ArchiveSet, DATA_ARCHIVES};

pub mod stat;

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
        Self::from_tables(&read("charstats.txt")?, &read("experience.txt")?)
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
        Ok(Self { classes, experience: levels })
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
    }
}
