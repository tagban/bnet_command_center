//! Vendors: what a vendor stocks and what its trades cost, as the 1.14d server works them out.
//!
//! **Stock** (`0x00576980`, made when a player opens the trade window, `0x00579430` →
//! `0x00578B30`): the store level is the player's level + 5, no higher than the act's cap in
//! normal difficulty (`0x00576890`). Every item row whose vendor column gives it a `Max` or
//! `MagicMax` and that is `spawnable` is either always in stock (`PermStoreItem`: potions, scrolls,
//! tomes, keys, arrows) or a stock entry (`0x00536D50`). An entry no higher in level than the store
//! gets `Min`–`Max` items of a rolled quality while the store level is under 25 (`0x00576900`), and,
//! when its `bitfield1` allows magic ones and its `MagicLvl` is reached, `MagicMin`–`MagicMax` magic
//! ones (one or two more past store level 24). Lord of Destruction items stay out of a classic
//! game. Each always-stocked item is there once, arrows and bolts a full stack. In nightmare and
//! hell, for players past level 25, an item may come as its exceptional or elite version, or its
//! `NightmareUpgrade`/`HellUpgrade` (`0x00576330`). No low-quality item called `Cracked` is stocked.
//! Items go on the page their type's `StorePage` names, a 10 × 10 grid, where a picked-up item
//! would go in an inventory; a full weapons page spills onto the next.
//!
//! **Prices** (`0x0062EFB0`): the item's `cost` — per piece for arrows and bolts, per charge on top
//! for a tome, scaled by defence for armour — plus what its qualities add: its automagic affix, then
//! a low-quality item half off, a magic, rare or crafted item's affixes, a set item's or unique's
//! `cost mult`/`cost add`, and, for superior items up, every stat's `ItemStatCost.txt`
//! `Multiply`/`Add` (a skill's `cost mult`/`cost add` for skill stats) and the staff skills
//! (`0x0062EDD0`). Selling, an ethereal or class item is worth a quarter. Then the vendor's
//! `Npc.txt` multiplier: `sell mult` for what it sells, `buy mult` for what it pays (no more than
//! `max buy`), and the stack's quantity.
//!
//! Not ported: quest-completion multipliers (no quest is completed in these games yet), the
//! player's reduced-prices stat, ears' and body parts' levels, socketed items' fillings, and stats
//! kept by time of day.

use std::collections::HashMap;

use d2_data::item_bits::{flags, Item, Quality};
use d2_data::items::{Code, ItemDef};
use d2_data::GameData;
use d2_drlg::rng::Seed;

use crate::inventory::free_spot;
use crate::loot::{self, Making};

/// A store page's width: `Inventory.txt`'s `Monster` grid.
pub const PAGE_WIDTH: u8 = 10;
/// A store page's height.
pub const PAGE_HEIGHT: u8 = 10;

/// How long a stock stands before the next player opening the window alone gets a new one
/// (`0x00537230`: 240,000 ms).
pub const RESTOCK_MS: u64 = 240_000;

/// A vendor: its monster class, the item tables' vendor columns it stocks from, and its act
/// (`0x00731188`; the vendor switch in `0x00536070`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vendor {
    /// `MonStats.txt` class.
    pub class: u16,
    /// Index into [`d2_data::items::VENDORS`].
    pub slot: usize,
    /// Act, 0–4.
    pub act: u8,
}

const fn vendor_of(class: u16, slot: usize, act: u8) -> Vendor {
    Vendor { class, slot, act }
}

/// The NPCs a player can trade with (`0x00579D60`, entity action 1): Gheed, Akara, Charsi, Drognan,
/// Fara, Elzix, Lysander, Asheara, Hratli, Alkor, Ormus, Halbu, Jamella, Larzuk, Anya and Malah.
pub const VENDORS: [Vendor; 16] = [
    vendor_of(147, 1, 0),
    vendor_of(148, 0, 0),
    vendor_of(154, 2, 0),
    vendor_of(177, 5, 1),
    vendor_of(178, 3, 1),
    vendor_of(199, 9, 1),
    vendor_of(202, 4, 1),
    vendor_of(252, 10, 2),
    vendor_of(253, 6, 2),
    vendor_of(254, 7, 2),
    vendor_of(255, 8, 2),
    vendor_of(257, 12, 3),
    vendor_of(405, 13, 3),
    vendor_of(511, 15, 4),
    vendor_of(512, 16, 4),
    vendor_of(513, 14, 4),
];

/// The vendor a monster class is, if it trades.
#[must_use]
pub fn vendor(class: u16) -> Option<Vendor> {
    VENDORS.iter().copied().find(|v| v.class == class)
}

/// The level a vendor's stock is made at (`0x00576890`): the player's level + 5, capped in normal
/// difficulty at 12, 20, 28, 36 and 45 by act.
#[must_use]
pub fn store_level(difficulty: u8, act: u8, player_level: u32) -> i32 {
    let level = i32::try_from(player_level).unwrap_or(99) + 5;
    match (difficulty, [12, 20, 28, 36, 45].get(usize::from(act))) {
        (0, Some(&cap)) => level.min(cap),
        _ => level,
    }
}

/// `min` + a roll under `max − min`, or `min` when there is no room (`0x004BC500`).
fn roll_between(rng: &mut Seed, min: i32, max: i32) -> i32 {
    if max <= min {
        min
    } else {
        min + rng.pick((max - min) as u32) as i32
    }
}

