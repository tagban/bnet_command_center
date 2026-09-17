//! Items in a game: the packets that show them, the rules for wearing them, and a player's
//! `.d2s` item list.
//!
//! Every item goes out as [`d2_data::item_bits`] writes it, inside `0x9C` or `0x9D`. A move the
//! client asks for is answered as the engine's per-frame item pass answers it (`0x005973F0`),
//! from the action flag the move set: lifted from a grid `0x9D` 5, put in a grid `0x9C` 4, worn
//! `0x9D` 6, taken off `0x9D` 8, swapped with a worn item `0x9D` 9 for each, swapped in a grid
//! `0x9C` `0x0D` for each, into the belt `0x9C` `0x0E`, out of it `0x9C` `0x0F`, swapped in it
//! `0x9C` `0x10` for each, picked up to the cursor `0x9C` 1. A lifted item keeps the fields of
//! where it was; a pair is sent lifted first.

use bnetcc_proto::d2gs::{self, item_action};
use d2_data::item_bits::{self, flags, Item, Location, Target};
use d2_data::GameData;
use d2_game::inventory::{Grid, Held, Inventory, Place, BODY_LOCATIONS, GRID_HEIGHT, GRID_WIDTH};

use super::PLAYER_GUID;

/// Item flag on a move whose packet carries the whole item for the client to build again
/// (`0x006280D0(item, 1, 1)`; the handlers at `0x004C2C80` and `0x004C2E90` test it). Sent, never
/// kept.
pub(super) const REBUILD: u32 = 0x1;
/// Item flag on the item worn by a body swap (`0x00560F00`; `0x004C3920` tests it).
pub(super) const SWAPPED_IN: u32 = 0x40;
/// Item flag on the item lifted by a body swap.
pub(super) const SWAPPED_OUT: u32 = 0x80;

/// The `0x9C` category byte of an item: its `component` (`0x00628660`).
pub(super) fn category(rules: &GameData, code: &[u8; 4]) -> u8 {
    rules.items().class_of(code).and_then(|c| rules.items().get(c)).map_or(16, |d| u8::try_from(d.component).unwrap_or(16))
}

fn bits(rules: &GameData, item: &Item) -> Vec<u8> {
    item_bits::write(item, rules.items(), rules.item_stats(), Target::Network)
}

/// `0x9C` for an item.
pub(super) fn world(rules: &GameData, action: u8, guid: u32, item: &Item) -> Vec<u8> {
    d2gs::item_world_bits(action, category(rules, &item.code), guid, &bits(rules, item))
}

/// `0x9D` for an item of the player's own.
pub(super) fn owned(rules: &GameData, action: u8, guid: u32, item: &Item) -> Vec<u8> {
    d2gs::item_owned_bits(action, category(rules, &item.code), guid, PLAYER_GUID, &bits(rules, item))
}

/// A held item going to its place (`used` false: `0x9C` 4 into the inventory, `0x9C` `0x0E`
/// into the belt, `0x9D` 6 worn — as a join or a pickup sends it) or used up out of it (`0x9C`
/// `0x0F` from the belt, `0x9D` 5 from the inventory, flagged used). `None` for a cursor item.
pub(super) fn held_packet(rules: &GameData, held: &Held, used: bool) -> Option<Vec<u8>> {
    let mut item = held.placed();
    if used {
        item.flags |= flags::USED;
    }
    Some(match (held.place, used) {
        (Place::Belt(_), false) => world(rules, item_action::PUT_IN_BELT, held.guid, &item),
        (Place::Belt(_), true) => world(rules, item_action::REMOVE_FROM_BELT, held.guid, &item),
        (Place::Grid { .. }, false) => world(rules, item_action::PUT_IN_CONTAINER, held.guid, &item),
        (Place::Grid { .. }, true) => owned(rules, item_action::REMOVE_FROM_CONTAINER, held.guid, &item),
        (Place::Body(_), _) => owned(rules, item_action::EQUIP, held.guid, &item),
        (Place::Cursor, _) => return None,
    })
}

