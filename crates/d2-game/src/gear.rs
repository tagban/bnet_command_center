//! What a player's worn items add to it: attributes, life, mana and stamina, defence, attack
//! rating and its weapon's damage.
//!
//! An item's own percentages apply to its own base values first (`ItemStatCost.txt` op 13): a
//! weapon's enhanced damage to its base damage, armour's enhanced defence to its base defence.
//! What other items add is summed over the player: flat damage, and enhanced damage that the
//! physical damage routine (`0x0057B420`) applies with the weapon's strength and dexterity bonus
//! to the whole.

use d2_data::item_bits::{flags, Item, Quality};
use d2_data::items::Code;
use d2_data::GameData;

/// Stat ids the sums read.
mod stat {
    pub const STRENGTH: u16 = 0;
    pub const ENERGY: u16 = 1;
    pub const DEXTERITY: u16 = 2;
    pub const VITALITY: u16 = 3;
    pub const MAXHP: u16 = 7;
    pub const MAXMANA: u16 = 9;
    pub const MAXSTAMINA: u16 = 11;
    pub const ARMOR_PERCENT: u16 = 16;
    pub const MAXDAMAGE_PERCENT: u16 = 17;
    pub const MINDAMAGE_PERCENT: u16 = 18;
    pub const TOHIT: u16 = 19;
    pub const MINDAMAGE: u16 = 21;
    pub const MAXDAMAGE: u16 = 22;
    pub const SECONDARY_MINDAMAGE: u16 = 23;
    pub const SECONDARY_MAXDAMAGE: u16 = 24;
    pub const ARMORCLASS: u16 = 31;
    pub const TOHIT_PERCENT: u16 = 119;
}

/// The weapon a player swings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weapon {
    /// Its damage with its own enhanced damage and flat damage: whole points.
    pub min: i32,
    /// See [`Self::min`].
    pub max: i32,
    /// `StrBonus`, `DexBonus`: hundredths of a percent of damage per point.
    pub bonus: (i32, i32),
    /// `wclass` (or `2handedwclass` when held in both hands): the attack animation's weapon class.
    pub class: Code,
}

/// What worn items add.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gear {
    /// The weapon in the right hand (or the left, when the right holds none).
    pub weapon: Option<Weapon>,
    /// Strength, energy, dexterity, vitality.
    pub attributes: [i32; 4],
    /// Whole life, mana and stamina.
    pub life: i32,
    /// See [`Self::life`].
    pub mana: i32,
    /// See [`Self::life`].
    pub stamina: i32,
    /// Defence: each armour's base with its own enhanced defence, and every flat bonus.
    pub defense: i32,
    /// Flat attack rating.
    pub to_hit: i32,
    /// Percent attack rating.
    pub to_hit_percent: i32,
    /// Flat damage from items other than the weapon.
    pub damage: (i32, i32),
    /// Enhanced damage from items other than the weapon.
    pub damage_percent: (i32, i32),
}

/// A stat's whole value summed over an item's own list.
fn sum(data: &GameData, item: &Item, id: u16) -> i32 {
    let shift = data.item_stats().get(id).map_or(0, |c| c.val_shift);
    item.stats.iter().filter(|s| s.id == id).map(|s| s.value >> shift).sum()
}

