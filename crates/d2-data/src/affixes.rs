//! What magic, rare, set and unique items are made of: `MagicPrefix.txt`, `MagicSuffix.txt`,
//! `AutoMagic.txt`, `RarePrefix.txt`, `RareSuffix.txt`, `Properties.txt`, `QualityItems.txt`,
//! `LowQualityItems.txt`, `UniqueItems.txt`, `SetItems.txt` and `Sets.txt`.
//!
//! Ids on the wire count rows from 1 within each table (`Expansion` markers skipped), as
//! `0x0062FFF0` writes them: the engine keeps suffixes, prefixes and auto-affixes in one table and
//! subtracts each section's start. A rare's first name is a `RarePrefix.txt` row counted after all
//! of `RareSuffix.txt` (the engine loads suffixes first, `0x00633F40`); its second a suffix row.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::items::{code, Code};

/// A property with its parameter and range: an affix's `mod1code`…, a unique's `prop1`….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    /// `Properties.txt` code.
    pub code: String,
    /// Parameter.
    pub param: i32,
    /// Minimum.
    pub min: i32,
    /// Maximum.
    pub max: i32,
}

/// A magic prefix, suffix or auto-affix row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affix {
    /// `Name`.
    pub name: String,
    /// `version`: 100 for Lord of Destruction only.
    pub version: i32,
    /// `spawnable`.
    pub spawnable: bool,
    /// `rare`: allowed on rare items.
    pub rare: bool,
    /// `level`: the least affix level.
    pub level: i32,
    /// `maxlevel`: the most, 0 for none.
    pub max_level: i32,
    /// `classspecific`/`class`: the class the affix is for, when there is one.
    pub class: Option<String>,
    /// `frequency`: its weight.
    pub frequency: i32,
    /// `group`: at most one affix of a group per item.
    pub group: i32,
    /// `mod1`…`mod3`.
    pub mods: Vec<Mod>,
    /// `itype1`…`itype7`: item types it spawns on.
    pub itypes: Vec<String>,
    /// `etype1`…`etype5`: item types it never spawns on.
    pub etypes: Vec<String>,
    /// `multiply` (`+0x88`): what the affix adds to an item's price, in 1024ths of it.
    pub cost_multiply: i32,
    /// `add` (`+0x8C`): gold it adds.
    pub cost_add: i32,
}

/// A rare name part: its item types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RareName {
    /// `name`.
    pub name: String,
    /// `version`.
    pub version: i32,
    /// `itype1`…`itype7`.
    pub itypes: Vec<String>,
    /// `etype1`…`etype4`.
    pub etypes: Vec<String>,
}

/// One function of a property: `Properties.txt` `set`, `val`, `func` and `stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyFunc {
    /// `set`n.
    pub set: i32,
    /// `val`n.
    pub val: i32,
    /// `func`n (1–36).
    pub func: i32,
    /// `stat`n, when given.
    pub stat: Option<String>,
}

/// A superior item's `QualityItems.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superior {
    /// `mod1`, `mod2` (`nummods` of them).
    pub mods: Vec<Mod>,
    /// `armor`, `weapon`, `shield`, `thrown`, `scepter`, `wand`, `staff`, `bow`, `boots`, `gloves`,
    /// `belt`.
    pub applies: [bool; 11],
}

/// A `UniqueItems.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueItem {
    /// `index`.
    pub name: String,
    /// `version`.
    pub version: i32,
    /// `enabled`.
    pub enabled: bool,
    /// `ladder`: ladder games only.
    pub ladder: bool,
    /// `rarity`.
    pub rarity: i32,
    /// `nolimit`: may drop more than once a game.
    pub no_limit: bool,
    /// `lvl`: the least item level.
    pub level: i32,
    /// `lvl req`.
    pub level_req: i32,
    /// `code`.
    pub code: Code,
    /// `prop1`…`prop12`.
    pub props: Vec<Mod>,
    /// `cost mult` (`+0x7C`), `cost add` (`+0x80`): what being this unique adds to the price.
    pub cost: (i32, i32),
}