/// A normal stock item's quality (`0x00576900`): at store levels under 5 low quality one time in
/// eleven, under 10 superior one time in seven, above that one time in four; else normal.
fn stock_quality(rng: &mut Seed, level: i32) -> u8 {
    let roll = rng.roll() % 100;
    if level < 5 {
        if roll > 90 {
            1
        } else {
            2
        }
    } else if (level < 10 && roll > 85) || (level >= 10 && roll > 74) {
        3
    } else {
        2
    }
}

/// An item in a vendor's stock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockItem {
    /// Item class.
    pub class: i32,
    /// Store page (`StorePage.txt` row).
    pub page: u8,
    /// Column.
    pub col: u8,
    /// Row.
    pub row: u8,
    /// The item.
    pub item: Item,
}

/// Whether an item is a low-quality one called `Cracked`.
fn cracked(data: &GameData, item: &Item) -> bool {
    matches!(item.quality, Quality::Inferior(row) if data.affixes().low_quality_names.get(usize::from(row)).is_some_and(|n| n.eq_ignore_ascii_case("Cracked")))
}

/// The code a stock item comes as (`0x00576330`): in nightmare, for a player past level 25, its
/// exceptional version when a roll under 100,000 falls below store level × 64 + 4,000, else its
/// `NightmareUpgrade`; in hell its elite version (Lord of Destruction games) under 1,000 plus store
/// level × 16, else its exceptional one under 5,000 plus store level × 128, and then its
/// `HellUpgrade` over either.
fn stock_code(data: &GameData, def: &ItemDef, making: Making, level: i32, player_level: u32, rng: &mut Seed) -> Code {
    let blank = |c: &Code| *c == *b"    " || *c == [0; 4];
    let known = |c: Code| data.items().class_of(&c).is_some();
    let mut code = def.code;
    if making.difficulty == 0 || player_level <= 25 {
        return code;
    }
    let roll = rng.pick(100_000) as i32;
    let [_, uber, ultra] = def.tiers;
    if making.difficulty == 1 {
        if roll < level * 64 + 4000 && !blank(&uber) {
            code = uber;
        } else if let Some(up) = def.upgrades[0] {
            code = up;
        }
    } else {
        if making.version >= 100 && roll < level * 16 + 1000 && !blank(&ultra) {
            code = ultra;
        } else if roll < level * 128 + 5000 && !blank(&uber) {
            code = uber;
        }
        if let Some(up) = def.upgrades[1] {
            code = up;
        }
    }
    if known(code) {
        code
    } else {
        def.code
    }
}

/// Make one stock item and find it a spot among `stock` (`0x00576330`); whether it went in.
#[allow(clippy::too_many_arguments)] // the engine routine's own arguments
fn stock_one(data: &GameData, stock: &mut Vec<StockItem>, class: i32, quality: u8, level: i32, player_level: u32, making: Making, rng: &mut Seed) -> Option<()> {
    let items = data.items();
    let def = items.get(class)?;
    let code = stock_code(data, def, making, level, player_level, rng);
    let class = items.class_of(&code)?;
    let def = items.get(class)?;
    let mut tries = 0;
    let mut item = loop {
        let item = loot::make_for_store(data, code, level, quality, making, rng.roll())?;
        tries += 1;
        if !cracked(data, &item) {
            break item;
        }
        if tries > 4 {
            return None;
        }
    };
    let mut page = items.types().get(def.item_type)?.store_page?;
    restock(data, class, &mut item);
    let occupied = |stock: &[StockItem], page: u8| {
        let cells: Vec<(u8, u8, (u8, u8))> = stock.iter().filter(|s| s.page == page).map(|s| (s.col, s.row, items.get(s.class).map_or((1, 1), |d| d.inv_size))).collect();
        move |c: u8, r: u8| cells.iter().any(|&(col, row, (w, h))| (col..col + w).contains(&c) && (row..row + h).contains(&r))
    };
    let mut spot = free_spot(PAGE_WIDTH, PAGE_HEIGHT, def.inv_size, &occupied(stock, page));
    if spot.is_none() && page == 1 {
        page = 2;
        spot = free_spot(PAGE_WIDTH, PAGE_HEIGHT, def.inv_size, &occupied(stock, page));
    }
    let (col, row) = spot?;
    stock.push(StockItem { class, page, col, row, item });
    Some(())
}

/// A stock item made whole (`0x005761C0`): a full stack and durability, for an item that has
/// durability to repair.
pub fn restock(data: &GameData, class: i32, item: &mut Item) {
    let Some(def) = data.items().get(class) else { return };
    if !repairable(def, item) {
        return;
    }
    if def.stackable {
        item.quantity = max_quantity(def, item) as u16;
    }
    if item.max_durability > 0 {
        item.durability = item.max_durability;
    }
}

