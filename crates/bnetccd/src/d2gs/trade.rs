//! Trading with vendors ([`d2_game::store`]): the trade window opening, buying and selling.
//!
//! Picking Trade from a vendor's menu (`0x38` action 1, `0x00579D60` → `0x00579430`) makes the
//! vendor's stock the first time, and again for a player opening it alone once four minutes have
//! passed (`0x00537230`); every stock item is then sent into the window (`0x9C` action `0x0B`).
//! Buying (`0x32`, `0x00577830`) copies the stock item to the player — into a free belt slot when it
//! goes in the belt, else the inventory — and takes it out of the stock unless the vendor always
//! has it; buying with the fill flag keeps buying such an item while the belt takes it. Selling
//! (`0x33`, `0x00579510`) takes the item, pays for it (what the purse cannot hold falls at the
//! player's feet, `0x0055B060`) and puts a mended copy into the stock. Each ends with `0x2A`.
//!
//! Not ported: gambling, repairs, hiring, identifying at Cain, scrolls bought into a tome, arrows
//! bought onto a worn quiver, and paying from the stash.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use bnetcc_proto::d2gs::{self, item_action, transaction};
use d2_data::item_bits::{flags, Location};
use d2_data::GameData;
use d2_game::inventory::{free_spot, Held, Place};
use d2_game::loot::Making;
use d2_game::population::{unit_type, Spawned};
use d2_game::store::{self, Deal, StockItem, Vendor, PAGE_HEIGHT, PAGE_WIDTH};
use tracing::info;

use super::{drop_gold_at, items, refresh_gear, Game, GameServer};

/// A vendor's stock in a game.
#[derive(Debug, Clone)]
pub(super) struct Store {
    vendor: Vendor,
    made: Instant,
    /// The stock, each item with its guid.
    pub(super) items: Vec<(u32, StockItem)>,
    /// Players with its trade window open.
    pub(super) open: HashSet<String>,
}

/// `0x9C` action `0x0B` for a stock item, at its page and cell.
fn stock_packet(rules: &GameData, guid: u32, s: &StockItem) -> Vec<u8> {
    let mut item = s.item.clone();
    item.location = Location::Stored { col: s.col, row: s.row, page: s.page };
    items::world(rules, item_action::ADD_TO_STORE, guid, &item)
}

/// A spot on a store page for an item of `class`, a full weapons page spilling onto the next.
fn store_spot(rules: &GameData, store: &Store, class: i32) -> Option<(u8, u8, u8)> {
    let items = rules.items();
    let def = items.get(class)?;
    let mut page = items.types().get(def.item_type)?.store_page?;
    loop {
        let taken = |c: u8, r: u8| {
            store.items.iter().any(|(_, s)| {
                let (w, h) = items.get(s.class).map_or((1, 1), |d| d.inv_size);
                s.page == page && (s.col..s.col + w).contains(&c) && (s.row..s.row + h).contains(&r)
            })
        };
        if let Some((col, row)) = free_spot(PAGE_WIDTH, PAGE_HEIGHT, def.inv_size, &taken) {
            return Some((page, col, row));
        }
        if page != 1 {
            return None;
        }
        page = 2;
    }
}

/// Every player with the store's window open but `except`, told `packet`.
fn tell_viewers(game: &mut Game, npc: u32, except: &str, packet: &[u8]) {
    let Some(store) = game.stores.get(&npc) else { return };
    for player in store.open.iter().filter(|p| p.as_str() != except) {
        game.outgoing.entry(player.clone()).or_default().push(packet.to_vec());
    }
}

/// The engine's mode number of where a held item is: 0 a grid, 1 worn, 2 the belt, 4 the cursor.
fn item_mode(place: Place) -> u16 {
    match place {
        Place::Grid { .. } => 0,
        Place::Body(_) => 1,
        Place::Belt(_) => 2,
        Place::Cursor => 4,
    }
}

