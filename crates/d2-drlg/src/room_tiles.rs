//! A room's tiles, as the engine builds them from its grids.
//!
//! Every room is a set of grids — floor layers, wall layers with their tile types, a shadow layer
//! — one cell per tile, one cell wider and taller than the room. A preset room's grids are its
//! window into the preset's DS1 (`DRLGPRESET_InitGridsFromDS1File`, `0x006667D0`); a plain
//! wilderness room's are laid by its init (`DRLGOUTROOM_InitGridCells`, `0x0067D2D0`): grass, the
//! road edges, then the waypoint, shrine and terrain pieces from `LvlSub.txt` maps. Then
//! `DRLGROOMTILE_InitRoomTiles` (`0x0066EC10`) walks each grid and `DRLGROOMTILE_ProcessTile`
//! (`0x0066E9B0`) turns each cell into floor, wall and shadow tiles, picking each on the room's
//! seed.
//!
//! Cells on a room's border are shared with its neighbours: `DRLGROOMTILE_UpdateOrAddTile`
//! (`0x0066E940`) lets a room built earlier that already owns the tile keep it, and may re-type it
//! (`DRLGROOMTILE_UpdateTileType`, `0x0066E740`). [`Seams`] is that shared state for one level.
//!
//! Ported from libd2 `packages/drlg/src/drlg/materialize.zig` and `tilegen.zig` (MIT, © 2026
//! jaenster), which reproduce the engine's collision maps byte for byte; addresses are
//! `Game.exe` 1.14d.

use std::collections::HashMap;

use d2_data::engine::TileTables;
use d2_data::lvlsub::LvlSub;
use d2_data::tiles::LvlWarps;
use d2_formats::ds1::{Ds1, SubstGroup};
use d2_formats::dt1::Tile;

use crate::rng::Seed;
use crate::tiles::Library;
use crate::Coords;

/// Cell has a wall tile.
const WALL: i32 = 0x01;
/// Cell has a floor tile.
const FLOOR: i32 = 0x02;
/// Cell is on a room's border: its tile may belong to a neighbour.
const SEAM: i32 = 0x04;
/// Cell has a shadow tile.
const SHADOW: i32 = 0x0800_0000;
/// Cell marks a preset unit or a warp rather than a tile.
const MARKER: i32 = i32::MIN;

/// Tile types (a wall grid's orientation layer).
const TYPE_FLOOR: i32 = 0;
const TYPE_ROOF_COMPANION: i32 = 3;
const TYPE_SHADOW_COMPANION: i32 = 4;
const TYPE_PRESET: i32 = 8;
const TYPE_PRESET_SUB: i32 = 9;
const TYPE_WARP_LEFT: i32 = 10;
const TYPE_WARP_RIGHT: i32 = 11;
const TYPE_SHADOW: i32 = 13;

/// Levels the engine singles out.
const ARCANE_SANCTUARY: i32 = 74;
const MATRONS_DEN: i32 = 133;

/// Which of a room's tile arrays a tile is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Floors.
    Floor,
    /// Walls, including the shadow a type 3 wall brings.
    Wall,
    /// Shadows and roofs.
    Roof,
}

/// A tile a room placed, in tiles from the room's corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomTile {
    /// Column.
    pub x: i32,
    /// Row.
    pub y: i32,
    /// The tile's draw flags (`D2DrlgTileDataStrc::nFlags`).
    pub flags: i32,
    /// Its tile type (wall orientation; 0 floor, 13 shadow).
    pub tile_type: i32,
    /// The library tile, `None` when nothing matched at all.
    pub tile: Option<Tile>,
    /// Which array.
    pub layer: Layer,
}

/// A warp tile of a room: where a level's exit to another level lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarpTile {
    /// Index of the room in its level's list.
    pub room: usize,
    /// The level's vis slot the warp belongs to.
    pub slot: u8,
    /// The tile's world subtile corner.
    pub x: i32,
    /// The tile's world subtile corner.
    pub y: i32,
}

/// A room's warp tiles from the cells its build set up as warps (`(x, y, slot)` from the room's
/// corner), one per vis slot in `slots`. The engine keeps each slot's tiles on the room's warp
/// node newest first (`DRLGROOMTILE_SetupWarpTile`, `0x0066E260`); the last one set up is kept.
#[must_use]
pub fn warp_tiles(room: usize, area: Coords, cells: &[(i32, i32, u8)], slots: u8) -> Vec<WarpTile> {
    let mut found: Vec<WarpTile> = Vec::new();
    for &(x, y, slot) in cells.iter().filter(|&&(_, _, s)| s < 8 && slots >> s & 1 != 0) {
        let warp = WarpTile { room, slot, x: (area.x + x) * 5, y: (area.y + y) * 5 };
        match found.iter_mut().find(|w| w.slot == slot) {
            Some(w) => *w = warp,
            None => found.push(warp),
        }
    }
    found
}

/// A grid of cell flags, as `D2DrlgGridStrc` reads it: row by row, a read past a row's end
/// landing in the next row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    width: i32,
    height: i32,
    cells: Vec<i32>,
}

impl Grid {
    /// A zeroed grid.
    #[must_use]
    pub fn new(width: i32, height: i32) -> Self {
        Self { width, height, cells: vec![0; (width.max(0) * height.max(0)) as usize] }
    }

    fn slot(&self, x: i32, y: i32) -> Option<usize> {
        if y < 0 || y >= self.height {
            return None;
        }
        usize::try_from(y * self.width + x).ok().filter(|&i| i < self.cells.len())
    }

