//! Act I wilderness levels: which 8×8-tile cells the engine makes rooms, and which it leaves void.
//!
//! A wilderness level is a grid of cells (`DRLGOUTDOOR_GenerateLevel`, `0x00675360`). The act
//! generator first walks the level's outline — its rectangle, split where it meets a
//! neighbouring level — and stamps border pieces along it (`PlaceAct1245OutdoorBorders`,
//! `0x00675850`), blanking the corners outside the border (`SetBlankBorderGridCells`,
//! `0x00675670`). Then come the substitution borders (`AddAct124SecondaryBorder`): cliff and
//! corner shapes from `LvlSub.txt` maps, whose empty tiles blank cells inside the level
//! (`DRLGOUTDOOR_ApplySubTileToGrid`, `0x0066F520`). The grid becomes rooms at the end
//! (`DRLGOUTDOOR_CreateOutdoorRoomExGrid`, `0x006750F0`): a plain cell is one room, a preset
//! piece's anchor cell becomes the piece cut into 8×8 rooms, and a blank cell is nothing.
//!
//! Everything that can blank a cell happens by the last substitution border, and what the Act I
//! generator does after it — exits and roads, the waypoint, shrines, set pieces — only places
//! pieces on cells already free (`TestOutdoorLevelPreset` refuses blank ones) or ORs flags that
//! do not change the room set. So the port stops there and knows every room, without the road
//! pathfinder. The one set piece stamped without that test, Burial Grounds' graveyard, is kept.
//!
//! The RNG is the level's own seed (`{act start + level id, 0x29A}`); every draw up to the stop
//! is reproduced in order, as are the engine's reads past a grid row's end (the grid is one
//! allocation, so they land in the next row).
//!
//! Ported from libd2 `packages/drlg/src/drlg/outdoors/{Outdoors,ActInit,Border,OutPlace,OutRoom,
//! OutSub}.zig`, `TileSub.zig`, `DrlgVer.zig`, `DrlgGrid.zig` and `drlg.zig` (MIT, © 2026
//! jaenster), checked against the 1.14d `Game.exe`. Lookup tables are read from the operator's
//! `Game.exe` ([`OutdoorTables`]).

use std::collections::HashMap;
use std::fmt;

use d2_data::engine::OutdoorTables;
use d2_data::levels::{DrlgType, Levels};
use d2_data::lvlsub::LvlSub;
use d2_data::GameData;
use d2_formats::ds1::{Ds1, SubstGroup};

use crate::act::Act;
use crate::preset::ROOM_TILES;
use crate::rng::{self, Seed};
use crate::Coords;

/// Outdoor cell flag: a border piece's cell.
const BORDER: i32 = 0x1;
/// Outdoor cell flag: no room.
const BLANK: i32 = 0x100;
/// Outdoor cell flag: part of a preset piece.
const PRESET: i32 = 0x200;
/// Outdoor cell flag: kept for a level transition.
const RESERVED: i32 = 0x400;
/// Cells a piece may not be placed over.
const OCCUPIED: i32 = 0x1B81;
/// A preset cell's file index, `<< 16`.
const FILE_INDEX: i32 = 0xF_0000;

/// The substitution borders' preset base: `LvlPrest.txt` 4..=15, the Act I wild borders.
const ACT1_BORDER_BASE: i32 = 4;
/// Level ids the Act I outdoor generator handles.
const MOO_MOO_FARM: i32 = 39;
const BLOOD_MOOR: i32 = 2;
const BURIAL_GROUNDS: i32 = 17;
const ROGUE_ENCAMPMENT: i32 = 1;
/// The levels Act I links edge to edge: `DRLGACTMISC_AllocDrlgLevelForAct` builds the
/// neighbours of every wilderness level in this id range.
const ACT1_LINKED: std::ops::RangeInclusive<i32> = 1..=17;

/// Why a level could not be generated.
#[derive(Debug)]
pub enum Error {
    /// Not an Act I wilderness level, or not placed by the act.
    NotAct1Wilderness(i32),
    /// A preset id with no `LvlPrest.txt` row.
    NoPreset(i32),
    /// A map file could not be read.
    Data(d2_data::Error),
    /// A map file is missing.
    MissingMap(String),
    /// A map file is malformed.
    Ds1(d2_formats::ds1::Error),
    /// The engine would halt here.
    Halt(&'static str),
    /// A path the engine can take that is not ported.
    NotPorted(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAct1Wilderness(l) => write!(f, "level {l} is not a placed Act I wilderness level"),
            Self::NoPreset(p) => write!(f, "LvlPrest.txt has no Def {p}"),
            Self::Data(e) => write!(f, "{e}"),
            Self::MissingMap(m) => write!(f, "{m} is not in the install"),
            Self::Ds1(e) => write!(f, "{e}"),
            Self::Halt(what) => write!(f, "the engine halts: {what}"),
            Self::NotPorted(what) => write!(f, "not ported: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// A generated wilderness level's cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutdoorLevel {
    /// `Levels.txt` id.
    pub id: i32,
    /// Its rectangle in tiles.
    pub area: Coords,
    /// Cells across.
    pub width: i32,
    /// Cells down.
    pub height: i32,
    /// `pDrlgLevelData->dwFlags`: road and transition bits.
    pub flags: u32,
    /// Outdoor flags per cell, row by row.
    pub outdoor: Vec<i32>,
    /// Preset id per cell (on a piece's anchor cell), row by row.
    pub presets: Vec<i32>,
    /// Its rooms, in the level's room list order.
    pub rooms: Vec<Coords>,
}

impl OutdoorLevel {
    /// A cell's outdoor flags.
    #[must_use]
    pub fn outdoor_at(&self, x: i32, y: i32) -> i32 {
        self.cell(&self.outdoor, x, y)
    }

    /// The preset anchored at a cell (0 for none).
    #[must_use]
    pub fn preset_at(&self, x: i32, y: i32) -> i32 {
        self.cell(&self.presets, x, y)
    }

    fn cell(&self, cells: &[i32], x: i32, y: i32) -> i32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return 0;
        }
        cells[(y * self.width + x) as usize]
    }
}

/// What generating an act's wilderness reads once: the neighbours each level sees and the
/// substitution maps.
pub struct Act1Outdoors<'a> {
    data: &'a GameData,
    tables: &'a OutdoorTables,
    act: &'a Act,
    start_seed: u32,
    /// `pDrlg->pWarpsInfo`: `Vis`/`Warp` per level after the placement lists' open edges.
    warps: HashMap<i32, ([i32; 8], [i32; 8])>,
    /// `LvlSub.txt` groups 0..=3 with their maps.
    borders: Vec<Vec<(LvlSub, Ds1)>>,
}

