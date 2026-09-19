//! `TreasureClassEx.txt`: what a dying monster drops.
//!
//! The resolver follows Game.exe's `0x0055A6D0`. A class makes `Picks` rolls. Each roll draws
//! from `NoDrop` plus the item probabilities: a draw inside `NoDrop` drops nothing, otherwise it
//! lands on the first item whose running probability passes it. A negative `Picks` instead drops
//! each item in turn, `Prob` times. An item naming another class is resolved in its place, and
//! at most [`MAX_DROPS`] things drop. With more players `NoDrop` shrinks (the `n`th power of its
//! share, see [`TreasureClass::no_drop_for`]).
//!
//! A monster's class comes from `MonStats.txt` `TreasureClass1`, upgraded within its `group` to
//! the highest `level` the monster has reached ([`TreasureClasses::upgraded`]).
//!
//! A class's `Unique`, `Set`, `Rare` and `Magic` columns better the quality roll of what drops
//! from it; nested classes carry the best of each down (`0x0055A6D0`). An entry naming an item
//! type and a level — `weap3`, `armo12` — is one of the classes the engine builds itself at load
//! (`0x006541C0`, [`TreasureClasses::add_item_classes`]). Items only in Lord of Destruction never
//! drop in a classic game.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::items::Items;
use crate::Error;

/// Most things one monster drops (`0x0055A6D0`'s default limit).
pub const MAX_DROPS: usize = 6;

/// One entry of a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// An item code, `gld`, or another class's name.
    pub name: String,
    /// The gold multiplier in 256ths (`gld,mul=N`), 0 when absent.
    pub mul: u32,
    /// Its probability.
    pub prob: u32,
    /// An item only in Lord of Destruction, left out of a classic game's rolls.
    pub expansion_only: bool,
}

/// How much a treasure class betters each quality's odds, in 1024ths of the chance number
/// (`0x00558640`: `chance −= chance × mod / 1024`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QualityMods {
    /// `Unique`.
    pub unique: u32,
    /// `Set`.
    pub set: u32,
    /// `Rare`.
    pub rare: u32,
    /// `Magic`.
    pub magic: u32,
}

impl QualityMods {
    /// The better of each.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self { unique: self.unique.max(other.unique), set: self.set.max(other.set), rare: self.rare.max(other.rare), magic: self.magic.max(other.magic) }
    }
}

/// A treasure class row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreasureClass {
    /// `Treasure Class`.
    pub name: String,
    /// `group`, 0 for none.
    pub group: i32,
    /// `level`.
    pub level: i32,
    /// `Picks`.
    pub picks: i32,
    /// `NoDrop`.
    pub no_drop: u32,
    /// `Item1`..`Item10` with their `Prob`s.
    pub entries: Vec<Entry>,
    /// `Unique`, `Set`, `Rare`, `Magic`.
    pub mods: QualityMods,
}

/// What one roll produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drop {
    /// A gold pile, with its `mul` (256ths; 0 for none).
    Gold {
        /// The multiplier.
        mul: u32,
    },
    /// An item code, with the quality mods of the classes it came through.
    Item(String, QualityMods),
}

impl TreasureClass {
    /// `NoDrop` for a game counting `players` (1–8): with `f = NoDrop / (NoDrop + total)`, the
    /// new `NoDrop` is `total × fⁿ / (1 − fⁿ)`, truncated.
    #[must_use]
    pub fn no_drop_for(&self, players: u32) -> u32 {
        self.no_drop_in(players, true)
    }

    fn no_drop_in(&self, players: u32, expansion: bool) -> u32 {
        let total: u32 = self.entries.iter().filter(|e| expansion || !e.expansion_only).map(|e| e.prob).sum();
        if self.no_drop == 0 || players < 2 {
            return self.no_drop;
        }
        let f = f64::from(self.no_drop) / f64::from(self.no_drop + total);
        let fn_ = f.powi(players.min(8) as i32);
        if (1.0 - fn_) == 0.0 {
            return 0;
        }
        (f64::from(total) * fn_ / (1.0 - fn_)) as u32
    }
}

