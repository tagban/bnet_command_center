//! `Npc.txt` and `Books.txt`: what a vendor charges and pays, and what a tome's charges cost.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::monsters::Monsters;

/// One `Npc.txt` row: a vendor's price multipliers (record stride `0x4C`, looked up by monster
/// class at `0x00656900`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NpcTrade {
    /// `sell mult` (`+0x04`): what the vendor sells for, in 1024ths of the item's price.
    pub sell_mult: i32,
    /// `buy mult` (`+0x08`): what the vendor pays, in 1024ths.
    pub buy_mult: i32,
    /// `rep mult` (`+0x0C`): what a repair costs, in 1024ths.
    pub repair_mult: i32,
    /// `max buy`, `max buy (N)`, `max buy (H)` (`+0x40`): the most the vendor pays for one item,
    /// by difficulty.
    pub max_buy: [i32; 3],
}

/// `Npc.txt`, by the monster class its `npc` column names.
#[derive(Debug, Clone, Default)]
pub struct NpcTrades {
    by_class: HashMap<i32, NpcTrade>,
}

impl NpcTrades {
    /// Parse the table, naming vendors by their `MonStats.txt` `Id`; rows naming no monster are
    /// left out.
    #[must_use]
    pub fn from_table(t: &Table, monsters: &Monsters) -> Self {
        let by_class = t
            .rows()
            .filter_map(|row| {
                let class = monsters.class_named(row.get("npc")?)?;
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let trade = NpcTrade {
                    sell_mult: int("sell mult"),
                    buy_mult: int("buy mult"),
                    repair_mult: int("rep mult"),
                    max_buy: [int("max buy"), int("max buy (N)"), int("max buy (H)")],
                };
                Some((class, trade))
            })
            .collect();
        Self { by_class }
    }

    /// A vendor's row by its monster class.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&NpcTrade> {
        self.by_class.get(&class)
    }
}

/// One `Books.txt` row: a tome and its scroll (`0x006374B0`, stride `0x20`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    /// `Name`.
    pub name: String,
    /// `CostPerCharge` (`+0x14`): what each scroll in the tome adds to its price.
    pub cost_per_charge: i32,
}

/// `Books.txt`, by row: Town Portal 0, Identify 1.
#[must_use]
pub fn books_from_table(t: &Table) -> Vec<Book> {
    t.rows()
        .map(|row| Book { name: row.get("Name").unwrap_or_default().to_string(), cost_per_charge: row.int("CostPerCharge").unwrap_or(0) as i32 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendors_are_found_by_their_monster_id() {
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\r\nakara\t148\takara\r\ncharsi\t154\tcharsi\r\n");
        let monstats2 = Table::parse(b"Id\r\nakara\r\ncharsi\r\n");
        let monsters = Monsters::from_tables(&monstats, &monstats2).unwrap();
        let npc = Table::parse(
            b"npc\tbuy mult\tsell mult\trep mult\tmax buy\tmax buy (N)\tmax buy (H)\r\n\
              akara\t512\t1024\t128\t5000\t30000\t35000\r\n\
              nobody\t512\t1024\t128\t1\t2\t3\r\n",
        );
        let trades = NpcTrades::from_table(&npc, &monsters);
        let akara = trades.get(148).unwrap();
        assert_eq!((akara.sell_mult, akara.buy_mult, akara.repair_mult, akara.max_buy), (1024, 512, 128, [5000, 30000, 35000]));
        assert!(trades.get(154).is_none(), "Charsi has no row here");
        let books = books_from_table(&Table::parse(b"Name\tCostPerCharge\r\nTome of Town Portal\t25\r\n"));
        assert_eq!(books[0].cost_per_charge, 25);
    }
}
