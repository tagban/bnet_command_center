//! `Hireling.txt`: the mercenaries the act's hirers offer — each row one kind of hireling in one
//! game version, act and difficulty, for one band of levels, with its stats and how they grow.
//!
//! The engine keeps the rows sorted by `Id` then `Level`, and reads a hireling's stats from the row
//! of its `Id` with the highest `Level` no more than its own (`0x006562F0`).

use d2_formats::excel::Table;

use crate::strings::Strings;

/// One row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hireling {
    /// `Version`: 0 classic, 100 expansion.
    pub version: i32,
    /// `Id`: the kind of hireling.
    pub id: i32,
    /// `Class`: its `MonStats.txt` row.
    pub class: i32,
    /// `Act`, 1-based.
    pub act: i32,
    /// `Difficulty`, 1-based.
    pub difficulty: i32,
    /// `Level`: the level this row's band starts at.
    pub level: i32,
    /// `Seller`: the NPC class that hires it out.
    pub seller: i32,
    /// `NameFirst`…`NameLast` as string ids: the names its hirelings go by.
    pub names: (u16, u16),
    /// `Gold`: its price at the row's level.
    pub gold: i32,
    /// `Exp/Lvl`.
    pub exp_per_level: i32,
    /// `HP`, `HP/Lvl`.
    pub hp: (i32, i32),
    /// `Defense`, `Def/Lvl`.
    pub defense: (i32, i32),
    /// `Str`, `Str/Lvl` (the latter in eighths).
    pub strength: (i32, i32),
    /// `Dex`, `Dex/Lvl` (eighths).
    pub dexterity: (i32, i32),
    /// `AR`, `AR/Lvl`.
    pub attack_rating: (i32, i32),
    /// `Share`.
    pub share: i32,
    /// `Dmg-Min`, `Dmg-Max`, `Dmg/Lvl` (eighths).
    pub damage: (i32, i32, i32),
    /// `Resist`, `Resist/Lvl` (quarters).
    pub resist: (i32, i32),
    /// `Skill1`…`Skill6` with their `Mode`, `Chance`, `ChancePerLvl`, `Level`, `LvlPerLvl`.
    pub skills: Vec<HirelingSkill>,
}

/// One of a hireling row's skills.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HirelingSkill {
    /// `Skill`n: a `Skills.txt` row.
    pub skill: i32,
    /// `Mode`n.
    pub mode: i32,
    /// `Chance`n, `ChancePerLvl`n.
    pub chance: (i32, i32),
    /// `Level`n, `LvlPerLvl`n (thirty-seconds).
    pub level: (i32, i32),
}

/// The table.
#[derive(Debug, Clone, Default)]
pub struct Hirelings {
    rows: Vec<Hireling>,
}

impl Hirelings {
    /// Parse the table, its names resolved through `strings`.
    #[must_use]
    pub fn from_table(t: &Table, strings: &Strings) -> Self {
        let rows = t
            .rows()
            .filter(|r| r.int("Id").is_some())
            .map(|r| {
                let int = |c: &str| r.int(c).unwrap_or(0) as i32;
                let name = |c: &str| r.get(c).and_then(|k| strings.id(k)).unwrap_or(0);
                Hireling {
                    version: int("Version"),
                    id: int("Id"),
                    class: int("Class"),
                    act: int("Act"),
                    difficulty: int("Difficulty"),
                    level: int("Level"),
                    seller: int("Seller"),
                    names: (name("NameFirst"), name("NameLast")),
                    gold: int("Gold"),
                    exp_per_level: int("Exp/Lvl"),
                    hp: (int("HP"), int("HP/Lvl")),
                    defense: (int("Defense"), int("Def/Lvl")),
                    strength: (int("Str"), int("Str/Lvl")),
                    dexterity: (int("Dex"), int("Dex/Lvl")),
                    attack_rating: (int("AR"), int("AR/Lvl")),
                    share: int("Share"),
                    damage: (int("Dmg-Min"), int("Dmg-Max"), int("Dmg/Lvl")),
                    resist: (int("Resist"), int("Resist/Lvl")),
                    skills: (1..=6)
                        .map(|n| HirelingSkill {
                            skill: int(&format!("Skill{n}")),
                            mode: int(&format!("Mode{n}")),
                            chance: (int(&format!("Chance{n}")), int(&format!("ChancePerLvl{n}"))),
                            level: (int(&format!("Level{n}")), int(&format!("LvlPerLvl{n}"))),
                        })
                        .filter(|s| s.skill > 0)
                        .collect(),
                }
            })
            .collect();
        Self { rows }
    }

    /// Every row.
    #[must_use]
    pub fn rows(&self) -> &[Hireling] {
        &self.rows
    }

    /// The first row a hirer sells in a version and difficulty (`0x006564D0`), whose names are its
    /// offers'.
    #[must_use]
    pub fn for_seller(&self, expansion: bool, seller: i32, difficulty: u8) -> Option<&Hireling> {
        let version = if expansion { 100 } else { 0 };
        self.rows.iter().find(|r| r.version == version && r.seller == seller && r.difficulty == i32::from(difficulty) + 1)
    }

    /// The rows of an act and difficulty that share the first such row's level (`0x00656580`):
    /// the kinds an offer may be.
    #[must_use]
    pub fn kinds(&self, expansion: bool, act: i32, difficulty: u8) -> Vec<&Hireling> {
        let version = if expansion { 100 } else { 0 };
        let mut rows = self.rows.iter().filter(|r| r.version == version && r.act == act && r.difficulty == i32::from(difficulty) + 1);
        let Some(first) = rows.next() else { return Vec::new() };
        std::iter::once(first).chain(rows.filter(|r| r.level == first.level)).collect()
    }

    /// The row a hireling of kind `id` at `level` takes its stats from (`0x006562F0`): the one of
    /// its version, act and difficulty with the highest `Level` no more than `level`.
    #[must_use]
    pub fn band(&self, like: &Hireling, level: i32) -> Option<&Hireling> {
        self.rows
            .iter()
            .filter(|r| r.version == like.version && r.id == like.id && r.difficulty == like.difficulty && r.level <= level)
            .max_by_key(|r| r.level)
    }
}
