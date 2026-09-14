//! Maze levels: the dungeons built from a grid of preset cells — for now Act I's caves, the Den
//! of Evil among them.
//!
//! `DRLGMAZE_GenerateLevel` lays out cells of `LvlMaze.txt` size: a first cell in the middle of
//! the level's rectangle, grown to the level's cell count in random directions, each cell taking
//! the preset for the sides it opens to (`DRLGMAZE_PickRoomPreset`), then the level type's special
//! cells — the way back, the way on, the Den's lair (`ReplaceRoom` tables). Every cell then becomes
//! its map, cut into 8×8-tile rooms (`InitAllRoomsEx` → `DRLGPRESET_AllocDrlgMap`, `BuildArea`).
//! Every room, file pick and seed follows the engine's draws on the level's seed.
//!
//! Ported from libd2 `packages/drlg/src/drlg/maze/{Maze,Act1,deps}.zig`, `DrlgRoom.zig` and
//! `preset.zig` (MIT, © 2026 jaenster), which reproduce the engine room for room; addresses are
//! theirs.

use std::collections::HashMap;
use std::fmt;

use d2_data::engine::EngineData;
use d2_data::levels::DrlgType;
use d2_data::GameData;
use d2_formats::ds1::{Ds1, UnitKind};

use crate::collision::{self, BuiltRoom, RoomCollision, TileSources};
use crate::rng::{level_seed, Seed};
use crate::room_tiles::{self, PresetWindow, RoomContext, Seams, Warps};
use crate::Coords;

/// A cell whose preset is fixed (`HAS_MAP_DS1`, bit 1 of the preset room's flags).
const LOCKED: i32 = 2;
/// `LvlTypes.txt` row of Act I's caves.
const ACT1_CAVES: i32 = 3;
/// The Den of Evil.
const DEN_OF_EVIL: i32 = 8;
/// Cave Level 1, the Cold Plains cave.
const CAVE_LEVEL_1: i32 = 9;
/// Underground Passage Level 1.
const UNDERGROUND_PASSAGE_1: i32 = 10;

/// Why a maze level could not be generated.
#[derive(Debug)]
pub enum Error {
    /// Not a maze level, or one whose level type is not ported.
    NotPorted(i32),
    /// A table row the level needs is missing.
    NoRow(&'static str, i32),
    /// A map file is missing.
    MissingMap(String),
    /// The layout could not reach its cell count.
    Stuck(i32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPorted(l) => write!(f, "level {l}: maze generation not ported"),
            Self::NoRow(t, v) => write!(f, "{t} has no row {v}"),
            Self::MissingMap(m) => write!(f, "{m} is not in the install"),
            Self::Stuck(l) => write!(f, "level {l}: maze layout did not grow"),
        }
    }
}

impl std::error::Error for Error {}

/// A room of a generated maze level: an 8×8 (or smaller) window of its cell's map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MazeRoom {
    /// The room in world tiles.
    pub area: Coords,
    /// `nSeed`.
    pub seed: u32,
    /// `LvlPrest.txt` `Def` of its cell.
    pub def: i32,
    /// Which of the preset's map files the cell uses.
    pub file: i32,
    /// The cell's corner in world tiles: where the map starts.
    pub origin: (i32, i32),
    /// The room's grid flags; bits 4–11 are the vis slots whose warps it holds.
    pub flags: i32,
}

impl MazeRoom {
    /// Vis slots whose warp tiles lie in the room.
    #[must_use]
    pub fn warp_slots(&self) -> u8 {
        (self.flags >> 4) as u8
    }
}

/// A generated maze level: its rooms in the level's list order (newest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MazeLevel {
    /// `Levels.txt` id.
    pub id: i32,
    /// The level's rectangle in tiles.
    pub area: Coords,
    /// Its rooms.
    pub rooms: Vec<MazeRoom>,
}

#[derive(Debug, Clone)]
struct Cell {
    area: Coords,
    seed: Seed,
    def: i32,
    variant: i32,
    flags: i32,
    /// Linked cells and the direction to each, newest first.
    orth: Vec<(usize, i32)>,
}