    /// A cell, 0 outside.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> i32 {
        self.slot(x, y).map_or(0, |i| self.cells[i])
    }

    /// Overwrite a cell.
    pub fn set(&mut self, x: i32, y: i32, value: i32) {
        if let Some(i) = self.slot(x, y) {
            self.cells[i] = value;
        }
    }

    fn or(&mut self, x: i32, y: i32, flag: i32) {
        if let Some(i) = self.slot(x, y) {
            self.cells[i] |= flag;
        }
    }

    /// `DRLGGRID_FlagOperations` with OR: the top and bottom rows, then the first and last cell
    /// of every other row.
    fn or_border(&mut self, flag: i32) {
        let (w, h) = (self.width, self.height);
        if w <= 0 || h <= 0 {
            return;
        }
        for x in 0..w {
            self.or(x, 0, flag);
            self.or(x, h - 1, flag);
        }
        for y in 1..h {
            self.or(0, y, flag);
            self.or(w - 1, y, flag);
        }
    }

    fn or_all(&mut self, flag: i32) {
        for c in &mut self.cells {
            *c |= flag;
        }
    }

    /// A window of a DS1 layer: `width × height` cells from (`x`, `y`).
    fn window(layer: &[u32], layer_width: i32, x: i32, y: i32, width: i32, height: i32, map: impl Fn(u32) -> i32) -> Self {
        let mut grid = Self::new(width, height);
        for gy in 0..height {
            for gx in 0..width {
                let at = usize::try_from((y + gy) * layer_width + x + gx).ok();
                grid.set(gx, gy, at.and_then(|i| layer.get(i)).map_or(0, |&v| map(v)));
            }
        }
        grid
    }
}

/// A tile's draw flags from its cell, the part `FillTileData`, `SetWallTileFlags` and
/// `CreateShadowTileData` share.
fn draw_bits(mut flags: i32, grid: i32) -> i32 {
    if grid & 0x80 != 0 {
        flags |= 1;
    }
    if grid & 0x1000_0000 != 0 {
        flags |= 0x102;
    }
    if grid & 0x2_0000 != 0 {
        flags |= 0x40;
    }
    if grid & 0x1_0000 != 0 {
        flags |= 0x80;
    }
    if grid & 8 != 0 {
        flags |= 4;
    }
    if grid < 0 {
        flags |= 8;
    } else {
        flags &= !8;
    }
    if grid & 0x400_0000 != 0 {
        flags |= 0x20C;
    }
    if grid & 0x2000_0000 != 0 {
        flags |= 0x800;
    }
    if grid & 4 != 0 {
        flags |= 0x2000;
    }
    flags
}

/// The layer a cell's grid belongs to, as a tile's flags carry it: `(layer + 1) << 14`.
fn layer_bits(grid: i32) -> i32 {
    ((grid >> 0x12) & 3) * 0x4000 + 0x4000
}

/// `DRLGROOMTILE_FillTileData` (`0x0066DDE0`).
fn floor_flags(grid: i32) -> i32 {
    draw_bits(layer_bits(grid), grid)
}

/// `DRLGROOMTILE_SetWallTileFlags` (`0x0066DB20`).
fn wall_flags(tile_type: i32, grid: i32) -> i32 {
    let mut flags = if tile_type == TYPE_SHADOW { 0 } else { layer_bits(grid) };
    if tile_type == 14 {
        flags |= 4;
    } else if (TYPE_PRESET..=TYPE_WARP_RIGHT).contains(&tile_type) {
        flags |= 2;
    }
    draw_bits(flags, grid)
}

/// `DRLGROOMTILE_CreateShadowTileData` (`0x0066DF40`).
fn shadow_flags(grid: i32) -> i32 {
    draw_bits(0, grid)
}

/// What a tile's draw flags add to every subtile of its collision (`TileLibrary_SetupCollision`,
/// `0x0064C790`).
#[must_use]
pub fn collision_from_flags(flags: i32) -> u8 {
    let mut extra = 0;
    if flags & 0x02 != 0 {
        extra |= 0x10;
    }
    if flags & 0x40 != 0 {
        extra |= 0x01;
    }
    if flags & 0x80 != 0 {
        extra |= 0x04;
    }
    extra
}

/// A seam tile re-typed by a later room: the tile and flags its owner's tile now has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swap {
    /// The new tile (`None`: nothing matched).
    pub tile: Option<Tile>,
    /// Its flags.
    pub flags: i32,
    /// The owner room's corner, in world tiles.
    pub owner: (i32, i32),
}

#[derive(Debug, Clone)]
struct Owner {
    library: Library,
    seed: Seed,
}

#[derive(Debug, Clone, Copy)]
struct SeamTile {
    tile_type: i32,
    flags: i32,
    owner: Coords,
    blank: bool,
    owner_index: usize,
}

/// The tiles a level's rooms built through `AddTileData`, which later rooms' border cells find
/// (`FindTileInNearRooms`, `0x0066E580`).
#[derive(Debug, Clone, Default)]
pub struct Seams {
    owners: Vec<Owner>,
    tiles: HashMap<(i32, i32, bool), SeamTile>,
    swaps: HashMap<(i32, i32), Swap>,
}

fn is_blank(tile: Option<&Tile>) -> bool {
    tile.is_some_and(|t| t.main == 30 && t.sub == 0)
}

impl Seams {
    /// An empty level.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The re-typed floor at a world tile, if a room re-typed one there.
    #[must_use]
    pub fn swap_at(&self, x: i32, y: i32) -> Option<Swap> {
        self.swaps.get(&(x, y)).copied()
    }

