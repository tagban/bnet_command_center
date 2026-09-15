//! Act I wilderness levels: which 8×8-tile cells the engine makes rooms, which it leaves void, and
//! what it places on them.
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
//! After the borders come the exits — a road searched cell by cell from each level transition
//! toward the middle (`DRLGOUTROOM_LinkOutdoorRoomExits`, `0x00681420`) — then the waypoint,
//! shrines and the level's set pieces (`DRLGOUTROOM_SpawnAct1LevelPresets`, `0x00680580`).
//! Rooms are listed without the per-room RNG of their creation (the room seeds and the preset
//! file draws of `DRLGPRESET_BuildArea`), which does not change which rooms there are.
//!
//! The RNG is the level's own seed (`{act start + level id, 0x29A}`); every placement draw is
//! reproduced in order, as are the engine's reads past a grid row's end (the grid is one
//! allocation, so they land in the next row).
//!
//! Ported from libd2 `packages/drlg/src/drlg/outdoors/{Outdoors,ActInit,Border,OutPlace,OutRoom,
//! OutSub}.zig`, `TileSub.zig`, `DrlgVer.zig`, `DrlgGrid.zig` and `drlg.zig` (MIT, © 2026
//! jaenster), checked against the 1.14d `Game.exe`. Lookup tables are read from the operator's
//! `Game.exe` ([`OutdoorTables`]).

use std::collections::HashMap;
use std::fmt;

use d2_data::engine::{EngineData, OutdoorTables};
use d2_data::levels::{DrlgType, Levels};
use d2_data::lvlsub::LvlSub;
use d2_data::GameData;
use d2_formats::ds1::{Ds1, SubstGroup, UnitKind};

use crate::act::Act;
use crate::collision::{self, BuiltRoom, RoomCollision, TileSources};
use crate::preset::ROOM_TILES;
use crate::room_tiles::{self, PresetWindow, RoomContext, Seams, Substitution, Warps};
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
/// Outdoor cell flag: on a road.
const ROAD: i32 = 0x80;
/// Outdoor cell flag: the waypoint's cell.
const WAYPOINT: i32 = 0x800;
/// Outdoor cell flag: a shrine's cell.
const SHRINE: i32 = 0x1000;
/// Level ids the Act I outdoor generator singles out.
const ROGUE_ENCAMPMENT: i32 = 1;
const BLOOD_MOOR: i32 = 2;
const COLD_PLAINS: i32 = 3;
const STONY_FIELD: i32 = 4;
const DARK_WOOD: i32 = 5;
const BLACK_MARSH: i32 = 6;
const TAMOE_HIGHLAND: i32 = 7;
const BURIAL_GROUNDS: i32 = 17;
const MONASTERY_GATE: i32 = 26;
const MOO_MOO_FARM: i32 = 39;
/// Nodes in the road search's pool.
const PATH_NODES: usize = 900;
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
    /// Link flags per cell, row by row: visibility toward neighbours, shrine styles, waypoint.
    pub link: Vec<i32>,
    /// Its rooms, in the level's room list order.
    pub rooms: Vec<OutdoorRoom>,
    /// The roads from its exits, as the lines of world tiles their jittered vertices make
    /// (`pAdjacentVertices`).
    pub roads: Vec<Vec<(i32, i32)>>,
}

/// A generated level's rooms, built: their collision maps and the units their init placed, both
/// in [`OutdoorLevel::rooms`] order.
#[derive(Debug, Clone)]
pub struct BuiltLevel {
    /// Each room's collision map.
    pub collision: Vec<RoomCollision>,
    /// Each room's units, in placement order.
    pub units: Vec<Vec<RoomUnit>>,
    /// The warp tiles of its set pieces.
    pub warps: Vec<room_tiles::WarpTile>,
}

/// A unit a wilderness room's init places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomUnit {
    /// Monster or object.
    pub kind: UnitKind,
    /// Engine class id: a `MonStats.txt` row, or an `objects.txt` row.
    pub class: i32,
    /// Mode the unit starts in.
    pub mode: i32,
    /// World position in subtiles.
    pub x: i32,
    /// World position in subtiles.
    pub y: i32,
}

