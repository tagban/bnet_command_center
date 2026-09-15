//! A player's inventory grid and belt, and where the engine puts an item it picks up.
//!
//! Picking up (`0x00563560`, after gold, stacking and auto-equip, none of which a simple item
//! meets): an item whose type is beltable and whose `Misc.txt` `autobelt` is set goes to the first
//! free belt slot (`0x0063C790` → `0x0063C600`); anything else to the inventory spot
//! `0x0063B950` → `0x0063B850` finds; with no room it stays on the ground.
//!
//! The inventory is 10 × 4. The belt without a belt worn has one row of four slots (the engine
//! reads the box count from `Belts.txt`; belts are not worn yet, so it is fixed here). Belt slot
//! `n` is the item's grid column `n`, row 0 (`0x0063AFD0` with grid 1).
//!
//! Worn items sit at their body location (1 head … 10 gloves); at most one item is on the cursor.

use d2_data::item_bits::{Item, Location};

/// Inventory columns.
pub const GRID_WIDTH: u8 = 10;
/// Inventory rows.
pub const GRID_HEIGHT: u8 = 4;
/// Belt slots with no belt worn.
pub const BELT_SLOTS: u8 = 4;

/// Body locations (`BodyLocs.txt`): 1 head … 10 gloves.
pub const BODY_LOCATIONS: std::ops::RangeInclusive<u8> = 1..=10;

/// Where a held item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The inventory, top-left cell.
    Grid {
        /// Column, 0 at the left.
        col: u8,
        /// Row, 0 at the top.
        row: u8,
    },
    /// A belt slot.
    Belt(u8),
    /// Worn at a body location.
    Body(u8),
    /// On the cursor.
    Cursor,
}

/// An item a player holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// Its unit guid in the game.
    pub guid: u32,
    /// Its item class (index into the item tables).
    pub class: i32,
    /// Inventory cells, width × height.
    pub size: (u8, u8),
    /// Where it is.
    pub place: Place,
    /// The item itself: code, version, quality, stats.
    pub item: Item,
}

impl Held {
    fn covers(&self, col: u8, row: u8) -> bool {
        match self.place {
            Place::Grid { col: c, row: r } => (c..c + self.size.0).contains(&col) && (r..r + self.size.1).contains(&row),
            _ => false,
        }
    }

    /// Its code, space-padded.
    #[must_use]
    pub fn code(&self) -> [u8; 4] {
        self.item.code
    }

    /// The item with the location its place gives (a cursor item keeps no fields).
    #[must_use]
    pub fn placed(&self) -> Item {
        let mut item = self.item.clone();
        item.location = match self.place {
            Place::Grid { col, row } => Location::Stored { col, row, page: 0 },
            Place::Belt(slot) => Location::Belt { slot },
            Place::Body(body) => Location::Equipped { body },
            Place::Cursor => Location::Cursor { body: 0, col: 0, row: 0, page: None },
        };
        item
    }
}

/// A player's items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    items: Vec<Held>,
}

impl Inventory {
    /// Everything held, in the order it came.
    #[must_use]
    pub fn items(&self) -> &[Held] {
        &self.items
    }

    /// An item by guid.
    #[must_use]
    pub fn get(&self, guid: u32) -> Option<&Held> {
        self.items.iter().find(|h| h.guid == guid)
    }

    /// Take an item out.
    pub fn remove(&mut self, guid: u32) -> Option<Held> {
        let at = self.items.iter().position(|h| h.guid == guid)?;
        Some(self.items.remove(at))
    }

    /// The item at a place: the one worn at a body location, in a belt slot or on the cursor, or
    /// the one covering a grid cell.
    #[must_use]
    pub fn at(&self, place: Place) -> Option<&Held> {
        match place {
            Place::Grid { col, row } => self.items.iter().find(|h| h.covers(col, row)),
            _ => self.items.iter().find(|h| h.place == place),
        }
    }

    /// Whether `held` would go where it says: inside, and nothing there.
    #[must_use]
    pub fn fits(&self, held: &Held) -> bool {
        match held.place {
            Place::Grid { col, row } => {
                col + held.size.0 <= GRID_WIDTH && row + held.size.1 <= GRID_HEIGHT && self.area_free(col, row, held.size)
            }
            Place::Belt(slot) => slot < BELT_SLOTS && self.at(held.place).is_none(),
            Place::Body(body) => BODY_LOCATIONS.contains(&body) && self.at(held.place).is_none(),
            Place::Cursor => self.at(Place::Cursor).is_none(),
        }
    }

