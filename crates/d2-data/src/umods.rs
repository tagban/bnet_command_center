//! `MonUMod.txt` — the modifiers a unique, champion or superunique monster carries (row = the
//! modifier's id) — and `MonType.txt`, the monster types they exclude.
//!
//! The `constants` column is not per modifier: the engine reads it as one global array of numbers
//! (`C[0]` the champion chance, `C[1..3]` a minion's extra life by difficulty, …), whatever row
//! each sits on.

use std::collections::HashMap;

use d2_formats::excel::Table;

/// One `MonUMod.txt` row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UniqueMod {
    /// `uniquemod`: its name.
    pub name: String,
    /// `enabled`.
    pub enabled: bool,
    /// `version`: 100 and up only in an expansion game.
    pub version: i32,
    /// `xfer`: minions get it too.
    pub xfer: bool,
    /// `champion`: a champion's kind rather than a unique's modifier.
    pub champion: bool,
    /// `fPick`: 1 wants an attack mode, 2 not melee and not `noMultiShot`, 3 a walk mode.
    pub fpick: i32,
    /// `exclude1`, `exclude2`: `MonType` codes it may not go on.
    pub exclude: [String; 2],
    /// `cpick` by difficulty: its weight among a champion's kinds.
    pub cpick: [i32; 3],
    /// `upick` by difficulty: its weight among a unique's modifiers.
    pub upick: [i32; 3],
}

/// `MonUMod.txt`, with its global `constants`.
#[derive(Debug, Clone, Default)]
pub struct UniqueMods {
    rows: Vec<UniqueMod>,
    constants: Vec<i32>,
}

impl UniqueMods {
    /// Read `MonUMod.txt`.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let mut rows = Vec::new();
        let mut constants = Vec::new();
        for r in t.rows() {
            let int = |c: &str| r.int(c).unwrap_or(0) as i32;
            constants.push(int("constants"));
            rows.push(UniqueMod {
                name: r.get("uniquemod").unwrap_or_default().to_string(),
                enabled: int("enabled") != 0,
                version: int("version"),
                xfer: int("xfer") != 0,
                champion: int("champion") != 0,
                fpick: int("fPick"),
                exclude: ["exclude1", "exclude2"].map(|c| r.get(c).unwrap_or_default().to_string()),
                cpick: ["cpick", "cpick (N)", "cpick (H)"].map(int),
                upick: ["upick", "upick (N)", "upick (H)"].map(int),
            });
        }
        Self { rows, constants }
    }

    /// A modifier by id.
    #[must_use]
    pub fn get(&self, id: usize) -> Option<&UniqueMod> {
        self.rows.get(id)
    }

    /// Every modifier, by id.
    #[must_use]
    pub fn rows(&self) -> &[UniqueMod] {
        &self.rows
    }

    /// `C[i]`, the `constants` column read as one array; 0 past its end.
    #[must_use]
    pub fn constant(&self, i: usize) -> i32 {
        self.constants.get(i).copied().unwrap_or(0)
    }
}

/// `MonType.txt`: each type's `equiv1`–`equiv3`, the types it counts as.
#[derive(Debug, Clone, Default)]
pub struct MonTypes {
    equivs: HashMap<String, Vec<String>>,
}

impl MonTypes {
    /// Read `MonType.txt`.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let equivs = t
            .rows()
            .filter_map(|r| {
                let ty = r.get("type").filter(|s| !s.is_empty())?.to_ascii_lowercase();
                let up = ["equiv1", "equiv2", "equiv3"].iter().filter_map(|c| r.get(c).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase)).collect();
                Some((ty, up))
            })
            .collect();
        Self { equivs }
    }

    /// Whether type `ty` is `ancestor` or counts as it through its `equiv` columns.
    #[must_use]
    pub fn is(&self, ty: &str, ancestor: &str) -> bool {
        let (ty, ancestor) = (ty.to_ascii_lowercase(), ancestor.to_ascii_lowercase());
        let mut seen = Vec::new();
        let mut todo = vec![ty];
        while let Some(t) = todo.pop() {
            if t == ancestor {
                return true;
            }
            if seen.contains(&t) {
                continue;
            }
            todo.extend(self.equivs.get(&t).cloned().unwrap_or_default());
            seen.push(t);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_one_array_and_types_inherit() {
        let mods = UniqueMods::from_table(&Table::parse(
            b"uniquemod\tid\tenabled\tversion\txfer\tchampion\tfPick\texclude1\texclude2\tcpick\tcpick (N)\tcpick (H)\tupick\tupick (N)\tupick (H)\tfInit\tconstants\r\n\
              none\t0\t0\t0\t1\t\t\t\t\t\t\t\t\t\t\t\t20\r\nstrong\t1\t1\t0\t1\t\t\t\t\t\t\t\t6\t6\t6\t\t100\r\nchampion\t2\t1\t0\t1\t1\t\t\t\t1\t1\t1\t\t\t\t\t0\r\n",
        ));
        assert_eq!((mods.constant(0), mods.constant(1), mods.get(1).unwrap().upick, mods.get(2).unwrap().champion), (20, 100, [6, 6, 6], true));
        let types = MonTypes::from_table(&Table::parse(b"type\tequiv1\tequiv2\tequiv3\r\nundead\t\t\t\r\nskeleton\tundead\t\t\r\n"));
        assert!(types.is("skeleton", "undead") && types.is("Undead", "undead") && !types.is("undead", "skeleton"));
    }
}