/// A wilderness room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutdoorRoom {
    /// Its rectangle in tiles.
    pub area: Coords,
    /// The preset piece it is part of (`LvlPrest.txt` `Def`), 0 for a plain cell.
    pub preset: i32,
    /// The piece's file index.
    pub file: i32,
    /// Its cell's outdoor flags (of the piece's anchor cell for a piece).
    pub outdoor: i32,
    /// Its cell's link flags (the anchor's for a piece).
    pub link: i32,
    /// `nSeed`: the room's own RNG seed, from which its tiles and units are rolled.
    pub seed: u32,
    /// `nSubThemePicked`: which rows of the level's terrain group a plain room uses
    /// (`DRLGROOMEX_RollLevelSubstitutionMask`), one bit per row.
    pub sub_picks: u32,
    /// `sSeed`'s low word once the room is made (after the terrain rolls); its init starts
    /// again from [`OutdoorRoom::seed`].
    pub seed_after: u32,
    /// A piece room's piece corner in world tiles (the room's own corner for a plain room).
    pub origin: (i32, i32),
    /// Vis slots whose warp tile lies in the room, by bit (`DRLGPRESET_BuildPresetArea`'s scan
    /// of a scanned piece's walls).
    pub warp_slots: u8,
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
    engine: &'a EngineData,
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
    pub fn new(data: &'a GameData, engine: &'a EngineData, act: &'a Act, game_seed: u32) -> Result<Self, Error> {
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
        Ok(Self { data, engine, tables: &engine.outdoor, act, start_seed: rng::act_start_seed(game_seed), warps, borders })
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
        let mut g = Generator::new(self, id, area, self.orths(levels, id, area));
        (g.vertices, g.head) = outline(area, &g.orths);

        // InitAct1OutdoorLevel (0x006807F0)
        g.road_flags();
        if !matches!(id, BLOOD_MOOR | COLD_PLAINS | BURIAL_GROUNDS) {
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
            g.link_exits()?;
        }
        if id == MOO_MOO_FARM {
            for group in 0..=3 {
                g.secondary_border(group)?;
            }
        }
        if (3..7).contains(&id) {
            g.spawn_waypoint();
        }
        if (2..8).contains(&id) {
            g.spawn_shrines(5);
        }
        g.spawn_level_presets()?;
        g.finish()
    }

    /// A generated level's rooms as the engine inits them: each room's tiles built in list order
    /// — a piece room from its map, a plain room from its init (`DRLGOUTROOM_InitGridCells`,
    /// `0x0067D2D0`: the floor, the roads' edges cut in (`0x00680C80`), then the waypoint, shrine
    /// and terrain pieces placed on the room's own seed) — with the units those pieces bring,
    /// and every room's collision map ([`collision::level_collision`]).
    ///
    /// # Errors
    ///
    /// [`Error`] if the level, a piece's row or a map is not in the install, or a substitution
    /// takes a path not ported.
    pub fn build_rooms(&self, sources: &TileSources, level: &OutdoorLevel) -> Result<BuiltLevel, Error> {
        let data = self.data;
        let def = data.levels().get(level.id).ok_or(Error::NotAct1Wilderness(level.id))?;
        let types = sources.tiles.level_type(data, def.level_type);
        let warp_ids = self.warps.get(&level.id).map_or(def.warp, |(_, w)| *w);
        // DRLGOUTDOOR_CreateOutdoorRoomExGrid (0x006750F0): a plain room's DT1 mask before its
        // terrain rows add theirs.
        let base_mask: u32 = match def.level_type {
            2 => 0x4_4103,
            0x10 | 0x16 | 0x1B | 0x1C => 1,
            0x15 => 4,
            0x1E | 0x1F => 0x11,
            _ => 0,
        };
        let terrain = if def.sub_type == -1 { &[][..] } else { data.lvl_subs().group(def.sub_type) };
        let mut seams = Seams::new();
        let mut built = Vec::with_capacity(level.rooms.len());
        let mut units = Vec::with_capacity(level.rooms.len());
        let mut warps = Vec::new();
        for (index, room) in level.rooms.iter().enumerate() {
            if room.preset != 0 {
                let row = data.lvl_prests().by_def(room.preset).ok_or(Error::NoPreset(room.preset))?;
                let file = row.file_for(room.file).ok_or(Error::NoPreset(room.preset))?;
                let map = sources.maps.get(data, file).ok_or_else(|| Error::MissingMap(file.to_string()))?;
                let library = types.room(row.dt1_mask);
                // DRLGROOMEX_LinkNearRoomsByVis (0x0066C2A0) for each warp slot, newest node first.
                let nodes = (0..8)
                    .rev()
                    .filter(|&s| room.warp_slots >> s & 1 != 0 && warp_ids[s] != -1)
                    .map(|s| data.lvl_warps().setup(warp_ids[s], b'b'))
                    .collect();
                let ctx = RoomContext {
                    library: &library,
                    tables: &self.engine.tiles,
                    level: level.id,
                    rect: room.area,
                    seed: room.seed,
                    seams: Some(&mut seams),
                    warps: Some(Warps { table: data.lvl_warps(), ids: warp_ids, nodes }),
                };
                let window = PresetWindow { origin: room.origin, size: row.size, fill_blanks: row.fill_blanks, kill_edge: row.kill_edge };
                let (tiles, cells) = room_tiles::preset_room_with_warps(ctx, &map, window);
                warps.extend(room_tiles::warp_tiles(index, room.area, &cells, room.warp_slots));
                built.push(BuiltRoom { area: room.area, tiles, preset: true });
                units.push(Vec::new());
                continue;
            }
            let mask = terrain
                .iter()
                .enumerate()
                .filter(|(i, _)| room.sub_picks >> (i & 31) & 1 != 0)
                .fold(base_mask, |m, (_, r)| m | r.dt1_mask as u32);
            let library = types.room(mask);
            let mut init = RoomInit::new(room);
            init.cut_roads(&level.roads, &self.tables.edge_orientations);
            // SubTypeWpShrine (0x006707A0) three times: waypoint, shrine, terrain.
            let (waypoints, shrines) = ((room.link >> 16) & 3, (room.link >> 12) & 0xF);
            let mut plan = Vec::new();
            if waypoints != 0 && def.sub_waypoint != -1 {
                plan.push((def.sub_waypoint, 0usize, waypoints as u32));
            }
            if shrines != 0 && def.sub_shrine != -1 {
                plan.push((def.sub_shrine, 0, shrines as u32));
            }
            if def.sub_type != -1 && def.sub_theme >= 0 && room.sub_picks != 0 {
                plan.push((def.sub_type, def.sub_theme as usize, room.sub_picks));
            }
            let mut maps = Vec::with_capacity(plan.len());
            for &(group, _, picks) in &plan {
                let mut row_maps = Vec::new();
                for (i, row) in data.lvl_subs().from_group(group).iter().enumerate().take(32) {
                    if picks >> i & 1 == 0 {
                        row_maps.push(None);
                        continue;
                    }
                    let map = sources.maps.get(data, &row.file).ok_or_else(|| Error::MissingMap(row.file.clone()))?;
                    if map.subst_groups.is_empty() {
                        return Err(Error::Halt("a substitution map without groups"));
                    }
                    if row.check_all {
                        return Err(Error::NotPorted("a room substitution that checks every position"));
                    }
                    row_maps.push(Some(map));
                }
                maps.push(row_maps);
            }
            let passes: Vec<Substitution<'_>> = plan
                .iter()
                .zip(&maps)
                .map(|(&(group, theme, picks), row_maps)| Substitution {
                    rows: data.lvl_subs().from_group(group),
                    maps: row_maps.iter().map(|m| m.as_deref()).collect(),
                    theme,
                    picks,
                })
                .collect();
            let ctx = RoomContext { library: &library, tables: &self.engine.tiles, level: level.id, rect: room.area, seed: room.seed, seams: Some(&mut seams), warps: None };
            let (tiles, stamps) = room_tiles::outdoor_room(ctx, init.floor_grid(), 0, &passes);
            units.push(stamps.iter().flat_map(|st| self.group_units((room.area.x, room.area.y), st.x, st.y, st.group, st.map)).collect());
            built.push(BuiltRoom { area: room.area, tiles, preset: false });
        }
        let void = if def.level_type == 19 { 0x01 } else { 0x05 };
        Ok(BuiltLevel { collision: collision::level_collision(level.id, &built, &seams, void), units, warps })
    }

    /// `0x0066FA10`: the map's units strictly inside the group's box, moved to where the box
    /// landed in the room. The engine walks the map's units newest first.
    fn group_units(&self, origin: (i32, i32), x: i32, y: i32, group: SubstGroup, map: &Ds1) -> Vec<RoomUnit> {
        let s = crate::preset::SUBTILES;
        let (bx, by, bw, bh) = (group.x * s, group.y * s, group.w * s, group.h * s);
        let act = u8::try_from(map.act).unwrap_or(0);
        map.units
            .iter()
            .rev()
            .filter(|u| bx < u.x && by < u.y && u.x < bx + bw && u.y < by + bh)
            .filter_map(|u| {
                let (class, mode) = match u.kind {
                    UnitKind::Monster => (self.data.mon_presets().engine_class(map.act, u.id), 1),
                    UnitKind::Object => (self.engine.preset_object_class(act, u.id)?, 0),
                    UnitKind::Other(_) => return None,
                };
                (class >= 0).then_some(RoomUnit {
                    kind: u.kind,
                    class,
                    mode,
                    x: origin.0 * s + x * s + u.x - bx,
                    y: origin.1 * s + y * s + u.y - by,
                })
            })
            .collect()
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
            let orth = Orth { level: vis[i], direction, preset, area: other };
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
    level: i32,
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

/// `DRLGGRID_SetEdgeGridFlags` (`0x0067C760`) with OR, endpoints included: the cells from
/// vertex `e` to the next vertex `n`.
fn edge_cells(grid: &mut Grid, e: Vertex, n: Vertex, flag: i32) {
    if e.x == n.x {
        if e.y == n.y {
            grid.or(e.x, e.y, flag);
            return;
        }
        let (mut y, end) = if n.y <= e.y { (n.y + 1, e.y) } else { (e.y + 1, n.y) };
        while y != end {
            grid.or(e.x, y, flag);
            y += 1;
        }
    } else {
        let (mut x, end) = if e.x < n.x { (e.x + 1, n.x) } else { (n.x + 1, e.x) };
        while x != end {
            grid.or(x, e.y, flag);
            x += 1;
        }
    }
    grid.or(e.x, e.y, flag);
    grid.or(n.x, n.y, flag);
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
    /// `sGridLink`.
    link: Grid,
    orths: Vec<Orth>,
    vertices: Vec<Vertex>,
    head: usize,
    trackers: Vec<FileTracker>,
    /// `aExitPoints1`..`4`: each exit, its snapped start, its snapped target, its target.
    exits: [[ExitPoint; 6]; 4],
    exit_count: usize,
    roads: Vec<Vec<(i32, i32)>>,
    last_room_seed: u32,
}

/// A road end (`D2DrlgExitPointStrc`): world tiles and a side (0..=3; 4 for none or the middle).
#[derive(Debug, Clone, Copy, Default)]
struct ExitPoint {
    x: i32,
    y: i32,
    kind: u8,
}

impl<'a, 'b> Generator<'a, 'b> {
    fn new(ctx: &'b Act1Outdoors<'a>, id: i32, area: Coords, orths: Vec<Orth>) -> Self {
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
            link: Grid::new(width, height),
            orths,
            vertices: Vec::new(),
            head: 0,
            trackers: Vec::new(),
            exits: [[ExitPoint::default(); 6]; 4],
            exit_count: 0,
            roads: Vec::new(),
            last_room_seed: 0,
        }
    }

    /// The level's `Vis` slots as the act wired them (`DRLGROOM_GetVisArrayFromLevelId`).
    fn vis(&self) -> [i32; 8] {
        self.ctx.warps.get(&self.id).map_or_else(|| self.ctx.data.levels().get(self.id).map_or([0; 8], |d| d.vis), |w| w.0)
    }

    /// `DRLGOUTDOOR_GetAdjacentLevelVisMask` (`Outdoors.cpp:360`): the link bit of a level this
    /// one sees.
    fn vis_mask(&self, level: i32) -> i32 {
        self.vis().iter().position(|&v| v == level).map_or(0, |i| 1 << ((i + 4) & 0x1F))
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

    /// `SetOutGridLinkFlags` (`0x00675770`): each open edge's cells get the neighbour's link bit
    /// and the edge's direction code.
    fn outline_flags(&mut self) {
        for i in self.ring() {
            let e = self.vertices[i];
            if e.flags & 1 != 0 {
                let n = self.vertices[e.next];
                let vis = self.link_vis_flag(e);
                edge_cells(&mut self.link, e, n, vis);
                edge_cells(&mut self.outdoor, e, n, e.direction * 2 + 1);
            }
        }
    }

    /// `GetOutLinkVisFlag` (`Outdoors.cpp:380`): the link bit of the neighbour an open edge's
    /// vertex looks into.
    fn link_vis_flag(&self, v: Vertex) -> i32 {
        let (right, bottom) = (self.width - 1, self.height - 1);
        let edge = if v.x == 0 {
            i32::from(v.y == 0)
        } else if v.y == 0 {
            i32::from(v.x == right) + 1
        } else if v.x == right {
            i32::from(v.y == bottom) + 2
        } else if v.y == bottom {
            3
        } else {
            return 0;
        };
        let (ox, oy) = self.ctx.tables.link_offsets[edge as usize];
        let (px, py) = (ox + v.x * 8 + self.area.x, oy + v.y * 8 + self.area.y);
        let inside = |c: Coords| px >= c.x && py >= c.y && px < c.x + c.w && py < c.y + c.h;
        self.orths.iter().find(|o| o.direction == edge && inside(o.area)).map_or(0, |o| self.vis_mask(o.level))
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

    /// The interior cells (the grid less its outer ring), 0-based from `(1, 1)`, in the order the
    /// engine's shuffle leaves them: two draws a swap, as many swaps as cells.
    fn shuffled_interior(&mut self) -> Vec<(i32, i32)> {
        let across = self.width - 2;
        let total = (self.height - 2).wrapping_mul(across);
        if total <= 0 {
            return Vec::new();
        }
        let mut cells: Vec<(i32, i32)> = (0..total).map(|i| (i % across, i / across)).collect();
        for _ in 0..total {
            let a = self.seed.pick(total as u32) as usize;
            let b = self.seed.pick(total as u32) as usize;
            cells.swap(a, b);
        }
        cells
    }

    /// `SpawnOutdoorLevelPreset` (`Outdoors.cpp:730`): the first free interior cell in a shuffle.
    fn spawn_anywhere(&mut self, preset: i32, file: i32, offset: i32, sides: i32) -> Result<bool, Error> {
        for (x, y) in self.shuffled_interior() {
            if self.fits(x + 1, y + 1, preset, offset, sides)? {
                self.spawn(x + 1, y + 1, preset, file, false)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `SpawnRandomOutdoorDS1` (`0x006745E0`): beside a road cell if a neighbour fits, else
    /// anywhere.
    fn spawn_near_road(&mut self, preset: i32, file: i32) -> Result<(), Error> {
        let cells = self.shuffled_interior();
        if cells.is_empty() {
            return Ok(());
        }
        for (x, y) in cells {
            let (gx, gy) = (x + 1, y + 1);
            if self.outdoor.get(gx, gy) & ROAD == 0 {
                continue;
            }
            for (dx, dy) in self.ctx.tables.neighbours {
                if self.fits(gx + dx, gy + dy, preset, 0, 0xF)? {
                    return self.spawn(gx + dx, gy + dy, preset, file, false);
                }
            }
        }
        self.spawn_anywhere(preset, file, 0, 0xF).map(|_| ())
    }

    /// `DRLGOUTROOM_SpawnRandomOutdoorDecorations` (`0x006804E0`): one or two of a piece by the
    /// roads, sometimes a camp too.
    fn decorations(&mut self, preset: i32, allow_camp: bool) -> Result<(), Error> {
        self.seed.step();
        if self.seed.low & 3 == 0 {
            self.spawn_near_road(preset, -1)?;
            self.spawn_near_road(preset, -1)?;
        } else {
            self.spawn_near_road(preset, -1)?;
            if allow_camp {
                self.seed.step();
                if self.seed.low & 1 != 0 {
                    self.spawn_near_road(0x31, -1)?;
                }
            }
        }
        Ok(())
    }

    /// `DRLGOUTROOM_SpawnAct1LevelPresets` (`0x00680580`): each level's set pieces.
    fn spawn_level_presets(&mut self) -> Result<(), Error> {
        let anywhere = |g: &mut Self, preset| g.spawn_anywhere(preset, -1, 0, 0xF).map(|_| ());
        match self.id {
            BLOOD_MOOR => {
                self.spawn_near_road(0x2E, -1)?;
                self.decorations(0x2F, false)?;
            }
            COLD_PLAINS => {
                self.decorations(0x30, true)?;
                anywhere(self, 0x2C)?;
            }
            STONY_FIELD => {
                self.spawn_near_road(0xA0, -1)?;
                self.spawn_near_road(0x2D, -1)?;
                anywhere(self, 0xA2)?;
                self.decorations(0x2F, true)?;
                self.decorations(0x2A, false)?;
                return anywhere(self, 0x1F);
            }
            DARK_WOOD => {
                for preset in [0xA1, 0x29, 0x28] {
                    anywhere(self, preset)?;
                }
                self.decorations(0x30, true)?;
                self.decorations(0x2B, false)?;
            }
            BLACK_MARSH => {
                for preset in [0xA3, 0x26, 0x27] {
                    anywhere(self, preset)?;
                }
                self.decorations(0x2F, true)?;
                self.decorations(0x2A, false)?;
            }
            TAMOE_HIGHLAND => {
                self.decorations(0x30, true)?;
                self.decorations(0x2B, false)?;
                return anywhere(self, 0x1F);
            }
            BURIAL_GROUNDS => return self.spawn(1, 1, 0x6C, -1, false),
            MOO_MOO_FARM => {
                for preset in [0x32, 0x2E, 0x1F, 0x26, 0x27] {
                    anywhere(self, preset)?;
                }
            }
            _ => {}
        }
        anywhere(self, 0x1D)?;
        anywhere(self, 0x1E)
    }

    /// `SpawnAct12Waypoint` (`0x006752A0`): Cold Plains' faces Blood Moor; elsewhere the first
    /// free interior cell in a shuffle.
    fn spawn_waypoint(&mut self) {
        if self.id == COLD_PLAINS {
            let at = self.vis().iter().position(|&v| v == BLOOD_MOOR).unwrap_or(8);
            let mask = 1i32 << ((at + 4) & 0x1F);
            for y in 0..self.height {
                for x in 0..self.width {
                    if self.link.get(x, y) & mask != 0 && self.outdoor.get(x, y) & RESERVED != 0 {
                        let (mut wx, mut wy) = (x, y);
                        if wx == 0 {
                            wx = 1;
                        }
                        if wy == 0 {
                            wy = 1;
                        }
                        if wx == self.width - 1 {
                            wx -= 1;
                        }
                        if wy == self.height - 1 {
                            wy -= 1;
                        }
                        self.link.or(wx, wy, 0x2_0000);
                        self.outdoor.or(wx, wy, WAYPOINT);
                        return;
                    }
                }
            }
        }
        for (x, y) in self.shuffled_interior() {
            if self.outdoor.get(x + 1, y + 1) & OCCUPIED == 0 {
                self.link.or(x + 1, y + 1, 0x1_0000);
                self.outdoor.or(x + 1, y + 1, WAYPOINT);
                return;
            }
        }
    }

    /// `SpawnAct12Shrines` (`0x00674E40`): up to `count` shrines on free interior cells, their
    /// styles taken in turn from a random one.
    fn spawn_shrines(&mut self, count: i32) {
        let mut style = (self.seed.roll() & 3) as usize;
        let mut left = count;
        for (x, y) in self.shuffled_interior() {
            if left < 1 {
                return;
            }
            if self.outdoor.get(x + 1, y + 1) & OCCUPIED == 0 {
                self.link.or(x + 1, y + 1, self.ctx.tables.shrine_styles[style]);
                self.outdoor.or(x + 1, y + 1, SHRINE);
                style = (style + 1) & 3;
                left -= 1;
            }
        }
    }

    /// `DRLGOUTROOM_LinkOutdoorRoomExits` (`0x00681420`): a road from each level transition
    /// toward the middle, marked on the cells it crosses.
    fn link_exits(&mut self) -> Result<(), Error> {
        self.build_exit_points();
        self.exit_targets();
        for i in 0..self.exit_count {
            if let Some(path) = self.find_path(i)? {
                for &(x, y) in &path {
                    if x >= 0 && x < self.width && y >= 0 && y < self.height {
                        self.outdoor.or(x, y, ROAD);
                    }
                }
                let road = self.jitter_road(i, &path);
                self.roads.push(road);
            }
        }
        Ok(())
    }

    /// `DRLGOUTROOM_BuildVertexPathsWithJitter` (`0x00681240`): the road's vertices in world
    /// tiles — the target's outer point, the snapped target, each cell between nudged off its
    /// centre in a turning direction, the snapped start and the exit itself.
    fn jitter_road(&mut self, i: usize, path: &[(i32, i32)]) -> Vec<(i32, i32)> {
        let [exit, start, target, outer] = [self.exits[0][i], self.exits[1][i], self.exits[2][i], self.exits[3][i]];
        let mut turn = (self.seed.roll() & 3) as usize;
        let mut road = Vec::with_capacity(path.len() + 2);
        if outer.kind != 4 {
            road.push((outer.x, outer.y));
        }
        road.push((target.x, target.y));
        if path.len() < 2 {
            return road;
        }
        for &(cx, cy) in &path[1..path.len() - 1] {
            let (jx, jy) = self.ctx.tables.jitter[turn];
            let x = self.area.x + cx * 8 + 3 + ((self.seed.roll() & 1) as i32 + 2) * jx;
            let y = self.area.y + cy * 8 + 3 + ((self.seed.roll() & 1) as i32 + 2) * jy;
            turn = (turn + 1) & 3;
            road.push((x, y));
        }
        road.push((start.x, start.y));
        road.push((exit.x, exit.y));
        road
    }

    /// `DRLGOUTROOM_SnapVertexToGrid` (`0x00680CC0`): an exit's cell edge on its side.
    fn snap(&self, p: ExitPoint) -> (i32, i32) {
        let (mut x, mut y) = (p.x - self.area.x, p.y - self.area.y);
        match p.kind {
            0 => x = x / 8 * 8 + 11,
            1 => y = y / 8 * 8 + 11,
            2 => x = x / 8 * 8 - 5,
            3 => y = y / 8 * 8 - 5,
            _ => {}
        }
        (x + self.area.x, y + self.area.y)
    }

    /// `DRLGOUTROOM_BuildExitPointArray` (`0x00680D70`): the camp's and the Monastery Gate's
    /// openings, then the road pieces on the grid.
    fn build_exit_points(&mut self) {
        self.exit_count = 0;
        for i in 0..self.orths.len() {
            let orth = self.orths[i];
            if self.exit_count >= 6 {
                continue;
            }
            let (ax, ay) = (orth.area.x, orth.area.y);
            self.exits[0][self.exit_count] = match orth.level {
                ROGUE_ENCAMPMENT => {
                    let (dx, dy) = match orth.direction {
                        0 => (0x3B, 0x13),
                        1 => (0x1D, 0x23),
                        2 => (4, 0x16),
                        _ => (0x1D, 3),
                    };
                    ExitPoint { x: ax + dx, y: ay + dy, kind: orth.direction as u8 }
                }
                MONASTERY_GATE => ExitPoint { x: ax + 0x1B, y: ay + 0xD, kind: 1 },
                _ => continue,
            };
            self.exit_count += 1;
        }
        for gx in 0..self.width {
            for gy in 0..self.height {
                if self.exit_count >= 6 {
                    break;
                }
                let flow = (self.outdoor.get(gx, gy) >> 16) & 0xF;
                let kind = match self.presets.get(gx, gy) {
                    4 if flow == 3 => 3,
                    5 if flow == 3 => 0,
                    6 if flow == 3 => 1,
                    7 if flow == 3 => 2,
                    0x18 => 1,
                    0x19 => 0,
                    0x1C if flow == 1 && gx == self.width - 2 => 2,
                    0x33 | 0x34 => u8::from(flow != 0),
                    _ => 4,
                };
                self.exits[0][self.exit_count] = ExitPoint { x: self.area.x + gx * 8 + 3, y: self.area.y + gy * 8 + 3, kind };
                if kind != 4 {
                    self.exit_count += 1;
                }
            }
        }
        for i in 0..self.exit_count {
            (self.exits[1][i].x, self.exits[1][i].y) = self.snap(self.exits[0][i]);
        }
    }

    /// `DRLGOUTROOM_ComputeExitTargetPositions` (`0x00681000`): where each road heads — across
    /// the river's crossing when the level has one, else a free cell near the exits' middle.
    fn exit_targets(&mut self) {
        let (wx, wy) = (self.area.x, self.area.y);
        let crossing = if self.flags & 0x10 != 0 {
            let x = self.width / 2 - 1;
            (1..self.width - 1)
                .find(|&y| self.presets.get(x, y) == 0x1C && (self.outdoor.get(x, y) >> 16) & 0xF == 1)
                .map(|y| (x, y))
        } else {
            None
        };
        if let Some((cx, cy)) = crossing {
            let (bx, by) = (wx + 3 + cx * 8, wy + 3 + cy * 8);
            for i in 0..self.exit_count {
                let right = self.exits[0][i].x > bx;
                self.exits[3][i] = ExitPoint { x: if right { bx + 8 } else { bx }, y: by, kind: if right { 0 } else { 2 } };
            }
        } else if self.exit_count > 0 {
            let (cx, cy) = if self.exit_count == 1 {
                (self.width / 2, self.height / 2)
            } else {
                let n = self.exit_count as i32 * 8;
                let sx: i32 = self.exits[0][..self.exit_count].iter().map(|e| e.x - wx).sum();
                let sy: i32 = self.exits[0][..self.exit_count].iter().map(|e| e.y - wy).sum();
                (sx / n, sy / n)
            };
            let (mut fx, mut fy) = (0, 0);
            'search: for radius in 0..8 {
                for (ox, oy) in self.ctx.tables.spiral {
                    (fx, fy) = (ox * radius + cx, oy * radius + cy);
                    if fx >= 0 && fx < self.width && fy >= 0 && fy < self.height && self.outdoor.get(fx, fy) & OCCUPIED == 0 {
                        break 'search;
                    }
                }
            }
            let target = ExitPoint { x: wx + fx * 8 + 3, y: wy + fy * 8 + 3, kind: 4 };
            for i in 0..self.exit_count {
                self.exits[3][i] = target;
            }
        }
        for i in 0..self.exit_count {
            (self.exits[2][i].x, self.exits[2][i].y) = self.snap(self.exits[3][i]);
        }
    }

    fn path_delta(&self, i: i32) -> Result<i32, Error> {
        usize::try_from(i).ok().and_then(|i| self.ctx.tables.path_deltas.get(i)).copied().ok_or(Error::Halt("the road search ran off its direction table"))
    }

    /// `DRLGPATH_GetPathDirection`: the direction (0..=7) from one cell toward another.
    fn path_direction(&self, from: (i32, i32), to: (i32, i32)) -> i32 {
        self.ctx.tables.path_directions[direction_index(to.0 - from.0, to.1 - from.1) as usize]
    }

    /// `DRLGOUTROOM_FindPathBetweenExits` (`0x006817D0`): a depth-first road search from an
    /// exit's cell to its target's, bounded by a cost that grows by 5 each try. The cells from
    /// the target back to the start, or `None`.
    fn find_path(&self, i: usize) -> Result<Option<Vec<(i32, i32)>>, Error> {
        let cell = |p: ExitPoint| ((p.x - self.area.x) / 8, (p.y - self.area.y) / 8);
        let (start, target) = (cell(self.exits[1][i]), cell(self.exits[2][i]));
        let (adx, ady) = ((start.0 - target.0).abs(), (start.1 - target.1).abs());
        if adx + ady <= 1 {
            return Ok(Some(vec![start, target]));
        }
        let h = adx.min(ady) + adx.max(ady) * 2;
        let direction = (self.path_direction(start, target) / 2) & 3;
        let mut bound = h / 2 + h;
        let limit = bound + 0x23;
        let mut pool = vec![[0i32; 10]; PATH_NODES];
        loop {
            // Node: [f, h, g, x, y, tries, cycle index, direction, parent + 1, child + 1].
            pool[0] = [h, h, 0, start.0, start.1, -1, 0, direction, 0, 0];
            let mut used = 1;
            let found = self.path_search(&mut pool, &mut used, target, bound)?;
            bound += 5;
            if used > PATH_NODES - 1 {
                return Ok(None);
            }
            if let Some(mut node) = found {
                let mut path = Vec::new();
                loop {
                    path.push((pool[node][3], pool[node][4]));
                    if pool[node][8] == 0 {
                        return Ok(Some(path));
                    }
                    node = (pool[node][8] - 1) as usize;
                }
            }
            if bound >= limit {
                return Ok(None);
            }
        }
    }

    /// `DRLGOUTROOM_PathSearchStep`: expand from the root until the target or exhaustion.
    fn path_search(&self, pool: &mut [[i32; 10]], used: &mut usize, target: (i32, i32), bound: i32) -> Result<Option<usize>, Error> {
        let mut idx = 0;
        loop {
            let node = pool[idx];
            if (node[3], node[4]) == target {
                return Ok(Some(idx));
            }
            let nx = self.path_delta(node[7] + 0x14)? + node[3];
            let ny = self.path_delta(node[7] + 0x10)? + node[4];
            if self.path_step_ok(pool, (nx, ny), idx, target) {
                let step = if node[3] == nx || node[4] == ny { 2 } else { 3 };
                let (a, b) = ((nx - target.0).abs(), (ny - target.1).abs());
                let h = a.min(b) + a.max(b) * 2;
                let f = h + node[2] + step;
                if f <= bound {
                    if pool[idx][9] == 0 {
                        let slot = *used;
                        if slot == PATH_NODES {
                            return Ok(None);
                        }
                        pool[slot] = [0; 10];
                        *used += 1;
                        pool[idx][9] = slot as i32 + 1;
                        pool[slot][8] = idx as i32 + 1;
                    }
                    let child = (pool[idx][9] - 1) as usize;
                    let parent = (pool[child][8] - 1) as usize;
                    let half = self.path_direction((nx, ny), target) / 2;
                    let cycle = (pool[parent][7] - half) & 3;
                    let turn = self.path_delta(cycle * 4)?;
                    let c = &mut pool[child];
                    (c[1], c[0], c[2], c[5]) = (h, f, node[2] + step, 0);
                    (c[6], c[7], c[3], c[4]) = (cycle * 4, (turn + half) & 3, nx, ny);
                    idx = child;
                    continue;
                }
            }
            match self.path_advance(pool, idx)? {
                Some(next) => idx = next,
                None => return Ok(None),
            }
        }
    }

    /// `DRLGOUTROOM_ValidatePathStep`: the target, or an in-grid cell off any piece and off the
    /// path so far.
    fn path_step_ok(&self, pool: &[[i32; 10]], (x, y): (i32, i32), mut idx: usize, target: (i32, i32)) -> bool {
        if (x, y) == target {
            return true;
        }
        if x < 0 || x >= self.width || y < 0 || y >= self.height || self.outdoor.get(x, y) & PRESET != 0 {
            return false;
        }
        loop {
            if (pool[idx][3], pool[idx][4]) == (x, y) {
                return false;
            }
            if pool[idx][8] == 0 {
                return true;
            }
            idx = (pool[idx][8] - 1) as usize;
        }
    }

    /// `DRLGOUTROOM_AdvancePathDirection`: the node's next direction, or back up the path when its
    /// three are spent.
    fn path_advance(&self, pool: &mut [[i32; 10]], mut idx: usize) -> Result<Option<usize>, Error> {
        if pool[idx][5] < 4 {
            pool[idx][6] += 1;
            let turn = self.path_delta(pool[idx][6])?;
            pool[idx][7] = (turn + pool[idx][7]) & 3;
        }
        pool[idx][5] += 1;
        if pool[idx][5] != 3 {
            return Ok(Some(idx));
        }
        while idx != 0 {
            idx = (pool[idx][8] - 1) as usize;
            pool[idx][6] += 1;
            let turn = self.path_delta(pool[idx][6])?;
            pool[idx][5] += 1;
            pool[idx][7] = (turn + pool[idx][7]) & 3;
            if pool[idx][5] != 3 {
                return Ok(Some(idx));
            }
        }
        Ok(None)
    }

    /// `DRLGOUTDOOR_CreateOutdoorRoomExGrid` (`0x006750F0`): the rooms, each taking its seed
    /// from the level's (`DRLGROOM_AllocRoomEx`, `0x0066B3F0`). A piece's anchor cell draws a
    /// map file (then replaced by the one its cell holds), reads the map's units if the piece is
    /// scanned (`DRLGPRESET_BuildPresetArea`), and cuts the piece into 8×8 rooms from its corner
    /// (`DRLGPRESET_BuildArea`). Rooms join the level's list at its head, so the list runs from
    /// the last room made.
    fn finish(mut self) -> Result<OutdoorLevel, Error> {
        let mut rooms = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                let (outdoor, link) = (self.outdoor.get(x, y), self.link.get(x, y));
                let (tx, ty) = (self.area.x + x * ROOM_TILES, self.area.y + y * ROOM_TILES);
                if outdoor & PRESET == 0 {
                    if outdoor & BLANK == 0 {
                        let mut state = self.room_seed();
                        let area = Coords { x: tx, y: ty, w: ROOM_TILES, h: ROOM_TILES };
                        let sub_picks = self.sub_picks(&mut state);
                        let (seed, seed_after) = (self.last_room_seed, state.low);
                        rooms.push(OutdoorRoom { area, preset: 0, file: 0, outdoor, link, seed, sub_picks, seed_after, origin: (tx, ty), warp_slots: 0 });
                    }
                    continue;
                }
                let preset = self.presets.get(x, y);
                if preset == 0 {
                    continue;
                }
                let ctx = self.ctx;
                let row = ctx.data.lvl_prests().by_def(preset).ok_or(Error::NoPreset(preset))?;
                if row.file_count >= 1 {
                    self.seed.step();
                }
                let file = (outdoor >> 16) & 0xF;
                let (w, h) = row.size;
                let mut warp_cells = HashMap::new();
                if row.scan || row.pops != 0 {
                    if let Some(map) = self.piece_units(row.file_for(file))? {
                        if row.scan && map.width - 1 == w && map.height - 1 == h {
                            warp_cells = scan_warps(&map);
                        }
                    }
                }
                for py in (0..h.max(0)).step_by(ROOM_TILES as usize) {
                    for px in (0..w.max(0)).step_by(ROOM_TILES as usize) {
                        let seed = self.room_seed().low;
                        let area = Coords { x: tx + px, y: ty + py, w: ROOM_TILES.min(w - px), h: ROOM_TILES.min(h - py) };
                        let warp_slots = warp_cells.get(&(px / ROOM_TILES, py / ROOM_TILES)).copied().unwrap_or(0);
                        rooms.push(OutdoorRoom { area, preset, file, outdoor, link, seed, sub_picks: 0, seed_after: seed, origin: (tx, ty), warp_slots });
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
            link: self.link.cells(),
            rooms,
            roads: self.roads,
        })
    }

    /// `DRLGROOMEX_RollLevelSubstitutionMask` (`Drlg.cpp:2422`): a percent roll per row of the
    /// level's terrain group, on the room's seed (which the room's init later resets).
    fn sub_picks(&self, room: &mut Seed) -> u32 {
        let Some(def) = self.ctx.data.levels().get(self.id) else { return 0 };
        if def.sub_type == -1 || def.sub_theme == -1 {
            return 0;
        }
        let mut picks = 0;
        for (i, row) in self.ctx.data.lvl_subs().group(def.sub_type).iter().enumerate() {
            let prob = usize::try_from(def.sub_theme).ok().and_then(|t| row.prob.get(t)).copied().unwrap_or(0);
            if ((room.roll() % 100) as i32) < prob {
                picks |= 1u32.wrapping_shl(i as u32 & 0x1F);
            }
        }
        picks
    }

    /// `DRLGROOM_AllocRoomEx`: a room's seed state is the level's low word stepped once from
    /// `{low, 0x29A}`; `nSeed` is its low word.
    fn room_seed(&mut self) -> Seed {
        self.seed.step();
        let mut room = Seed::new(self.seed.low, 0x29A);
        room.step();
        self.last_room_seed = room.low;
        room
    }

    /// The level-seed draws of `DRLGPRESET_AddPresetUnitToDrlgMap` (`0x006675F0`) for a scanned
    /// piece's map: some units — certain monsters and placements, and a few objects — are kept
    /// only on a roll. The engine walks the units newest first.
    fn piece_units(&mut self, file: Option<&str>) -> Result<Option<Ds1>, Error> {
        let Some(file) = file else { return Ok(None) };
        let member = format!("data\\global\\tiles\\{}", file.replace('/', "\\"));
        let bytes = self.ctx.data.read_file(&member).map_err(Error::Data)?.ok_or(Error::MissingMap(member))?;
        let map = Ds1::parse(&bytes).map_err(Error::Ds1)?;
        let monsters = self.ctx.data.mon_presets();
        let monstats = monsters.monstats_rows();
        for unit in map.units.iter().rev() {
            let rolls = match unit.kind {
                UnitKind::Monster => {
                    let class = monsters.engine_class(map.act, unit.id);
                    if class < 0 {
                        continue;
                    }
                    if class < monstats {
                        matches!(class, 0xCC | 0xCD | 0x173 | 0x174)
                    } else {
                        matches!(class - monstats, 0x21..=0x23)
                    }
                }
                UnitKind::Object => {
                    let act = u8::try_from(map.act).unwrap_or(0);
                    matches!(self.ctx.engine.preset_object_class(act, unit.id), Some(0xC4 | 0x105 | 0x245))
                }
                UnitKind::Other(_) => continue,
            };
            if rolls {
                self.seed.step();
            }
        }
        Ok(Some(map))
    }
}

/// `DRLGPRESET_BuildPresetArea`'s warp scan: each wall tile of type 10 or 11
/// whose main index is a vis slot (below 8) and whose sub index is 0 or 4 (or that marks a unit)
/// flags its 8×8 room with the slot. Keyed by room column and row in the map.
fn scan_warps(map: &Ds1) -> HashMap<(i32, i32), u8> {
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
                    *cells.entry((x / ROOM_TILES, y / ROOM_TILES)).or_insert(0u8) |= 1 << slot;
                }
            }
        }
    }
    cells
}