/// Every treasure class by name.
#[derive(Debug, Clone, Default)]
pub struct TreasureClasses {
    by_name: HashMap<String, TreasureClass>,
}

impl TreasureClasses {
    /// Read `TreasureClassEx.txt`.
    ///
    /// # Errors
    ///
    /// [`Error`] if the table has no `Treasure Class` column.
    pub fn from_table(table: &Table) -> Result<Self, Error> {
        if table.column("Treasure Class").is_none() {
            return Err(Error::BadTable { table: "treasureclassex.txt", problem: "no Treasure Class column".into() });
        }
        let by_name = table
            .rows()
            .filter_map(|row| {
                let name = row.get("Treasure Class").filter(|n| !n.is_empty())?.to_string();
                let int = |c: &str| row.int(c).unwrap_or(0);
                let entries = (1..=10)
                    .filter_map(|i| {
                        // An entry with a comma is quoted in the file: `"gld,mul=1280"`.
                        let item = row.get(&format!("Item{i}")).map(|s| s.trim_matches('"')).filter(|s| !s.is_empty())?;
                        let prob = u32::try_from(row.int(&format!("Prob{i}")).unwrap_or(0)).ok().filter(|&p| p > 0)?;
                        let mut parts = item.split(',');
                        let name = parts.next().unwrap_or_default().to_string();
                        let mul = parts.find_map(|p| p.strip_prefix("mul=")).and_then(|m| m.parse().ok()).unwrap_or(0);
                        Some(Entry { name, mul, prob, expansion_only: false })
                    })
                    .collect();
                let tc = TreasureClass {
                    name: name.clone(),
                    group: int("group") as i32,
                    level: int("level") as i32,
                    picks: int("Picks") as i32,
                    no_drop: u32::try_from(int("NoDrop")).unwrap_or(0),
                    entries,
                    mods: QualityMods {
                        unique: u32::try_from(int("Unique")).unwrap_or(0),
                        set: u32::try_from(int("Set")).unwrap_or(0),
                        rare: u32::try_from(int("Rare")).unwrap_or(0),
                        magic: u32::try_from(int("Magic")).unwrap_or(0),
                    },
                };
                Some((name.to_ascii_lowercase(), tc))
            })
            .collect();
        Ok(Self { by_name })
    }

    /// Build the classes the engine makes for each item type flagged `TreasureClass` (`0x006541C0`):
    /// `<code><n>` for n = 3, 6, … 96, holding every spawnable, non-quest item of the type (by
    /// `type` or `type2`) whose `level` is above n − 3 and at most n, each weighted by its own
    /// type's `Rarity` (at least 1). Throwing potions only go in their own type's classes.
    pub fn add_item_classes(&mut self, items: &Items) {
        let types = items.types();
        let tpot = types.id("tpot");
        for (type_id, t) in types.iter().enumerate().filter(|(_, t)| t.treasure_class) {
            let type_id = type_id as i32;
            for top in (3..=96).step_by(3) {
                let entries = items
                    .iter()
                    .enumerate()
                    .filter(|&(class, def)| {
                        let class = class as i32;
                        !def.quest
                            && def.spawnable
                            && items.is_type(class, type_id)
                            && (Some(type_id) == tpot || !tpot.is_some_and(|p| items.is_type(class, p)))
                            && def.level > top - 3
                            && def.level <= top
                    })
                    .map(|(_, def)| Entry {
                        name: crate::items::code_str(&def.code),
                        mul: 0,
                        prob: types.get(def.item_type).map_or(1, |ty| ty.rarity.max(1)) as u32,
                        expansion_only: def.version >= 100,
                    })
                    .collect();
                let name = format!("{}{top}", t.code);
                self.by_name.entry(name.to_ascii_lowercase()).or_insert(TreasureClass {
                    name,
                    group: 0,
                    level: top - 3,
                    picks: 1,
                    no_drop: 0,
                    entries,
                    mods: QualityMods::default(),
                });
            }
        }
    }