    fn publish(&mut self, library: &Library, seed: Seed, rect: Coords, linked: &[(i32, i32, i32, i32, bool)]) {
        let owner_index = self.owners.len();
        self.owners.push(Owner { library: library.clone(), seed });
        for &(x, y, tile_type, flags, blank) in linked {
            // A companion shadow is never found; the first tile at a cell wins.
            if tile_type == TYPE_SHADOW_COMPANION {
                continue;
            }
            self.tiles.entry((x, y, tile_type == TYPE_FLOOR)).or_insert(SeamTile { tile_type, flags, owner: rect, blank, owner_index });
        }
    }

    /// `DRLGROOMTILE_UpdateTileType`'s choice (`0x0066E740`): the type the owner's tile becomes,
    /// or `None` when it is left alone.
    #[allow(clippy::too_many_arguments)]
    fn update_type(tables: &TileTables, visit: i32, held: SeamTile, grid: i32, searcher: Coords, x: i32, y: i32) -> Option<i32> {
        // A pinned tile (grid 0x80): only preset walls keep their type, nothing is swapped.
        if held.flags & 1 != 0 {
            return matches!(held.tile_type, TYPE_PRESET | TYPE_PRESET_SUB).then_some(held.tile_type);
        }
        if grid & 0x80 == 0 {
            let edge = matches!(visit, TYPE_PRESET | TYPE_PRESET_SUB) && (x == searcher.x || y == searcher.y);
            if !edge {
                if matches!(held.tile_type, TYPE_PRESET | TYPE_PRESET_SUB) && (x == held.owner.x || y == held.owner.y) {
                    return None;
                }
                let row = usize::try_from(visit).ok().and_then(|v| tables.mapping_by_type.get(v)).copied().unwrap_or(-1);
                if row < 0 || held.tile_type > 7 {
                    if row != -1 {
                        return None;
                    }
                } else {
                    return tables.mapping_transitions.get((row * 7 + held.tile_type) as usize).copied();
                }
            }
        }
        Some(visit)
    }

    /// `FindTileInNearRooms` → `FindTileAtPosition` (`0x0066E4C0`): whether a room built earlier
    /// and touching `searcher` owns the tile at world (`x`, `y`) for this cell. A blank floor it
    /// finds is re-typed for the cell on the owner's own seed and library.
    #[allow(clippy::too_many_arguments)]
    fn find(&mut self, tables: &TileTables, x: i32, y: i32, tile_type: i32, grid: i32, searcher: Coords) -> bool {
        let Some(held) = self.tiles.get(&(x, y, tile_type == TYPE_FLOOR)).copied() else { return false };
        let o = held.owner;
        if o.x + o.w < searcher.x || searcher.x + searcher.w < o.x || o.y + o.h < searcher.y || searcher.y + searcher.h < o.y {
            return false;
        }
        if held.blank {
            if let Some(layer) = Self::update_type(tables, tile_type, held, grid, searcher, x, y) {
                let flags = wall_flags(layer, grid);
                let owner = &mut self.owners[held.owner_index];
                let tile = owner.library.pick(&mut owner.seed, layer, grid as u32).copied();
                self.swaps.insert((x, y), Swap { tile, flags, owner: (o.x, o.y) });
                // Later finds see the re-typed tile; this one still answers from what it found.
                let updated = SeamTile { tile_type: layer, flags, blank: layer == TYPE_FLOOR && is_blank(tile.as_ref()), ..held };
                self.tiles.insert((x, y, tile_type == TYPE_FLOOR), updated);
            }
        }
        if held.tile_type == TYPE_SHADOW_COMPANION || (held.tile_type != TYPE_SHADOW && grid & SHADOW != 0) {
            return false;
        }
        if held.flags & 0x1C000 == 0 {
            return true;
        }
        ((held.flags >> 0xE & 7) - 1) == ((grid >> 0x12) & 3)
    }
}

/// A room's warps: the level's warp ids by vis slot and the room's warp nodes, each an
/// `LvlWarp.txt` row (`DRLGROOMEX_AllocRoomTile`, `0x0066BE10`), newest first.
#[derive(Debug, Clone)]
pub struct Warps<'a> {
    /// `LvlWarp.txt`.
    pub table: &'a LvlWarps,
    /// The level's warp id per vis slot, -1 for none.
    pub ids: [i32; 8],
    /// The room's warp nodes.
    pub nodes: Vec<Option<usize>>,
}

/// Builds one room's tiles.
struct Builder<'a> {
    library: &'a Library,
    tables: &'a TileTables,
    level: i32,
    /// The room in world tiles.
    rect: Coords,
    seed: Seed,
    warps: Option<Warps<'a>>,
    tiles: Vec<RoomTile>,
    /// Tiles made through `AddTileData`, which neighbours can find: world x, y, type, flags,
    /// blank floor.
    linked: Vec<(i32, i32, i32, i32, bool)>,
    seams: Option<&'a mut Seams>,
    /// Cells set up as warps: x, y and vis slot.
    warp_cells: Vec<(i32, i32, u8)>,
}

impl<'a> Builder<'a> {
    fn new(library: &'a Library, tables: &'a TileTables, level: i32, rect: Coords, seed: u32, seams: Option<&'a mut Seams>, warps: Option<Warps<'a>>) -> Self {
        Self { library, tables, level, rect, seed: Seed::new(seed, 0x29A), warps, tiles: Vec::new(), linked: Vec::new(), seams, warp_cells: Vec::new() }
    }