/// A plain room's floor during its init, one cell wider and taller than the room.
struct RoomInit {
    origin: (i32, i32),
    size: (i32, i32),
    floor: Grid,
}

impl RoomInit {
    fn new(room: &OutdoorRoom) -> Self {
        let (w, h) = (room.area.w, room.area.h);
        let mut floor = Grid::new(w + 1, h + 1);
        for y in 0..8 {
            for x in 0..8 {
                floor.set(x, y, 0x4_0002);
            }
        }
        Self { origin: (room.area.x, room.area.y), size: (w, h), floor }
    }

    /// `0x00680C80`: rasterize the roads two tiles wide into an edge grid a tile bigger than the
    /// room on every side (`DRLGGRID_SetOutRoomEdgeFlags`, `0x00680A70`), then give each floor
    /// cell on an edge the orientation its neighbours call for (`0x00680B10`).
    fn cut_roads(&mut self, roads: &[Vec<(i32, i32)>], orientations: &[u8; 256]) {
        let (ex, ey, ew, eh) = (self.origin.0 - 1, self.origin.1 - 1, self.size.0 + 3, self.size.1 + 3);
        let mut edge = Grid::new(ew, eh);
        let inside = |x: i32, y: i32| x >= ex && y >= ey && x < ex + ew && y < ey + eh;
        for road in roads {
            for pair in road.windows(2) {
                let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                let (dx, dy) = ((x1 - x0).abs(), (y1 - y0).abs());
                let (sx, sy) = (if x1 < x0 { -1 } else { 1 }, if y1 < y0 { -1 } else { 1 });
                let steep = dx < dy;
                let mut mark = |x: i32, y: i32| {
                    for i in 0..2 {
                        let (px, py) = if steep { (x + i, y) } else { (x, y + i) };
                        if inside(px, py) {
                            edge.or(px - ex, py - ey, 1);
                        }
                    }
                };
                // DRLGGRID_SetLineFlagsWithWidth (0x0067C8E0): Bresenham, widened across the line.
                mark(x0, y0);
                let (mut x, mut y, mut error) = (x0, y0, 0);
                if steep {
                    for _ in 0..dy {
                        y += sy;
                        error += dx;
                        if dy < error {
                            x += sx;
                            error -= dy;
                        }
                        mark(x, y);
                    }
                } else {
                    for _ in 0..dx {
                        x += sx;
                        error += dy;
                        if dx < error {
                            y += sy;
                            error -= dx;
                        }
                        mark(x, y);
                    }
                }
            }
        }
        for oy in 0..=self.size.1 {
            for ox in 0..=self.size.0 {
                let (cx, cy) = (ox + 1, oy + 1);
                if edge.get(cx, cy) == 0 {
                    continue;
                }
                let neighbours = [(1, -1), (1, 0), (1, 1), (0, -1), (0, 1), (-1, -1), (-1, 0), (-1, 1)];
                let mask = neighbours.iter().fold(0usize, |m, &(dx, dy)| (m << 1) | usize::from(edge.get(cx + dx, cy + dy) != 0));
                let orientation = orientations[mask];
                if mask != 0 && orientation != 0 {
                    self.floor.set(ox, oy, (i32::from(orientation) << 8) | 0x82);
                }
            }
        }
    }