/// A `SetItems.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetItem {
    /// `index`.
    pub name: String,
    /// `set`: the `Sets.txt` index.
    pub set: String,
    /// The set's `Sets.txt` row (-1 when the set is not there).
    pub set_row: i32,
    /// The set's `version`: 100 for Lord of Destruction only.
    pub version: i32,
    /// `add func`: 0 when the bonuses are always on, else they count worn pieces.
    pub add_func: i32,
    /// `item`: the base item's code.
    pub code: Code,
    /// `rarity`.
    pub rarity: i32,
    /// `lvl`.
    pub level: i32,
    /// `lvl req`.
    pub level_req: i32,
    /// `prop1`…`prop9`.
    pub props: Vec<Mod>,
    /// `aprop1a`/`b` … `aprop5a`/`b`: the bonuses for wearing 2 … 6 pieces.
    pub bonuses: [Vec<Mod>; 5],
    /// `cost mult` (`+0x38`), `cost add` (`+0x3C`): what being this set item adds to the price.
    pub cost: (i32, i32),
}

/// The item-making tables.
#[derive(Debug, Clone, Default)]
pub struct Affixes {
    /// `MagicPrefix.txt`, by row.
    pub prefixes: Vec<Affix>,
    /// `MagicSuffix.txt`.
    pub suffixes: Vec<Affix>,
    /// `AutoMagic.txt`.
    pub auto: Vec<Affix>,
    /// `RarePrefix.txt`.
    pub rare_prefixes: Vec<RareName>,
    /// `RareSuffix.txt`.
    pub rare_suffixes: Vec<RareName>,
    /// `QualityItems.txt`.
    pub superior: Vec<Superior>,
    /// `LowQualityItems.txt` rows.
    pub low_quality: usize,
    /// `LowQualityItems.txt` `Name`s, by row: a vendor stocks no `Cracked` item and takes none
    /// back into its stock.
    pub low_quality_names: Vec<String>,
    /// `UniqueItems.txt`.
    pub uniques: Vec<UniqueItem>,
    /// `SetItems.txt`.
    pub set_items: Vec<SetItem>,
    properties: HashMap<String, Vec<PropertyFunc>>,
}

fn mods(row: &d2_formats::excel::Row, names: &[(String, String, String, String)]) -> Vec<Mod> {
    names
        .iter()
        .filter_map(|(c, p, lo, hi)| {
            let code = row.get(c).filter(|s| !s.is_empty())?.to_string();
            let int = |col: &str| row.get(col).and_then(|s| s.trim().parse::<i32>().ok()).unwrap_or(0);
            Some(Mod { code, param: int(p), min: int(lo), max: int(hi) })
        })
        .collect()
}

fn texts(row: &d2_formats::excel::Row, prefix: &str, count: usize) -> Vec<String> {
    (1..=count).filter_map(|i| row.get(&format!("{prefix}{i}")).map(str::to_string)).collect()
}

fn affixes(t: &Table) -> Vec<Affix> {
    let names: Vec<(String, String, String, String)> =
        (1..=3).map(|i| (format!("mod{i}code"), format!("mod{i}param"), format!("mod{i}min"), format!("mod{i}max"))).collect();
    t.rows()
        .map(|row| {
            let int = |c: &str| row.int(c).unwrap_or(0) as i32;
            Affix {
                name: row.get("Name").unwrap_or_default().to_string(),
                version: int("version"),
                spawnable: int("spawnable") != 0,
                rare: int("rare") != 0,
                level: int("level"),
                max_level: int("maxlevel"),
                class: row.get("classspecific").map(str::to_string),
                frequency: int("frequency"),
                group: int("group"),
                mods: mods(&row, &names),
                itypes: texts(&row, "itype", 7),
                etypes: texts(&row, "etype", 5),
                cost_multiply: int("multiply"),
                cost_add: int("add"),
            }
        })
        .collect()
}

fn rare_names(t: &Table) -> Vec<RareName> {
    t.rows()
        .map(|row| RareName {
            name: row.get("name").unwrap_or_default().to_string(),
            version: row.int("version").unwrap_or(0) as i32,
            itypes: texts(&row, "itype", 7),
            etypes: texts(&row, "etype", 4),
        })
        .collect()
}