/// What the items worn at body locations add. `worn` pairs each item with its body location
/// (4 the right hand, 5 the left).
#[must_use]
pub fn gear(data: &GameData, worn: &[(u8, &Item)]) -> Gear {
    let items = data.items();
    let mut g = Gear::default();
    let weapon_at = |body: u8| {
        worn.iter().find(|(b, i)| *b == body && items.class_of(&i.code).is_some_and(|c| items.is(c, "weap"))).map(|(_, i)| *i)
    };
    let weapon = weapon_at(4).or_else(|| weapon_at(5));
    let two_handed = weapon.and_then(|w| items.class_of(&w.code)).and_then(|c| items.get(c)).is_some_and(|d| d.two_handed || d.damage == (0, 0));
    let (flat_min, flat_max) = if two_handed { (stat::SECONDARY_MINDAMAGE, stat::SECONDARY_MAXDAMAGE) } else { (stat::MINDAMAGE, stat::MAXDAMAGE) };
    for &(_, item) in worn {
        let Some(def) = items.class_of(&item.code).and_then(|c| items.get(c)) else { continue };
        let s = |id| sum(data, item, id);
        for (slot, id) in [stat::STRENGTH, stat::ENERGY, stat::DEXTERITY, stat::VITALITY].into_iter().enumerate() {
            g.attributes[slot] += s(id);
        }
        g.life += s(stat::MAXHP);
        g.mana += s(stat::MAXMANA);
        g.stamina += s(stat::MAXSTAMINA);
        g.to_hit += s(stat::TOHIT);
        g.to_hit_percent += s(stat::TOHIT_PERCENT);
        g.defense += item.defense + item.defense * s(stat::ARMOR_PERCENT) / 100 + s(stat::ARMORCLASS);
        if weapon.is_some_and(|w| std::ptr::eq(w, item)) {
            // Ethereal and low quality weapons keep their changed base damage in the engine's base
            // list (`0x0065E4D0`, `0x005C2D40`); it is worked out again here.
            let (lo, hi) = if two_handed { def.two_hand_damage } else { def.damage };
            let scale = |v: i32| {
                let v = if item.flags & flags::ETHEREAL != 0 { v * 3 / 2 } else { v };
                if matches!(item.quality, Quality::Inferior(_)) { v * 75 / 100 } else { v }
            };
            let (lo, hi) = (scale(lo), scale(hi));
            g.weapon = Some(Weapon {
                min: lo + lo * s(stat::MINDAMAGE_PERCENT) / 100 + s(flat_min),
                max: hi + hi * s(stat::MAXDAMAGE_PERCENT) / 100 + s(flat_max),
                bonus: (def.str_bonus, def.dex_bonus),
                class: if two_handed { def.two_handed_class.or(def.weapon_class) } else { def.weapon_class }.unwrap_or(*b"hth "),
            });
        } else {
            g.damage.0 += s(flat_min);
            g.damage.1 += s(flat_max);
            g.damage_percent.0 += s(stat::MINDAMAGE_PERCENT);
            g.damage_percent.1 += s(stat::MAXDAMAGE_PERCENT);
        }
    }
    g
}