impl<'a> Act1Outdoors<'a> {
    /// Prepare Act I for `game_seed`.
    ///
    /// # Errors
    ///
    /// [`Error`] if a substitution map cannot be read.
    pub fn new(data: &'a GameData, tables: &'a OutdoorTables, act: &'a Act, game_seed: u32) -> Result<Self, Error> {
        let levels = data.levels();
        // DRLGLEVEL_ParseLevelData (0x006774xx): each list node gets an open edge (warp -1) to its
        // predecessor, both ways, through DRLGACT_AllocWarpsInfo (0x006428A0) and
        // DRLGACT_SetWarpConnection (0x00642920).
        let mut warps: HashMap<i32, ([i32; 8], [i32; 8])> = HashMap::new();
        let info = |warps: &mut HashMap<i32, ([i32; 8], [i32; 8])>, level: i32| {
            warps.entry(level).or_insert_with(|| levels.get(level).map_or(([0; 8], [-1; 8]), |d| (d.vis, d.warp)));
        };
        let connect = |warps: &mut HashMap<i32, ([i32; 8], [i32; 8])>, level: i32, to: i32| {
            let (vis, warp) = warps.get_mut(&level).expect("warps info allocated");
            if let Some(i) = vis.iter().position(|&v| v == to) {
                warp[i] = -1;
            } else if let Some(i) = (0..8).find(|&i| vis[i] == 0 && warp[i] == -1) {
                (vis[i], warp[i]) = (to, -1);
            }
        };
        for (level, prev) in act.placement_links() {
            info(&mut warps, level);
            info(&mut warps, prev);
            connect(&mut warps, level, prev);
            connect(&mut warps, prev, level);
        }
        let mut borders = Vec::new();
        for group in 0..=3 {
            let mut rows = Vec::new();
            for row in data.lvl_subs().group(group) {
                let path = format!("data\\global\\tiles\\{}", row.file.replace('/', "\\"));
                let bytes = data.read_file(&path).map_err(Error::Data)?.ok_or(Error::MissingMap(path))?;
                rows.push((row.clone(), Ds1::parse(&bytes).map_err(Error::Ds1)?));
            }
            borders.push(rows);
        }
        Ok(Self { data, tables, act, start_seed: rng::act_start_seed(game_seed), warps, borders })
    }

    /// Generate a wilderness level's cells and rooms.
    ///
    /// # Errors
    ///
    /// [`Error`] for a level this generator does not cover or data it cannot find.
    pub fn generate(&self, id: i32) -> Result<OutdoorLevel, Error> {
        let levels = self.data.levels();
        let def = levels
            .get(id)
            .filter(|d| d.act == 0 && d.drlg_type == DrlgType::Wilderness && d.level_type == 2)
            .ok_or(Error::NotAct1Wilderness(id))?;
        let area = self.act.coords(levels, def.id).ok_or(Error::NotAct1Wilderness(id))?;
        let mut g = Generator::new(self, id, area);
        (g.vertices, g.head) = outline(area, &self.orths(levels, id, area));

        // InitAct1OutdoorLevel (0x006807F0)
        g.road_flags();
        if !matches!(id, BLOOD_MOOR | 3 | BURIAL_GROUNDS) {
            g.mark_border_junctions();
        }
        g.outline_flags();
        g.place_borders()?;
        if (2..8).contains(&id) {
            g.secondary_border(0)?;
            g.fill_border_corners_and_presets()?;
            g.secondary_border(1)?;
            g.secondary_border(2)?;
            g.spawn_town_transitions_and_caves()?;
            g.secondary_border(3)?;
        }
        if id == MOO_MOO_FARM {
            for group in 0..=3 {
                g.secondary_border(group)?;
            }
        }
        if id == BURIAL_GROUNDS {
            // DRLGOUTROOM_SpawnAct1LevelPresets (0x00680580): the graveyard, stamped untested.
            g.spawn(1, 1, 0x6C, -1, false)?;
        }
        g.finish()
    }

    /// `DRLGLEVEL_AllocDrlgLevelFromLevelIdToLevelId` (`0x00677680`): an orth for each level
    /// this one sees across an open edge, sorted as `DRLGROOM_ReplaceSub00` (`0x0066B720`)
    /// inserts them.
    fn orths(&self, levels: &Levels, id: i32, area: Coords) -> Vec<Orth> {
        let mut orths: Vec<Orth> = Vec::new();
        if !ACT1_LINKED.contains(&id) {
            return orths;
        }
        let Some((vis, warp)) = self.warps.get(&id).copied().or_else(|| levels.get(id).map(|d| (d.vis, d.warp))) else {
            return orths;
        };
        for i in 0..8 {
            if vis[i] == 0 || warp[i] != -1 {
                continue;
            }
            let Some(other) = self.act.coords(levels, vis[i]) else { continue };
            let Some(direction) = direction_between(area, other) else { continue };
            let preset = levels.get(vis[i]).is_some_and(|d| d.drlg_type == DrlgType::Preset);
            let orth = Orth { direction, preset, area: other };
            // The insertion walk never compares the head once the list has two entries.
            match orths.len() {
                0 => orths.push(orth),
                1 => {
                    if orth.goes_before(&orths[0]) {
                        orths.insert(0, orth);
                    } else {
                        orths.push(orth);
                    }
                }
                _ => {
                    let at = (1..orths.len()).find(|&j| orth.goes_before(&orths[j])).unwrap_or(orths.len());
                    orths.insert(at, orth);
                }
            }
        }
        orths
    }
}

/// `DRLG_GetDirectionFromCoordinates`: the side of `a` that `b` touches — 0 left, 1 top,
/// 2 right, 3 bottom.
fn direction_between(a: Coords, b: Coords) -> Option<i32> {
    if b.x < a.x {
        if a.x == b.w + b.x {
            return Some(0);
        }
    } else if b.x == a.w + a.x {
        return Some(2);
    }
    if b.y < a.y {
        if a.y == b.h + b.y {
            return Some(1);
        }
    } else if b.y == a.h + a.y {
        return Some(3);
    }
    None
}

/// A neighbouring level across an open edge (`D2DrlgOrthStrc`).
#[derive(Debug, Clone, Copy)]
struct Orth {
    direction: i32,
    preset: bool,
    area: Coords,
}

impl Orth {
    /// `DRLGROOM_CompareByDirectionAndCoord` (`0x0066B6A0`).
    fn goes_before(&self, other: &Self) -> bool {
        if self.direction != other.direction {
            return self.direction < other.direction;
        }
        match self.direction {
            0 => self.area.y < other.area.y,
            1 => other.area.x < self.area.x,
            2 => other.area.y < self.area.y,
            _ => self.area.x < other.area.x,
        }
    }
}

/// A `D2DrlgGridStrc` over its one allocation: `height` row offsets, then the cells. A read or
/// write past a row's end lands in the next row, as in the engine; outside the allocation a
/// read is 0 and a write is dropped.
#[derive(Debug, Clone)]
struct Grid {
    height: i32,
    mem: Vec<i32>,
}

impl Grid {
    fn new(width: i32, height: i32) -> Self {
        let (w, h) = (width.max(0) as usize, height.max(0) as usize);
        let mut mem = vec![0; h + w * h];
        for (row, slot) in mem.iter_mut().take(h).enumerate() {
            *slot = (row * w) as i32;
        }
        Self { height, mem }
    }

    fn slot(&self, x: i32, y: i32) -> Option<usize> {
        let row = *self.mem.get(usize::try_from(y).ok()?)?;
        usize::try_from(i64::from(self.height) + i64::from(row) + i64::from(x)).ok().filter(|&i| i < self.mem.len())
    }

    fn get(&self, x: i32, y: i32) -> i32 {
        self.slot(x, y).map_or(0, |i| self.mem[i])
    }

    fn apply(&mut self, x: i32, y: i32, f: impl FnOnce(i32) -> i32) {
        if let Some(i) = self.slot(x, y) {
            self.mem[i] = f(self.mem[i]);
        }
    }

    fn or(&mut self, x: i32, y: i32, flag: i32) {
        self.apply(x, y, |v| v | flag);
    }

