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
}

/// Monster classes by id (`hcIdx`).
#[derive(Debug, Clone, Default)]
pub struct Monsters {
    by_class: HashMap<i32, MonsterClass>,
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
        let display: HashMap<String, (bool, [u8; 16])> = monstats2
            .rows()
            .filter_map(|row| {
                let mut components = [0u8; 16];
                for (count, column) in components.iter_mut().zip(COMPONENT_COLUMNS) {
                    let variants = row.get(column).map_or(0, |v| v.split(',').filter(|s| !s.trim().is_empty()).count());
                    *count = u8::try_from(variants).unwrap_or(u8::MAX);
                }
                Some((row.get("Id")?.to_ascii_lowercase(), (row.int("critter").unwrap_or(0) != 0, components)))
            })
            .collect();
        let by_class = monstats
            .rows()
            .filter_map(|row| {
                let class = i32::try_from(row.int("hcIdx")?).ok()?;
                let id = row.get("Id")?.to_string();
                let &(critter, components) = display.get(&row.get("MonStatsEx")?.to_ascii_lowercase())?;
                let flag = |c: &str| row.int(c).unwrap_or(0) != 0;
                Some((class, MonsterClass { id, critter, components, interact: flag("interact"), npc: flag("npc") }))
            })
            .collect();
        Ok(Self { by_class })
    }

    /// A class by id.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&MonsterClass> {
        self.by_class.get(&class)
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
    }
}