impl Cell {
    fn locked(&self) -> bool {
        self.flags & LOCKED != 0
    }
}

/// One `ReplaceRoom` table entry: a cell of `from` becomes `to`, or a `to` cell grows in `dir`.
#[derive(Debug, Clone, Copy)]
struct Replacement {
    from: i32,
    to: i32,
    dir: i32,
}

const fn table(from: [i32; 4], to: [i32; 4]) -> [Replacement; 4] {
    [
        Replacement { from: from[0], to: to[0], dir: 3 },
        Replacement { from: from[1], to: to[1], dir: 0 },
        Replacement { from: from[2], to: to[2], dir: 1 },
        Replacement { from: from[3], to: to[3], dir: 2 },
    ]
}

/// Act I cave cells opening one way: N, E, S, W.
const CAVE_ENDS: [i32; 4] = [60, 54, 56, 53];
/// `Act1.cpp` Caves tables (`0x00672550`): the way back, the Den's lair, the way down, Coldcrow's
/// cell, the way on.
const CAVE_PREV: [Replacement; 4] = table(CAVE_ENDS, [86, 84, 85, 83]);
const CAVE_DEN: [Replacement; 4] = table(CAVE_ENDS, [98, 96, 97, 95]);
const CAVE_DOWN: [Replacement; 4] = table(CAVE_ENDS, [94, 92, 93, 91]);
const CAVE_COLDCROW: [Replacement; 4] = table(CAVE_ENDS, [102, 100, 101, 99]);
const CAVE_NEXT: [Replacement; 4] = table(CAVE_ENDS, [90, 88, 89, 87]);

struct Layout<'a> {
    data: &'a GameData,
    engine: &'a EngineData,
    id: i32,
    area: Coords,
    cell: (i32, i32),
    merge: i32,
    seed: Seed,
    cells: Vec<Cell>,
    /// The level's room list, head first.
    list: Vec<usize>,
}