    fn pick(&mut self, tile_type: i32, grid: i32) -> Option<Tile> {
        self.library.pick(&mut self.seed, tile_type, grid as u32).copied()
    }

    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.rect.w && y < self.rect.h
    }

    fn push(&mut self, layer: Layer, x: i32, y: i32, flags: i32, tile_type: i32, tile: Option<Tile>) -> usize {
        self.tiles.push(RoomTile { x, y, flags, tile_type, tile, layer });
        self.tiles.len() - 1
    }

    /// `DRLGROOMTILE_CreateWallTileData` (`0x0066DC50`).
    fn wall(&mut self, x: i32, y: i32, grid: i32, tile: Option<Tile>, tile_type: i32) -> usize {
        let at = self.push(Layer::Wall, x, y, 0, tile_type, tile);
        if matches!(tile_type, TYPE_PRESET | TYPE_PRESET_SUB) {
            self.preset_roll(grid, x, y, tile_type == TYPE_PRESET_SUB);
        }
        self.tiles[at].flags = wall_flags(tile_type, grid);
        if tile_type == TYPE_ROOF_COMPANION {
            let shadow = self.pick(TYPE_SHADOW_COMPANION, grid);
            self.wall(x, y, grid, shadow, TYPE_SHADOW_COMPANION);
        }
        at
    }

    /// `Preset::CreatesPresets` (`0x0066D9E0`), for its roll: a tomb shrine marker keeps its unit
    /// on a room seed roll.
    fn preset_roll(&mut self, grid: i32, x: i32, y: i32, sub: bool) {
        let (main, orientation) = (grid >> 20 & 0x3F, grid >> 8 & 0xFF);
        let tables = self.tables;
        let row = tables.preset_levels.iter().filter(|h| h[0] == self.level).find_map(|h| {
            (h[1]..=h[2]).map(|i| tables.preset_rows[i as usize]).find(|r| r[0] == main && r[1] == orientation && r[2] == i32::from(sub))
        });
        let Some([_, _, _, class, unit_type, dx, dy]) = row else { return };
        let (ux, uy) = (x * 5 + dx, y * 5 + dy);
        if ux < 0 || uy < 0 || ux >= self.rect.w * 5 || uy >= self.rect.h * 5 {
            return;
        }
        if unit_type == 2 && class > 0x5A && class < 0x5D {
            self.seed.pick(3);
        }
    }

    fn warp_node(&self, grid: i32) -> Option<usize> {
        let warps = self.warps.as_ref()?;
        let id = warps.ids[(grid >> 20 & 0x3F) as usize & 7];
        warps.nodes.iter().position(|n| n.and_then(|r| warps.table.row(r)).is_some_and(|r| r.id == id))
    }

    /// `DRLGROOMTILE_SetWarpTileDirection` (`0x0066E160`).
    fn set_warp_direction(&mut self, node: usize, tile_type: i32) {
        let Some(warps) = self.warps.as_mut() else { return };
        let Some(row) = warps.nodes[node].and_then(|r| warps.table.row(r)).copied() else { return };
        let wanted = if tile_type == TYPE_WARP_RIGHT { b'r' } else { b'l' };
        if row.direction != b'b' && row.direction != wanted {
            warps.nodes[node] = warps.table.setup(row.id, wanted);
        }
    }

    fn node_row(&self, node: usize) -> Option<d2_data::tiles::LvlWarp> {
        let warps = self.warps.as_ref()?;
        warps.nodes[node].and_then(|r| warps.table.row(r)).copied()
    }

    /// `DRLGROOMTILE_SetupWarpTile` (`0x0066E260`): a lit warp gets a second wall tile.
    fn setup_warp(&mut self, x: i32, y: i32, grid: i32, tile_type: i32) {
        let Some(node) = self.warp_node(grid) else { return };
        self.warp_cells.push((x, y, (grid >> 20 & 7) as u8));
        let orientation = grid >> 8 & 0xFF;
        if orientation == 0 || orientation == 4 {
            let wanted = if tile_type == TYPE_WARP_RIGHT { b'r' } else { b'l' };
            let Some(warps) = self.warps.as_ref() else { return };
            if warps.table.setup(warps.ids[(grid >> 20 & 0x3F) as usize & 7], wanted).is_none() {
                return;
            }
            if x == self.rect.w || y == self.rect.h {
                return;
            }
        }
        self.set_warp_direction(node, tile_type);
        let Some(row) = self.node_row(node).filter(|r| r.lit_version) else { return };
        let lit = (row.tiles << 8) | grid;
        let tile = self.pick(tile_type, lit);
        let at = self.wall(x, y, lit, tile, tile_type);
        self.tiles[at].flags |= 8;
    }

    /// `DRLGROOMTILE_InitWarpCacheTiles` (`0x0066E360`): a lit warp's four floor tiles.
    fn warp_floors(&mut self, grid: i32, x: i32, y: i32, tile_type: i32) {
        let Some(node) = self.warp_node(grid) else { return };
        self.warp_cells.push((x, y, (grid >> 20 & 7) as u8));
        self.set_warp_direction(node, tile_type);
        if !self.node_row(node).is_some_and(|r| r.lit_version) {
            return;
        }
        let sub = grid >> 8 & 0xFF;
        for (i, (dx, dy)) in self.tables.warp_tile_offsets.into_iter().enumerate() {
            let cell = ((sub << 12) | i as i32 | 4) << 8;
            let tile = self.pick(TYPE_FLOOR, cell);
            self.push(Layer::Floor, x - 1 + dx, y - 1 + dy, floor_flags(cell) | 8, TYPE_FLOOR, tile);
        }
    }

    fn wall_cell(&mut self, x: i32, y: i32, grid: i32, tile_type: i32) {
        let tile = self.pick(tile_type, grid);
        self.wall(x, y, grid, tile, tile_type);
        if matches!(tile_type, TYPE_WARP_LEFT | TYPE_WARP_RIGHT) && self.level != MATRONS_DEN {
            self.setup_warp(x, y, grid, tile_type);
        }
    }

    fn shadow_cell(&mut self, x: i32, y: i32, grid: i32) {
        let tile = self.pick(TYPE_SHADOW, grid);
        self.push(Layer::Roof, x, y, shadow_flags(grid), TYPE_SHADOW, tile);
    }

    /// `DRLGROOMTILE_UpdateOrAddTile` (`0x0066E940`) → `AddTileData` (`0x0066E620`).
    fn update_or_add(&mut self, tile_type: i32, x: i32, y: i32, grid: i32) {
        let (wx, wy) = (self.rect.x + x, self.rect.y + y);
        if let Some(seams) = self.seams.as_deref_mut() {
            if seams.find(self.tables, wx, wy, tile_type, grid, self.rect) {
                return;
            }
        }
        let warp = matches!(tile_type, TYPE_WARP_LEFT | TYPE_WARP_RIGHT);
        if warp && !self.inside(x, y) {
            return;
        }
        let tile = self.pick(tile_type, grid);
        // SetupWarpTile leaves the room's wall link chain holding only the warp tile.
        if warp && self.seams.is_some() {
            self.linked.retain(|t| t.2 == TYPE_FLOOR);
        }
        let at = match tile_type {
            TYPE_FLOOR => self.push(Layer::Floor, x, y, floor_flags(grid), TYPE_FLOOR, tile),
            TYPE_SHADOW => self.push(Layer::Roof, x, y, shadow_flags(grid), TYPE_SHADOW, tile),
            _ => self.wall(x, y, grid, tile, tile_type),
        };
        if self.seams.is_some() {
            let blank = tile_type == TYPE_FLOOR && is_blank(tile.as_ref());
            self.linked.push((wx, wy, tile_type, self.tiles[at].flags, blank));
        }
        if warp && self.level != MATRONS_DEN {
            self.setup_warp(x, y, grid, tile_type);
        }
    }

    /// `DRLGROOMTILE_ProcessTile` (`0x0066E9B0`).
    fn process(&mut self, cell: i32, x: i32, y: i32, fill_blanks: bool, tile_type: i32) {
        let mut cell = cell;
        let main = cell >> 20 & 0x3F;
        let warp = matches!(tile_type, TYPE_WARP_LEFT | TYPE_WARP_RIGHT);
        if warp && main > 7 {
            return;
        }
        let orientation = cell >> 8 & 0xFF;
        if tile_type == TYPE_FLOOR && main == 30 && (orientation == 0 || orientation == 1) {
            cell |= MARKER;
        }
        if cell & MARKER != 0 {
            if matches!(tile_type, TYPE_PRESET | TYPE_PRESET_SUB) {
                // The Act V barricade levels draw their marker walls as walls.
                if self.level < 0x6F || (self.level > 0x70 && self.level != 0x75) {
                    self.preset_roll(cell, x, y, tile_type == TYPE_PRESET_SUB);
                    return;
                }
            } else if warp {
                self.warp_floors(cell, x, y, tile_type);
                return;
            }
        }
        if cell & SEAM != 0 {
            if cell & FLOOR != 0 {
                return self.update_or_add(TYPE_FLOOR, x, y, cell);
            }
            if cell & WALL != 0 {
                return self.update_or_add(tile_type, x, y, cell);
            }
            if cell & SHADOW != 0 && cell & MARKER == 0 {
                return self.update_or_add(TYPE_SHADOW, x, y, cell);
            }
        }
        if cell & FLOOR == 0 {
            if !fill_blanks || !self.inside(x, y) {
                if cell & WALL != 0 {
                    self.wall_cell(x, y, cell, tile_type);
                }
                if cell & SHADOW != 0 {
                    self.shadow_cell(x, y, cell);
                }
                return;
            }
            let grid = (cell & !0x80) | MARKER;
            let blank = if self.level == ARCANE_SANCTUARY { 0x1E0_0100 } else { 0x1E0_0000 };
            let tile = self.pick(TYPE_FLOOR, blank);
            self.push(Layer::Floor, x, y, floor_flags(grid), TYPE_FLOOR, tile);
        } else {
            let tile = self.pick(TYPE_FLOOR, cell);
            self.push(Layer::Floor, x, y, floor_flags(cell), TYPE_FLOOR, tile);
        }
        if cell & WALL != 0 {
            self.wall_cell(x, y, cell, tile_type);
        }
        if cell & SHADOW != 0 {
            self.shadow_cell(x, y, cell);
        }
    }

    /// `DRLGROOMTILE_InitRoomTiles` (`0x0066EC10`): the room's cells plus, unless killed, the
    /// column and row past its far edges.
    fn walk(&mut self, grid: &Grid, types: Option<&Grid>, fill_blanks: bool, kill: (bool, bool)) {
        let width = self.rect.w + i32::from(!kill.0);
        let height = self.rect.h + i32::from(!kill.1);
        for y in 0..height {
            for x in 0..width {
                let tile_type = types.map_or(0, |t| t.get(x, y));
                self.process(grid.get(x, y), x, y, fill_blanks, tile_type);
            }
        }
    }

    /// Hand the room's linked tiles to the level and keep the tiles a collision map reads:
    /// floors and walls up to the far edges, roofs inside the room (`0x0064C900`).
    fn finish(self) -> Vec<RoomTile> {
        self.finish_with_warps().0
    }

    /// The room's tiles and the cells inside it set up as warps.
    fn finish_with_warps(mut self) -> (Vec<RoomTile>, Vec<(i32, i32, u8)>) {
        if let Some(seams) = self.seams.take() {
            seams.publish(self.library, self.seed, self.rect, &self.linked);
        }
        let (w, h) = (self.rect.w, self.rect.h);
        self.tiles.retain(|t| {
            let edge = i32::from(t.layer != Layer::Roof);
            t.tile.is_some() && t.x >= 0 && t.y >= 0 && t.x < w + edge && t.y < h + edge
        });
        self.warp_cells.retain(|&(x, y, _)| x >= 0 && y >= 0 && x < w && y < h);
        (self.tiles, self.warp_cells)
    }
}