impl Gear {
    /// A swing's physical damage range in 256ths of a point (`0x0057B420` for a player's normal
    /// attack): the weapon's (or the fist's 1–2) with the flat damage added, each end raised by the
    /// matching enhanced damage and the strength and dexterity bonus (`StrBonus × strength / 100`
    /// percent; a percent a point of strength for the fist), no lower than −90%; the maximum at
    /// least the minimum and a point.
    #[must_use]
    pub fn damage_range(&self, strength: i32, dexterity: i32) -> (i32, i32) {
        let (lo, hi, bonus) = match self.weapon {
            Some(w) => (w.min + self.damage.0, w.max + self.damage.1, strength * w.bonus.0 / 100 + dexterity * w.bonus.1 / 100),
            None => ((1 + self.damage.0).max(1), (2 + self.damage.1).max(2), strength),
        };
        let mut lo = lo << 8;
        let mut hi = hi << 8;
        if lo < 1 {
            lo = 0x100;
        }
        if hi <= lo {
            hi = lo + 0x100;
        }
        let bonus = bonus.max(-90);
        (lo + lo * (self.damage_percent.0 + bonus) / 100, hi + hi * (self.damage_percent.1 + bonus) / 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::item_bits::{ItemStat, Location};
    use d2_data::item_stats::ItemStats;
    use d2_data::items::{code, Items};
    use d2_formats::excel::Table;

    fn data() -> GameData {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\r\n");
        for name in d2_data::CLASSES {
            cs.push_str(&format!("{name}\t25\t20\t15\t20\t0\t80\t30\r\n"));
        }
        let exp = "Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\tExpRatio\r\nMaxLvl\t99\t99\t99\t99\t99\t99\t99\t10\r\n0\t0\t0\t0\t0\t0\t0\t0\t1024\r\n1\t500\t500\t500\t500\t500\t500\t500\t1024\r\n";
        let mut data = GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(exp.as_bytes())).unwrap();
        let types = Table::parse(b"ItemType\tCode\tEquiv1\r\nWeapon\tweap\t\r\nAxe\taxe\tweap\r\nArmor\tarmo\t\r\nHelm\thelm\tarmo\r\n");
        let weapons = Table::parse(
            b"name\tcode\ttype\tmindam\tmaxdam\t2handmindam\t2handmaxdam\t2handed\tStrBonus\tDexBonus\twclass\t2handedwclass\r\n\
              Hand Axe\thax\taxe\t3\t6\t\t\t0\t100\t0\t1hs\t1hs\r\n\
              Great Axe\tgix\taxe\t\t\t9\t30\t1\t100\t0\t2hs\t2hs\r\n",
        );
        let armor = Table::parse(b"name\tcode\ttype\r\nCap\tcap\thelm\r\n");
        let misc = Table::parse(b"name\tcode\ttype\r\n");
        data.set_items(Items::from_tables(&types, &weapons, &armor, &misc).unwrap(), Vec::new());
        let stats = ItemStats::from_table(&Table::parse(
            b"Stat\tID\tValShift\r\nstrength\t0\t\r\nmaxhp\t7\t8\r\nitem_armor_percent\t16\t\r\nitem_maxdamage_percent\t17\t\r\nitem_mindamage_percent\t18\t\r\ntohit\t19\t\r\nmindamage\t21\t\r\nmaxdamage\t22\t\r\nsecondary_mindamage\t23\t\r\nsecondary_maxdamage\t24\t\r\narmorclass\t31\t\r\n",
        ))
        .unwrap();
        data.set_item_rules(stats, d2_data::item_stats::ItemRatios::default());
        data
    }

    fn item(c: &str, stats: &[(u16, i32)]) -> Item {
        let mut i = Item::new(code(c), 2, 10, Location::Equipped { body: 4 });
        i.stats = stats.iter().map(|&(id, value)| ItemStat { id, param: 0, value }).collect();
        i
    }

    #[test]
    fn a_weapons_own_percent_applies_to_its_base_and_other_items_to_the_whole() {
        let data = data();
        let axe = item("hax", &[(18, 50), (17, 50), (21, 1), (22, 2)]);
        let mut cap = item("cap", &[(16, 50), (31, 3), (7, 10 << 8), (0, 5), (17, 20), (18, 20)]);
        cap.defense = 4;
        let g = gear(&data, &[(4, &axe), (1, &cap)]);
        let w = g.weapon.unwrap();
        assert_eq!((w.min, w.max, w.class), (3 + 1 + 1, 6 + 3 + 2, *b"1hs "), "3–6 +50%, then +1–2");
        assert_eq!((g.defense, g.life, g.attributes[0], g.damage_percent), (4 + 2 + 3, 10, 5, (20, 20)));
        // 30 strength at StrBonus 100 is +30%; with the cap's +20%: 5 × 1.5 = 7.5, 11 × 1.5 = 16.5.
        assert_eq!(g.damage_range(30, 0), (1920, 4224));
        let bare = gear(&data, &[]);
        assert_eq!(bare.damage_range(30, 0), (332, 665), "the fist, a percent a point of strength");
    }

    #[test]
    fn a_two_handed_weapon_uses_its_two_handed_damage_and_stats() {
        let data = data();
        let great = item("gix", &[(21, 5), (23, 2), (24, 4)]);
        let w = gear(&data, &[(4, &great)]).weapon.unwrap();
        assert_eq!((w.min, w.max, w.class), (11, 34, *b"2hs "));
    }
}