impl Layout<'_> {
    /// `DRLGROOM_AllocRoomEx`: a cell of maze size; its seed state is the level's low word stepped
    /// from `{low, 0x29A}`.
    fn alloc(&mut self) -> usize {
        self.seed.step();
        let mut seed = Seed::new(self.seed.low, 0x29A);
        seed.step();
        self.cells.push(Cell { area: Coords { x: 0, y: 0, w: self.cell.0, h: self.cell.1 }, seed, def: 0, variant: 0, flags: 0, orth: Vec::new() });
        self.cells.len() - 1
    }

    /// `DRLGROOM_AddRoomExToLevel`: at the list's head.
    fn add(&mut self, cell: usize) {
        self.list.insert(0, cell);
    }

    /// `DRLGROOM_FreeDrlgRoomEx`: unlink it and drop it from the list.
    fn free(&mut self, cell: usize) {
        let linked: Vec<usize> = self.cells[cell].orth.iter().map(|&(c, _)| c).collect();
        for other in linked {
            if let Some(i) = self.cells[cell].orth.iter().position(|&(c, _)| c == other) {
                self.cells[cell].orth.remove(i);
            }
            if let Some(i) = self.cells[other].orth.iter().position(|&(c, _)| c == cell) {
                self.cells[other].orth.remove(i);
            }
        }
        self.list.retain(|&c| c != cell);
    }

    /// `DRLGROOM_AllocNodesForBothRoomEx`.
    fn link(&mut self, a: usize, b: usize, dir: i32) {
        for (from, to, d) in [(a, b, dir), (b, a, (dir - 2) & 3)] {
            if !self.cells[from].orth.iter().any(|&(c, _)| c == to) {
                self.cells[from].orth.insert(0, (to, d));
            }
        }
    }

    /// `DRLGROOM_IsWithinDistance`'s gaps between two rectangles.
    fn gaps(a: Coords, b: Coords) -> (i32, i32) {
        let gx = if a.x < b.x { b.x - a.w - a.x } else { a.x - b.w - b.x };
        let gy = if a.y < b.y { b.y - a.h - a.y } else { a.y - b.h - b.y };
        (gx, gy)
    }

    /// `ReplaceRoomWithNewRoom` (`Drlg.cpp:2492`): put `new` beside `src` in `dir`; whether it
    /// overlaps neither `src`'s links nor any other room of the level.
    fn place(&mut self, dir: i32, new: usize, src: usize) -> bool {
        let s = self.cells[src].area;
        let (x, y) = match dir {
            0 => (s.x - s.w, s.y),
            1 => (s.x, s.y - s.h),
            2 => (s.x + s.w, s.y),
            3 => (s.x, s.y + s.h),
            4 => (s.x - s.w, s.y - s.h),
            5 => (s.x + s.w, s.y - s.h),
            6 => (s.x + s.w, s.y + s.h),
            7 => (s.x - s.w, s.y + s.h),
            _ => (self.cells[new].area.x, self.cells[new].area.y),
        };
        (self.cells[new].area.x, self.cells[new].area.y) = (x, y);
        let n = self.cells[new].area;
        let overlaps = |b: Coords| {
            let (gx, gy) = Self::gaps(n, b);
            gx < 0 && gy < 0
        };
        if self.cells[src].orth.iter().any(|&(c, _)| overlaps(self.cells[c].area)) {
            return false;
        }
        !self.list.iter().any(|&c| c != new && c != src && overlaps(self.cells[c].area))
    }

    /// `GetDirectionFromCoordinates` (`Drlg.cpp:282`).
    fn direction(a: Coords, b: Coords) -> i32 {
        if b.x < a.x {
            if a.x == b.x + b.w {
                return 0;
            }
        } else if b.x == a.x + a.w {
            return 2;
        }
        if b.y < a.y {
            if a.y == b.y + b.h {
                return 1;
            }
        } else if b.y == a.y + a.h {
            return 3;
        }
        -1
    }

    /// `DRLGMAZE_PickRoomPreset` (`0x006709B0`) for Act I caves: the cell's open sides.
    fn pick_preset(&mut self, cell: usize) {
        let bits = self.cells[cell].orth.iter().fold(0, |bits, &(_, dir)| {
            bits | match dir {
                0 => 1,
                1 => 8,
                2 => 2,
                3 => 4,
                _ => 0,
            }
        });
        let c = &mut self.cells[cell];
        c.def = bits + 0x34;
        c.variant = -1;
        c.flags &= !LOCKED;
    }

    /// `DRLGMAZE_PickRoomPresets` (`Maze.cpp:277`): join the new cell to touching cells it is not
    /// linked to, each on a per-mille roll of that cell's seed under `Merge`.
    fn merge_touching(&mut self, cell: usize) {
        if self.cells[cell].locked() {
            return;
        }
        for other in self.list.clone() {
            if other == cell || self.cells[other].locked() {
                continue;
            }
            let (gx, gy) = Self::gaps(self.cells[cell].area, self.cells[other].area);
            if !(gx < 1 && gy < 1) || gx == gy || self.cells[cell].orth.iter().any(|&(c, _)| c == other) {
                continue;
            }
            self.cells[other].seed.step();
            if (self.cells[other].seed.low % 1000) as i32 >= self.merge {
                continue;
            }
            let dir = Self::direction(self.cells[other].area, self.cells[cell].area);
            if dir != -1 {
                self.link(other, cell, dir);
                self.pick_preset(other);
            }
        }
    }

    /// Grow a cell off `src` in `dir`, as every generator step does; the new cell, if it fit.
    fn grow(&mut self, src: usize, dir: i32) -> Option<usize> {
        let new = self.alloc();
        if !self.place(dir, new, src) {
            self.free(new);
            return None;
        }
        self.link(src, new, dir);
        self.merge_touching(new);
        self.add(new);
        self.pick_preset(src);
        self.pick_preset(new);
        Some(new)
    }

    /// `ActualLevelGeneration` (`Maze.cpp:590`): grow off random cells to the level's cell count.
    fn grow_to(&mut self, rooms: i32) -> Result<(), Error> {
        let mut tries = 0;
        while (self.list.len() as i32) < rooms {
            tries += 1;
            if tries > 100_000 {
                return Err(Error::Stuck(self.id));
            }
            // GetRandomRoomExFromLevel (Maze.cpp:559), on the low word.
            self.seed.step();
            let count = self.list.len() as u32;
            let pick = if count & (count - 1) == 0 { self.seed.low & (count - 1) } else { self.seed.low % count };
            let cell = self.list[pick as usize];
            self.cells[cell].seed.step();
            let dir = (self.cells[cell].seed.low & 3) as i32;
            if !self.cells[cell].locked() {
                self.grow(cell, dir);
            }
        }
        Ok(())
    }

    /// `ReplaceRoom` (`Maze.cpp:935`): the first free cell of the table entry's source preset takes
    /// the destination preset; with none, one grows off the first free cell that has room
    /// (`ReplaceRoomWith`). The roll moves on either way.
    fn replace(&mut self, tables: &[Replacement; 4], roll: &mut i32) {
        let r = tables[(*roll & 3) as usize];
        *roll = (*roll + 1) & 3;
        if let Some(&cell) = self.list.iter().find(|&&c| !self.cells[c].locked() && self.cells[c].def == r.from) {
            let c = &mut self.cells[cell];
            (c.flags, c.def, c.variant) = (c.flags | LOCKED, r.to, -1);
            return;
        }
        for cell in self.list.clone() {
            if self.cells[cell].locked() {
                continue;
            }
            let new = self.alloc();
            if !self.place(r.dir, new, cell) {
                self.free(new);
                continue;
            }
            self.link(cell, new, r.dir);
            self.add(new);
            self.pick_preset(cell);
            let n = &mut self.cells[new];
            (n.flags, n.def, n.variant) = (n.flags | LOCKED, r.to, -1);
            return;
        }
    }

    /// `ForAllButDenOfEvil` (`Maze.cpp:983`): lock a fifth of the plain cells (at least two) as
    /// their themed variants, fifteen presets on.
    fn theme_cells(&mut self) {
        if self.id == DEN_OF_EVIL {
            return;
        }
        let first = 0x34;
        let mut shuffle: [i32; 16] = std::array::from_fn(|i| i as i32);
        self.seed.step();
        shuffle[15] = (self.seed.low % 15) as i32;
        let count = self.list.len() as i32;
        let mut target = (count / 5 + 1).max(2);
        let mut tries = count * 2;
        let mut position = 0;
        for _ in 0..15 {
            self.seed.step();
            let a = (self.seed.low % 15) as usize;
            self.seed.step();
            let b = (self.seed.low % 15) as usize;
            shuffle.swap(a, b);
            position = shuffle[15];
        }
        while target != 0 && tries != 0 {
            let def = shuffle[position as usize] + first;
            shuffle[15] = def + 15;
            if let Some(&cell) = self.list.iter().find(|&&c| !self.cells[c].locked() && self.cells[c].def == def) {
                let c = &mut self.cells[cell];
                (c.flags, c.def, c.variant) = (c.flags | LOCKED, def + 15, -1);
                target -= 1;
            }
            position = (position + 1) % 15;
            tries -= 1;
        }
    }
}