/// How a preset room sits in its map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetWindow {
    /// The map's corner in world tiles.
    pub origin: (i32, i32),
    /// The map's size in tiles (`LvlPrest.txt` `SizeX`/`SizeY`).
    pub size: (i32, i32),
    /// `LvlPrest.txt` `FillBlanks`.
    pub fill_blanks: bool,
    /// `LvlPrest.txt` `KillEdge`.
    pub kill_edge: bool,
}

/// What a room is built for.
pub struct RoomContext<'a> {
    /// The room's tile files.
    pub library: &'a Library,
    /// The engine's tile tables.
    pub tables: &'a TileTables,
    /// `Levels.txt` id.
    pub level: i32,
    /// The room in world tiles.
    pub rect: Coords,
    /// `nSeed`.
    pub seed: u32,
    /// The level's seams.
    pub seams: Option<&'a mut Seams>,
    /// The room's warps.
    pub warps: Option<Warps<'a>>,
}

/// A preset room's tiles (`DRLGPRESET_InitializePresetRoom`, `0x00666AC0`).
#[must_use]
pub fn preset_room(ctx: RoomContext<'_>, map: &Ds1, window: PresetWindow) -> Vec<RoomTile> {
    preset_room_with_warps(ctx, map, window).0
}

/// A preset room's tiles and the cells it set up as warps (`x`, `y`, vis slot).
#[must_use]
pub fn preset_room_with_warps(ctx: RoomContext<'_>, map: &Ds1, window: PresetWindow) -> (Vec<RoomTile>, Vec<(i32, i32, u8)>) {
    let rect = ctx.rect;
    let (ox, oy) = (rect.x - window.origin.0, rect.y - window.origin.1);
    let (w, h) = (rect.w + 1, rect.h + 1);
    let at_far = (rect.x + rect.w == window.origin.0 + window.size.0, rect.y + rect.h == window.origin.1 + window.size.1);
    // Floors, the first wall layer and shadows stop at every edge the map continues past (a
    // neighbour's window scans it) or, with KillEdge, at the far edge too; other wall layers only
    // at a killed far edge.
    let kill = (window.kill_edge || !at_far.0, window.kill_edge || !at_far.1);
    let kill_more = (window.kill_edge && at_far.0, window.kill_edge && at_far.1);

    let mut floors: Vec<Grid> = map.floors.iter().map(|l| Grid::window(l, map.width, ox, oy, w, h, |v| v as i32)).collect();
    let mut walls: Vec<Grid> = map.walls.iter().map(|l| Grid::window(l, map.width, ox, oy, w, h, |v| v as i32)).collect();
    let types: Vec<Grid> = map.orientations.iter().map(|l| Grid::window(l, map.width, ox, oy, w, h, |v| (v & 0xFF) as i32)).collect();
    let mut shadow = Grid::window(&map.shadow, map.width, ox, oy, w, h, |v| v as i32);

    // DRLGPRESET_InitGridsFromDS1File's marks.
    if let Some(first) = walls.first_mut() {
        first.or_border(SEAM);
    }
    for (i, g) in walls.iter_mut().enumerate().skip(1) {
        g.or_all((i as i32) << 0x12);
    }
    for (i, g) in floors.iter_mut().enumerate() {
        g.or_all((i as i32) << 0x12);
        g.or_border(SEAM);
    }
    shadow.or_border(SEAM);

    let mut b = Builder::new(ctx.library, ctx.tables, ctx.level, rect, ctx.seed, ctx.seams, ctx.warps);
    for (i, g) in floors.iter().enumerate() {
        b.walk(g, None, i == 0 && window.fill_blanks, kill);
    }
    for (i, g) in walls.iter().enumerate() {
        b.walk(g, types.get(i), false, if i == 0 { kill } else { kill_more });
    }
    b.walk(&shadow, None, false, kill);
    b.finish_with_warps()
}