    /// A class by name, any case.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TreasureClass> {
        self.by_name.get(&name.to_ascii_lowercase())
    }

    /// `name`, or the class of its group with the highest level not above `monster_level` when
    /// that is higher.
    #[must_use]
    pub fn upgraded(&self, name: &str, monster_level: i32) -> Option<&TreasureClass> {
        let base = self.get(name)?;
        if base.group == 0 {
            return Some(base);
        }
        Some(
            self.by_name
                .values()
                .filter(|tc| tc.group == base.group && tc.level <= monster_level && tc.level > base.level)
                .max_by_key(|tc| tc.level)
                .unwrap_or(base),
        )
    }

    /// Roll `class` for an expansion game of `players`, drawing numbers from `pick` (uniform in
    /// `[0, n)`).
    pub fn roll(&self, class: &TreasureClass, players: u32, pick: &mut dyn FnMut(u32) -> u32) -> Vec<Drop> {
        self.roll_for(class, players, true, pick)
    }

    /// Roll `class` for a game of `players`, classic or expansion.
    pub fn roll_for(&self, class: &TreasureClass, players: u32, expansion: bool, pick: &mut dyn FnMut(u32) -> u32) -> Vec<Drop> {
        let mut drops = Vec::new();
        self.roll_into(class, players, expansion, true, class.mods, pick, &mut drops, 0);
        drops
    }

    /// Roll `class` as Find Item does (`0x0055A6D0` with its fifth argument set): `NoDrop` is
    /// ignored at every level, so every pick yields something.
    pub fn roll_found(&self, class: &TreasureClass, players: u32, expansion: bool, pick: &mut dyn FnMut(u32) -> u32) -> Vec<Drop> {
        let mut drops = Vec::new();
        self.roll_into(class, players, expansion, false, class.mods, pick, &mut drops, 0);
        drops
    }

    #[allow(clippy::too_many_arguments)] // the recursion's own state
    fn roll_into(&self, class: &TreasureClass, players: u32, expansion: bool, with_no_drop: bool, mods: QualityMods, pick: &mut dyn FnMut(u32) -> u32, drops: &mut Vec<Drop>, depth: usize) {
        if depth > 63 {
            return;
        }
        let entries: Vec<&Entry> = class.entries.iter().filter(|e| expansion || !e.expansion_only).collect();
        let picks = class.picks.unsigned_abs().max(1);
        for taken in 0..picks {
            if drops.len() >= MAX_DROPS {
                return;
            }
            let entry = if class.picks < 0 {
                // Sequential: entry i drops Prob_i times.
                let mut running = 0;
                entries.iter().find(|e| {
                    running += e.prob;
                    running > taken
                })
            } else {
                let no_drop = if with_no_drop { class.no_drop_in(players, expansion) } else { 0 };
                let total: u32 = entries.iter().map(|e| e.prob).sum::<u32>() + no_drop;
                if total == 0 {
                    continue;
                }
                let r = pick(total);
                if r < no_drop {
                    continue;
                }
                let r = r - no_drop;
                let mut running = 0;
                entries.iter().find(|e| {
                    running += e.prob;
                    running > r
                })
            };
            let Some(entry) = entry else { continue };
            if let Some(sub) = self.get(&entry.name) {
                self.roll_into(sub, players, expansion, with_no_drop, mods.max(sub.mods), pick, drops, depth + 1);
            } else if entry.name.eq_ignore_ascii_case("gld") {
                drops.push(Drop::Gold { mul: entry.mul });
            } else {
                drops.push(Drop::Item(entry.name.clone(), mods));
            }
        }
    }
}

