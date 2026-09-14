//! What a server needs to know about a monster class before it simulates one: whether the
//! server spawns it at all, and how many variants each of its graphics components has.
//!
//! `MonStats.txt` gives the class (`hcIdx`) and its `MonStatsEx`, which names the
//! `MonStats2.txt` row holding the display side.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::Error;

/// Graphics components in engine order: the `MonStats2.txt` variant columns behind the 16
/// counts at `+0x15` of the compiled record.
pub const COMPONENT_COLUMNS: [&str; 16] =
    ["HDv", "TRv", "LGv", "Rav", "Lav", "RHv", "LHv", "SHv", "S1v", "S2v", "S3v", "S4v", "S5v", "S6v", "S7v", "S8v"];

/// One monster class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonsterClass {
    /// `MonStats.txt` `Id`.
    pub id: String,
    /// `MonStats2.txt` `critter`: the client spawns these itself (from `Levels.txt` `cmon*`), so
    /// the server skips them when it places a map's preset monsters (flag 13, tested by
    /// `0x0054E490`).
    pub critter: bool,
    /// Variants per component, as the engine counts them: the entries in each variant column.
    pub components: [u8; 16],
    /// `MonStats.txt` `interact` (flag bit 9, record `+0xD` bit 1): a player can talk to it
    /// (`0x00572C10` refuses otherwise).
    pub interact: bool,
    /// `MonStats.txt` `npc` (flag bit 8).
    pub npc: bool,
    /// `MonStats.txt` `Align` (record `+0x4C`): 1 for the player's side, 2 neutral, else hostile.
    pub align: u8,
    /// `MonStats2.txt` `SizeX` (record `+8`): the shape a spot is tested with — 1 one subtile, 2
    /// a cross, 3 a 3×3 square (`0x0064D9B0`).
    pub size: u8,
    /// `MonStats2.txt` `spawnCol` (record `+0xA`): which collision bits keep it from standing
    /// somewhere (`0x005B2A00`).
    pub spawn_collision: u8,
    /// How the class spawns in a level's rooms.
    pub spawn: SpawnRules,
}

/// The `MonStats.txt` columns room population reads, class names resolved to class ids (-1 for
/// none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpawnRules {
    /// `isSpawn`: may be picked for a level's roster.
    pub is_spawn: bool,
    /// `Rarity`: its weight in the roster pick.
    pub rarity: i32,
    /// `rangedtype`: counts as ranged for a `rangedspawn` level's first pick.
    pub ranged: bool,
    /// `MinGrp`/`MaxGrp`: how many of it a spawn places.
    pub group: (i32, i32),
    /// `PartyMin`/`PartyMax`: how many minions come with each.
    pub party: (i32, i32),
    /// `minion1`/`minion2`.
    pub minions: [i32; 2],
    /// `spawn`: the class it can be replaced by when placed, with `placespawn`.
    pub spawn: i32,
    /// `placespawn`.
    pub place_spawn: bool,
    /// `sparsePopulate`: percent chance a placement goes ahead.
    pub sparse: i32,
    /// `BaseId`.
    pub base: i32,
}

/// Monster classes by id (`hcIdx`).
#[derive(Debug, Clone, Default)]
pub struct Monsters {
    by_class: HashMap<i32, MonsterClass>,
    by_name: HashMap<String, i32>,
}