/// The substitution passes of a plain wilderness room: its terrain rows and maps.
pub struct Substitution<'a> {
    /// Rows from the group's first, as `SubTypeWpShrine` indexes them by bit.
    pub rows: &'a [LvlSub],
    /// Each row's map, by index into `rows`; `None` if missing.
    pub maps: Vec<Option<&'a Ds1>>,
    /// Theme index into `Max`/`Trials`.
    pub theme: usize,
    /// Which rows, by bit.
    pub picks: u32,
}

/// A piece a substitution pass stamped: its map, its group and where in the room it landed.
#[derive(Debug, Clone, Copy)]
pub struct Stamp<'a> {
    /// The map.
    pub map: &'a Ds1,
    /// The group.
    pub group: SubstGroup,
    /// Column in the room.
    pub x: i32,
    /// Row in the room.
    pub y: i32,
}

/// A plain wilderness room's grids while its init lays them.
struct OutdoorGrids {
    floor: Grid,
    wall: Grid,
    types: Grid,
}

fn cell0(layers: &[Vec<u32>], map: &Ds1, x: i32, y: i32) -> i32 {
    let Some(layer) = layers.first() else { return 0 };
    if y < 0 || y >= map.height {
        return 0;
    }
    usize::try_from(y * map.width + x).ok().and_then(|i| layer.get(i)).map_or(0, |&v| v as i32)
}

