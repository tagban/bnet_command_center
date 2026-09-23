//! `Runes.txt`: runewords — the runes that, filling a plain item's sockets in order, make it one,
//! and the mods it then carries (`0x0062BED0` finds the row, `0x006600A0` rolls the mods).

use d2_formats::excel::Table;

use crate::affixes::{mods, Mod};
use crate::items::{code, Code, Items};
use crate::strings::Strings;

/// One runeword.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Runeword {
    /// `Name`, its string key (`Runeword1`…).
    pub key: String,
    /// `Rune Name`, for logs.
    pub name: String,
    /// `complete`: rows without it never match.
    pub complete: bool,
    /// `server`: made only in a ladder game.
    pub ladder_only: bool,
    /// `itype1`–`6`: the item must be one of these.
    pub types: Vec<String>,
    /// `etype1`–`3`: and none of these.
    pub excluded: Vec<String>,
    /// `Rune1`–`6`, in socket order.
    pub runes: Vec<Code>,
    /// `T1Code1`–`7`.
    pub mods: Vec<Mod>,
    /// Its name's string id, sent in the item's bits (0 until [`Runewords::name_ids`]).
    pub name_id: u16,
}

/// `Runes.txt`, in row order.
#[derive(Debug, Clone, Default)]
pub struct Runewords {
    rows: Vec<Runeword>,
}

impl Runewords {
    /// Read `Runes.txt`.
    #[must_use]
    pub fn from_table(table: &Table) -> Self {
        let mod_names: Vec<(String, String, String, String)> =
            (1..=7).map(|i| (format!("T1Code{i}"), format!("T1Param{i}"), format!("T1Min{i}"), format!("T1Max{i}"))).collect();
        let texts = |row: &d2_formats::excel::Row, prefix: &str, count: usize| -> Vec<String> {
            (1..=count).filter_map(|i| row.get(&format!("{prefix}{i}")).filter(|s| !s.is_empty()).map(str::to_string)).collect()
        };
        let rows = table
            .rows()
            .map(|row| Runeword {
                key: row.get("Name").unwrap_or_default().to_string(),
                name: row.get("Rune Name").unwrap_or_default().to_string(),
                complete: row.int("complete").unwrap_or(0) != 0,
                ladder_only: row.int("server").unwrap_or(0) != 0,
                types: texts(&row, "itype", 6),
                excluded: texts(&row, "etype", 3),
                runes: texts(&row, "Rune", 6).iter().map(|c| code(c)).collect(),
                mods: mods(&row, &mod_names),
                name_id: 0,
            })
            .collect();
        Self { rows }
    }

    /// Look each row's name up in the string tables, for its id.
    pub fn name_ids(&mut self, strings: &Strings) {
        for row in &mut self.rows {
            row.name_id = strings.id(&row.key).unwrap_or(0);
        }
    }

    /// The rows.
    #[must_use]
    pub fn rows(&self) -> &[Runeword] {
        &self.rows
    }

    /// The runeword an item of item class `class` becomes with `runes` filling all its sockets,
    /// in order (`0x0062BED0`): the first complete row with exactly those runes whose types take
    /// the item — a ladder-only one only in a ladder game. The caller checks the item may be one
    /// (not magic or better, not a quest item, every socket filled).
    #[must_use]
    pub fn matching(&self, items: &Items, class: i32, runes: &[Code], ladder: bool) -> Option<&Runeword> {
        self.rows.iter().find(|r| {
            r.complete
                && (ladder || !r.ladder_only)
                && r.runes == runes
                && !r.excluded.iter().any(|t| items.is(class, t))
                && r.types.iter().any(|t| items.is(class, t))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runeword_row_is_read_in_order() {
        let table = Table::parse(
            b"Name\tRune Name\tcomplete\tserver\titype1\titype2\tetype1\tRune1\tRune2\tRune3\tT1Code1\tT1Param1\tT1Min1\tT1Max1\tT1Code2\tT1Min2\tT1Max2\r\n\
              Runeword1\tSteel\t1\t\tswor\taxe\t\tr13\tr10\t\tswing2\t\t25\t25\tdmg%\t20\t20\r\n\
              Runeword2\tUnused\t0\t\tswor\t\t\tr01\t\t\t\t\t\t\t\t\t\r\n",
        );
        let words = Runewords::from_table(&table);
        let steel = &words.rows()[0];
        assert_eq!(steel.name, "Steel");
        assert!(steel.complete && !steel.ladder_only);
        assert_eq!(steel.types, ["swor", "axe"]);
        assert_eq!(steel.runes, [code("r13"), code("r10")]);
        assert_eq!(steel.mods.len(), 2);
        assert_eq!(steel.mods[1].code, "dmg%");
        assert!(!words.rows()[1].complete);
    }
}