/// Generate a maze level of the act whose game seed is `game_seed`, on `difficulty`.
///
/// # Errors
///
/// [`Error`] if the level is not a maze level of a ported type, or a table row is missing.
pub fn generate(data: &GameData, engine: &EngineData, game_seed: u32, difficulty: u8, level: i32) -> Result<MazeLevel, Error> {
    let def = data.levels().get(level).filter(|d| d.drlg_type == DrlgType::Maze).ok_or(Error::NotPorted(level))?;
    if def.level_type != ACT1_CAVES {
        return Err(Error::NotPorted(level));
    }
    let maze = data.lvl_mazes().for_level(level).ok_or(Error::NoRow("lvlmaze.txt", level))?;
    let (w, h) = def.size[usize::from(difficulty.min(2))];
    let area = Coords { x: def.offset.0, y: def.offset.1, w, h };
    let mut l = Layout {
        data,
        engine,
        id: level,
        area,
        cell: maze.size,
        merge: maze.merge,
        seed: level_seed(crate::rng::act_start_seed(game_seed), level),
        cells: Vec::new(),
        list: Vec::new(),
    };

    // generateRoomLayout (Maze.cpp:1256): the first cell centred in the level.
    let root = l.alloc();
    l.cells[root].area.x = (area.w - maze.size.0) / 2 + area.x;
    l.cells[root].area.y = (area.h - maze.size.1) / 2 + area.y;
    l.add(root);
    l.grow_to(maze.rooms[usize::from(difficulty.min(2))])?;
    // Act1::Caves (0x00672550).
    l.seed.step();
    let mut roll = (l.seed.low & 3) as i32;
    l.replace(&CAVE_PREV, &mut roll);
    l.replace(if level == DEN_OF_EVIL { &CAVE_DEN } else { &CAVE_DOWN }, &mut roll);
    if level == CAVE_LEVEL_1 {
        l.replace(&CAVE_COLDCROW, &mut roll);
    }
    if level == UNDERGROUND_PASSAGE_1 {
        l.replace(&CAVE_NEXT, &mut roll);
    }
    // DRLGLEVEL_AdjustRoomCoordinates (Drlg.cpp:492): the cells' corner to the level's.
    let min_x = l.list.iter().map(|&c| l.cells[c].area.x).min().unwrap_or(0);
    let min_y = l.list.iter().map(|&c| l.cells[c].area.y).min().unwrap_or(0);
    for &c in &l.list {
        l.cells[c].area.x += area.x - min_x;
        l.cells[c].area.y += area.y - min_y;
    }
    l.theme_cells();
    rooms(&mut l, def.vis, def.warp)
}