impl OutdoorGrids {
    /// `DRLGOUTDOOR_CheckSubTileOverlap` (`0x0066FCF0`).
    fn fits(&self, x: i32, y: i32, group: SubstGroup, map: &Ds1) -> bool {
        for dy in 0..group.h {
            for dx in 0..group.w {
                let (sx, sy) = (group.x + dx, group.y + dy);
                let walled = !map.walls.is_empty() && cell0(&map.walls, map, sx, sy) & WALL != 0;
                if cell0(&map.floors, map, sx, sy) & FLOOR == 0 && !walled {
                    continue;
                }
                let cell = self.floor.get(x + dx, y + dy);
                if cell & 0x3F0_FF00 != 0 || cell & FLOOR == 0 || self.wall.get(x + dx, y + dy) & WALL != 0 {
                    return false;
                }
            }
        }
        true
    }

    /// `DRLGOUTDOOR_ApplyLvlSubTileData` (`0x0066FAD0`): floors, the first wall layer with its
    /// types, and a shadow tile per shadow cell, each picked on the room's seed.
    fn stamp(&mut self, b: &mut Builder<'_>, x: i32, y: i32, group: SubstGroup, map: &Ds1) {
        for dy in 0..group.h {
            for dx in 0..group.w {
                let (sx, sy) = (group.x + dx, group.y + dy);
                let floor = cell0(&map.floors, map, sx, sy);
                if floor & FLOOR != 0 {
                    self.floor.set(x + dx, y + dy, floor | 0x80);
                }
                if !map.walls.is_empty() {
                    let wall = cell0(&map.walls, map, sx, sy);
                    if wall & WALL != 0 {
                        self.wall.set(x + dx, y + dy, wall);
                        self.types.set(x + dx, y + dy, cell0(&map.orientations, map, sx, sy));
                    }
                }
                let shadow = if sy < 0 || sy >= map.height {
                    0
                } else {
                    usize::try_from(sy * map.width + sx).ok().and_then(|i| map.shadow.get(i)).map_or(0, |&v| v as i32)
                };
                if shadow & SHADOW != 0 {
                    b.shadow_cell(x + dx, y + dy, shadow);
                }
            }
        }
    }

    /// `SubTypeWpShrine` (`0x006707A0`) → `DRLGOUTDOOR_DoNotCheckAll` (`0x00670170`).
    fn pass<'m>(&mut self, b: &mut Builder<'_>, sub: &Substitution<'m>, stamps: &mut Vec<Stamp<'m>>) {
        let mut bits = sub.picks;
        for (i, row) in sub.rows.iter().enumerate() {
            if bits == 0 {
                break;
            }
            if bits & 1 != 0 && !row.check_all {
                if let Some(map) = sub.maps.get(i).copied().flatten() {
                    self.place(b, row, map, sub.theme, stamps);
                }
            }
            bits >>= 1;
        }
    }

    fn place<'m>(&mut self, b: &mut Builder<'_>, row: &LvlSub, map: &'m Ds1, theme: usize, stamps: &mut Vec<Stamp<'m>>) {
        if map.subst_groups.is_empty() || theme >= 5 || row.max[theme] < 1 {
            return;
        }
        let (size_x, size_y) = (b.rect.w, b.rect.h);
        for _ in 0..row.max[theme] {
            let group = map.subst_groups[b.seed.pick(map.subst_groups.len() as u32) as usize];
            let (max_x, max_y) = (size_x.wrapping_sub(group.w), size_y.wrapping_sub(group.h));
            if max_x.wrapping_add(1) <= 1 || max_y.wrapping_add(1) <= 1 {
                continue;
            }
            let spot = if row.trials[theme] == -1 {
                let total = (max_y as u32).wrapping_mul(max_x as u32);
                let mut spots: Vec<(i32, i32)> = (0..total).map(|i| ((i % max_x as u32) as i32, (i / max_x as u32) as i32)).collect();
                for _ in 0..total {
                    let a = b.seed.pick(total) as usize;
                    let c = b.seed.pick(total) as usize;
                    spots.swap(a, c);
                }
                spots.into_iter().map(|(x, y)| (x + 1, y + 1)).find(|&(x, y)| self.fits(x, y, group, map))
            } else {
                (0..row.trials[theme]).find_map(|_| {
                    let x = b.seed.pick(max_x as u32) as i32 + 1;
                    let y = b.seed.pick(max_y as u32) as i32 + 1;
                    self.fits(x, y, group, map).then_some((x, y))
                })
            };
            if let Some((x, y)) = spot {
                self.stamp(b, x, y, group, map);
                stamps.push(Stamp { map, group, x, y });
            }
        }
    }
}

