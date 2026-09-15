//! `Skills.txt`: the columns making an item reads — a skill's class, the weapon type it needs,
//! its required and highest levels. A skill's id is its row.

use d2_formats::excel::Table;

use crate::items::class_index;

/// One skill row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skill {
    /// `skill`.
    pub name: String,
    /// `charclass`, as a class index.
    pub class: Option<u8>,
    /// `itypea1`: the item type the skill needs (`+0x18`), when it needs one.
    pub item_type: Option<String>,
    /// `reqlevel`.
    pub req_level: i32,
    /// `maxlvl`.
    pub max_level: i32,
    /// `cost mult` (`+0x234`): what a point of the skill on an item adds to its price, in 1024ths.
    pub cost_mult: i32,
    /// `cost add` (`+0x238`): gold a point of it adds.
    pub cost_add: i32,
}

/// Every skill, by id.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    rows: Vec<Skill>,
}

impl Skills {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|row| Skill {
                name: row.get("skill").unwrap_or_default().to_string(),
                class: row.get("charclass").and_then(class_index),
                item_type: row.get("itypea1").filter(|s| !s.is_empty()).map(str::to_string),
                req_level: row.int("reqlevel").unwrap_or(0) as i32,
                max_level: row.int("maxlvl").unwrap_or(0) as i32,
                cost_mult: row.int("cost mult").unwrap_or(0) as i32,
                cost_add: row.int("cost add").unwrap_or(0) as i32,
            })
            .collect();
        Self { rows }
    }

    /// A skill by id.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&Skill> {
        usize::try_from(id).ok().and_then(|i| self.rows.get(i))
    }

    /// How many skills there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A class's first skill: the one its item skills count from (`0x006460F0` with index 0).
    #[must_use]
    pub fn first_of(&self, class: u8) -> Option<i32> {
        self.rows.iter().position(|s| s.class == Some(class)).map(|i| i as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_keep_row_ids_and_class_order() {
        let t = Table::parse(
            b"skill\tId\tcharclass\titypea1\treqlevel\tmaxlvl\r\nAttack\t0\t\t\t\t\r\nMagic Arrow\t1\tama\tbow\t1\t20\r\nJab\t2\tama\tspea\t1\t20\r\nFire Bolt\t3\tsor\t\t1\t20\r\n",
        );
        let skills = Skills::from_table(&t);
        assert_eq!((skills.first_of(0), skills.first_of(1), skills.first_of(2)), (Some(1), Some(3), None));
        assert_eq!(skills.get(2).and_then(|s| s.item_type.as_deref()), Some("spea"));
        assert_eq!(skills.get(0).map(|s| s.class), Some(None));
    }
}