/// A grid item lifted onto the cursor, remembering the page (panel) it came from — the stash
/// or the cube — so the client removes it from the right one.
pub(super) fn lifted_from(held: &Held, page: u8) -> Item {
    let mut item = held.item.clone();
    if let Place::Grid { col, row } = held.place {
        item.location = Location::Cursor { body: 0, col, row, page: Some(page) };
    }
    item
}

/// A held item on the cursor, keeping the fields of where it was lifted from.
pub(super) fn lifted(held: &Held, from: Place) -> Item {
    let mut item = held.item.clone();
    item.location = match from {
        Place::Grid { col, row } => Location::Cursor { body: 0, col, row, page: Some(0) },
        Place::Belt(slot) => Location::Cursor { body: 0, col: slot, row: 0, page: None },
        Place::Body(body) => Location::Cursor { body, col: 0, row: 0, page: None },
        Place::Cursor => Location::Cursor { body: 0, col: 0, row: 0, page: None },
    };
    item
}

/// A body location's `BodyLocs.txt` row by its code.
pub(super) fn body_location(code: &str) -> Option<u8> {
    ["head", "neck", "tors", "rarm", "larm", "rrin", "lrin", "belt", "feet", "glov"].iter().position(|c| *c == code).map(|i| i as u8 + 1)
}

/// The body locations an item's type is worn at.
fn body_locations(rules: &GameData, class: i32) -> Vec<u8> {
    let items = rules.items();
    items.get(class).and_then(|d| items.types().get(d.item_type)).map_or_else(Vec::new, |t| t.body_locations.iter().filter_map(|c| body_location(c)).collect())
}

/// The player an item's requirements are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Wearer {
    pub class: u8,
    pub level: u32,
    pub strength: i32,
    pub dexterity: i32,
}

/// Whether a player meets an item's requirements (`0x0062EAF0`): its type's class, the base
/// item's level (or its unique's or set item's), strength and dexterity (ten less of each for an
/// ethereal item). Attributes the player's items add are not counted yet.
pub(super) fn meets_requirements(rules: &GameData, class: i32, item: &Item, wearer: Wearer) -> bool {
    let items = rules.items();
    let Some(def) = items.get(class) else { return false };
    if items.types().get(def.item_type).and_then(|t| t.class).is_some_and(|c| c != wearer.class) {
        return false;
    }
    let affixes = rules.affixes();
    let named_level = match item.quality {
        item_bits::Quality::Unique(id) => affixes.uniques.get(usize::from(id)).map_or(0, |u| u.level_req),
        item_bits::Quality::Set(id) => affixes.set_items.get(usize::from(id)).map_or(0, |s| s.level_req),
        _ => 0,
    };
    let ethereal = if item.flags & flags::ETHEREAL != 0 { 10 } else { 0 };
    let (strength, dexterity) = def.requirements;
    wearer.level as i32 >= def.level_req.max(named_level) && wearer.strength >= strength - ethereal && wearer.dexterity >= dexterity - ethereal
}

/// Whether item `class` may be worn at `body` beside what is worn (`0x0063DE60`, as far as it is
/// read): at a body location its type names, by its type's class, and in a hand only beside
/// something it can share the hands with — nothing beside a two-handed weapon, one weapon and
/// one shield (two weapons for a Barbarian), and arrows or bolts only beside their bow or
/// crossbow.
pub(super) fn wearable_at(rules: &GameData, inventory: &Inventory, class: i32, body: u8, wearer_class: u8) -> bool {
    let items = rules.items();
    let Some(def) = items.get(class) else { return false };
    if !BODY_LOCATIONS.contains(&body) || !body_locations(rules, class).contains(&body) {
        return false;
    }
    if items.types().get(def.item_type).and_then(|t| t.class).is_some_and(|c| c != wearer_class) {
        return false;
    }
    if body != 4 && body != 5 {
        return true;
    }
    let Some(other) = inventory.at(Place::Body(9 - body)) else { return true };
    let Some(other_def) = items.get(other.class) else { return true };
    let ammo = |c: i32| if items.is(c, "bowq") { Some("bow") } else if items.is(c, "xboq") { Some("xbow") } else { None };
    match (ammo(class), ammo(other.class)) {
        (Some(launcher), None) => return items.is(other.class, launcher),
        (None, Some(launcher)) => return items.is(class, launcher),
        (Some(_), Some(_)) => return false,
        (None, None) => {}
    }
    if def.two_handed || other_def.two_handed {
        return false;
    }
    match (items.is(class, "weap"), items.is(other.class, "weap")) {
        (true, true) => wearer_class == 4,
        (false, false) => false,
        _ => true,
    }
}