/// A vendor's stock for a player of `player_level` (`0x00576980`, § [module](self)), made from
/// `seed`.
#[must_use]
pub fn stock(data: &GameData, vendor: Vendor, making: Making, player_level: u32, seed: u32) -> Vec<StockItem> {
    let items = data.items();
    let level = store_level(making.difficulty, vendor.act, player_level);
    let mut rng = Seed::new(seed, 666);
    let mut out = Vec::new();
    let mut failures = 0;
    let listed = |d: &ItemDef| d.spawnable && d.vendors.get(vendor.slot).is_some_and(|v| v.max != 0 || v.magic_max != 0);
    for (class, def) in items.iter().enumerate().filter(|(_, d)| listed(d) && !d.perm_store_item) {
        let class = class as i32;
        let columns = def.vendors[vendor.slot];
        if def.level > level {
            continue;
        }
        let normal = if level < 25 { roll_between(&mut rng, i32::from(columns.min), i32::from(columns.max) + 1) } else { 0 };
        if def.version >= 100 && making.version < 100 {
            continue;
        }
        for _ in 0..normal {
            let quality = stock_quality(&mut rng, level);
            if stock_one(data, &mut out, class, quality, level, player_level, making, &mut rng).is_none() {
                failures += 1;
            }
            if failures > 32 {
                return out;
            }
        }
        if def.bitfield1 & 1 == 1 && i32::from(columns.magic_level) <= level {
            let extra = if level >= 25 { roll_between(&mut rng, 1, 3) + 1 } else { 1 };
            let magic = roll_between(&mut rng, i32::from(columns.magic_min), i32::from(columns.magic_max) + extra);
            for _ in 0..magic {
                if stock_one(data, &mut out, class, 4, level, player_level, making, &mut rng).is_none() {
                    failures += 1;
                }
            }
        }
    }
    for (class, _) in items.iter().enumerate().filter(|(_, d)| listed(d) && d.perm_store_item) {
        let before = out.len();
        if stock_one(data, &mut out, class as i32, 2, level, player_level, making, &mut rng).is_none() {
            failures += 1;
        } else if let Some(made) = out.get_mut(before) {
            if matches!(&made.item.code, b"aqv " | b"cqv ") {
                if let Some(def) = items.get(made.class) {
                    made.item.quantity = max_quantity(def, &made.item) as u16;
                }
            }
        }
        if failures > 32 {
            break;
        }
    }
    out
}

/// Whether `item` is one a vendor always has (`0x00576ED0`): a `PermStoreItem` it stocks, and in
/// nightmare and hell greater and super healing and mana potions.
#[must_use]
pub fn always_stocked(data: &GameData, vendor: Vendor, difficulty: u8, item: &Item) -> bool {
    if difficulty != 0 && matches!(&item.code, b"hp4 " | b"hp5 " | b"mp4 " | b"mp5 ") {
        return true;
    }
    data.items().class_of(&item.code).and_then(|c| data.items().get(c)).is_some_and(|d| {
        d.spawnable && d.perm_store_item && d.vendors.get(vendor.slot).is_some_and(|v| v.max != 0 || v.magic_max != 0)
    })
}

/// Whether a vendor puts an item it buys into its stock (`0x00579510`): not a `Cracked` item, a
/// broken one, an ear, a personalised or ethereal item, or one it always has.
#[must_use]
pub fn takes_into_stock(data: &GameData, vendor: Vendor, difficulty: u8, item: &Item) -> bool {
    let broken = item.max_durability > 0 && item.durability == 0;
    let ear = item.flags & flags::EAR != 0;
    !(cracked(data, item) || broken || ear || item.flags & (flags::PERSONALIZED | flags::ETHEREAL) != 0 || always_stocked(data, vendor, difficulty, item))
}

/// Which side of a trade a price is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deal {
    /// The player buys (price mode 0).
    Buy,
    /// The player sells (price mode 1).
    Sell,
}

/// A stat on an item as the price reads it: id, parameter, value.
type PriceStat = (u16, u16, i32);

/// A stack's most (`0x006295B0`): `maxstack` plus the item's extra stack stat (254), at most 511.
fn max_quantity(def: &ItemDef, item: &Item) -> i32 {
    let extra: i32 = item.stats.iter().filter(|s| s.id == 254).map(|s| s.value).sum();
    (def.stack.1 + extra).min(511)
}

/// Whether an item has durability to repair (`0x00629930`, as `0x0062E660` asks it): identified,
/// not ethereal, durability in its row and not indestructible.
fn repairable(def: &ItemDef, item: &Item) -> bool {
    let indestructible: i32 = item.stats.iter().filter(|s| s.id == 152).map(|s| s.value).sum();
    item.identified() && item.flags & flags::ETHEREAL == 0 && !def.no_durability && def.durability != 0 && indestructible < 1
}

/// The stats the price reads (`0x00625C30`): the base stats item creation sets (`0x00557AB0`) —
/// armour's defence, block and speed, a weapon's damage and speed, durability and a stack's
/// quantity — and the item's own, summed by stat and parameter. A stat that comes to 0 is not kept.
fn price_stats(data: &GameData, class: i32, def: &ItemDef, item: &Item) -> Vec<PriceStat> {
    let items = data.items();
    let mut base: Vec<(u16, i32)> = Vec::new();
    if items.is(class, "armo") {
        base.extend([(20, def.block), (67, -def.speed), (72, item.durability), (73, item.max_durability), (31, item.defense)]);
    } else if items.is(class, "weap") {
        if def.stackable {
            base.push((70, i32::from(item.quantity)));
        }
        base.extend([(72, item.durability), (73, item.max_durability), (22, def.damage.1), (21, def.damage.0), (23, def.two_hand_damage.0), (24, def.two_hand_damage.1)]);
        if def.missile_damage.1 != 0 {
            base.extend([(159, def.missile_damage.0), (160, def.missile_damage.1)]);
        }
        base.push((68, -def.speed));
    } else if def.stackable {
        base.push((70, i32::from(item.quantity)));
    }
    let mut summed: HashMap<(u16, u16), i32> = HashMap::new();
    let mut order = Vec::new();
    for (id, param, value) in base.into_iter().map(|(id, v)| (id, 0, v)).chain(item.stats.iter().map(|s| (s.id, s.param, s.value))) {
        if !summed.contains_key(&(id, param)) {
            order.push((id, param));
        }
        *summed.entry((id, param)).or_insert(0) += value;
    }
    order.into_iter().filter_map(|key| summed.get(&key).copied().filter(|&v| v != 0).map(|v| (key.0, key.1, v))).collect()
}