    /// Put an item where it says, if that place is free and inside; whether it went in.
    pub fn insert(&mut self, held: Held) -> bool {
        let fits = self.fits(&held) && self.get(held.guid).is_none();
        if fits {
            self.items.push(held);
        }
        fits
    }

    /// Move an item to `place` when it fits there; whether it moved.
    pub fn move_to(&mut self, guid: u32, place: Place) -> bool {
        let Some(mut held) = self.remove(guid) else { return false };
        let from = held.place;
        held.place = place;
        if self.fits(&held) {
            self.items.push(held);
            return true;
        }
        held.place = from;
        self.items.push(held);
        false
    }

    fn occupied(&self, col: u8, row: u8) -> bool {
        self.items.iter().any(|h| h.covers(col, row))
    }

    fn area_free(&self, col: u8, row: u8, (w, h): (u8, u8)) -> bool {
        (col..col + w).all(|c| (row..row + h).all(|r| !self.occupied(c, r)))
    }

    /// The first free belt slot (`0x0063C600`'s autobelt pass; with one row there is no column of
    /// matching potions to stack above).
    #[must_use]
    pub fn free_belt_slot(&self) -> Option<u8> {
        (0..BELT_SLOTS).find(|&slot| !self.items.iter().any(|h| h.place == Place::Belt(slot)))
    }

    /// Where the engine puts a `size` item in a player's inventory ([`free_spot`] on the 10 × 4
    /// grid).
    #[must_use]
    pub fn grid_spot(&self, size: (u8, u8)) -> Option<(u8, u8)> {
        free_spot(GRID_WIDTH, GRID_HEIGHT, size, &|c, r| self.occupied(c, r))
    }

    /// Where a picked-up item goes: a free belt slot for a beltable `autobelt` item, else the
    /// inventory; `None` when neither has room.
    #[must_use]
    pub fn place_for(&self, size: (u8, u8), belts: bool) -> Option<Place> {
        if belts && size == (1, 1) {
            if let Some(slot) = self.free_belt_slot() {
                return Some(Place::Belt(slot));
            }
        }
        self.grid_spot(size).map(|(col, row)| Place::Grid { col, row })
    }
}

/// How snugly a `size` item sits at (`col`, `row`) in a `width` × `height` grid (`0x0063B340`):
/// the occupied cells and grid edges along its four sides, or 255 when every side is closed.
fn snugness(width: u8, height: u8, occupied: &dyn Fn(u8, u8) -> bool, col: u8, row: u8, (w, h): (u8, u8)) -> u32 {
    let side = |cells: &mut dyn Iterator<Item = (u8, u8)>| cells.filter(|&(c, r)| occupied(c, r)).count() as u32;
    let left = if col == 0 { u32::from(h) } else { side(&mut (row..row + h).map(|r| (col - 1, r))) };
    let right = if col + w >= width { u32::from(h) } else { side(&mut (row..row + h).map(|r| (col + w, r))) };
    let top = if row == 0 { u32::from(w) } else { side(&mut (col..col + w).map(|c| (c, row - 1))) };
    let bottom = if row + h >= height { u32::from(w) } else { side(&mut (col..col + w).map(|c| (c, row + h))) };
    let score = left + right + top + bottom;
    if score >= 2 * (u32::from(w) + u32::from(h)) {
        255
    } else {
        score
    }
}