impl Monsters {
    /// Join `MonStats.txt` to `MonStats2.txt`.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if a needed column is missing.
    pub fn from_tables(monstats: &Table, monstats2: &Table) -> Result<Self, Error> {
        for (t, table, column) in [
            (monstats, "monstats.txt", "Id"),
            (monstats, "monstats.txt", "hcIdx"),
            (monstats, "monstats.txt", "MonStatsEx"),
            (monstats2, "monstats2.txt", "Id"),
        ] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table, problem: format!("no {column} column") });
            }
        }
        let display: HashMap<String, (bool, [u8; 16], u8, u8)> = monstats2
            .rows()
            .filter_map(|row| {
                let mut components = [0u8; 16];
                for (count, column) in components.iter_mut().zip(COMPONENT_COLUMNS) {
                    let variants = row.get(column).map_or(0, |v| v.split(',').filter(|s| !s.trim().is_empty()).count());
                    *count = u8::try_from(variants).unwrap_or(u8::MAX);
                }
                let byte = |c: &str| u8::try_from(row.int(c).unwrap_or(0)).unwrap_or(0);
                Some((row.get("Id")?.to_ascii_lowercase(), (row.int("critter").unwrap_or(0) != 0, components, byte("SizeX"), byte("spawnCol"))))
            })
            .collect();
        let by_name: HashMap<String, i32> = monstats
            .rows()
            .filter_map(|row| Some((row.get("Id")?.to_ascii_lowercase(), i32::try_from(row.int("hcIdx")?).ok()?)))
            .collect();
        let class_of = |name: Option<&str>| name.and_then(|n| by_name.get(&n.to_ascii_lowercase()).copied()).unwrap_or(-1);
        let by_class = monstats
            .rows()
            .filter_map(|row| {
                let class = i32::try_from(row.int("hcIdx")?).ok()?;
                let id = row.get("Id")?.to_string();
                let &(critter, components, size, spawn_collision) = display.get(&row.get("MonStatsEx")?.to_ascii_lowercase())?;
                let flag = |c: &str| row.int(c).unwrap_or(0) != 0;
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let spawn = SpawnRules {
                    is_spawn: flag("isSpawn"),
                    rarity: int("Rarity"),
                    ranged: flag("rangedtype"),
                    group: (int("MinGrp"), int("MaxGrp")),
                    party: (int("PartyMin"), int("PartyMax")),
                    minions: [class_of(row.get("minion1")), class_of(row.get("minion2"))],
                    spawn: class_of(row.get("spawn")),
                    place_spawn: flag("placespawn"),
                    sparse: int("sparsePopulate"),
                    base: class_of(row.get("BaseId")),
                };
                Some((class, MonsterClass { id, critter, components, interact: flag("interact"), npc: flag("npc"), align: int("Align") as u8, size, spawn_collision, spawn }))
            })
            .collect();
        Ok(Self { by_class, by_name })
    }

    /// A class id by `MonStats.txt` `Id`, any case.
    #[must_use]
    pub fn class_named(&self, name: &str) -> Option<i32> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// A class by id.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&MonsterClass> {
        self.by_class.get(&class)
    }
}

impl MonsterClass {
    /// The alignment the engine gives a monster of this class (`0x005B2A00`): `Align` 1 is good
    /// (2), 2 neutral (1), anything else evil (0).
    #[must_use]
    pub fn alignment(&self) -> u8 {
        match self.align {
            1 => 2,
            2 => 1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_join_their_display_row_and_count_variants() {
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\tnpc\tinteract\r\nguard\t7\tguardex\t1\t1\r\nhen\t8\thenex\t\t\r\nlost\t9\tnowhere\t\t\r\n");
        let mut ms2 = String::from("Id\tcritter");
        for c in COMPONENT_COLUMNS {
            ms2.push('\t');
            ms2.push_str(c);
        }
        ms2.push_str("\r\nGUARDEX\t\t\tlit\t\t\t\t\tsbw,lbw\r\nhenex\t1\t\tlit\r\n");
        let m = Monsters::from_tables(&monstats, &Table::parse(ms2.as_bytes())).unwrap();
        let guard = m.get(7).unwrap();
        assert_eq!((guard.id.as_str(), guard.critter, guard.interact, guard.npc), ("guard", false, true, true));
        assert!(!m.get(8).unwrap().interact);
        assert_eq!(guard.components[..8], [0, 1, 0, 0, 0, 0, 2, 0], "TR one variant, LH two");
        assert!(m.get(8).unwrap().critter);
        assert!(m.get(9).is_none(), "no display row, no class");
        assert_eq!(m.class_named("HEN"), Some(8));
    }
}