impl GameServer {
    /// A player picks from an NPC's menu (`0x38`): Trade opens the trade window of a vendor;
    /// the other entries are not ported.
    pub(super) fn npc_action(&self, game_id: u16, name: &str, action: u32, npc: u32) -> Vec<Vec<u8>> {
        if action != 1 {
            return Vec::new();
        }
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some((_, &Spawned::Monster { class, .. })) = game.population.as_ref().and_then(|p| p.find(unit_type::MONSTER, npc)) else {
            return Vec::new();
        };
        let Some(vendor) = store::vendor(class) else { return Vec::new() };
        let Some(level) = game.battle.player_level(name) else { return Vec::new() };
        let mut replies = Vec::new();
        let stale = game.stores.get(&npc).map_or(true, |s| s.open.is_empty() && s.made.elapsed() >= Duration::from_millis(store::RESTOCK_MS));
        if stale {
            let making = Making { version: game.item_version, difficulty: game.difficulty, ladder: game.ladder, magic_find: 0 };
            let stock = store::stock(rules, vendor, making, level, game.item_seeds.roll());
            let open = game.stores.remove(&npc).map(|old| {
                for (guid, _) in &old.items {
                    let gone = d2gs::remove_unit(unit_type::ITEM, *guid);
                    for player in &game.connected {
                        game.outgoing.entry(player.clone()).or_default().push(gone.clone());
                    }
                }
                old.open
            });
            let Some(population) = game.population.as_mut() else { return Vec::new() };
            let items = stock.into_iter().map(|s| (population.next_guid(unit_type::ITEM), s)).collect::<Vec<_>>();
            info!(game_id, player = name, npc = class, items = items.len(), level, "vendor stocked");
            game.stores.insert(npc, Store { vendor, made: Instant::now(), items, open: open.unwrap_or_default() });
        }
        let Some(store) = game.stores.get_mut(&npc) else { return replies };
        store.open.insert(name.to_string());
        replies.extend(store.items.iter().map(|(guid, s)| stock_packet(rules, *guid, s)));
        replies
    }

    /// A player closes an NPC's menu or window (`0x30`).
    pub(super) fn npc_cancel(&self, game_id: u16, name: &str, npc: u32) {
        if let Some(store) = self.lock().by_id.get_mut(&game_id).and_then(|g| g.stores.get_mut(&npc)) {
            store.open.remove(name);
        }
    }