    fn clear(&mut self, x: i32, y: i32, flag: i32) {
        self.apply(x, y, |v| v & !flag);
    }

    fn set(&mut self, x: i32, y: i32, value: i32) {
        self.apply(x, y, |_| value);
    }

    fn cells(&self) -> Vec<i32> {
        self.mem[self.height.max(0) as usize..].to_vec()
    }
}

/// A map layer as `DRLGGRID_InitGridFromTileData` views it: rows of `width` cells; rows outside
/// the map read 0.
fn layer_at(layers: &[Vec<u32>], width: i32, height: i32, x: i32, y: i32) -> u32 {
    let Some(layer) = layers.first() else { return 0 };
    if y < 0 || y >= height {
        return 0;
    }
    usize::try_from(i64::from(y) * i64::from(width) + i64::from(x)).ok().and_then(|i| layer.get(i)).copied().unwrap_or(0)
}

/// A vertex of the level's outline (`D2DrlgVertexStrc`), linked into a ring.
#[derive(Debug, Clone, Copy)]
struct Vertex {
    x: i32,
    y: i32,
    /// 1: an open edge to a neighbour starts here; 2: the neighbour is a preset level.
    flags: u32,
    /// 1 on a junction run (`DRLGOUTROOM_MarkBorderJunctions`).
    direction: i32,
    next: usize,
}

/// A level's outline in cells, as a ring with its head: `DRLGVER_CreateRoomVertices`
/// (`0x0067D050`) with `DRLGVER_CreateVerticesFromEdges` (`0x0067CE20`), then
/// `DRLGOUTDOOR_SimplifyOutdoorPoints`. A vertex flagged 1 starts an open edge to a neighbour
/// (3 for a preset neighbour, whose edge gets no border pieces).
fn outline(area: Coords, orths: &[Orth]) -> (Vec<Vertex>, usize) {
    let insert_after = |vertices: &mut Vec<Vertex>, at: usize, x: i32, y: i32| {
        let next = vertices[at].next;
        vertices.push(Vertex { x, y, flags: 0, direction: 0, next });
        let new = vertices.len() - 1;
        vertices[at].next = new;
        new
    };
    let c = Coords { w: area.w - 1, h: area.h - 1, ..area };
    let corner = |x, y, next| Vertex { x, y, flags: 0, direction: 0, next };
    let mut vertices = vec![corner(c.x, c.y + c.h, 1), corner(c.x, c.y, 2), corner(c.x + c.w, c.y, 3), corner(c.x + c.w, c.y + c.h, 0)];
    let mut head = 0;
    let (first, second, third, fourth) = (0, 1, 2, 3);
    for orth in orths {
        let o = Coords { w: orth.area.w - 1, h: orth.area.h - 1, ..orth.area };
        let v = &vertices;
        // (near end, far end, anchor, edge start, sign, edge vertex, the edge runs along y)
        let (near, far, anchor, start, sign, mut edge, vertical) = match orth.direction {
            0 => (o.y + o.h, o.y, v[second].y, v[first].y, -1, first, true),
            1 => (o.x, o.x + o.w, v[third].x, v[second].x, 1, second, false),
            2 => (o.y, o.y + o.h, v[fourth].y, v[third].y, 1, third, true),
            _ => (o.x + o.w, o.x, v[first].x, v[fourth].x, -1, fourth, false),
        };
        let (start, near_s) = (start * sign, near * sign);
        let mut touches = false;
        if near_s <= start {
            touches = start <= far * sign;
        } else if near_s <= anchor * sign {
            let (x, y) = if vertical { (vertices[edge].x, near) } else { (near, vertices[edge].y) };
            edge = insert_after(&mut vertices, edge, x, y);
            touches = true;
        }
        if touches {
            vertices[edge].flags |= if orth.preset { 3 } else { 1 };
            if far * sign < anchor * sign {
                let (x, y) = if vertical { (vertices[edge].x, far) } else { (far, vertices[edge].y) };
                insert_after(&mut vertices, edge, x, y);
            }
        }
    }
    let mut p = head;
    loop {
        let v = &mut vertices[p];
        v.x = (v.x - area.x) / 8;
        v.y = (v.y - area.y) / 8;
        p = v.next;
        if p == head {
            break;
        }
    }
    let mut p = head;
    loop {
        let n = vertices[p].next;
        if (vertices[p].x, vertices[p].y) == (vertices[n].x, vertices[n].y) {
            if n == head {
                head = p;
            }
            let gone = vertices[n];
            let v = &mut vertices[p];
            v.next = gone.next;
            v.flags |= gone.flags;
            v.direction = gone.direction;
        }
        p = vertices[p].next;
        if p == head {
            break;
        }
    }
    (vertices, head)
}

/// `DRLGOUTDOOR_AllocPresetFileTracker`'s node: a preset's rotating file index.
struct FileTracker {
    def: i32,
    files: i32,
    index: i32,
}

struct Generator<'a, 'b> {
    ctx: &'b Act1Outdoors<'a>,
    id: i32,
    area: Coords,
    width: i32,
    height: i32,
    flags: u32,
    seed: Seed,
    /// `sGridPreset`.
    presets: Grid,
    /// `sGridOutdoor`.
    outdoor: Grid,
    vertices: Vec<Vertex>,
    head: usize,
    trackers: Vec<FileTracker>,
}

impl<'a, 'b> Generator<'a, 'b> {
    fn new(ctx: &'b Act1Outdoors<'a>, id: i32, area: Coords) -> Self {
        let (width, height) = (area.w / ROOM_TILES, area.h / ROOM_TILES);
        Self {
            ctx,
            id,
            area,
            width,
            height,
            flags: 0,
            seed: rng::level_seed(ctx.start_seed, id),
            presets: Grid::new(width, height),
            outdoor: Grid::new(width, height),
            vertices: Vec::new(),
            head: 0,
            trackers: Vec::new(),
        }
    }

    /// `ACT1_fpLevelDataFn2_A` (`0x00677180`).
    fn road_flags(&mut self) {
        let Some((dir, next)) = self.ctx.act.direction_pair(self.id) else { return };
        for rule in &self.ctx.tables.road_flags {
            if (self.id == rule[0] || rule[0] == 0) && self.id != rule[1] && self.id != rule[2] && dir == rule[3] && next == rule[4] {
                self.flags |= rule[5] as u32;
            }
        }
    }

    /// `DRLGOUTROOM_MarkBorderJunctions` (`Drlg.cpp:6160`).
    fn mark_border_junctions(&mut self) {
        let v = |g: &Self, i: usize| g.vertices[i];
        let mut cur = self.head;
        let mut prev = cur;
        loop {
            let n = self.vertices[prev].next;
            if n == cur {
                break;
            }
            prev = n;
        }
        let mut complete = false;
        loop {
            let start = cur;
            let (c, cn, p) = (v(self, cur), v(self, v(self, cur).next), v(self, prev));
            let free = c.flags & 1 == 0 && p.flags & 1 == 0;
            let turns = (c.x < cn.x && c.y < p.y) || (cn.y < c.y && c.x < p.x);
            let mut next = cur;
            if turns && free {
                let mut last = None;
                loop {
                    if next == start {
                        complete = true;
                    }
                    let (a, b) = (v(self, next), v(self, v(self, next).next));
                    if a.y < b.y || b.x < a.x || a.flags & 1 != 0 || b.flags & 1 != 0 {
                        break;
                    }
                    let bn = v(self, b.next);
                    if (a.x < b.x && b.y < bn.y && b.flags & 1 == 0) || (b.y < a.y && b.x < bn.x && b.flags & 1 == 0) {
                        last = Some(next);
                    }
                    next = a.next;
                    if next == cur {
                        break;
                    }
                }
                if let Some(last) = last {
                    while cur != last {
                        self.vertices[cur].direction = 1;
                        cur = self.vertices[cur].next;
                    }
                    self.vertices[cur].direction = 1;
                    self.flags |= 0x20;
                }
            }
            cur = self.vertices[next].next;
            if complete {
                return;
            }
            prev = next;
            if cur == self.head {
                return;
            }
        }
    }