    /// The floor grid as the tile pass reads it.
    fn floor_grid(&self) -> room_tiles::Grid {
        let mut grid = room_tiles::Grid::new(self.size.0 + 1, self.size.1 + 1);
        for y in 0..=self.size.1 {
            for x in 0..=self.size.0 {
                grid.set(x, y, self.floor.get(x, y));
            }
        }
        grid
    }
}

/// `DRLGPATH_GetDirectionIndex`: a direction's cell (0..=24) in a 5×5 grid centred on 12.
fn direction_index(dx: i32, dy: i32) -> i32 {
    let (mut dx, mut dy) = (dx, dy);
    let (ax, ay) = (dx.abs(), dy.abs());
    if ax < ay * 2 {
        if ax * 2 <= ay {
            if dx < 0 {
                dx = -1;
            } else {
                dx &= 1;
            }
        }
    } else if dy < 0 {
        dy = -1;
    } else {
        dy &= 1;
    }
    dx = dx.clamp(-2, 2);
    dx * 5 + 12 + dy.clamp(-2, 2)
}

#[cfg(test)]
pub(crate) mod tests {
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
        let left = |y| Orth { level: 3, direction: 0, preset: false, area: Coords { x: 60, y, w: 40, h: 40 } };
        assert!(left(90).goes_before(&left(130)), "down the left side from the top");
        let top = |x| Orth { level: 1, direction: 1, preset: false, area: Coords { x, y: 60, w: 40, h: 40 } };
        assert!(top(140).goes_before(&top(100)), "along the top from the right");
        assert!(left(130).goes_before(&top(100)), "by side first");
    }

    #[test]
    fn an_edge_shared_with_a_neighbour_splits_the_outline() {
        // Blood Moor-like: 56x96 tiles with a 56x40 town above its right part, and a level on
        // its left covering the lower half.
        let me = Coords { x: 1000, y: 1000, w: 56, h: 96 };
        let town = Orth { level: 1, direction: 1, preset: true, area: Coords { x: 1008, y: 960, w: 56, h: 40 } };
        let west = Orth { level: 3, direction: 0, preset: false, area: Coords { x: 920, y: 1048, w: 80, h: 80 } };
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
    pub(crate) fn install() -> Option<(GameData, EngineData, PathBuf)> {
        let (Ok(dir), Ok(exe), Ok(libd2)) =
            (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE"), std::env::var("LIBD2_DIR"))
        else {
            return None;
        };
        let data = GameData::load(dir).expect("game data");
        let engine = EngineData::from_game_exe(&std::fs::read(exe).expect("Game.exe")).expect("1.14d tables");
        Some((data, engine, PathBuf::from(libd2).join("packages/drlg/src/golden")))
    }

    pub(crate) fn number(line: &str, key: &str, from: usize) -> Option<(i64, usize)> {
        let at = line.get(from..)?.find(key)? + from + key.len();
        let end = line[at..].find(|c: char| !(c.is_ascii_digit() || c == '-')).map_or(line.len(), |e| e + at);
        Some((line[at..end].parse().ok()?, end))
    }

    pub(crate) fn read_golden(path: &std::path::Path) -> String {
        if path.extension().is_some_and(|e| e == "gz") {
            let out = std::process::Command::new("gzip").arg("-dc").arg(path).output().expect("gzip");
            String::from_utf8(out.stdout).expect("utf-8")
        } else {
            std::fs::read_to_string(path).expect("golden file")
        }
    }

    /// Recorded rooms by `(level, px, py)`: width and cells.
    pub(crate) type RecordedRooms = HashMap<(i32, i32, i32), (i32, Vec<u16>)>;

    /// A collision recording's seed and rooms, strips joined.
    pub(crate) fn recorded_collision(text: &str, levels: &[i32]) -> (u32, RecordedRooms) {
        let mut seed = 0;
        let mut rooms = RecordedRooms::new();
        for line in text.lines() {
            if line.contains("\"drlg_seed\"") {
                seed = number(line, "\"seed\":", 0).map_or(0, |(s, _)| s as u32);
                continue;
            }
            let Some((level, _)) = number(line, "\"levelId\":", 0) else { continue };
            if !levels.contains(&(level as i32)) {
                continue;
            }
            let get = |key| number(line, key, 0).map(|(v, _)| v as i32).unwrap();
            let (px, py, w, h, y0) = (get("\"px\":"), get("\"py\":"), get("\"w\":"), get("\"h\":"), get("\"y0\":"));
            let at = line.find("\"cells\":[").unwrap() + 9;
            let end = line[at..].find(']').unwrap() + at;
            let strip: Vec<u16> = line[at..end].split(',').filter(|v| !v.is_empty()).map(|v| v.trim().parse::<u32>().unwrap() as u16).collect();
            let room = rooms.entry((level as i32, px, py)).or_insert((w, Vec::new()));
            let need = ((y0 + h) * w) as usize;
            if room.1.len() < need {
                room.1.resize(need, 0);
            }
            room.1[(y0 * w) as usize..need].copy_from_slice(&strip[..(h * w) as usize]);
        }
        (seed, rooms)
    }

    /// With the operator's install and libd2's engine recordings: each Act I wilderness room's
    /// collision map equals the engine's, bit for bit in the terrain bits (`0x1F`).
    #[test]
    fn with_libd2_recordings_collision_matches_the_engine() {
        let Some((data, engine, golden)) = install() else { return };
        let sources = TileSources::new();
        for file in [
            "coll_seed1_all.jsonl.gz",
            "coll_seed2_all.jsonl.gz",
            "coll_seed17_all.jsonl.gz",
            "coll_seed18_all.jsonl.gz",
            "coll_seed777_all.jsonl.gz",
            "coll_seed_holdout.jsonl.gz",
            "coll_seed_holdout2.jsonl.gz",
        ] {
            let (seed, recorded) = recorded_collision(&read_golden(&golden.join(file)), &ACT1_WILDERNESS);
            let act = Act::build(data.levels(), 0, 1, seed);
            let outdoors = Act1Outdoors::new(&data, &engine, &act, seed).expect("substitution maps");
            let mut report = Vec::new();
            let (mut rooms, mut cells, mut wrong) = (0, 0usize, 0usize);
            for &id in &ACT1_WILDERNESS {
                let level = outdoors.generate(id).expect("level");
                let maps = outdoors.build_rooms(&sources, &level).expect("rooms").collision;
                let (mut level_wrong, mut missing) = (0usize, 0);
                let mut worst: Vec<(usize, Coords)> = Vec::new();
                for room in &maps {
                    let Some((w, theirs)) = recorded.get(&(id, room.area.x * 5, room.area.y * 5)) else {
                        missing += 1;
                        continue;
                    };
                    rooms += 1;
                    assert_eq!(*w, room.area.w * 5, "{file} level {id} room {:?} width", room.area);
                    let bad = room.cells.iter().zip(theirs).filter(|(a, b)| **a & 0x1F != (**b & 0x1F) as u8).count();
                    cells += room.cells.len();
                    level_wrong += bad;
                    if bad > 0 {
                        worst.push((bad, room.area));
                    }
                }
                worst.sort_by_key(|w| std::cmp::Reverse(w.0));
                wrong += level_wrong;
                report.push(format!("level {id}: {} rooms, {missing} not recorded, {level_wrong} cells differ, worst {:?}", maps.len(), &worst[..worst.len().min(4)]));
            }
            eprintln!("{file}: {rooms} rooms, {cells} cells, {wrong} differ\n{}", report.join("\n"));
            assert_eq!(wrong, 0, "{file}");
        }
    }

    /// With the operator's install and libd2's recordings: for 200 seeds on Normal and on Hell,
    /// each Act I wilderness level's collision checksum equals the engine's — an FNV-1a hash of
    /// each room's subtile corner, size and terrain bits, summed over the level's rooms.
    #[test]
    fn with_libd2_recordings_collision_checksums_match_for_200_seeds() {
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
                    if ACT1_WILDERNESS.contains(&(level as i32)) {
                        recorded.insert((seed as u32, level as i32), crc as u32);
                    }
                }
            }
            let (mut checked, mut wrong) = (0, Vec::new());
            for seed in 1..=200u32 {
                let act = Act::build(data.levels(), 0, difficulty, seed);
                let outdoors = Act1Outdoors::new(&data, &engine, &act, seed).expect("substitution maps");
                for &id in &ACT1_WILDERNESS {
                    let Some(&theirs) = recorded.get(&(seed, id)) else { continue };
                    let level = outdoors.generate(id).expect("level");
                    let ours = outdoors.build_rooms(&sources, &level).expect("rooms").collision.iter().fold(0u32, |sum, room| {
                        let mut h = 0x811C_9DC5;
                        for v in [room.area.x * 5, room.area.y * 5, room.area.w * 5, room.area.h * 5] {
                            fnv(&mut h, v as u32);
                        }
                        for &c in &room.cells {
                            fnv(&mut h, u32::from(c & 0x1F));
                        }
                        sum.wrapping_add(h)
                    });
                    checked += 1;
                    if ours != theirs {
                        wrong.push((seed, id));
                    }
                }
            }
            assert!(checked > 1000, "{file}: only {checked} levels recorded");
            assert!(wrong.is_empty(), "{file}: {} of {checked} levels differ: {:?}", wrong.len(), &wrong[..wrong.len().min(20)]);
        }
    }

    fn generate(data: &GameData, engine: &EngineData, seed: u32, difficulty: u8) -> HashMap<i32, Result<OutdoorLevel, String>> {
        let act = Act::build(data.levels(), 0, difficulty, seed);
        let outdoors = Act1Outdoors::new(data, engine, &act, seed).expect("substitution maps");
        ACT1_WILDERNESS.iter().map(|&id| (id, outdoors.generate(id).map_err(|e| e.to_string()))).collect()
    }

    /// Our rooms against recorded room corners (tiles): the same set. The list order can differ
    /// where a piece placed after the port's stop (the tree of Inifuss, a ruin) makes rooms.
    fn compare(level: &Result<OutdoorLevel, String>, mut recorded: Vec<(i32, i32)>) -> Option<String> {
        let level = match level {
            Ok(level) => level,
            Err(e) => return Some(e.clone()),
        };
        let mut got: Vec<(i32, i32)> = level.rooms.iter().map(|r| (r.area.x, r.area.y)).collect();
        got.sort_unstable();
        recorded.sort_unstable();
        (got != recorded).then(|| {
            let missing: Vec<_> = recorded.iter().filter(|r| !got.contains(r)).collect();
            let extra: Vec<_> = got.iter().filter(|r| !recorded.contains(r)).collect();
            format!("{} rooms, recorded {}; voids we make rooms: {missing:?}; rooms we leave void: {extra:?}", got.len(), recorded.len())
        })
    }

    /// The rooms of every Act I wilderness level for the seeds libd2 recorded room by room: where
    /// they are, each piece's id, and each plain cell's link flags — neighbours, shrine styles and
    /// the waypoint, which come after the roads and so check the road search's draws. (The
    /// recordings keep neither the pieces' files nor the outdoor flags.)
    #[test]
    fn with_libd2_recordings_the_rooms_match_the_engine() {
        let Some((data, engine, golden)) = install() else { return };
        let (mut checked, mut rooms_checked, mut wrong) = (0, 0, Vec::new());
        for (file, seed) in [("deep_seed_1.jsonl", 1u32), ("deep_seed_2.jsonl", 2), ("deep_seed_305419896.jsonl", 305_419_896)] {
            let ours = generate(&data, &engine, seed, 0);
            for line in read_golden(&golden.join(file)).lines().filter(|l| l.contains("\"evt\":\"drlg_level\"")) {
                let Some((id, _)) = number(line, "\"levelId\":", 0) else { continue };
                let Some(level) = ours.get(&(id as i32)) else { continue };
                let rooms_at = line.find("\"rooms\":").expect("rooms");
                let records: Vec<&str> = line[rooms_at..].split("{\"x\":").skip(1).collect();
                let recorded: Vec<(i32, i32)> = records
                    .iter()
                    .map(|r| {
                        let r = format!("{{\"x\":{r}");
                        (number(&r, "\"x\":", 0).unwrap().0 as i32, number(&r, "\"y\":", 0).unwrap().0 as i32)
                    })
                    .collect();
                checked += 1;
                if let Some(problem) = compare(level, recorded) {
                    wrong.push(format!("seed {seed} level {id}: {problem}"));
                    continue;
                }
                let level = level.as_ref().unwrap();
                for r in records {
                    let r = format!("{{\"x\":{r}");
                    let field = |key| number(&r, key, 0).map(|(v, _)| v as i32);
                    let (x, y) = (field("\"x\":").unwrap(), field("\"y\":").unwrap());
                    let ours = level.rooms.iter().find(|o| (o.area.x, o.area.y) == (x, y)).unwrap();
                    // A piece's room flags carry more than its link bits.
                    let picks = field("\"subThemePicked\":").map_or(0, |p| p as u32);
                    let seed = number(&r, "\"seed\":", 0).unwrap().0 as u32;
                    let (expected, got) = if field("\"nPresetType\":") == Some(2) {
                        ((field("\"def\":").unwrap(), 0, seed, 0), (ours.preset, 0, ours.seed, 0))
                    } else {
                        ((0, field("\"flags\":").unwrap() & 0x3_FFFF, seed, picks), (ours.preset, ours.link & 0x3_FFFF, ours.seed_after, ours.sub_picks))
                    };
                    rooms_checked += 1;
                    if got != expected {
                        wrong.push(format!("seed {seed} level {id} room ({x}, {y}): (piece, link, seed, terrain picks) {got:x?}, recorded {expected:x?}"));
                    }
                }
            }
        }
        eprintln!("{checked} levels, {rooms_checked} rooms compared");
        assert!(checked >= 21, "only {checked} levels compared");
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong[..wrong.len().min(60)].join("\n"));
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

    /// With the operator's install: each waypoint room of levels 3–6 gets one waypoint object
    /// inside it, and each shrine room its shrine or well, for a few seeds.
    #[test]
    fn with_a_real_install_waypoint_and_shrine_rooms_get_their_objects() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(dir).expect("game data");
        let engine = EngineData::from_game_exe(&std::fs::read(exe).expect("Game.exe")).expect("1.14d tables");
        let sources = TileSources::new();
        for seed in [1u32, 2, 305_419_896, 0x1234_5678] {
            let act = Act::build(data.levels(), 0, 0, seed);
            let outdoors = Act1Outdoors::new(&data, &engine, &act, seed).unwrap();
            for id in ACT1_WILDERNESS {
                let level = outdoors.generate(id).unwrap();
                let built = outdoors.build_rooms(&sources, &level).unwrap();
                let (mut waypoints, mut shrines) = (0, 0);
                for (room, units) in level.rooms.iter().zip(&built.units) {
                    let objects: Vec<_> = units.iter().filter(|u| u.kind == UnitKind::Object).collect();
                    if (room.link >> 16) & 3 != 0 {
                        let waypoint: Vec<_> =
                            objects.iter().filter(|u| data.objects().get(u.class).is_some_and(|o| o.sub_class & 0x40 != 0)).collect();
                        eprintln!("seed {seed:#x} level {id}: waypoint room {:?} units {units:?}", room.area);
                        assert_eq!(waypoint.len(), 1, "seed {seed:#x} level {id}: {units:?}");
                        let w = waypoint[0];
                        let s = crate::preset::SUBTILES;
                        assert!(w.x >= room.area.x * s && w.x < (room.area.x + 8) * s && w.y >= room.area.y * s && w.y < (room.area.y + 8) * s);
                        waypoints += 1;
                    }
                    if (room.link >> 12) & 0xF != 0 {
                        eprintln!("seed {seed:#x} level {id}: shrine room {:?} units {:?}", room.area, objects.iter().map(|u| (data.objects().name(u.class), u.x, u.y)).collect::<Vec<_>>());
                        assert!(!objects.is_empty(), "seed {seed:#x} level {id}: shrine room {:?} has no object", room.area);
                        shrines += 1;
                    }
                }
                assert_eq!(waypoints, usize::from((3..=6).contains(&id)), "seed {seed:#x} level {id}");
                assert!(shrines <= 5);
            }
        }
    }
}