/// A plain wilderness room (`DRLGOUTROOM_InitGridCells`, `0x0067D2D0`, then the maze-room tile
/// pass): the floor with `roads` edges already cut in (a 9×9 grid of cells), then the waypoint,
/// shrine and terrain passes, then its tiles. Returns the tiles and what the passes stamped.
#[must_use]
pub fn outdoor_room<'m>(ctx: RoomContext<'_>, floor: Grid, fill_flag: i32, passes: &[Substitution<'m>]) -> (Vec<RoomTile>, Vec<Stamp<'m>>) {
    let rect = ctx.rect;
    let mut b = Builder::new(ctx.library, ctx.tables, ctx.level, rect, ctx.seed, ctx.seams, ctx.warps);
    let mut grids = OutdoorGrids { floor, wall: Grid::new(rect.w + 1, rect.h + 1), types: Grid::new(rect.w + 1, rect.h + 1) };
    let mut stamps = Vec::new();
    for sub in passes {
        grids.pass(&mut b, sub, &mut stamps);
    }
    if fill_flag != 0 {
        for c in &mut grids.floor.cells {
            if *c & 0x3F0_FF80 == 0 {
                *c |= fill_flag;
            }
        }
    }
    grids.wall.or_border(SEAM);
    grids.floor.or_border(SEAM);
    b.walk(&grids.floor, None, false, (false, false));
    b.walk(&grids.wall, Some(&grids.types), false, (false, false));
    (b.finish(), stamps)
}

/// Stamp one tile into a room's collision map (`TileLibrary_AddCollision`, `0x0064C4C0`): its 25
/// subtile flags with the rows read bottom first, plus what its draw flags add.
pub fn stamp_tile(cells: &mut [u8], width: usize, height: usize, x: usize, y: usize, tile: &Tile, flags: i32) {
    let extra = collision_from_flags(flags);
    for dy in 0..5 {
        let gy = y + dy;
        if gy >= height {
            break;
        }
        for dx in 0..5 {
            let gx = x + dx;
            if gx < width {
                cells[gy * width + gx] |= tile.subtile(dx, 4 - dy) | extra;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::TileFile;
    use d2_formats::dt1::Dt1;
    use std::sync::Arc;

    fn tables() -> TileTables {
        TileTables {
            preset_levels: [[0, 0, 0]; 37],
            preset_rows: [[0; 7]; 34],
            warp_tile_offsets: [(0, 0), (1, 0), (0, 1), (1, 1)],
            mapping_transitions: [0; 43],
            mapping_by_type: [-1; 20],
        }
    }

    fn library(tiles: Vec<Tile>) -> Library {
        Library::from_files(vec![Arc::new(TileFile::new(Dt1 { tiles }))])
    }

    fn tile(orientation: i32, main: i32, sub: i32, flag: u8) -> Tile {
        Tile { orientation, main, sub, rarity: 1, flags: [flag; 25] }
    }

    #[test]
    fn draw_flags_follow_the_cell() {
        assert_eq!(floor_flags(0x2), 0x4000);
        assert_eq!(floor_flags(0x2 | (1 << 0x12)), 0x8000, "second layer");
        assert_eq!(collision_from_flags(wall_flags(1, 0x2_0001)), 0x01, "0x20000 blocks walking");
        assert_eq!(collision_from_flags(wall_flags(TYPE_WARP_LEFT, 1)), 0x10, "warp walls are preset cells");
        assert_eq!(shadow_flags(0x80 | 0x1_0000), 0x81);
        assert_eq!(wall_flags(TYPE_SHADOW, 0), 0, "a shadow wall has no layer");
    }

    #[test]
    fn a_later_room_leaves_a_shared_border_tile_to_its_owner() {
        let lib = library(vec![tile(0, 0, 0, 0), tile(10, 0, 0, 0)]);
        let tables = tables();
        let mut seams = Seams::new();
        let floor = |w, h| {
            let mut g = Grid::new(w + 1, h + 1);
            for y in 0..=h {
                for x in 0..=w {
                    g.set(x, y, FLOOR);
                }
            }
            g
        };
        let left = Coords { x: 0, y: 0, w: 2, h: 2 };
        let right = Coords { x: 2, y: 0, w: 2, h: 2 };
        let ctx = RoomContext { library: &lib, tables: &tables, level: 2, rect: left, seed: 1, seams: Some(&mut seams), warps: None };
        let (a, _) = outdoor_room(ctx, floor(2, 2), 0, &[]);
        assert_eq!(a.len(), 9, "3×3 cells with the far column and row");
        let ctx = RoomContext { library: &lib, tables: &tables, level: 2, rect: right, seed: 1, seams: Some(&mut seams), warps: None };
        let (b, _) = outdoor_room(ctx, floor(2, 2), 0, &[]);
        // The right room's left column is the left room's far column: owned, not rebuilt.
        assert!(b.iter().all(|t| t.x > 0), "{b:?}");
        assert_eq!(b.len(), 6);
    }

    #[test]
    fn a_tile_stamps_its_rows_bottom_first() {
        let mut t = tile(0, 0, 0, 0);
        t.flags[20] = 0x01; // bottom-left subtile in the file
        let mut cells = vec![0u8; 25];
        stamp_tile(&mut cells, 5, 5, 0, 0, &t, 0x40);
        assert_eq!(cells[0], 0x01);
        assert!(cells.iter().all(|&c| c & 0x01 != 0), "0x40 blocks the whole tile");
        let mut cells = vec![0u8; 25];
        stamp_tile(&mut cells, 5, 5, 0, 0, &t, 0);
        assert_eq!((cells[0], cells[20]), (0x01, 0), "file row 4 is the map's row 0");
    }
}