/// A gold pile's size for a drop at item level `level` (`0x00557AB0`): `level + rand(5 × level)`,
/// at least 1, then scaled by the class entry's `mul` in 256ths when it has one (`0x0055A6D0`).
pub fn gold_amount(level: i32, mul: u32, pick: &mut dyn FnMut(u32) -> u32) -> u32 {
    let level = level.max(0) as u32;
    let base = (level + pick(level * 5)).max(1);
    if mul == 0 {
        base
    } else {
        ((u64::from(base) * u64::from(mul)) >> 8) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, group: i32, level: i32, picks: i32, no_drop: u32, entries: &[(&str, u32)]) -> TreasureClass {
        TreasureClass {
            name: name.into(),
            group,
            level,
            picks,
            no_drop,
            entries: entries.iter().map(|&(n, p)| Entry { name: n.into(), mul: 0, prob: p, expansion_only: false }).collect(),
            mods: QualityMods::default(),
        }
    }

    fn classes(list: Vec<TreasureClass>) -> TreasureClasses {
        TreasureClasses { by_name: list.into_iter().map(|c| (c.name.to_ascii_lowercase(), c)).collect() }
    }

    #[test]
    fn a_roll_lands_on_no_drop_or_the_running_probability() {
        let tcs = classes(vec![class("Mob", 0, 0, 1, 100, &[("gld", 21), ("Junk", 21)]), class("Junk", 0, 0, 1, 0, &[("hp1", 1)])]);
        let mob = tcs.get("mob").unwrap();
        let with = |r: u32| tcs.roll(mob, 1, &mut |n| r.min(n - 1));
        assert!(with(99).is_empty(), "inside NoDrop");
        assert_eq!(with(100), [Drop::Gold { mul: 0 }]);
        assert_eq!(with(120), [Drop::Gold { mul: 0 }]);
        assert_eq!(with(121), [Drop::Item("hp1".into(), QualityMods::default())], "a sub-class resolves in place");
    }

    #[test]
    fn a_quoted_gold_entry_keeps_its_multiplier() {
        let table = Table::parse(b"Treasure Class\tgroup\tlevel\tPicks\tNoDrop\tItem1\tProb1\r\nChamp\t\t\t-1\t\t\"gld,mul=1280\"\t1\r\n");
        let tcs = TreasureClasses::from_table(&table).unwrap();
        let champ = tcs.get("Champ").unwrap();
        assert_eq!((champ.entries[0].name.as_str(), champ.entries[0].mul), ("gld", 1280));
        assert_eq!(tcs.roll(champ, 1, &mut |_| 0), [Drop::Gold { mul: 1280 }]);
    }

    #[test]
    fn negative_picks_drop_every_entry_its_count() {
        let tcs = classes(vec![class("Boss", 0, 0, -3, 0, &[("gld", 2), ("hp1", 1)])]);
        let drops = tcs.roll(tcs.get("Boss").unwrap(), 1, &mut |_| panic!("no draws"));
        assert_eq!(drops, [Drop::Gold { mul: 0 }, Drop::Gold { mul: 0 }, Drop::Item("hp1".into(), QualityMods::default())]);
    }

    #[test]
    fn at_most_six_things_drop() {
        let tcs = classes(vec![class("Lots", 0, 0, 9, 0, &[("gld", 1)])]);
        assert_eq!(tcs.roll(tcs.get("Lots").unwrap(), 1, &mut |_| 0).len(), MAX_DROPS);
    }

    #[test]
    fn more_players_shrink_no_drop() {
        let tc = class("Mob", 0, 0, 1, 100, &[("gld", 60)]);
        assert_eq!(tc.no_drop_for(1), 100);
        // f = 100/160; f² = 0.390625; 60 × f² / (1 − f²) = 38.46.
        assert_eq!(tc.no_drop_for(2), 38);
        assert!(tc.no_drop_for(8) < tc.no_drop_for(3));
    }

    #[test]
    fn classes_upgrade_within_their_group() {
        let tcs = classes(vec![class("A", 7, 0, 1, 0, &[]), class("B", 7, 38, 1, 0, &[]), class("C", 7, 60, 1, 0, &[]), class("Solo", 0, 0, 1, 0, &[])]);
        assert_eq!(tcs.upgraded("A", 2).unwrap().name, "A");
        assert_eq!(tcs.upgraded("A", 40).unwrap().name, "B");
        assert_eq!(tcs.upgraded("a", 99).unwrap().name, "C");
        assert_eq!(tcs.upgraded("B", 5).unwrap().name, "B", "never down");
        assert_eq!(tcs.upgraded("Solo", 99).unwrap().name, "Solo");
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`): a Blood Moor zombie's class drops
    /// gold, junk, equipment or nothing, and a few hundred rolls see gold.
    #[test]
    fn a_zombie_drops_from_its_install_class() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else { return };
        let data = crate::GameData::load(dir).expect("install loads");
        let zombie = data.monsters().class_named("zombie1").and_then(|c| data.monsters().get(c)).expect("zombie1");
        let name = &zombie.combat.treasure[0];
        assert_eq!(name, "Act 1 H2H A");
        let tc = data.treasure().upgraded(name, zombie.combat.level[0]).expect("class");
        assert_eq!((tc.picks, tc.no_drop, tc.entries[0].name.as_str()), (1, 100, "gld"));
        let mut state = 7u64;
        let mut pick = |n: u32| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as u32) % n.max(1)
        };
        let mut gold = 0;
        for _ in 0..300 {
            let drops = data.treasure().roll(tc, 1, &mut pick);
            assert!(drops.len() <= 1);
            gold += drops.iter().filter(|d| matches!(d, Drop::Gold { .. })).count();
        }
        assert!((20..70).contains(&gold), "about 21 in 160 rolls, 39 of 300: {gold}");
    }

    /// With the operator's install: the engine's own `weap3` and `armo3` classes hold the
    /// low-level bases, weighted by their type's rarity, and Act I's equipment class reaches them
    /// with its quality mods.
    #[test]
    fn with_a_real_install_item_type_classes_are_built() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else { return };
        let data = crate::GameData::load(dir).expect("install loads");
        let weap3 = data.treasure().get("weap3").expect("weap3");
        let names: Vec<&str> = weap3.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hax") && names.contains(&"ssd") && !names.contains(&"lsd"), "{names:?}");
        assert!(weap3.entries.iter().all(|e| e.prob >= 1));
        let armo3 = data.treasure().get("armo3").expect("armo3");
        assert!(armo3.entries.iter().any(|e| e.name == "cap"));
        assert!(data.treasure().get("weap96").is_some() && data.treasure().get("weap99").is_none());
        let mut state = 5u64;
        let mut pick = |n: u32| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as u32) % n.max(1)
        };
        let equip = data.treasure().get("Act 1 Equip A").expect("Act 1 Equip A");
        let drops: Vec<Drop> = (0..200).flat_map(|_| data.treasure().roll_for(equip, 1, false, &mut pick)).collect();
        assert!(drops.iter().all(|d| matches!(d, Drop::Item(code, _) if data.items().class_of(&crate::items::code(code)).is_some())), "{drops:?}");
        assert!(!drops.iter().any(|d| matches!(d, Drop::Item(code, _) if data.items().get(data.items().class_of(&crate::items::code(code)).unwrap()).unwrap().version >= 100)), "no expansion items in a classic game");
    }

    #[test]
    fn gold_scales_with_level_and_mul() {
        assert_eq!(gold_amount(2, 0, &mut |n| {
            assert_eq!(n, 10);
            7
        }), 9);
        assert_eq!(gold_amount(0, 0, &mut |_| 0), 1, "at least 1");
        assert_eq!(gold_amount(10, 1280, &mut |_| 0), 50, "×5");
    }
}