    /// A player buys stock item `guid` from vendor `npc` (`0x32`; `how` its flags word, bit 31 to
    /// fill the belt, low word 0 a trade).
    pub(super) fn buy(&self, game_id: u16, name: &str, npc: u32, guid: u32, how: u32) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let gold = game.battle.player_gold(name).unwrap_or(0);
        let refuse = |result: u8, gold: u32| vec![d2gs::npc_transaction(transaction::REFUSED, result, u32::MAX, gold)];
        let Some(store) = game.stores.get(&npc).filter(|s| s.open.contains(name)) else { return refuse(transaction::CANNOT, gold) };
        if how & 0xFFFF != 0 {
            return refuse(transaction::CANNOT, gold);
        }
        let Some((_, stock)) = store.items.iter().find(|(g, _)| *g == guid).cloned() else {
            return vec![d2gs::npc_transaction(transaction::REFUSED, transaction::NO_ITEM, guid, gold)];
        };
        let vendor = store.vendor;
        let Some(price) = store::price(rules, &stock.item, i32::from(vendor.class), game.difficulty, Deal::Buy).and_then(|p| u32::try_from(p).ok()) else {
            return refuse(transaction::CANNOT, gold);
        };
        if gold < price {
            return refuse(transaction::NO_GOLD, gold);
        }
        let Some(def) = rules.items().get(stock.class) else { return refuse(transaction::CANNOT, gold) };
        let always = store::always_stocked(rules, vendor, game.difficulty, &stock.item);
        let beltable = rules.items().beltable(stock.class) && def.inv_size == (1, 1);
        let Some(carried) = game.carried.get(name).filter(|c| c.complete) else { return refuse(transaction::CANNOT, gold) };
        if carried.inventory.at(Place::Cursor).is_some() {
            return refuse(transaction::NO_ITEM, gold);
        }
        let mut fill = how & 0x8000_0000 != 0 && always && beltable && carried.inventory.free_belt_slot().is_some();
        let mut replies = Vec::new();
        let mut bought = 0;
        loop {
            if bought > 0 && !fill {
                break;
            }
            let Some(gold) = game.battle.player_gold(name) else { break };
            if gold < price {
                replies.extend(refuse(transaction::NO_GOLD, gold));
                break;
            }
            let Some(carried) = game.carried.get_mut(name) else { break };
            let belt = if beltable && (def.auto_belt || carried.inventory.free_belt_slot().is_some()) { carried.inventory.free_belt_slot() } else { None };
            if beltable && belt.is_none() {
                fill = false;
            }
            let place = belt.map(Place::Belt).or_else(|| carried.inventory.grid_spot(def.inv_size).map(|(col, row)| Place::Grid { col, row }));
            let Some(place) = place else {
                if bought == 0 {
                    replies.extend(refuse(transaction::NO_ROOM, gold));
                }
                break;
            };
            let Some(population) = game.population.as_mut() else { break };
            let copy = population.next_guid(unit_type::ITEM);
            let held = Held { guid: copy, class: stock.class, size: def.inv_size, place, item: stock.item.clone() };
            let Some(total) = game.battle.pay_gold(name, price) else { break };
            let packet = items::held_packet(rules, &held, false);
            if let Some(carried) = game.carried.get_mut(name) {
                carried.inventory.insert(held);
            }
            bought += 1;
            info!(game_id, player = name, npc = vendor.class, item = %d2_data::items::code_str(&stock.item.code), price, gold = total, "bought");
            replies.push(d2gs::npc_transaction(transaction::BOUGHT, transaction::OK, copy, total));
            replies.push(d2gs::gold_update(gold, total));
            if !always {
                if let Some(store) = game.stores.get_mut(&npc) {
                    store.items.retain(|(g, _)| *g != guid);
                }
                let gone = items::world(rules, item_action::REMOVE_FROM_STORE, guid, &stock.item);
                tell_viewers(game, npc, name, &gone);
                replies.push(gone);
            }
            replies.extend(packet);
        }
        if bought > 0 {
            refresh_gear(rules, game, name);
        }
        replies
    }

    /// A player sells item `guid` to vendor `npc` (`0x33`; `mode` where the client says the item
    /// is: 0 the inventory, 2 the belt, 4 the cursor).
    pub(super) fn sell(&self, game_id: u16, name: &str, npc: u32, guid: u32, mode: u16) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let gold = game.battle.player_gold(name).unwrap_or(0);
        let refuse = |result: u8| vec![d2gs::npc_transaction(transaction::REFUSED, result, u32::MAX, gold)];
        let Some(store) = game.stores.get(&npc).filter(|s| s.open.contains(name)) else { return refuse(transaction::NOT_OPEN) };
        let vendor = store.vendor;
        let Some(held) = game.carried.get(name).filter(|c| c.complete).and_then(|c| c.inventory.get(guid)).cloned() else { return Vec::new() };
        let Some(def) = rules.items().get(held.class) else { return refuse(transaction::CANNOT) };
        if item_mode(held.place) != mode || matches!(held.place, Place::Body(_)) || held.item.flags & 0x1000 != 0 || def.quest {
            return refuse(transaction::CANNOT);
        }
        let Some(mut price) = store::price(rules, &held.item, i32::from(vendor.class), game.difficulty, Deal::Sell) else { return refuse(transaction::CANNOT) };
        let mut restocked = None;
        if store::takes_into_stock(rules, vendor, game.difficulty, &held.item) {
            let mut copy = held.item.clone();
            copy.flags &= !flags::USED;
            store::restock(rules, held.class, &mut copy);
            if let Some(mended) = store::price(rules, &copy, i32::from(vendor.class), game.difficulty, Deal::Sell) {
                price = price.min(mended);
            }
            if let Some((page, col, row)) = store_spot(rules, store, held.class) {
                restocked = Some(StockItem { class: held.class, page, col, row, item: copy });
            }
        }
        let price = u32::try_from(price).unwrap_or(0);
        let mut replies = match held.place {
            Place::Cursor => vec![d2gs::remove_unit(unit_type::ITEM, guid)],
            _ => items::held_packet(rules, &held, true).into_iter().collect(),
        };
        if let Some(carried) = game.carried.get_mut(name) {
            carried.inventory.remove(guid);
        }
        let (taken, total) = game.battle.pick_up_gold(name, price).unwrap_or((0, gold));
        info!(game_id, player = name, npc = vendor.class, item = %d2_data::items::code_str(&held.item.code), price, gold = total, "sold");
        replies.push(d2gs::npc_transaction(transaction::SOLD, transaction::SOLD_OK, guid, total));
        if taken > 0 {
            replies.push(d2gs::gold_update(total - taken, total));
        }
        if let Some((room, pile)) = drop_gold_at(rules, game, name, price - taken) {
            for (player, view) in &game.views {
                if player != name && view.contains(&room) {
                    game.outgoing.entry(player.clone()).or_default().push(pile.clone());
                }
            }
            replies.push(pile);
        }
        if let (Some(stock), Some(population)) = (restocked, game.population.as_mut()) {
            let copy = population.next_guid(unit_type::ITEM);
            let shown = stock_packet(rules, copy, &stock);
            if let Some(store) = game.stores.get_mut(&npc) {
                store.items.push((copy, stock));
            }
            tell_viewers(game, npc, name, &shown);
            replies.push(shown);
        }
        refresh_gear(rules, game, name);
        replies
    }
}