/// The three running prices: buying, selling, repairing.
#[derive(Debug, Clone, Copy, Default)]
struct Prices {
    buy: i32,
    sell: i32,
    repair: i32,
}

/// `x × mult / 1024` for a price term with gold `add`, the product formed first while `decider` is
/// under 65,536 (or the multiplier 0) and the division first above (`0x0062EFB0`'s affix terms).
fn affix_terms(p: Prices, mult: i32, add: i32) -> Prices {
    let term = |x: i32| {
        if p.buy < 0x10000 || mult == 0 {
            mult.wrapping_mul(x) / 1024 + add
        } else {
            (x / 1024).wrapping_mul(mult) + add
        }
    };
    Prices { buy: term(p.buy), sell: term(p.sell), repair: term(p.repair) }
}

fn add_to(total: &mut Prices, d: Prices) {
    total.buy = total.buy.wrapping_add(d.buy);
    total.sell = total.sell.wrapping_add(d.sell);
    total.repair = total.repair.wrapping_add(d.repair);
}

/// What the item's stats add (`0x00628E70`), each over `divisor`.
fn stat_costs(data: &GameData, stats: &[PriceStat], p: &mut Prices, divisor: i32) {
    let mut d = Prices::default();
    for &(id, param, value) in stats {
        let Some(cost) = data.item_stats().get(id) else { continue };
        let v = value >> cost.val_shift;
        match cost.encode {
            1 => {
                let Some(skill) = data.skills().get(i32::from(param)) else { continue };
                let (m, a) = (skill.cost_mult, skill.cost_add);
                if p.buy.wrapping_mul(v) < 0x10000 || m == 0 {
                    d.buy += m.wrapping_mul(p.buy).wrapping_mul(v) / 1024 + a;
                    d.sell += p.sell.wrapping_mul(m).wrapping_mul(v) / 4096 + a;
                    d.repair += p.repair.wrapping_mul(m).wrapping_mul(v) / 1024 + a;
                } else {
                    d.buy += p.buy.wrapping_mul(v) / 1024 * m + a;
                    d.sell += p.sell.wrapping_mul(v) / 4096 * m + a;
                    d.repair += p.repair.wrapping_mul(v) / 1024 * m + a;
                }
            }
            2 | 3 => {
                let (skill, level) = (i32::from(param >> 6), i32::from(param & 0x3F));
                let Some(skill) = data.skills().get(skill) else { continue };
                let (m, a) = (skill.cost_mult, skill.cost_add);
                if p.buy.wrapping_mul(level) > 0xFFFF && m != 0 {
                    d.buy += p.buy.wrapping_mul(level) / 1024 * m + a;
                    d.sell += p.sell.wrapping_mul(level) / 4096 * m + a;
                    d.repair += p.repair.wrapping_mul(level) / 1024 * m + a;
                } else {
                    d.buy += m.wrapping_mul(p.buy).wrapping_mul(level) / 1024 + a;
                    d.sell += p.sell.wrapping_mul(m).wrapping_mul(level) / 4096 + a;
                    d.repair += p.repair.wrapping_mul(m).wrapping_mul(level) / 1024 + a;
                }
            }
            4 => {}
            _ => {
                let (m, a) = (cost.cost_multiply, cost.cost_add);
                if p.buy.wrapping_mul(v) < 0x10000 || m == 0 {
                    d.buy += m.wrapping_mul(p.buy).wrapping_mul(v) / 1024 + a;
                    d.sell += p.sell.wrapping_mul(m).wrapping_mul(v) / 1024 + a;
                    d.repair += p.repair.wrapping_mul(m).wrapping_mul(v) / 1024 + a;
                } else {
                    d.buy += p.buy.wrapping_mul(v) / 1024 * m + a;
                    d.sell += p.sell.wrapping_mul(v) / 1024 * m + a;
                    d.repair += p.repair.wrapping_mul(v) / 1024 * m + a;
                }
            }
        }
    }
    p.buy += d.buy / divisor;
    p.sell += d.sell / divisor;
    p.repair += d.repair / divisor;
}

/// What an item's staff skills add (`0x0062EDD0`), for a type with staff mods: each skill's
/// `cost mult`/`cost add` term, times twice its level less one, over `divisor`.
fn staff_skill_costs(data: &GameData, stats: &[PriceStat], p: &mut Prices, divisor: i32) {
    let mut d = Prices::default();
    for &(_, param, value) in stats.iter().filter(|s| s.0 == 107) {
        let Some(skill) = data.skills().get(i32::from(param)) else { continue };
        let (m, a) = (skill.cost_mult, skill.cost_add);
        let n = value * 2 - 1;
        if p.buy < 0x10000 || m == 0 {
            d.buy += (m.wrapping_mul(p.buy) / 1024 + a) * n;
            d.sell += (p.sell.wrapping_mul(m) / 4096 + a) * n;
            d.repair += (p.repair.wrapping_mul(m) / 1024 + a) * n;
        } else {
            d.buy += ((m / 1024) * p.buy + a) * n;
            d.sell += ((m / 4096) * p.sell + a) * n;
            d.repair += (p.repair * (m / 1024) + a) * n;
        }
    }
    p.buy += d.buy / divisor;
    p.sell += d.sell / divisor;
    p.repair += d.repair / divisor;
}

