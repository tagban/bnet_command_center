//! `MonLvl.txt`: how a monster's `MonStats.txt` percentages scale with its level — defence,
//! attack rating, life, damage and experience, per difficulty.

use d2_formats::excel::Table;

use crate::Error;

/// A column group of `MonLvl.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// `AC`: defence.
    Defense,
    /// `TH`: attack rating.
    ToHit,
    /// `HP`: life.
    Life,
    /// `DM`: damage.
    Damage,
    /// `XP`: experience.
    Experience,
}

impl Scale {
    fn column(self) -> &'static str {
        match self {
            Self::Defense => "AC",
            Self::ToHit => "TH",
            Self::Life => "HP",
            Self::Damage => "DM",
            Self::Experience => "XP",
        }
    }
}

/// `MonLvl.txt` by level: for each [`Scale`], classic and `L-` (expansion) values per difficulty.
#[derive(Debug, Clone, Default)]
pub struct MonLvls {
    /// `rows[level][scale][expansion][difficulty]`.
    rows: Vec<[[[i32; 3]; 2]; 5]>,
}

impl MonLvls {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Level` or `HP` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Level", "HP"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "monlvl.txt", problem: format!("no {column} column") });
            }
        }
        let scales = [Scale::Defense, Scale::ToHit, Scale::Life, Scale::Damage, Scale::Experience];
        let mut rows = Vec::new();
        for row in t.rows() {
            let Some(level) = row.int("Level").and_then(|l| usize::try_from(l).ok()) else { continue };
            if rows.len() <= level {
                rows.resize(level + 1, [[[0; 3]; 2]; 5]);
            }
            for (s, scale) in scales.iter().enumerate() {
                for (e, prefix) in ["", "L-"].iter().enumerate() {
                    for (d, suffix) in ["", "(N)", "(H)"].iter().enumerate() {
                        rows[level][s][e][d] = row.int(&format!("{prefix}{}{suffix}", scale.column())).unwrap_or(0) as i32;
                    }
                }
            }
        }
        Ok(Self { rows })
    }

    /// The percentage `scale` gives a monster of `level` on `difficulty` (0..=2), from the
    /// `L-` columns in an expansion game.
    #[must_use]
    pub fn get(&self, level: i32, scale: Scale, difficulty: u8, expansion: bool) -> i32 {
        let s = [Scale::Defense, Scale::ToHit, Scale::Life, Scale::Damage, Scale::Experience].iter().position(|&x| x == scale).unwrap_or(0);
        usize::try_from(level).ok().and_then(|l| self.rows.get(l)).map_or(0, |r| r[s][usize::from(expansion)][usize::from(difficulty.min(2))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_row_scales_by_difficulty_and_game_type() {
        let t = Table::parse(b"Level\tAC\tAC(N)\tAC(H)\tL-AC\tL-AC(N)\tL-AC(H)\tHP\tHP(N)\tHP(H)\tL-HP\tL-HP(N)\tL-HP(H)\r\n0\t1\t1\t1\t1\t1\t1\t1\t1\t1\t1\t1\t1\r\n1\t6\t92\t147\t6\t108\t173\t7\t107\t830\t7\t107\t1107\r\n");
        let m = MonLvls::from_table(&t).unwrap();
        assert_eq!(m.get(1, Scale::Life, 2, false), 830);
        assert_eq!(m.get(1, Scale::Life, 2, true), 1107);
        assert_eq!(m.get(1, Scale::Defense, 1, true), 108);
        assert_eq!(m.get(1, Scale::Experience, 0, false), 0, "no XP column in this table");
        assert_eq!(m.get(5, Scale::Life, 0, false), 0, "past the table");
    }
}