impl Affixes {
    /// Parse the tables.
    #[allow(clippy::too_many_arguments)] // one table each
    #[must_use]
    pub fn from_tables(
        prefix: &Table,
        suffix: &Table,
        automagic: &Table,
        rare_prefix: &Table,
        rare_suffix: &Table,
        properties: &Table,
        quality: &Table,
        low_quality: &Table,
        uniques: &Table,
        set_items: &Table,
        sets: &Table,
    ) -> Self {
        let set_rows: Vec<(String, i32)> =
            sets.rows().map(|row| (row.get("index").unwrap_or_default().to_string(), row.int("version").unwrap_or(0) as i32)).collect();
        let properties = properties
            .rows()
            .filter_map(|row| {
                let code = row.get("code")?.to_string();
                let funcs = (1..=7)
                    .filter_map(|i| {
                        let func = row.int(&format!("func{i}")).unwrap_or(0) as i32;
                        (func != 0).then(|| PropertyFunc {
                            set: row.int(&format!("set{i}")).unwrap_or(0) as i32,
                            val: row.int(&format!("val{i}")).unwrap_or(0) as i32,
                            func,
                            stat: row.get(&format!("stat{i}")).map(str::to_string),
                        })
                    })
                    .collect();
                Some((code.to_ascii_lowercase(), funcs))
            })
            .collect();
        let quality_names: Vec<(String, String, String, String)> =
            (1..=2).map(|i| (format!("mod{i}code"), format!("mod{i}param"), format!("mod{i}min"), format!("mod{i}max"))).collect();
        let superior = quality
            .rows()
            .map(|row| {
                let columns = ["armor", "weapon", "shield", "thrown", "scepter", "wand", "staff", "bow", "boots", "gloves", "belt"];
                let mut applies = [false; 11];
                for (slot, c) in applies.iter_mut().zip(columns) {
                    *slot = row.int(c).unwrap_or(0) != 0;
                }
                let count = row.int("nummods").unwrap_or(0).clamp(0, 2) as usize;
                let mut mods = mods(&row, &quality_names);
                mods.truncate(count);
                Superior { mods, applies }
            })
            .collect();
        let prop_names = |n: usize| -> Vec<(String, String, String, String)> {
            (1..=n).map(|i| (format!("prop{i}"), format!("par{i}"), format!("min{i}"), format!("max{i}"))).collect()
        };
        let unique_props = prop_names(12);
        let uniques = uniques
            .rows()
            .map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                UniqueItem {
                    name: row.get("index").unwrap_or_default().to_string(),
                    version: int("version"),
                    enabled: int("enabled") != 0,
                    ladder: int("ladder") != 0,
                    rarity: int("rarity"),
                    no_limit: int("nolimit") != 0,
                    level: int("lvl"),
                    level_req: int("lvl req"),
                    code: code(row.get("code").unwrap_or_default()),
                    props: mods(&row, &unique_props),
                    cost: (int("cost mult"), int("cost add")),
                }
            })
            .collect();
        let set_props = prop_names(9);
        let set_items = set_items
            .rows()
            .map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let bonus = |n: usize| -> Vec<Mod> {
                    let names: Vec<(String, String, String, String)> = ["a", "b"]
                        .iter()
                        .map(|s| (format!("aprop{n}{s}"), format!("apar{n}{s}"), format!("amin{n}{s}"), format!("amax{n}{s}")))
                        .collect();
                    mods(&row, &names)
                };
                let set = row.get("set").unwrap_or_default().to_string();
                let set_row = set_rows.iter().position(|(name, _)| *name == set);
                SetItem {
                    name: row.get("index").unwrap_or_default().to_string(),
                    set_row: set_row.map_or(-1, |i| i as i32),
                    version: set_row.map_or(0, |i| set_rows[i].1),
                    add_func: int("add func"),
                    set,
                    code: code(row.get("item").unwrap_or_default()),
                    rarity: int("rarity"),
                    level: int("lvl"),
                    level_req: int("lvl req"),
                    props: mods(&row, &set_props),
                    bonuses: [bonus(1), bonus(2), bonus(3), bonus(4), bonus(5)],
                    cost: (int("cost mult"), int("cost add")),
                }
            })
            .collect();
        Self {
            prefixes: affixes(prefix),
            suffixes: affixes(suffix),
            auto: affixes(automagic),
            rare_prefixes: rare_names(rare_prefix),
            rare_suffixes: rare_names(rare_suffix),
            superior,
            low_quality: low_quality.len(),
            low_quality_names: low_quality.rows().map(|row| row.get("Name").unwrap_or_default().to_string()).collect(),
            uniques,
            set_items,
            properties,
        }
    }

    /// A property's functions by code, any case.
    #[must_use]
    pub fn property(&self, code: &str) -> Option<&[PropertyFunc]> {
        self.properties.get(&code.to_ascii_lowercase()).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affixes_rare_names_uniques_and_sets_read_their_columns() {
        let prefix = Table::parse(
            b"Name\tversion\tspawnable\trare\tlevel\tmaxlevel\tclassspecific\tfrequency\tgroup\tmod1code\tmod1param\tmod1min\tmod1max\titype1\titype2\tetype1\r\n\
              Sturdy\t0\t1\t1\t1\t6\t\t3\t101\tac%\t\t10\t20\tarmo\t\t\r\nExpansion\r\nSnake's\t0\t1\t1\t6\t\t\t3\t102\tmana\t\t5\t10\tshld\tamul\tamaz\r\n",
        );
        let empty = Table::parse(b"Name\r\n");
        let props = Table::parse(b"code\tset1\tval1\tfunc1\tstat1\tfunc2\tstat2\r\nres-all\t\t\t1\tfireresist\t3\tlightresist\r\nama\t\t0\t21\titem_addclassskills\t\t\r\n");
        let quality = Table::parse(b"nummods\tmod1code\tmod1param\tmod1min\tmod1max\tmod2code\tmod2param\tmod2min\tmod2max\tarmor\tweapon\r\n1\tatt\t0\t1\t3\tdmg%\t0\t5\t15\t0\t1\r\n");
        let low = Table::parse(b"Name\r\nCrude\r\nCracked\r\n");
        let uniques = Table::parse(b"index\tversion\tenabled\trarity\tnolimit\tlvl\tlvl req\tcode\tprop1\tpar1\tmin1\tmax1\r\nThe Gnasher\t0\t1\t1\t\t7\t5\thax\tdmg%\t\t60\t70\r\n");
        let set_items = Table::parse(b"index\tset\titem\trarity\tlvl\tadd func\tprop1\tmin1\tmax1\taprop1a\tamin1a\tamax1a\r\nCiverb's Ward\tCiverb's Vestments\tlrg\t7\t9\t1\tac\t15\t15\tmana\t21\t21\r\n");
        let sets = Table::parse(b"index\tversion\r\nCleglaw's Brace\t0\r\nCiverb's Vestments\t0\r\n");
        let a = Affixes::from_tables(&prefix, &prefix, &empty, &empty, &empty, &props, &quality, &low, &uniques, &set_items, &sets);
        assert_eq!(a.prefixes.len(), 2, "the marker takes no row");
        let snake = &a.prefixes[1];
        assert_eq!((snake.level, snake.max_level, snake.group, snake.itypes.as_slice(), snake.etypes.as_slice()), (6, 0, 102, &["shld".to_string(), "amul".into()][..], &["amaz".to_string()][..]));
        assert_eq!(snake.mods, [Mod { code: "mana".into(), param: 0, min: 5, max: 10 }]);
        assert_eq!(a.property("RES-ALL").map(<[PropertyFunc]>::len), Some(2));
        assert_eq!(a.property("ama").unwrap()[0], PropertyFunc { set: 0, val: 0, func: 21, stat: Some("item_addclassskills".into()) });
        assert_eq!((a.superior[0].mods.len(), a.superior[0].applies[1], a.low_quality), (1, true, 2), "nummods 1 keeps one mod");
        assert_eq!((a.uniques[0].code, a.uniques[0].level, a.uniques[0].props[0].max), (*b"hax ", 7, 70));
        assert_eq!((a.set_items[0].code, a.set_items[0].bonuses[0][0].code.as_str()), (*b"lrg ", "mana"));
        assert_eq!((a.set_items[0].set_row, a.set_items[0].add_func), (1, 1));
    }
}