    /// The ring's vertices from the head, each once.
    fn ring(&self) -> Vec<usize> {
        let mut out = vec![self.head];
        let mut p = self.vertices[self.head].next;
        while p != self.head {
            out.push(p);
            p = self.vertices[p].next;
        }
        out
    }

    /// The outdoor half of `SetOutGridLinkFlags` (`0x00675770`): each open edge's cells get its
    /// direction code. (The link grid's visibility bits do not bear on rooms.)
    fn outline_flags(&mut self) {
        for i in self.ring() {
            let e = self.vertices[i];
            if e.flags & 1 != 0 {
                self.edge_cells(i, e.direction * 2 + 1);
            }
        }
    }

    /// `DRLGGRID_SetEdgeGridFlags` (`0x0067C760`) with OR, endpoints included.
    fn edge_cells(&mut self, i: usize, flag: i32) {
        let e = self.vertices[i];
        let n = self.vertices[e.next];
        if e.x == n.x {
            if e.y == n.y {
                self.outdoor.or(e.x, e.y, flag);
                return;
            }
            let (mut y, end) = if n.y <= e.y { (n.y + 1, e.y) } else { (e.y + 1, n.y) };
            while y != end {
                self.outdoor.or(e.x, y, flag);
                y += 1;
            }
        } else {
            let (mut x, end) = if e.x < n.x { (e.x + 1, n.x) } else { (n.x + 1, e.x) };
            while x != end {
                self.outdoor.or(x, e.y, flag);
                x += 1;
            }
        }
        self.outdoor.or(e.x, e.y, flag);
        self.outdoor.or(n.x, n.y, flag);
    }

    fn preset_size(&self, preset: i32) -> Result<(i32, i32), Error> {
        let row = self.ctx.data.lvl_prests().by_def(preset).ok_or(Error::NoPreset(preset))?;
        Ok((row.size.0 / 8, row.size.1 / 8))
    }

    /// `DRLGOUTDOOR_AllocPresetFileTracker` (`Outdoors.cpp:550`): the next file of a preset,
    /// starting from a random one the first time the level uses it.
    fn next_file(&mut self, preset: i32) -> Result<i32, Error> {
        let at = match self.trackers.iter().position(|t| t.def == preset) {
            Some(at) => at,
            None => {
                let row = self.ctx.data.lvl_prests().by_def(preset).ok_or(Error::NoPreset(preset))?;
                let index = self.seed.pick(row.file_count as u32) as i32;
                self.trackers.push(FileTracker { def: row.def, files: row.file_count, index });
                self.trackers.len() - 1
            }
        };
        let t = &mut self.trackers[at];
        if t.files == 0 {
            return Err(Error::Halt("a preset with no files rotated"));
        }
        t.index = (t.index + 1) % t.files;
        Ok(t.index)
    }

    /// `SpawnOutdoorLevelPresetEx` (`Outdoors.cpp:583`).
    fn spawn(&mut self, x: i32, y: i32, preset: i32, file: i32, stamp_border: bool) -> Result<(), Error> {
        let (w, h) = self.preset_size(preset)?;
        let file = if file == -1 { self.next_file(preset)? } else { file };
        let border_piece = (3 < preset && preset < 0x10) || (preset.wrapping_sub(0x16C) as u32) < 0xC;
        for cy in y..y + h {
            for cx in x..x + w {
                self.outdoor.clear(cx, cy, FILE_INDEX);
                self.outdoor.or(cx, cy, (file << 16) | PRESET);
                if stamp_border && border_piece {
                    self.outdoor.or(cx, cy, BORDER);
                }
                self.presets.set(cx, cy, 0);
            }
        }
        self.presets.set(x, y, preset);
        Ok(())
    }