/// `InitAllRoomsEx` (`0x00673A60`) for every cell in list order: pick its map file
/// (`DRLGPRESET_AllocDrlgMap`, then `DRLGMAZE_SelectRandomPresetFile`), read it for units and warps
/// when the preset scans (`DRLGPRESET_BuildPresetArea`), and cut it into rooms (`BuildArea`).
fn rooms(l: &mut Layout<'_>, vis: [i32; 8], warp: [i32; 8]) -> Result<MazeLevel, Error> {
    let data = l.data;
    // Rooms come out at the list's head, newest first; cells leave it as they are cut.
    let mut out: Vec<MazeRoom> = Vec::new();
    let mut file_tracker: HashMap<i32, (i32, i32)> = HashMap::new();
    let vis_flags = (0..8).filter(|&i| vis[i] != 0 && warp[i] == -1).fold(0, |f, i| f | 0x10 << i);
    for cell in l.list.clone() {
        let c = l.cells[cell].clone();
        let row = data.lvl_prests().by_def(c.def).ok_or(Error::NoRow("lvlprest.txt", c.def))?;
        let files = row.file_count;
        let mut file = if files < 1 {
            0
        } else {
            l.seed.step();
            let n = files as u32;
            (if n & (n - 1) == 0 { l.seed.low & (n - 1) } else { l.seed.low % n }) as i32
        };
        if c.variant != -1 {
            file = c.variant;
        } else if (0x34..0x34 + 16).contains(&c.def) {
            // The cave cells rotate through their files from a random start, per preset.
            let entry = match file_tracker.get(&c.def) {
                Some(&e) => e,
                None => (files, l.seed.pick(files as u32) as i32),
            };
            let next = if entry.0 > 0 { (entry.1 + 1).rem_euclid(entry.0) } else { 0 };
            file_tracker.insert(c.def, (entry.0, next));
            file = next;
        }
        let (sx, sy) = if row.size.0 == 0 || row.size.1 == 0 { (c.area.w, c.area.h) } else { row.size };
        let mut grid: HashMap<(i32, i32), i32> = HashMap::new();
        if row.scan || row.pops != 0 {
            let path = row.file_for(file).ok_or(Error::NoRow("lvlprest.txt", c.def))?;
            let map = read_map(data, path)?;
            unit_draws(data, l.engine, &map, &mut l.seed);
            if row.scan && map.width == sx + 1 && map.height == sy + 1 || row.scan && map.width == sx && map.height == sy {
                for (&(gx, gy), &slots) in &warp_cells(&map) {
                    *grid.entry((gx, gy)).or_insert(0) |= i32::from(slots) << 4;
                }
            }
        }
        for gy in 0..(sy + 7) / 8 {
            for gx in 0..(sx + 7) / 8 {
                let (x, y) = (c.area.x + gx * 8, c.area.y + gy * 8);
                let (w, h) = ((sx - gx * 8).min(8), (sy - gy * 8).min(8));
                if w == 0 || h == 0 {
                    continue;
                }
                l.seed.step();
                let mut seed = Seed::new(l.seed.low, 0x29A);
                seed.step();
                out.insert(0, MazeRoom {
                    area: Coords { x, y, w, h },
                    seed: seed.low,
                    def: c.def,
                    file,
                    origin: (c.area.x, c.area.y),
                    flags: vis_flags | grid.get(&(gx, gy)).copied().unwrap_or(0),
                });
            }
        }
    }
    Ok(MazeLevel { id: l.id, area: l.area, rooms: out })
}

