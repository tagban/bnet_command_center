//! `ItemStatCost.txt`: how each stat is written into an item's bits, and `ItemRatio.txt`: the
//! odds an item comes out unique, set, rare, magic, superior, normal or low quality.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::Error;

/// How one stat is saved and sent inside an item (the columns `0x0062FFF0` and `0x0062CBE0`
/// read from the row, stride `0x144`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatCost {
    /// `Stat`.
    pub name: String,
    /// `Save Bits` (`+0x19`): 0 for a stat never written into an item.
    pub save_bits: u8,
    /// `Save Add` (`+0x1C`): added before writing, so negative values fit.
    pub save_add: i32,
    /// `Save Param Bits` (`+0x24`): a parameter written before the value (a skill, a class).
    pub param_bits: u8,
    /// `ValShift` (`+0x18`): the stat is kept shifted left this far.
    pub val_shift: u8,
    /// `Multiply` (`+0x10`): what each point adds to an item's price, in 1024ths of it.
    pub cost_multiply: i32,
    /// `Add` (`+0x14`): gold an item with the stat costs more.
    pub cost_add: i32,
    /// `Encode` (`+0x30`): how the parameter is packed — 1 a skill, 2 a skill cast on an event
    /// with its level, 3 a charged skill, 4 a value by time of day.
    pub encode: u8,
}

/// Every stat, by id.
#[derive(Debug, Clone, Default)]
pub struct ItemStats {
    by_id: Vec<Option<StatCost>>,
    by_name: HashMap<String, u16>,
}

impl ItemStats {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if it has no `ID` column.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        if t.column("ID").is_none() {
            return Err(Error::BadTable { table: "itemstatcost.txt", problem: "no ID column".into() });
        }
        let mut stats = Self::default();
        for row in t.rows() {
            let Some(id) = row.int("ID").and_then(|i| u16::try_from(i).ok()) else { continue };
            let int = |c: &str| row.int(c).unwrap_or(0);
            let cost = StatCost {
                name: row.get("Stat").unwrap_or_default().to_string(),
                save_bits: u8::try_from(int("Save Bits")).unwrap_or(0),
                save_add: int("Save Add") as i32,
                param_bits: u8::try_from(int("Save Param Bits")).unwrap_or(0),
                val_shift: u8::try_from(int("ValShift")).unwrap_or(0),
                cost_multiply: int("Multiply") as i32,
                cost_add: int("Add") as i32,
                encode: u8::try_from(int("Encode")).unwrap_or(0),
            };
            let at = usize::from(id);
            if stats.by_id.len() <= at {
                stats.by_id.resize(at + 1, None);
            }
            stats.by_name.insert(cost.name.to_ascii_lowercase(), id);
            stats.by_id[at] = Some(cost);
        }
        Ok(stats)
    }

    /// A stat by id.
    #[must_use]
    pub fn get(&self, id: u16) -> Option<&StatCost> {
        self.by_id.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// A stat's id by name, any case.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<u16> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }
}

/// One quality's odds: `(ratio − (item level − quality level) / divisor) × 128`, but no better
/// than `min` (`0x00558640`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Odds {
    /// The ratio.
    pub ratio: i32,
    /// The divisor (at least 1).
    pub divisor: i32,
    /// The floor of the chance number; 0 where the table has none.
    pub min: i32,
}

impl Odds {
    /// The chance number for an item `levels` above its quality level: an item of the quality
    /// comes when a roll below it falls under 128.
    #[must_use]
    pub fn chance(&self, levels: i32) -> i32 {
        (self.ratio - levels / self.divisor.max(1)) * 128
    }
}

/// One `ItemRatio.txt` row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ratio {
    /// `Version`: 0 classic, 1 Lord of Destruction.
    pub version: i32,
    /// `Uber`: for exceptional and elite items.
    pub uber: bool,
    /// `Class Specific`.
    pub class_specific: bool,
    /// `Unique`, `UniqueDivisor`, `UniqueMin`.
    pub unique: Odds,
    /// `Rare`…
    pub rare: Odds,
    /// `Set`…
    pub set: Odds,
    /// `Magic`…
    pub magic: Odds,
    /// `HiQuality`, `HiQualityDivisor`.
    pub superior: Odds,
    /// `Normal`, `NormalDivisor`: against low quality.
    pub normal: Odds,
}

/// `ItemRatio.txt`.
#[derive(Debug, Clone, Default)]
pub struct ItemRatios {
    rows: Vec<Ratio>,
}

impl ItemRatios {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let odds = |name: &str, min: bool| Odds { ratio: int(name), divisor: int(&format!("{name}Divisor")), min: if min { int(&format!("{name}Min")) } else { 0 } };
                Ratio {
                    version: int("Version"),
                    uber: int("Uber") != 0,
                    class_specific: int("Class Specific") != 0,
                    unique: odds("Unique", true),
                    rare: odds("Rare", true),
                    set: odds("Set", true),
                    magic: odds("Magic", true),
                    superior: odds("HiQuality", false),
                    normal: odds("Normal", false),
                }
            })
            .collect();
        Self { rows }
    }

    /// The row for an item (`0x00637910`, always asked for version 100): the matching uber and
    /// class-specific row with the highest version.
    #[must_use]
    pub fn for_item(&self, uber: bool, class_specific: bool) -> Option<&Ratio> {
        self.rows.iter().filter(|r| r.uber == uber && r.class_specific == class_specific).max_by_key(|r| r.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_are_found_by_id_and_name() {
        let t = Table::parse(b"Stat\tID\tSave Bits\tSave Add\tSave Param Bits\tValShift\r\nstrength\t0\t8\t32\t\t\r\nmaxhp\t7\t9\t32\t\t8\r\narmorclass\t31\t11\t10\t\t\r\n");
        let stats = ItemStats::from_table(&t).unwrap();
        assert_eq!(stats.get(31).map(|s| (s.save_bits, s.save_add)), Some((11, 10)));
        assert_eq!(stats.get(7).map(|s| s.val_shift), Some(8));
        assert_eq!(stats.id("ArmorClass"), Some(31));
        assert!(stats.get(5).is_none());
    }

    #[test]
    fn ratios_pick_the_newest_matching_row() {
        let t = Table::parse(
            b"Function\tVersion\tUber\tClass Specific\tUnique\tUniqueDivisor\tUniqueMin\tRare\tRareDivisor\tRareMin\tSet\tSetDivisor\tSetMin\tMagic\tMagicDivisor\tMagicMin\tHiQuality\tHiQualityDivisor\tNormal\tNormalDivisor\r\n\
              r\t0\t0\t0\t400\t2\t6400\t160\t3\t3200\t125\t6\t5600\t30\t16\t192\t12\t16\t4\t8\r\n\
              r\t1\t0\t0\t400\t1\t6400\t100\t2\t3200\t160\t2\t5600\t34\t3\t192\t12\t8\t2\t2\r\n\
              u\t1\t1\t0\t400\t1\t6400\t100\t2\t3200\t160\t2\t5600\t34\t3\t192\t12\t8\t1\t1\r\n",
        );
        let ratios = ItemRatios::from_table(&t);
        let plain = ratios.for_item(false, false).unwrap();
        assert_eq!((plain.version, plain.magic, plain.normal), (1, Odds { ratio: 34, divisor: 3, min: 192 }, Odds { ratio: 2, divisor: 2, min: 0 }));
        assert_eq!(plain.unique.chance(4), (400 - 4) * 128);
        assert!(ratios.for_item(true, false).unwrap().uber);
        assert!(ratios.for_item(false, true).is_none());
    }
}