    /// `TestOutdoorLevelPreset` (`Outdoors.cpp:482`).
    fn fits(&self, x: i32, y: i32, preset: i32, offset: i32, sides: i32) -> Result<bool, Error> {
        let (mut w, mut h) = if preset != 0 { self.preset_size(preset)? } else { (1, 1) };
        let (mut x0, mut y0) = (x, y);
        if offset != 0 {
            if sides & 1 != 0 {
                y0 -= offset;
                h += offset;
            }
            if sides & 2 != 0 {
                w += offset;
            }
            if sides & 4 != 0 {
                h += offset;
            }
            if sides & 8 != 0 {
                x0 -= offset;
                w += offset;
            }
        }
        for cy in y0..y0 + h {
            for cx in x0..x0 + w {
                if cx < 0 || cy < 0 || cx >= self.width || cy >= self.height || self.outdoor.get(cx, cy) & OCCUPIED != 0 {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// `PlaceAct1245OutdoorBorders` (`0x00675850`) for Act I.
    fn place_borders(&mut self) -> Result<(), Error> {
        let t = self.ctx.tables;
        let dir_index = |dx: i32, dy: i32| t.road_directions[(dx + dy * 3 + 4) as usize];
        let corner_row = |i: i32| usize::try_from(i + 40).ok().and_then(|i| t.corners.get(i)).copied().unwrap_or(-1);
        let widen = |d: i32| if d < 0 { d - 2 } else if d > 0 { d + 2 } else { d };
        let head = self.head;
        let mut cur = head;
        loop {
            let c = self.vertices[cur];
            let n = self.vertices[c.next];
            let nn = self.vertices[n.next];
            let (dx, dy) = ((n.x - c.x).signum(), (n.y - c.y).signum());
            let (ndx, ndy) = ((nn.x - n.x).signum(), (nn.y - n.y).signum());
            let length = if dx == 0 { (c.y - n.y).abs() } else { (c.x - n.x).abs() };
            let mut border_flags = c.direction * 2 + 1;
            // Level type 2: the border style is 1 on a plain edge, 0 on a junction run.
            let road = i32::from(c.direction == 0);
            let piece = t.road_presets[(1 + dir_index(dx, dy)) as usize][road as usize];
            if c.flags & 2 == 0 {
                let (mut x, mut y) = (c.x, c.y);
                while x != n.x || y != n.y {
                    y += dy;
                    x += dx;
                    self.spawn(x, y, piece, -1, false)?;
                    self.outdoor.or(x, y, border_flags);
                }
            }
            if c.flags & 1 != 0 && c.flags & 2 == 0 {
                // The opening onto the neighbour, mid-edge.
                let mx = c.x.min(n.x) + length * dx.abs() / 2;
                let my = c.y.min(n.y) + length * dy.abs() / 2;
                self.outdoor.clear(mx, my, FILE_INDEX);
                self.outdoor.or(mx, my, if self.id == BURIAL_GROUNDS { 0x4_0400 } else { 0x3_0400 });
            }
            let junction = if c.direction == 0 { n.direction } else { c.direction };
            if junction != 0 {
                border_flags |= 2;
            }
            let road_b = i32::from(junction == 0);
            let row = if c.flags & 2 == 0 {
                let a = if n.flags & 2 == 0 { widen(ndx * 2) + ndy * 2 } else { widen(ndx) + ndy };
                corner_row(widen(dx * 2) + a * 9 + dy * 2)
            } else {
                let b = if n.flags & 2 == 0 { widen(ndx * 2) + ndy * 2 } else { widen(ndx) + ndy };
                corner_row(widen(dx) + b * 9 + dy)
            };
            if row != -1 {
                let mut corner = t.road_presets[row as usize][road_b as usize];
                if corner == 0x13 {
                    corner = if c.direction != 0 { i32::from(n.direction == 0) + 0x13 } else { 0x15 };
                }
                if corner != 0 {
                    self.spawn(n.x, n.y, corner, -1, false)?;
                    self.outdoor.or(n.x, n.y, border_flags);
                }
            }
            cur = c.next;
            if cur == head {
                return self.blank_border_corners();
            }
        }
    }

    /// `SetBlankBorderGridCells` (`0x00675670`): from each corner, row by row, blank the cells
    /// before the border until a row starts on it.
    fn blank_border_corners(&mut self) -> Result<(), Error> {
        for (max_x, max_y, step_x, step_y) in [(false, false, 1, 1), (true, false, -1, 1), (false, true, 1, -1), (true, true, -1, -1)] {
            let x0 = if max_x { self.width - 1 } else { 0 };
            let mut y = if max_y { self.height - 1 } else { 0 };
            loop {
                // Past the grid's allocation the engine reads whatever the heap holds.
                if self.outdoor.slot(x0, y).is_none() {
                    return Err(Error::Halt("the blank border scan found no border"));
                }
                if self.outdoor.get(x0, y) & BORDER != 0 {
                    break;
                }
                let mut x = x0;
                while self.outdoor.get(x, y) & BORDER == 0 {
                    if self.outdoor.slot(x, y).is_none() {
                        return Err(Error::Halt("the blank border scan found no border"));
                    }
                    self.outdoor.or(x, y, BLANK);
                    x += step_x;
                }
                y += step_y;
            }
        }
        Ok(())
    }

    /// `TILESUB_AddSecondaryBorder` (`0x00670750`) as `AddAct124SecondaryBorder` sets it up:
    /// each map of the group in turn.
    fn secondary_border(&mut self, group: i32) -> Result<(), Error> {
        let mut prev = -1;
        let ctx = self.ctx;
        for (row, map) in &ctx.borders[group as usize] {
            self.substitution_group(group, row, map, &mut prev)?;
        }
        Ok(())
    }

    /// `DRLGOUTDOOR_ApplySubstitutionGroup` (`0x0066F990`).
    fn substitution_group(&mut self, group: i32, row: &LvlSub, map: &Ds1, prev: &mut i32) -> Result<(), Error> {
        let n = map.subst_groups.len() as i32;
        if n == 0 {
            return Err(Error::Halt("a substitution map without groups"));
        }
        if *prev == -1 {
            *prev = 0x3E;
        }
        let start = if row.bord_type == 0 { self.seed.pick(n as u32) as i32 } else { 0 };
        for i in 0..n {
            let sub = map.subst_groups[((start + i) % n) as usize];
            if self.place_sub_tiles(group, row, map, sub, *prev)? && row.bord_type == 0 {
                return Ok(());
            }
        }
        Ok(())
    }

    /// `DRLGOUTDOOR_PlaceRandomBorderSubTiles` (`0x0066F690`).
    fn place_sub_tiles(&mut self, group: i32, row: &LvlSub, map: &Ds1, sub: SubstGroup, prev: i32) -> Result<bool, Error> {
        let middle = group == 1;
        let mut shrink = 1;
        if middle && self.flags & 0xC != 0 {
            shrink = -1;
        }
        let across = self.width - sub.w * row.grid_size + shrink;
        let down = self.height - sub.h * row.grid_size + 1;
        let total = across.wrapping_mul(down);
        if total <= 0 {
            return Ok(false);
        }
        let strict = middle && (2..=7).contains(&self.id) && across <= 5 && down <= 5;
        let mut positions: Vec<(i32, i32)> = (0..total).map(|i| (i % across, i / across)).collect();
        for _ in 0..total {
            let a = self.seed.pick(total as u32) as usize;
            let b = self.seed.pick(total as u32) as usize;
            positions.swap(a, b);
        }
        for (x, y) in positions {
            if strict && (x, y) == (2, 2) {
                continue;
            }
            if self.sub_fits(x, y, sub, row, map, prev)? {
                let variant = self.seed.pick(sub.variants as u32) as i32;
                self.apply_sub(x, y, sub, row, map, (sub.w + 1) * (variant + 1), prev)?;
                if row.bord_type == 0 || row.bord_type == 1 {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn sub_cell(row: &LvlSub, base: i32, d: i32) -> Result<i32, Error> {
        if row.grid_size == 0 {
            return Err(Error::Halt("a substitution map with grid size 0"));
        }
        Ok(row.grid_size * d + (base - base % row.grid_size))
    }

    /// `DRLGOUTDOOR_ValidateSubTilePlacement` (`0x0066F3B0`) without a border callback.
    fn sub_fits(&self, bx: i32, by: i32, sub: SubstGroup, row: &LvlSub, map: &Ds1, prev: i32) -> Result<bool, Error> {
        for dy in 0..sub.h {
            for dx in 0..sub.w {
                let floor = layer_at(&map.floors, map.width, map.height, sub.x + dx, sub.y + dy);
                let wall = layer_at(&map.walls, map.width, map.height, sub.x + dx, sub.y + dy);
                let (cx, cy) = (Self::sub_cell(row, bx, dx)?, Self::sub_cell(row, by, dy)?);
                if wall & 1 != 0 {
                    let index = ((wall >> 8) & 0xFF) as i32 - 1;
                    if index != prev && ACT1_BORDER_BASE + index != self.presets.get(cx, cy) {
                        return Ok(false);
                    }
                    if self.outdoor.get(cx, cy) & RESERVED != 0 {
                        return Ok(false);
                    }
                } else if floor & 2 != 0 && !self.fits(cx, cy, 0, 0, 0)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// `DRLGOUTDOOR_ApplySubTileToGrid` (`0x0066F520`): wall tiles stamp border pieces, floor
    /// tiles clear the cell, and empty tiles blank it.
    #[allow(clippy::too_many_arguments)] // the engine routine's own parameters
    fn apply_sub(&mut self, bx: i32, by: i32, sub: SubstGroup, row: &LvlSub, map: &Ds1, offset: i32, prev: i32) -> Result<(), Error> {
        let (mx, my) = (sub.x + offset, sub.y);
        for r in 0..sub.h {
            for c in 0..sub.w {
                let wall = layer_at(&map.walls, map.width, map.height, c + mx, r + my);
                let floor = layer_at(&map.floors, map.width, map.height, c + mx, r + my);
                let (cx, cy) = (Self::sub_cell(row, bx, c)?, Self::sub_cell(row, by, r)?);
                if wall & 1 == 0 {
                    self.presets.set(cx, cy, 0);
                    self.outdoor.set(cx, cy, if floor & 2 == 0 { BLANK } else { 0 });
                } else {
                    let main = ((wall >> 8) & 0xFF) as i32 - 1;
                    let preset = ACT1_BORDER_BASE + main;
                    if preset != -5 && main != prev {
                        self.spawn(cx, cy, preset, 0, true)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `DRLGOUTROOM_IsGridColumnEmpty` (`0x0067FC70`): no cell of columns `x` and `x + 1` has
    /// bit 2.
    fn column_empty(&self, x: i32) -> bool {
        (0..self.height).all(|y| self.outdoor.get(x, y) & 2 == 0 && self.outdoor.get(x + 1, y) & 2 == 0)
    }

    /// `DRLGOUTROOM_SpawnVerticalBorderPresets` (`0x0067FE90`).
    fn vertical_border(&mut self, column: i32) -> Result<(), Error> {
        for y in 0..self.height {
            for (side, x, piece) in [(0, column, 0x1A), (1, column + 1, 0x1B)] {
                let flags = self.outdoor.get(x, y);
                let preset = self.presets.get(x, y);
                let variant = if preset == 0 {
                    if flags & BLANK != 0 {
                        0
                    } else {
                        3
                    }
                } else if preset == 7 && flags & FILE_INDEX == 0x3_0000 {
                    3
                } else {
                    let row = usize::try_from(preset)
                        .ok()
                        .and_then(|p| self.ctx.tables.vertical_borders.get(p))
                        .ok_or(Error::NotPorted("a vertical border beside a preset past the variant table"))?;
                    row[side]
                };
                self.spawn(x, y, piece, variant, false)?;
            }
        }
        if self.flags & 0x14 != 0 {
            self.river_crossing(column)?;
        }
        Ok(())
    }

    /// `DRLGOUTROOM_PlaceRiverCrossingPreset` (`0x0067FD20`).
    fn river_crossing(&mut self, column: i32) -> Result<(), Error> {
        let rows = self.height - 2;
        if rows < 1 {
            return Ok(());
        }
        let start = self.seed.pick(rows as u32) as i32;
        let roads = self.flags & 4 != 0;
        let empty = |g: &Self, x, y| g.outdoor.get(x, y) & OCCUPIED == 0;
        for i in 0..rows {
            let y = (i + start) % rows + 1;
            if !empty(self, column - 1, y) || (!roads && !empty(self, column + 2, y)) {
                continue;
            }
            if self.outdoor.get(column, y) & FILE_INDEX != 0x3_0000 || self.outdoor.get(column + 1, y) & FILE_INDEX != 0x3_0000 {
                continue;
            }
            self.spawn(column, y, 0x1C, 1, false)?;
            self.spawn(column + 1, y, 0x1C, i32::from(roads) + 2, false)?;
            return Ok(());
        }
        Ok(())
    }

    /// `DRLGOUTROOM_FillBorderCornersAndPresets` (`0x00680200`).
    fn fill_border_corners_and_presets(&mut self) -> Result<(), Error> {
        if self.id == MOO_MOO_FARM {
            return Ok(());
        }
        if self.flags & 0xC != 0 && self.column_empty(self.width - 2) {
            self.vertical_border(self.width - 2)?;
        }
        if self.flags & 0x20 != 0 && self.flags & 0x40 == 0 {
            self.seed.step();
            // Both scans cover height × width; the second swaps which counter is x.
            let even = self.seed.low & 1 == 0;
            let mut found = false;
            'scan: for outer in 0..self.height {
                for inner in 0..self.width {
                    let (x, y) = if even { (inner, outer) } else { (outer, inner) };
                    let fill = match self.presets.get(x, y) {
                        0x10 => 0x19,
                        0x11 => 0x18,
                        _ => continue,
                    };
                    self.spawn(x, y, fill, -1, false)?;
                    self.flags |= 0x40;
                    found = true;
                    break 'scan;
                }
            }
            if !found {
                return Err(Error::Halt("no cliff corner to fill"));
            }
        }
        if self.flags & 0x1C != 0 && self.flags & 0x40 == 0 {
            self.seed.step();
            let low = self.seed.low;
            let x = if low & 1 == 0 { self.width - if self.flags & 0x10 != 0 { 4 } else { 5 } } else { 3 };
            let y = if (low & 3) / 2 == 0 { self.height - 4 } else { 3 };
            let piece = if self.id == BLOOD_MOOR { 0x34 } else { 0x33 };
            self.spawn(x, y, piece, -1, false)?;
            self.flags |= 0x40;
        }
        Ok(())
    }

    /// `SpawnTownTransitionsAndCaves` (`0x006803D0`).
    fn spawn_town_transitions_and_caves(&mut self) -> Result<(), Error> {
        if self.id == MOO_MOO_FARM {
            return Ok(());
        }
        if self.flags & 0x10 != 0 {
            let middle = self.width / 2 - 1;
            if self.column_empty(middle) {
                self.vertical_border(middle)?;
            }
        }
        if self.flags & 0x80 != 0 {
            self.spawn(0, 0, 3, 1, false)?;
        }
        if self.flags & 0x100 != 0 {
            self.spawn(self.width - 7, 0, 3, 2, false)?;
        }
        if self.flags & 0x200 != 0 {
            self.spawn(0, 1, 2, 1, false)?;
        }
        if self.flags & 0x400 != 0 {
            self.spawn(0, self.height - 6, 2, 1, false)?;
        }
        if self.flags & 0x40 != 0 {
            return Ok(());
        }
        if self.id == BLOOD_MOOR {
            let town = self.ctx.act.coords(self.ctx.data.levels(), ROGUE_ENCAMPMENT).ok_or(Error::NotAct1Wilderness(ROGUE_ENCAMPMENT))?;
            self.spawn_far_from(town, 0x34, -1, 1, 0xF)?;
        } else {
            self.spawn_anywhere(0x33, -1, 1, 0xF)?;
        }
        self.flags |= 0x40;
        Ok(())
    }

    /// `SpawnPresetFarAway` (`Outdoors.cpp:626`): the free interior cell farthest from `from`.
    fn spawn_far_from(&mut self, from: Coords, preset: i32, file: i32, offset: i32, sides: i32) -> Result<bool, Error> {
        let (range_x, range_y) = (self.width - 2, self.height - 2);
        let start_x = if range_x < 1 { 0 } else { self.seed.pick(range_x as u32) as i32 };
        let start_y = if range_y < 1 { 0 } else { self.seed.pick(range_y as u32) as i32 };
        if range_y <= -1 {
            return Ok(false);
        }
        let (mut best, mut best_x, mut best_y) = (0, -1, -1);
        for ly in 0..=range_y {
            if range_x <= -1 {
                continue;
            }
            let y = if range_y == 0 { return Err(Error::Halt("far placement in a two-cell level")) } else { (ly + start_y) % range_y + 1 };
            for lx in 0..=range_x {
                let x = if range_x == 0 { return Err(Error::Halt("far placement in a two-cell level")) } else { (start_x + lx) % range_x + 1 };
                if !self.fits(x, y, preset, offset, sides)? {
                    continue;
                }
                let dx = (x * 8 - (from.w / 2 + from.x) + 4 + self.area.x).abs();
                let dy = (y * 8 - (from.h / 2 + from.y) + 4 + self.area.y).abs();
                let distance = if dy < dx { dy + dx * 2 } else { dx + dy * 2 };
                if best < distance / 2 {
                    (best, best_x, best_y) = (distance / 2, x, y);
                }
            }
        }
        if best_x == -1 || best_y == -1 {
            return Ok(false);
        }
        self.spawn(best_x, best_y, preset, file, false)?;
        Ok(true)
    }

    /// `SpawnOutdoorLevelPreset` (`Outdoors.cpp:730`): the first free interior cell in a shuffle.
    fn spawn_anywhere(&mut self, preset: i32, file: i32, offset: i32, sides: i32) -> Result<bool, Error> {
        let across = self.width - 2;
        let total = (self.height - 2).wrapping_mul(across);
        if total <= 0 {
            return Ok(false);
        }
        let mut cells: Vec<(i32, i32)> = (0..total).map(|i| (i % across, i / across)).collect();
        for _ in 0..total {
            let a = self.seed.pick(total as u32) as usize;
            let b = self.seed.pick(total as u32) as usize;
            cells.swap(a, b);
        }
        for (x, y) in cells {
            if self.fits(x + 1, y + 1, preset, offset, sides)? {
                self.spawn(x + 1, y + 1, preset, file, false)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `DRLGOUTDOOR_CreateOutdoorRoomExGrid` (`0x006750F0`): the rooms. Rooms join the level's
    /// list at its head, so the list runs from the last room made.
    fn finish(self) -> Result<OutdoorLevel, Error> {
        let mut rooms = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                let flags = self.outdoor.get(x, y);
                let (tx, ty) = (self.area.x + x * ROOM_TILES, self.area.y + y * ROOM_TILES);
                if flags & PRESET == 0 {
                    if flags & BLANK == 0 {
                        rooms.push(Coords { x: tx, y: ty, w: ROOM_TILES, h: ROOM_TILES });
                    }
                    continue;
                }
                let preset = self.presets.get(x, y);
                if preset == 0 {
                    continue;
                }
                let (w, h) = self.preset_size(preset)?;
                for py in 0..h {
                    for px in 0..w {
                        rooms.push(Coords { x: tx + px * ROOM_TILES, y: ty + py * ROOM_TILES, w: ROOM_TILES, h: ROOM_TILES });
                    }
                }
            }
        }
        rooms.reverse();
        Ok(OutdoorLevel {
            id: self.id,
            area: self.area,
            width: self.width,
            height: self.height,
            flags: self.flags,
            outdoor: self.outdoor.cells(),
            presets: self.presets.cells(),
            rooms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::engine::EngineData;
    use std::path::PathBuf;

    const ACT1_WILDERNESS: [i32; 7] = [2, 3, 4, 5, 6, 7, 17];

    #[test]
    fn a_grid_reads_past_a_row_into_the_next_as_the_engine_does() {
        let mut g = Grid::new(3, 2);
        g.set(1, 1, 7);
        assert_eq!(g.get(4, 0), 7, "x 4 of row 0 is x 1 of row 1");
        g.or(3, 0, 0x100);
        assert_eq!(g.cells(), vec![0, 0, 0, 0x100, 7, 0]);
        assert_eq!(g.get(3, 1), 0, "past the allocation");
        assert!(g.slot(0, -1).is_none());
    }

    #[test]
    fn neighbours_are_found_by_the_side_they_touch_and_sorted_along_it() {
        let me = Coords { x: 100, y: 100, w: 80, h: 80 };
        assert_eq!(direction_between(me, Coords { x: 60, y: 90, w: 40, h: 40 }), Some(0), "left");
        assert_eq!(direction_between(me, Coords { x: 120, y: 60, w: 40, h: 40 }), Some(1), "top");
        assert_eq!(direction_between(me, Coords { x: 180, y: 150, w: 40, h: 40 }), Some(2), "right");
        assert_eq!(direction_between(me, Coords { x: 90, y: 180, w: 40, h: 40 }), Some(3), "bottom");
        assert_eq!(direction_between(me, Coords { x: 300, y: 300, w: 40, h: 40 }), None);
        let left = |y| Orth { direction: 0, preset: false, area: Coords { x: 60, y, w: 40, h: 40 } };
        assert!(left(90).goes_before(&left(130)), "down the left side from the top");
        let top = |x| Orth { direction: 1, preset: false, area: Coords { x, y: 60, w: 40, h: 40 } };
        assert!(top(140).goes_before(&top(100)), "along the top from the right");
        assert!(left(130).goes_before(&top(100)), "by side first");
    }

    #[test]
    fn an_edge_shared_with_a_neighbour_splits_the_outline() {
        // Blood Moor-like: 56x96 tiles with a 56x40 town above its right part, and a level on
        // its left covering the lower half.
        let me = Coords { x: 1000, y: 1000, w: 56, h: 96 };
        let town = Orth { direction: 1, preset: true, area: Coords { x: 1008, y: 960, w: 56, h: 40 } };
        let west = Orth { direction: 0, preset: false, area: Coords { x: 920, y: 1048, w: 80, h: 80 } };
        let (vertices, head) = outline(me, &[west, town]);
        let mut ring = Vec::new();
        let mut p = head;
        loop {
            let v = vertices[p];
            ring.push((v.x, v.y, v.flags));
            p = v.next;
            if p == head {
                break;
            }
        }
        // Corners in cells (6x11 inclusive), with the open edges starting where the neighbours do.
        assert_eq!(ring, vec![(0, 11, 1), (0, 6, 0), (0, 0, 0), (1, 0, 3), (6, 0, 0), (6, 11, 0)]);
    }

    /// The operator's install (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`) and a libd2 checkout
    /// (`LIBD2_DIR`) whose engine recordings the tests compare against; nothing from them is
    /// copied here.
    fn install() -> Option<(GameData, EngineData, PathBuf)> {
        let (Ok(dir), Ok(exe), Ok(libd2)) =
            (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE"), std::env::var("LIBD2_DIR"))
        else {
            return None;
        };
        let data = GameData::load(dir).expect("game data");
        let engine = EngineData::from_game_exe(&std::fs::read(exe).expect("Game.exe")).expect("1.14d tables");
        Some((data, engine, PathBuf::from(libd2).join("packages/drlg/src/golden")))
    }

    fn number(line: &str, key: &str, from: usize) -> Option<(i64, usize)> {
        let at = line.get(from..)?.find(key)? + from + key.len();
        let end = line[at..].find(|c: char| !(c.is_ascii_digit() || c == '-')).map_or(line.len(), |e| e + at);
        Some((line[at..end].parse().ok()?, end))
    }

    fn read_golden(path: &std::path::Path) -> String {
        if path.extension().is_some_and(|e| e == "gz") {
            let out = std::process::Command::new("gzip").arg("-dc").arg(path).output().expect("gzip");
            String::from_utf8(out.stdout).expect("utf-8")
        } else {
            std::fs::read_to_string(path).expect("golden file")
        }
    }

    fn generate(data: &GameData, engine: &EngineData, seed: u32, difficulty: u8) -> HashMap<i32, Result<OutdoorLevel, String>> {
        let act = Act::build(data.levels(), 0, difficulty, seed);
        let outdoors = Act1Outdoors::new(data, &engine.outdoor, &act, seed).expect("substitution maps");
        ACT1_WILDERNESS.iter().map(|&id| (id, outdoors.generate(id).map_err(|e| e.to_string()))).collect()
    }

    /// Our rooms against recorded room corners (tiles): the same set. The list order can differ
    /// where a piece placed after the port's stop (the tree of Inifuss, a ruin) makes rooms.
    fn compare(level: &Result<OutdoorLevel, String>, mut recorded: Vec<(i32, i32)>) -> Option<String> {
        let level = match level {
            Ok(level) => level,
            Err(e) => return Some(e.clone()),
        };
        let mut got: Vec<(i32, i32)> = level.rooms.iter().map(|r| (r.x, r.y)).collect();
        got.sort_unstable();
        recorded.sort_unstable();
        (got != recorded).then(|| {
            let missing: Vec<_> = recorded.iter().filter(|r| !got.contains(r)).collect();
            let extra: Vec<_> = got.iter().filter(|r| !recorded.contains(r)).collect();
            format!("{} rooms, recorded {}; voids we make rooms: {missing:?}; rooms we leave void: {extra:?}", got.len(), recorded.len())
        })
    }

    /// The rooms of every Act I wilderness level for the seeds libd2 recorded room by room.
    #[test]
    fn with_libd2_recordings_the_rooms_match_the_engine() {
        let Some((data, engine, golden)) = install() else { return };
        let (mut checked, mut wrong) = (0, Vec::new());
        for (file, seed) in [("deep_seed_1.jsonl", 1u32), ("deep_seed_2.jsonl", 2), ("deep_seed_305419896.jsonl", 305_419_896)] {
            let ours = generate(&data, &engine, seed, 0);
            for line in read_golden(&golden.join(file)).lines().filter(|l| l.contains("\"evt\":\"drlg_level\"")) {
                let Some((id, _)) = number(line, "\"levelId\":", 0) else { continue };
                let Some(level) = ours.get(&(id as i32)) else { continue };
                let rooms_at = line.find("\"rooms\":").expect("rooms");
                let mut recorded = Vec::new();
                let mut at = rooms_at;
                while let Some((x, next)) = number(line, "{\"x\":", at) {
                    let (y, next) = number(line, "\"y\":", next).unwrap();
                    recorded.push((x as i32, y as i32));
                    at = next;
                }
                checked += 1;
                if let Some(problem) = compare(level, recorded) {
                    wrong.push(format!("seed {seed} level {id}: {problem}"));
                }
            }
        }
        eprintln!("{checked} levels compared");
        assert!(checked >= 21, "only {checked} levels compared");
        assert!(wrong.is_empty(), "{} of {checked} differ:\n{}", wrong.len(), wrong.join("\n"));
    }
    /// Every recorded room of the Act I wilderness for the seeds libd2 recorded on Hell, where
    /// the rooms are listed one line each.
    #[test]
    fn with_libd2_recordings_hell_rooms_match_the_engine() {
        let Some((data, engine, golden)) = install() else { return };
        let (mut checked, mut wrong) = (0, Vec::new());
        for file in ["coll_seed1_all.jsonl.gz", "coll_seed2_all.jsonl.gz", "coll_seed17_all.jsonl.gz", "coll_seed18_all.jsonl.gz", "coll_seed777_all.jsonl.gz"] {
            let mut recorded: HashMap<(u32, u8, i32), Vec<(i32, i32)>> = HashMap::new();
            for line in read_golden(&golden.join(file)).lines().filter(|l| l.contains("\"evt\":\"drlg_coll\"")) {
                let field = |key| number(line, key, 0).map(|(v, _)| v);
                let (Some(seed), Some(diff), Some(id), Some(px), Some(py)) =
                    (field("\"seed\":"), field("\"diff\":"), field("\"levelId\":"), field("\"px\":"), field("\"py\":"))
                else {
                    continue;
                };
                if ACT1_WILDERNESS.contains(&(id as i32)) {
                    recorded.entry((seed as u32, diff as u8, id as i32)).or_default().push((px as i32 / 5, py as i32 / 5));
                }
            }
            let mut acts: HashMap<(u32, u8), HashMap<i32, Result<OutdoorLevel, String>>> = HashMap::new();
            for ((seed, diff, id), rooms) in recorded {
                let ours = acts.entry((seed, diff)).or_insert_with(|| generate(&data, &engine, seed, diff));
                checked += 1;
                if let Some(problem) = compare(&ours[&id], rooms) {
                    wrong.push(format!("{file} seed {seed} diff {diff} level {id}: {problem}"));
                }
            }
        }
        eprintln!("{checked} levels compared");
        assert!(checked >= 30, "only {checked} levels compared");
        assert!(wrong.is_empty(), "{} of {checked} differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    /// Room counts of the Act I wilderness for the 200 seeds libd2 recorded on Normal and Hell.
    #[test]
    fn with_libd2_recordings_room_counts_match_for_200_seeds() {
        let Some((data, engine, golden)) = install() else { return };
        let (mut checked, mut wrong) = (0, Vec::new());
        let mut acts: HashMap<(u32, u8), HashMap<i32, Result<OutdoorLevel, String>>> = HashMap::new();
        for file in ["coll_crc_masked_200_normal.jsonl.gz", "coll_crc_masked_200_hell.jsonl.gz"] {
            for line in read_golden(&golden.join(file)).lines().filter(|l| l.contains("\"evt\":\"drlg_coll_crc\"")) {
                let field = |key| number(line, key, 0).map(|(v, _)| v);
                let (Some(seed), Some(diff), Some(id), Some(cells)) =
                    (field("\"seed\":"), field("\"diff\":"), field("\"levelId\":"), field("\"cells\":"))
                else {
                    continue;
                };
                let id = id as i32;
                if !ACT1_WILDERNESS.contains(&id) {
                    continue;
                }
                let ours = acts.entry((seed as u32, diff as u8)).or_insert_with(|| generate(&data, &engine, seed as u32, diff as u8));
                checked += 1;
                match &ours[&id] {
                    Ok(level) if level.rooms.len() as i64 * 1600 == cells => {}
                    Ok(level) => wrong.push(format!("seed {seed} diff {diff} level {id}: {} rooms, recorded {}", level.rooms.len(), cells / 1600)),
                    Err(e) => wrong.push(format!("seed {seed} diff {diff} level {id}: {e}")),
                }
            }
        }
        eprintln!("{checked} levels compared");
        assert!(checked >= 2000, "only {checked} levels compared");
        assert!(wrong.is_empty(), "{} of {checked} differ:\n{}", wrong.len(), wrong[..wrong.len().min(40)].join("\n"));
    }
    /// With the operator's install alone: the generator runs to the end, without a halt or an
    /// unported path, for many seeds on every difficulty (`BNETCC_D2_OUTDOOR_SEEDS`, default 100).
    #[test]
    fn with_a_real_install_every_seed_generates() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(dir).expect("game data");
        let engine = EngineData::from_game_exe(&std::fs::read(exe).expect("Game.exe")).expect("1.14d tables");
        let seeds: u32 = std::env::var("BNETCC_D2_OUTDOOR_SEEDS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
        let mut failures = Vec::new();
        for i in 0..seeds {
            let seed = i.wrapping_mul(0x9E37_79B9) ^ 0x1234_5678;
            for difficulty in 0..3 {
                for (id, level) in generate(&data, &engine, seed, difficulty) {
                    if let Err(e) = level {
                        failures.push(format!("seed {seed:#x} diff {difficulty} level {id}: {e}"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures[..failures.len().min(30)].join("\n"));
    }
}