fn read_map(data: &GameData, path: &str) -> Result<Ds1, Error> {
    let member = format!("data\\global\\tiles\\{}", path.replace('/', "\\"));
    let bytes = data.read_file(&member).ok().flatten().ok_or_else(|| Error::MissingMap(member.clone()))?;
    Ds1::parse(&bytes).map_err(|_| Error::MissingMap(member))
}

/// `DRLGPRESET_AddPresetUnitToDrlgMap` (`0x006675F0`)'s level-seed draws: some monsters and
/// objects are kept only on a roll. Units are walked newest first.
fn unit_draws(data: &GameData, engine: &EngineData, map: &Ds1, seed: &mut Seed) {
    let monsters = data.mon_presets();
    let rows = monsters.monstats_rows();
    let act = u8::try_from(map.act).unwrap_or(0);
    for unit in map.units.iter().rev() {
        let rolls = match unit.kind {
            UnitKind::Monster => {
                let class = monsters.engine_class(map.act, unit.id);
                class >= 0 && if class < rows { matches!(class, 0xCC | 0xCD | 0x173 | 0x174) } else { matches!(class - rows, 0x21..=0x23) }
            }
            UnitKind::Object => matches!(engine.preset_object_class(act, unit.id), Some(0xC4 | 0x105 | 0x245)),
            UnitKind::Other(_) => false,
        };
        if rolls {
            seed.step();
        }
    }
}

/// The DS1's warp tiles by 8×8 cell (`DRLGPRESET_BuildPresetArea`'s scan): a wall of type 10 or
/// 11 whose main index is a vis slot and whose sub index is 0 or 4.
fn warp_cells(map: &Ds1) -> HashMap<(i32, i32), u8> {
    let mut cells = HashMap::new();
    for (walls, types) in map.walls.iter().zip(&map.orientations).take(4) {
        for y in 0..map.height {
            for x in 0..map.width {
                let at = (y * map.width + x) as usize;
                let (Some(&wall), Some(&kind)) = (walls.get(at), types.get(at)) else { continue };
                if kind != 10 && kind != 11 {
                    continue;
                }
                let (slot, sub) = (wall >> 20 & 0x3F, wall >> 8 & 0xFF);
                if slot < 8 && (sub == 0 || sub == 4 || wall & 0x8000_0000 != 0) {
                    *cells.entry((x / 8, y / 8)).or_insert(0u8) |= 1 << slot;
                }
            }
        }
    }
    cells
}