/// Where a picked-up item is worn straight away (`0x0055D710`): an identified item whose
/// requirements the player meets, not a throwing potion, at the first of its type's body
/// locations that is free and suits it. `None` to carry it instead.
pub(super) fn auto_equip(rules: &GameData, inventory: &Inventory, class: i32, item: &Item, wearer: Wearer) -> Option<u8> {
    if !item.identified() || !meets_requirements(rules, class, item, wearer) || rules.items().is(class, "tpot") {
        return None;
    }
    body_locations(rules, class).into_iter().find(|&body| inventory.at(Place::Body(body)).is_none() && wearable_at(rules, inventory, class, body, wearer.class))
}

/// The item-location page the stash and the Horadric Cube keep their items on. A stored item's
/// page picks the panel a client files it under and the `.d2s` keeps it in; the backpack is 0.
pub(super) const PAGE_STASH: u8 = 4;
pub(super) const PAGE_CUBE: u8 = 3;

/// A player's items as its `.d2s` lists them: the backpack, then the stash and the cube — one
/// list, each item carrying the page that says where it lives. An item on the cursor is written
/// into a free backpack spot (left out when there is none).
pub(super) fn save_list(inventory: &Inventory, stash: &Grid, cube: &Grid) -> Vec<Item> {
    let mut out: Vec<Item> = inventory.items().iter().filter(|h| h.place != Place::Cursor).map(Held::placed).collect();
    if let Some(cursor) = inventory.at(Place::Cursor) {
        let spot = (0..GRID_HEIGHT).flat_map(|row| (0..GRID_WIDTH).map(move |col| (col, row))).find(|&(col, row)| {
            let mut probe = cursor.clone();
            probe.place = Place::Grid { col, row };
            inventory.fits(&probe)
        });
        if let Some((col, row)) = spot {
            let mut item = cursor.item.clone();
            item.location = Location::Stored { col, row, page: 0 };
            out.push(item);
        }
    }
    out.extend(stash.items().iter().map(|h| stash.stored(h)));
    out.extend(cube.items().iter().map(|h| cube.stored(h)));
    out
}

/// Which of a player's containers a saved item belongs to, and where in it.
pub(super) enum SavedIn {
    /// The backpack, belt or worn.
    Inventory(Place),
    /// The stash, at a cell.
    Stash(u8, u8),
    /// The Horadric Cube, at a cell.
    Cube(u8, u8),
}

/// Where a saved item is held. `None` for anywhere not modelled (a socket, the trade panel).
pub(super) fn saved_where(item: &Item) -> Option<SavedIn> {
    match item.location {
        Location::Stored { col, row, page: 0 } => Some(SavedIn::Inventory(Place::Grid { col, row })),
        Location::Stored { col, row, page: PAGE_STASH } => Some(SavedIn::Stash(col, row)),
        Location::Stored { col, row, page: PAGE_CUBE } => Some(SavedIn::Cube(col, row)),
        Location::Belt { slot } => Some(SavedIn::Inventory(Place::Belt(slot))),
        Location::Equipped { body } if BODY_LOCATIONS.contains(&body) => Some(SavedIn::Inventory(Place::Body(body))),
        _ => None,
    }
}

/// `0x9C` action 4 for an item put into a grid at its stored place — the stash or the cube on
/// its own page, as a join sends it.
pub(super) fn container_packet(rules: &GameData, guid: u32, item: &Item) -> Vec<u8> {
    world(rules, item_action::PUT_IN_CONTAINER, guid, item)
}