/// Where the engine puts a `size` item in a `width` × `height` grid whose taken cells `occupied`
/// names (`0x0063B850`) — a player's inventory, or a vendor's store page: of the free spots it
/// fits, the snuggest, the first found winning a tie and a fully enclosed one ending the search. A
/// one-row item is looked for column by column from the right, each from the bottom up
/// (`0x0063B490`); others row by row from the top, each from the left (`0x0063B620`; the engine's
/// own passes for 2 × 2 and three-row items, `0x0063B7D0` and `0x0063B790`, are not read).
#[must_use]
pub fn free_spot(width: u8, height: u8, size: (u8, u8), occupied: &dyn Fn(u8, u8) -> bool) -> Option<(u8, u8)> {
    let (w, h) = size;
    if w == 0 || h == 0 || w > width || h > height {
        return None;
    }
    let order: Vec<(u8, u8)> = if h == 1 {
        (0..width).rev().flat_map(|c| (0..height).rev().map(move |r| (c, r))).collect()
    } else {
        (0..height).flat_map(|r| (0..width).map(move |c| (c, r))).collect()
    };
    let free = |col: u8, row: u8| (col..col + w).all(|c| (row..row + h).all(|r| !occupied(c, r)));
    let mut best: Option<((u8, u8), u32)> = None;
    for (col, row) in order {
        if col + w > width || row + h > height || !free(col, row) {
            continue;
        }
        let score = snugness(width, height, occupied, col, row, size);
        if best.map_or(score > 0, |(_, b)| score > b) {
            best = Some(((col, row), score));
            if score == 255 {
                break;
            }
        }
    }
    best.map(|(spot, _)| spot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(guid: u32, size: (u8, u8), place: Place) -> Held {
        Held { guid, class: 0, size, place, item: Item::new(*b"hp1 ", 101, 1, Location::Cursor { body: 0, col: 0, row: 0, page: None }) }
    }

    #[test]
    fn small_items_fill_the_right_column_from_the_bottom() {
        let mut inv = Inventory::default();
        let mut spots = Vec::new();
        for guid in 1..=6 {
            let (col, row) = inv.grid_spot((1, 1)).unwrap();
            assert!(inv.insert(held(guid, (1, 1), Place::Grid { col, row })));
            spots.push((col, row));
        }
        assert_eq!(spots, [(9, 3), (9, 2), (9, 1), (9, 0), (8, 3), (8, 2)]);
    }

    #[test]
    fn a_snug_hole_wins_and_a_full_grid_has_no_spot() {
        let mut inv = Inventory::default();
        let mut guid = 0;
        for col in 0..GRID_WIDTH {
            for row in 0..GRID_HEIGHT {
                if (col, row) != (4, 1) && (col, row) != (0, 0) {
                    guid += 1;
                    inv.insert(held(guid, (1, 1), Place::Grid { col, row }));
                }
            }
        }
        assert_eq!(inv.grid_spot((1, 1)), Some((4, 1)), "enclosed on all sides ends the search");
        inv.insert(held(100, (1, 1), Place::Grid { col: 4, row: 1 }));
        assert_eq!(inv.grid_spot((1, 1)), Some((0, 0)));
        inv.insert(held(101, (1, 1), Place::Grid { col: 0, row: 0 }));
        assert_eq!(inv.grid_spot((1, 1)), None);
        assert_eq!(inv.place_for((1, 1), false), None);
        assert!(!inv.insert(held(102, (1, 1), Place::Grid { col: 3, row: 3 })), "taken");
    }

    #[test]
    fn autobelt_potions_take_the_belt_until_it_is_full() {
        let mut inv = Inventory::default();
        for guid in 1..=4 {
            let place = inv.place_for((1, 1), true).unwrap();
            assert_eq!(place, Place::Belt(guid as u8 - 1));
            assert!(inv.insert(held(guid, (1, 1), place)));
        }
        assert_eq!(inv.place_for((1, 1), true), Some(Place::Grid { col: 9, row: 3 }), "then the inventory");
        inv.remove(2);
        assert_eq!(inv.free_belt_slot(), Some(1));
        assert!(!inv.insert(held(9, (1, 1), Place::Belt(4))), "four slots without a belt");
    }

    #[test]
    fn items_move_between_the_grid_the_cursor_and_the_body() {
        let mut inv = Inventory::default();
        assert!(inv.insert(held(1, (2, 3), Place::Grid { col: 0, row: 0 })));
        assert_eq!(inv.at(Place::Grid { col: 1, row: 2 }).map(|h| h.guid), Some(1), "a cell it covers");
        assert!(inv.move_to(1, Place::Cursor));
        assert!(inv.at(Place::Grid { col: 1, row: 2 }).is_none());
        assert!(!inv.insert(held(2, (1, 1), Place::Cursor)), "one item on the cursor");
        assert!(inv.move_to(1, Place::Body(4)));
        assert!(!inv.move_to(1, Place::Body(11)), "no such body location");
        assert_eq!(inv.at(Place::Body(4)).map(|h| h.placed().location), Some(Location::Equipped { body: 4 }));
    }

    #[test]
    fn bigger_items_are_looked_for_row_by_row() {
        let inv = Inventory::default();
        assert_eq!(inv.grid_spot((2, 3)), Some((0, 0)), "the top-left corner is the first snuggest");
        assert_eq!(inv.grid_spot((11, 1)), None);
    }
}