/// Build a maze level's room tiles and collision maps, in list order.
///
/// # Errors
///
/// [`Error::MissingMap`] if a cell's map is missing.
pub fn collision(data: &GameData, engine: &EngineData, sources: &TileSources, level: &MazeLevel) -> Result<Vec<RoomCollision>, Error> {
    let def = data.levels().get(level.id).ok_or(Error::NotPorted(level.id))?;
    let types = sources.tiles.level_type(data, def.level_type);
    let mut seams = Seams::new();
    let mut built = Vec::with_capacity(level.rooms.len());
    for room in &level.rooms {
        let row = data.lvl_prests().by_def(room.def).ok_or(Error::NoRow("lvlprest.txt", room.def))?;
        let file = row.file_for(room.file).ok_or(Error::NoRow("lvlprest.txt", room.def))?;
        let map = sources.maps.get(data, file).ok_or_else(|| Error::MissingMap(file.to_string()))?;
        let library = types.room(row.dt1_mask);
        let slots = room.warp_slots();
        let nodes = (0..8).rev().filter(|&s| slots >> s & 1 != 0 && def.warp[s] != -1).map(|s| data.lvl_warps().setup(def.warp[s], b'b')).collect();
        let ctx = RoomContext {
            library: &library,
            tables: &engine.tiles,
            level: level.id,
            rect: room.area,
            seed: room.seed,
            seams: Some(&mut seams),
            warps: Some(Warps { table: data.lvl_warps(), ids: def.warp, nodes }),
        };
        let size = if row.size.0 == 0 || row.size.1 == 0 { (level.area.w, level.area.h) } else { row.size };
        let window = PresetWindow { origin: room.origin, size, fill_blanks: row.fill_blanks, kill_edge: row.kill_edge };
        built.push(BuiltRoom { area: room.area, tiles: room_tiles::preset_room(ctx, &map, window), preset: true });
    }
    Ok(collision::level_collision(level.id, &built, &seams, 0x05))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libd2's engine recordings (`LIBD2_DIR`) of the Den of Evil and the other Act I maze caves for
    /// game seeds 1, 2 and 305419896: every room's place, seed, preset and flags, in list order.
    #[test]
    fn with_libd2_recordings_the_act1_caves_match_the_engine() {
        let (Ok(libd2), Ok(dir), Ok(exe)) = (std::env::var("LIBD2_DIR"), std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let golden = std::path::Path::new(&libd2).join("packages/drlg/src/golden");
        let mut checked = 0;
        for (file, seed) in [("deep_seed_1.jsonl", 1u32), ("deep_seed_2.jsonl", 2), ("deep_seed_305419896.jsonl", 305_419_896)] {
            let text = std::fs::read_to_string(golden.join(file)).unwrap();
            for line in text.lines() {
                let num = |s: &str, key: &str| -> Option<i64> {
                    let at = s.find(key)? + key.len();
                    s[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect::<String>().parse().ok()
                };
                let Some(level) = num(line, "\"levelId\":") else { continue };
                if !(8..=12).contains(&level) {
                    continue;
                }
                let ours = generate(&data, &engine, seed, 0, level as i32).unwrap_or_else(|e| panic!("seed {seed} level {level}: {e}"));
                let rooms = line.split("{\"x\":").skip(1).filter(|r| r.contains("\"seed\":")).map(|r| {
                    let r = format!("\"x\":{r}");
                    (num(&r, "\"x\":").unwrap() as i32, num(&r, "\"y\":").unwrap() as i32, num(&r, "\"seed\":").unwrap() as u32, num(&r, "\"def\":").unwrap() as i32, num(&r, "\"flags\":").unwrap() as i32)
                });
                let recorded: Vec<(i32, i32, u32, i32, i32)> = rooms.collect();
                let got: Vec<(i32, i32, u32, i32, i32)> = ours.rooms.iter().map(|r| (r.area.x, r.area.y, r.seed, r.def, r.flags)).collect();
                assert_eq!(got, recorded, "seed {seed} level {level}");
                checked += 1;
            }
        }
        assert_eq!(checked, 15, "the five maze caves of three seeds");
    }

    /// The maze caves' levels.
    const CAVES: [i32; 5] = [8, 9, 10, 11, 12];

    /// With libd2's engine recordings: each maze cave room's collision map equals the engine's in
    /// the terrain bits (`0x1F`).
    #[test]
    fn with_libd2_recordings_cave_collision_matches_the_engine() {
        use crate::outdoor::tests::{install, read_golden, recorded_collision};
        let Some((data, engine, golden)) = install() else { return };
        let sources = TileSources::new();
        for file in ["coll_seed1_all.jsonl.gz", "coll_seed2_all.jsonl.gz", "coll_seed17_all.jsonl.gz", "coll_seed18_all.jsonl.gz", "coll_seed777_all.jsonl.gz"] {
            let text = read_golden(&golden.join(file));
            let difficulty = text.lines().next().and_then(|l| l.split("\"diff\":").nth(1)).and_then(|d| d[..1].parse::<u8>().ok()).unwrap_or(0);
            let (seed, recorded) = recorded_collision(&text, &CAVES);
            let mut report = Vec::new();
            let mut wrong = 0;
            for &id in &CAVES {
                let level = generate(&data, &engine, seed, difficulty, id).expect("level");
                let maps = collision(&data, &engine, &sources, &level).expect("rooms");
                let (mut bad_cells, mut missing) = (0, 0);
                for room in &maps {
                    let Some((w, theirs)) = recorded.get(&(id, room.area.x * 5, room.area.y * 5)) else {
                        missing += 1;
                        continue;
                    };
                    assert_eq!(*w, room.area.w * 5);
                    bad_cells += room.cells.iter().zip(theirs).filter(|(a, b)| **a & 0x1F != (**b & 0x1F) as u8).count();
                }
                wrong += bad_cells + missing;
                report.push(format!("level {id}: {} rooms, {missing} not recorded, {bad_cells} cells differ", maps.len()));
            }
            eprintln!("{file} (seed {seed}, difficulty {difficulty}):\n{}", report.join("\n"));
            assert_eq!(wrong, 0, "{file}");
        }
    }

    /// With libd2's recordings: for 200 seeds on Normal and on Hell, each maze cave's collision
    /// checksum equals the engine's (FNV-1a over each room's subtile corner, size and terrain
    /// bits, summed over the rooms).
    #[test]
    fn with_libd2_recordings_cave_checksums_match_for_200_seeds() {
        use crate::outdoor::tests::{install, number, read_golden};
        let Some((data, engine, golden)) = install() else { return };
        let sources = TileSources::new();
        let fnv = |h: &mut u32, v: u32| {
            for b in v.to_le_bytes() {
                *h = (*h ^ u32::from(b)).wrapping_mul(0x0100_0193);
            }
        };
        for (file, difficulty) in [("coll_crc_masked_200_normal.jsonl.gz", 0u8), ("coll_crc_masked_200_hell.jsonl.gz", 2)] {
            let mut recorded: HashMap<(u32, i32), u32> = HashMap::new();
            for line in read_golden(&golden.join(file)).lines() {
                let get = |key| number(line, key, 0).map(|(v, _)| v);
                if let (Some(seed), Some(level), Some(crc)) = (get("\"seed\":"), get("\"levelId\":"), get("\"crc\":")) {
                    if CAVES.contains(&(level as i32)) {
                        recorded.insert((seed as u32, level as i32), crc as u32);
                    }
                }
            }
            let mut wrong = Vec::new();
            for seed in 1..=200u32 {
                for &id in &CAVES {
                    let Some(&theirs) = recorded.get(&(seed, id)) else { continue };
                    let level = generate(&data, &engine, seed, difficulty, id).expect("level");
                    let ours = collision(&data, &engine, &sources, &level).expect("rooms").iter().fold(0u32, |sum, room| {
                        let mut h = 0x811C_9DC5;
                        for v in [room.area.x * 5, room.area.y * 5, room.area.w * 5, room.area.h * 5] {
                            fnv(&mut h, v as u32);
                        }
                        for &c in &room.cells {
                            fnv(&mut h, u32::from(c & 0x1F));
                        }
                        sum.wrapping_add(h)
                    });
                    if ours != theirs {
                        wrong.push((seed, id));
                    }
                }
            }
            assert!(recorded.len() >= 1000, "{file}: {} recorded", recorded.len());
            assert!(wrong.is_empty(), "{file}: {} of {} differ: {:?}", wrong.len(), recorded.len(), &wrong[..wrong.len().min(10)]);
        }
    }
}