/// What a trade of `item` with vendor `npc` (a monster class) costs on `difficulty`
/// (`0x0062EFB0`, § [module](self)); `None` for an item or vendor the tables lack.
#[must_use]
pub fn price(data: &GameData, item: &Item, npc: i32, difficulty: u8, deal: Deal) -> Option<i32> {
    let items = data.items();
    let class = items.class_of(&item.code)?;
    let def = items.get(class)?;
    let kind = items.types().get(def.item_type)?;
    let trade = data.npc_trades().get(npc)?;
    if item.flags & flags::STARTER != 0 {
        return Some(1);
    }
    let quantity = i32::from(item.quantity).max(1);
    let most = max_quantity(def, item);
    let mut p = Prices::default();
    let mut divisor = 1;
    if item.flags & flags::EAR != 0 {
        p.buy = (i32::from(item.level) & 0xFF) * def.cost;
    } else if items.is(class, "body") {
        p.buy = def.cost;
    } else if items.is(class, "book") {
        p.buy = data.books().get(usize::from(item.book)).map_or(0, |b| b.cost_per_charge) * quantity + def.cost;
    } else if kind.quiver {
        p.buy = def.cost * quantity / 1024;
        p.repair = most * def.cost / 1024;
    } else {
        p.buy = def.cost;
        p.repair = def.cost;
        if def.stackable && most >= 2 {
            divisor = most;
        }
    }
    p.sell = p.buy;
    if items.is(class, "armo") && def.defense.1 - def.defense.0 != -1 && def.defense.1 != 0 {
        let scaled = def.cost * item.defense / def.defense.1;
        p = Prices { buy: scaled, sell: scaled, repair: scaled };
    }
    let stats = price_stats(data, class, def, item);
    let quality = item.quality.number();
    let magic_or_better = (4..=9).contains(&quality);
    let staff_mods = kind.staff_mods.is_some();
    if !magic_or_better && staff_mods {
        staff_skill_costs(data, &stats, &mut p, divisor);
    }
    if item.identified() {
        let affixes = data.affixes();
        let magic = |id: u16, prefix: bool| {
            let table = if prefix { &affixes.prefixes } else { &affixes.suffixes };
            usize::from(id).checked_sub(1).and_then(|i| table.get(i)).map(|a| (a.cost_multiply, a.cost_add))
        };
        let mut d = match usize::from(item.auto_affix).checked_sub(1).and_then(|i| affixes.auto.get(i)) {
            Some(auto) => affix_terms(p, auto.cost_multiply, auto.cost_add),
            None => Prices::default(),
        };
        let magic_costs = |d: &mut Prices, p: &mut Prices, prefix: u16, suffix: u16| {
            for (m, a) in [magic(prefix, true), magic(suffix, false)].into_iter().flatten() {
                add_to(d, affix_terms(*p, m, a));
            }
            stat_costs(data, &stats, p, divisor);
        };
        match &item.quality {
            Quality::Inferior(_) => d = Prices { buy: -(p.buy / 2), sell: -(p.sell / 2), repair: -(p.repair / 2) },
            Quality::Superior(_) | Quality::Tempered(..) => stat_costs(data, &stats, &mut p, divisor),
            Quality::Magic { prefix, suffix } => magic_costs(&mut d, &mut p, *prefix, *suffix),
            Quality::Set(id) => {
                if let Some(set) = affixes.set_items.get(usize::from(*id)) {
                    add_to(&mut d, affix_terms(p, set.cost.0, set.cost.1));
                }
            }
            Quality::Rare(ids) | Quality::Crafted(ids) => {
                for i in 0..3 {
                    for (m, a) in [magic(ids.prefixes[i], true), magic(ids.suffixes[i], false)].into_iter().flatten() {
                        add_to(&mut d, affix_terms(p, m, a));
                    }
                }
                stat_costs(data, &stats, &mut p, divisor);
            }
            Quality::Unique(id) => match affixes.uniques.get(usize::from(*id)) {
                Some(unique) => add_to(&mut d, affix_terms(p, unique.cost.0, unique.cost.1)),
                None => magic_costs(&mut d, &mut p, 0, 0),
            },
            Quality::Normal => {}
        }
        p.buy += d.buy / divisor;
        p.sell += d.sell / divisor;
        p.repair += d.repair / divisor;
        if magic_or_better && staff_mods {
            staff_skill_costs(data, &stats, &mut p, divisor);
        }
    }
    if item.flags & flags::ETHEREAL != 0 {
        p.sell /= 4;
    }
    if kind.class.is_some() {
        p.sell /= 4;
    }
    if deal == Deal::Sell && item.flags & flags::ETHEREAL != 0 && !def.no_durability && def.durability != 0 && item.max_durability != 0 && item.durability < 1 {
        p.sell = 0;
    }
    let scale = |x: i32, mult: i32| if x < 0x10000 || mult == 0 { mult.wrapping_mul(x) / 1024 } else { (x / 1024).wrapping_mul(mult) };
    let mut buy = scale(p.buy, trade.sell_mult);
    let mut sell = scale(p.sell, trade.buy_mult);
    let repair = scale(p.repair, trade.repair_mult);
    if !items.is(class, "book") && !kind.quiver {
        buy = buy.wrapping_mul(quantity);
        if !def.stackable || !repairable(def, item) {
            sell = sell.wrapping_mul(quantity);
        } else if quantity < most && !item.stats.iter().any(|s| s.id == 253 && s.value != 0) {
            sell = most.wrapping_mul(sell) - repair.wrapping_mul(most - quantity);
        } else {
            sell = most.wrapping_mul(sell);
        }
    }
    let sell = sell.min(trade.max_buy[usize::from(difficulty.min(2))]);
    Some(match deal {
        Deal::Buy => buy.max(1),
        Deal::Sell => sell.max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::item_bits::{ItemStat, Location};
    use d2_data::items::{code, Items};
    use d2_data::trade::{books_from_table, NpcTrades};
    use d2_formats::excel::Table;

    /// Akara, Charsi and Gheed's rows; a short staff Akara stocks (magic from store level 1), a
    /// skull cap, a buckler Charsi stocks, arrows and healing potions always in stock, and a
    /// Lord of Destruction axe.
    fn rules() -> GameData {
        let mut data = GameData::from_tables(
            &Table::parse(
                b"class\tstr\tdex\tint\tvit\tstamina\thpadd\r\n\
                  Amazon\t20\t25\t15\t20\t84\t30\r\nSorceress\t10\t25\t35\t10\t74\t30\r\nNecromancer\t15\t25\t25\t15\t79\t30\r\n\
                  Paladin\t25\t20\t15\t25\t89\t30\r\nBarbarian\t30\t20\t10\t25\t92\t30\r\nDruid\t15\t20\t20\t25\t84\t30\r\nAssassin\t20\t20\t25\t20\t95\t30\r\n",
            ),
            &Table::parse(b"Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\r\n0\t0\t0\t0\t0\t0\t0\t0\r\n1\t500\t500\t500\t500\t500\t500\t500\r\n"),
        )
        .unwrap();
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\tQuiver\tStaffMods\tClass\tStorePage\tNormal\tMagic\tRare\r\n\
              Weapon\tweap\t\t\t\t\t\tweap\t\t\t1\r\n\
              Staff\tstaf\tweap\t\t\tsor\t\tweap\t\t\t1\r\n\
              Axe\taxe\tweap\t\t\t\t\tweap\t\t\t1\r\n\
              Any Armor\tarmo\t\t\t\t\t\tarmo\t\t\t1\r\n\
              Helm\thelm\tarmo\t\t\t\t\tarmo\t\t\t1\r\n\
              Shield\tshie\tarmo\t\t\t\t\tarmo\t\t\t1\r\n\
              Misc\tmisc\t\t\t\t\t\tmisc\t\t\t\r\n\
              Bow Quiver\tbowq\tmisc\t\tbow\t\t\tmisc\t\t\t\r\n\
              Potion\tpoti\tmisc\t\t\t\t\tmisc\t1\t\t\r\n",
        );
        let weapons = Table::parse(
            b"name\tcode\ttype\tlevel\tversion\tspawnable\tcost\tbitfield1\tinvwidth\tinvheight\tdurability\tmindam\tmaxdam\t2handmindam\t2handmaxdam\tspeed\tAkaraMin\tAkaraMax\tAkaraMagicMin\tAkaraMagicMax\tAkaraMagicLvl\tCharsiMin\tCharsiMax\r\n\
              Short Staff\tsst\tstaf\t1\t0\t1\t168\t1\t1\t3\t20\t\t\t1\t5\t10\t5\t9\t5\t9\t1\t\t\r\n\
              Cleaver\t9xx\taxe\t1\t100\t1\t300\t1\t2\t3\t24\t4\t11\t\t\t0\t\t\t\t\t\t1\t1\r\n",
        );
        let armor = Table::parse(
            b"name\tcode\ttype\tlevel\tversion\tspawnable\tcost\tbitfield1\tinvwidth\tinvheight\tdurability\tminac\tmaxac\tblock\tCharsiMin\tCharsiMax\tCharsiMagicMin\tCharsiMagicMax\tCharsiMagicLvl\r\n\
              Cap\tcap\thelm\t1\t0\t1\t64\t1\t2\t2\t12\t3\t5\t0\t1\t1\t1\t1\t5\r\n\
              Buckler\tbuc\tshie\t1\t0\t1\t68\t1\t2\t2\t12\t4\t6\t30\t1\t1\t1\t1\t3\r\n",
        );
        let misc = Table::parse(
            b"name\tcode\ttype\tlevel\tversion\tspawnable\tcost\tcompactsave\tstackable\tminstack\tmaxstack\tinvwidth\tinvheight\tPermStoreItem\tAkaraMax\tCharsiMax\r\n\
              Arrows\taqv\tbowq\t0\t0\t1\t256\t0\t1\t250\t350\t1\t3\t1\t\t5\r\n\
              Minor Healing Potion\thp1\tpoti\t1\t0\t1\t30\t1\t0\t\t\t1\t1\t1\t1\t\r\n",
        );
        data.set_items(Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap(), Vec::new());
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\r\ngheed\t147\tgheed\r\nakara\t148\takara\r\ncharsi\t154\tcharsi\r\n");
        let monsters = d2_data::monsters::Monsters::from_tables(&monstats, &Table::parse(b"Id\r\n")).unwrap();
        let npc = Table::parse(
            b"npc\tbuy mult\tsell mult\trep mult\tmax buy\tmax buy (N)\tmax buy (H)\r\n\
              gheed\t512\t1088\t128\t5000\t30000\t35000\r\n\
              charsi\t512\t960\t128\t5000\t30000\t35000\r\n\
              akara\t512\t1024\t128\t5000\t30000\t35000\r\n",
        );
        data.set_trade_tables(NpcTrades::from_table(&npc, &monsters), books_from_table(&Table::parse(b"Name\tCostPerCharge\r\n")));
        let stats = Table::parse(
            b"Stat\tID\tAdd\tMultiply\tValShift\tEncode\tSave Bits\r\n\
              toblock\t20\t89\t204\t\t\t6\r\nmindamage\t21\t122\t25\t\t\t6\r\nmaxdamage\t22\t94\t16\t\t\t7\r\n\
              secondary_mindamage\t23\t97\t15\t\t\t6\r\nsecondary_maxdamage\t24\t85\t11\t\t\t7\r\narmorclass\t31\t17\t10\t\t\t11\r\n\
              maxdurability\t73\t9\t4\t\t\t8\r\nmaxhp\t7\t56\t20\t8\t\t9\r\n",
        );
        data.set_item_rules(d2_data::item_stats::ItemStats::from_table(&stats).unwrap(), d2_data::item_stats::ItemRatios::default());
        data
    }

    const CLASSIC: Making = Making { version: 1, difficulty: 0, ladder: false, magic_find: 0 };

    fn plain(data: &GameData, c: &str) -> Item {
        let class = data.items().class_of(&code(c)).unwrap();
        let def = data.items().get(class).unwrap();
        let mut item = Item::new(code(c), 1, 5, Location::Ground { x: 0, y: 0 });
        item.max_durability = def.durability;
        item.durability = def.durability;
        item
    }

    #[test]
    fn stores_are_capped_by_act_in_normal_only() {
        assert_eq!((store_level(0, 0, 1), store_level(0, 0, 30), store_level(0, 4, 50), store_level(1, 0, 30)), (6, 12, 45, 35));
        assert_eq!(vendor(154), Some(Vendor { class: 154, slot: 2, act: 0 }));
        assert!(vendor(146).is_none(), "Cain does not trade");
    }

    #[test]
    fn a_vendor_stocks_its_columns_on_their_pages() {
        let data = rules();
        for seed in 1..40 {
            let charsi = stock(&data, vendor(154).unwrap(), CLASSIC, 1, seed);
            let codes: Vec<&[u8; 4]> = charsi.iter().map(|s| &s.item.code).collect();
            assert!(!codes.contains(&b"9xx "), "no expansion item in a classic game");
            assert!(!codes.contains(&b"sst "), "Akara's staff is not Charsi's");
            assert_eq!(codes.iter().filter(|c| **c == b"aqv ").count(), 1, "arrows always, once");
            let arrows = charsi.iter().find(|s| &s.item.code == b"aqv ").unwrap();
            assert_eq!((arrows.page, arrows.item.quantity), (3, 350), "misc page, a full stack");
            for s in &charsi {
                assert!(s.item.identified() && s.item.flags & flags::ETHEREAL == 0);
                if &s.item.code == b"cap " || &s.item.code == b"buc " {
                    assert_eq!(s.page, 0);
                    assert!(matches!(s.item.quality.number(), 1..=4));
                    assert_eq!(s.item.durability, s.item.max_durability, "whole");
                }
            }
            let caps = charsi.iter().filter(|s| &s.item.code == b"cap ").count();
            assert!((1..=1 + 1).contains(&caps), "one normal, up to one magic at store level 6: {caps}");
            let cells: Vec<(u8, u8, u8)> = charsi.iter().map(|s| (s.page, s.col, s.row)).collect();
            let mut unique = cells.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(cells.len(), unique.len(), "no two items at one spot");
        }
        let akara = stock(&data, vendor(148).unwrap(), CLASSIC, 1, 7);
        let staves: Vec<&StockItem> = akara.iter().filter(|s| &s.item.code == b"sst ").collect();
        assert!(staves.len() >= 10, "5–9 normal and 5–9 magic: {}", staves.len());
        assert!(staves.iter().all(|s| s.page == 1 || s.page == 2), "the weapons pages");
        assert!(akara.iter().any(|s| &s.item.code == b"hp1 "));
    }

    #[test]
    fn prices_follow_cost_quality_defence_and_the_vendors_multipliers() {
        let data = rules();
        let charsi = 154;
        let cap = plain(&data, "cap");
        let mut worn = cap.clone();
        worn.defense = 5;
        // Defence 5 of 5: the whole cost, 64; Charsi sells at 960/1024 and buys at half.
        assert_eq!(price(&data, &worn, charsi, 0, Deal::Buy), Some(64 * 960 / 1024));
        assert_eq!(price(&data, &worn, charsi, 0, Deal::Sell), Some(32));
        worn.defense = 3;
        assert_eq!(price(&data, &worn, charsi, 0, Deal::Buy), Some((64 * 3 / 5) * 960 / 1024), "scaled by defence");
        let mut low = worn.clone();
        low.quality = Quality::Inferior(0);
        let base = 64 * 3 / 5;
        assert_eq!(price(&data, &low, charsi, 0, Deal::Buy), Some((base - base / 2) * 960 / 1024), "low quality: half");
        let mut superior = worn.clone();
        superior.quality = Quality::Superior(0);
        // armorclass 3: 10 × 38 × 3 / 1024 + 17; maxdurability 12: 4 × 38 × 12 / 1024 + 9.
        let with_stats = base + (10 * base * 3 / 1024 + 17) + (4 * base * 12 / 1024 + 9);
        assert_eq!(price(&data, &superior, charsi, 0, Deal::Buy), Some(with_stats * 960 / 1024));
        let mut life = superior.clone();
        life.quality = Quality::Magic { prefix: 0, suffix: 0 };
        life.stats.push(ItemStat { id: 7, param: 0, value: 10 << 8 });
        let with_life = with_stats + (20 * base * 10 / 1024 + 56);
        assert_eq!(price(&data, &life, charsi, 0, Deal::Buy), Some(with_life * 960 / 1024), "a stat's value unshifted");
        let mut hidden = life.clone();
        hidden.flags &= !flags::IDENTIFIED;
        assert_eq!(price(&data, &hidden, charsi, 0, Deal::Buy), Some(base * 960 / 1024), "unidentified: the base");
        let mut starter = cap;
        starter.flags |= flags::STARTER;
        assert_eq!(price(&data, &starter, charsi, 0, Deal::Sell), Some(1));
        let mut arrows = plain(&data, "aqv");
        arrows.quantity = 350;
        assert_eq!(price(&data, &arrows, charsi, 0, Deal::Buy), Some((256 * 350 / 1024) * 960 / 1024), "per piece, no stack multiple");
        let mut rich = superior.clone();
        rich.stats.push(ItemStat { id: 7, param: 0, value: 50_000 << 8 });
        rich.quality = Quality::Magic { prefix: 0, suffix: 0 };
        assert_eq!(price(&data, &rich, charsi, 0, Deal::Sell), Some(5000), "no more than max buy");
        assert_eq!(price(&data, &rich, charsi, 1, Deal::Sell).map(|p| p > 5000), Some(true), "nightmare's max buy");
        assert_eq!(price(&data, &worn, 999, 0, Deal::Buy), None, "no such vendor");
    }

    /// Every vendor stocks from the install's tables at several levels and difficulties, every
    /// item written back whole; Akara's potions and scrolls and Charsi's arrows cost what the
    /// columns give.
    #[test]
    fn with_a_real_install_every_vendor_stocks_and_prices() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else { return };
        let data = GameData::load(dir).unwrap();
        for v in VENDORS {
            for (making, level) in [(CLASSIC, 1), (Making { version: 101, difficulty: 0, ladder: false, magic_find: 0 }, 30), (Making { version: 101, difficulty: 2, ladder: false, magic_find: 0 }, 80)] {
                let items = stock(&data, v, making, level, v.class.into());
                assert!(!items.is_empty(), "vendor {} at level {level}", v.class);
                for s in &items {
                    let bytes = d2_data::item_bits::write(&s.item, data.items(), data.item_stats(), d2_data::item_bits::Target::Network);
                    d2_data::item_bits::read(&bytes, data.items(), data.item_stats(), d2_data::item_bits::Target::Network).unwrap();
                    let buy = price(&data, &s.item, i32::from(v.class), making.difficulty, Deal::Buy).unwrap();
                    let sell = price(&data, &s.item, i32::from(v.class), making.difficulty, Deal::Sell).unwrap();
                    assert!(buy >= 1 && sell >= 1 && sell <= buy.max(1), "{} buys {buy} sells {sell}", d2_data::items::code_str(&s.item.code));
                }
            }
        }
        let akara = stock(&data, vendor(148).unwrap(), CLASSIC, 1, 1);
        let cost = |c: &[u8; 4], npc: i32| akara.iter().chain(stock(&data, vendor(154).unwrap(), CLASSIC, 1, 1).iter()).find(|s| &s.item.code == c).map(|s| price(&data, &s.item, npc, 0, Deal::Buy));
        assert_eq!(cost(b"hp1 ", 148), Some(Some(30)));
        assert_eq!(cost(b"tsc ", 148), Some(Some(100)));
        assert_eq!(cost(b"isc ", 148), Some(Some(80)));
        assert_eq!(cost(b"aqv ", 154), Some(Some(256 * 350 / 1024 * 960 / 1024)), "a full quiver, per piece, at Charsi's 960");
    }

    #[test]
    fn sold_items_join_the_stock_unless_the_vendor_has_them_always() {
        let data = rules();
        let charsi = vendor(154).unwrap();
        let cap = plain(&data, "cap");
        assert!(takes_into_stock(&data, charsi, 0, &cap));
        let mut ethereal = cap.clone();
        ethereal.flags |= flags::ETHEREAL;
        assert!(!takes_into_stock(&data, charsi, 0, &ethereal));
        let mut broken = cap;
        broken.durability = 0;
        assert!(!takes_into_stock(&data, charsi, 0, &broken));
        assert!(!takes_into_stock(&data, charsi, 0, &plain(&data, "aqv")), "arrows are always there");
        assert!(always_stocked(&data, charsi, 1, &Item::new(code("hp5"), 1, 1, Location::Ground { x: 0, y: 0 })));
    }
}
