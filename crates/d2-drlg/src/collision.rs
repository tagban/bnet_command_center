//! Collision maps: what blocks walking, missiles and sight, per subtile, per room.
//!
//! The engine gives each room a map of `WorldSize × 5` subtiles when the room comes into play
//! (`DRLGROOM_AllocRoomCollisionGrid`, `0x0064C900`): every floor, wall and roof tile of the room
//! and of the rooms around it whose corner lies inside the room is stamped in
//! (`TileLibrary_AddCollision`, `0x0064C4C0`) — its DT1 subtile flags, plus bits its draw flags
//! carry. A cell of a plain room no floor tile covers is solid rock (`Blank.dt1`).
//!
//! Bits, with the engine's names (libd2 `packages/core/src/collision.zig` reads them off the D2R
//! debug build): `0x01` `COLBIT_WALL`, blocks walking; `0x02` `COLBIT_VISIBLE`, blocks sight;
//! `0x04` `COLBIT_MISSILE_BARRIER`; `0x08` `COLBIT_NOPLAYER`, blocks players only; `0x10`
//! `COLBIT_PRESET`, a preset tile, blocking nothing. Units add higher bits at run time.
//!
//! Ported from libd2 `packages/drlg/src/lib.zig` `buildLevelRoomColl` (MIT, © 2026 jaenster),
//! which reproduces the engine's maps byte for byte.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use d2_data::GameData;
use d2_formats::ds1::Ds1;

use crate::room_tiles::{stamp_tile, Layer, RoomTile, Seams};
use crate::tiles::{TileFiles, NO_COLLISION};
use crate::Coords;

/// Subtiles per tile edge.
pub const SUBTILES: i32 = 5;

/// `COLBIT_WALL`: blocks walking.
pub const WALL: u8 = 0x01;
/// `COLBIT_VISIBLE`: blocks sight.
pub const VISIBLE: u8 = 0x02;
/// `COLBIT_MISSILE_BARRIER`: blocks missiles.
pub const MISSILE_BARRIER: u8 = 0x04;
/// `COLBIT_NOPLAYER`: blocks players, not monsters.
pub const NOPLAYER: u8 = 0x08;
/// `COLBIT_PRESET`: a preset tile; blocks nothing.
pub const PRESET: u8 = 0x10;

/// DS1 map files read from the install, each parsed once.
#[derive(Debug, Default)]
pub struct MapFiles {
    maps: Mutex<HashMap<String, Option<Arc<Ds1>>>>,
}

impl MapFiles {
    /// A map by its path under `data\global\tiles`; `None` if missing or malformed.
    pub fn get(&self, data: &GameData, path: &str) -> Option<Arc<Ds1>> {
        let key = path.to_ascii_lowercase().replace('/', "\\");
        if let Some(found) = self.maps.lock().ok()?.get(&key) {
            return found.clone();
        }
        let member = format!("data\\global\\tiles\\{key}");
        let map = data.read_file(&member).ok().flatten().and_then(|b| Ds1::parse(&b).ok()).map(Arc::new);
        self.maps.lock().ok()?.insert(key, map.clone());
        map
    }
}

/// The files room tiles come from, shared by every game.
#[derive(Debug, Default)]
pub struct TileSources {
    /// DT1 tile libraries.
    pub tiles: TileFiles,
    /// DS1 maps.
    pub maps: MapFiles,
}

impl TileSources {
    /// Empty caches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// A room's tiles, ready to stamp.
#[derive(Debug, Clone)]
pub struct BuiltRoom {
    /// The room in world tiles.
    pub area: Coords,
    /// Its tiles.
    pub tiles: Vec<RoomTile>,
    /// A preset room: its uncovered cells are left as they are.
    pub preset: bool,
}

/// One room's collision map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomCollision {
    /// `Levels.txt` id.
    pub level: i32,
    /// The room in world tiles.
    pub area: Coords,
    /// Flags per subtile, row by row, `area.w × 5` wide.
    pub cells: Vec<u8>,
}

impl RoomCollision {
    /// The flags at a world subtile inside the room.
    #[must_use]
    pub fn at(&self, x: i32, y: i32) -> Option<u8> {
        let (lx, ly) = (x - self.area.x * SUBTILES, y - self.area.y * SUBTILES);
        let w = self.area.w * SUBTILES;
        if lx < 0 || ly < 0 || lx >= w || ly >= self.area.h * SUBTILES {
            return None;
        }
        self.cells.get((ly * w + lx) as usize).copied()
    }
}

/// Every room's collision map for a level whose rooms were built in list order with `seams`.
/// `void` is what an uncovered cell of a plain room gets: `0x05`, or `0x01` in the Arcane
/// Sanctuary.
#[must_use]
pub fn level_collision(level: i32, rooms: &[BuiltRoom], seams: &Seams, void: u8) -> Vec<RoomCollision> {
    rooms
        .iter()
        .map(|r| {
            let (gw, gh) = ((r.area.w * SUBTILES) as usize, (r.area.h * SUBTILES) as usize);
            let mut cells = vec![0u8; gw * gh];
            let mut covered = vec![false; (r.area.w * r.area.h) as usize];
            for a in rooms {
                // A room reaches this one if its tiles, one past its far edges, overlap it.
                if a.area.x + a.area.w < r.area.x || a.area.x >= r.area.x + r.area.w {
                    continue;
                }
                if a.area.y + a.area.h < r.area.y || a.area.y >= r.area.y + r.area.h {
                    continue;
                }
                for t in &a.tiles {
                    let (ox, oy) = (a.area.x + t.x, a.area.y + t.y);
                    if ox < r.area.x || oy < r.area.y || ox >= r.area.x + r.area.w || oy >= r.area.y + r.area.h {
                        continue;
                    }
                    let Some(mut tile) = t.tile else { continue };
                    let mut flags = t.flags;
                    // A blank floor a later room re-typed (`DRLGROOMTILE_UpdateTileType`).
                    if t.layer == Layer::Floor && tile.main == 30 {
                        if let Some(swap) = seams.swap_at(ox, oy).filter(|s| s.owner == (a.area.x, a.area.y)) {
                            tile = swap.tile.unwrap_or(NO_COLLISION);
                            flags = swap.flags;
                        }
                    }
                    let (rx, ry) = ((ox - r.area.x) as usize, (oy - r.area.y) as usize);
                    stamp_tile(&mut cells, gw, gh, rx * 5, ry * 5, &tile, flags);
                    if t.layer == Layer::Floor {
                        covered[ry * r.area.w as usize + rx] = true;
                    }
                }
            }
            if !r.preset {
                for (i, c) in cells.iter_mut().enumerate() {
                    let (x, y) = (i % gw / 5, i / gw / 5);
                    if !covered[y * r.area.w as usize + x] {
                        *c |= void;
                    }
                }
            }
            RoomCollision { level, area: r.area, cells }
        })
        .collect()
}
