//! The rooms of an act a player can walk through, across level edges.
//!
//! Which rooms a client holds follows its player: the room it stands in and the rooms near it
//! (`DRLGROOM_DefineRoomsNear`, `0x0066BC20`). Walking from the Rogue Encampment into Blood
//! Moor, those rooms belong to two levels, so the model spans the act's placed levels — the
//! towns and wilderness trunks the placement walk lays out — not one map.
//!
//! Room shapes:
//! - a preset level is cut into 8×8-tile rooms from its corner, the last row and column
//!   narrower (`DRLGPRESET_BuildArea`, ported in libd2 `preset.zig`), as the town already is;
//! - a wilderness level is a grid of 8×8-tile cells, each a room, part of a preset piece that
//!   is itself cut into 8×8 rooms from the cell, or a void — which is what [`crate::outdoor`]
//!   works out (`DRLGOUTDOOR_CreateOutdoorRoomExGrid`, `0x006750F0`). The client only needs a
//!   point inside a room to load it (`0x07` → `0x0061B640` → `0x00642630`), and a `0x07`
//!   pointing into a void would make it dereference null, so the voids matter and the tiles do
//!   not.
//!
//! A wilderness level also keeps each room's collision map ([`crate::collision`]), which says where
//! units may stand and walk.
//!
//! Rooms of different levels are near when their gap is under 6 tiles on both axes, as within a
//! level. The engine links cross-level rooms only through a room's visibility slots
//! (`DRLGROOMEX_LinkNearRoomsByVis`, `0x0066C220`), so two levels placed edge to edge without a
//! passage between them are near here and not in the engine; a client then loads terrain it
//! cannot reach.

use d2_data::engine::EngineData;
use d2_data::levels::DrlgType;
use d2_data::GameData;

use d2_formats::ds1::UnitKind;

use crate::act::Act;
use crate::collision::{RoomCollision, TileSources};
use crate::outdoor::Act1Outdoors;
use crate::preset::{PlacedUnit, PresetLevel, UnitClass, ROOM_TILES, SUBTILES};
use crate::Coords;

/// A room: its level and its index in that level's rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId {
    /// `Levels.txt` id.
    pub level: i32,
    /// Index into the level's rooms.
    pub index: usize,
}

/// One walkable level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldLevel {
    /// `Levels.txt` id.
    pub id: i32,
    /// Its rectangle in tiles.
    pub area: Coords,
    /// Its rooms in tiles.
    pub rooms: Vec<Coords>,
    /// The units its map and its rooms' init place, in world subtiles.
    pub units: Vec<PlacedUnit>,
    /// The preset piece each room is part of (`LvlPrest.txt` `Def`), 0 for a plain wilderness
    /// cell; empty when not known (preset levels).
    pub pieces: Vec<i32>,
    /// Each room's collision map, in [`WorldLevel::rooms`] order; empty when not built.
    pub collision: Vec<RoomCollision>,
}

/// The walkable levels of an act.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct World {
    levels: Vec<WorldLevel>,
    /// Levels that would be walkable but could not be generated, with why.
    unbuilt: Vec<(i32, String)>,
}

/// A level's rooms from its rectangle and generator.
#[must_use]
pub fn grid_rooms(area: Coords, drlg_type: DrlgType) -> Vec<Coords> {
    let mut rooms = Vec::new();
    let (cols, rows) = match drlg_type {
        // Whole cells only: `floor(size / 8)` of them.
        DrlgType::Wilderness => (area.w.div_euclid(ROOM_TILES), area.h.div_euclid(ROOM_TILES)),
        // Partial cells at the far edges.
        _ => ((area.w + ROOM_TILES - 1).div_euclid(ROOM_TILES), (area.h + ROOM_TILES - 1).div_euclid(ROOM_TILES)),
    };
    for row in 0..rows.max(0) {
        for col in 0..cols.max(0) {
            let (x, y) = (col * ROOM_TILES, row * ROOM_TILES);
            rooms.push(Coords { x: area.x + x, y: area.y + y, w: ROOM_TILES.min(area.w - x), h: ROOM_TILES.min(area.h - y) });
        }
    }
    rooms
}

/// Signed gap between two spans, as `DRLGROOM_DefineRoomsNear` measures it: the left span's
/// width is the one subtracted; negative when they overlap.
fn gap(a0: i32, aw: i32, b0: i32, bw: i32) -> i32 {
    if a0 < b0 {
        b0 - aw - a0
    } else {
        a0 - bw - b0
    }
}

/// The engine's near distance: rooms under this many tiles apart on both axes.
pub const NEAR_GAP: i32 = 6;

/// Whether two rooms are near: under [`NEAR_GAP`] tiles apart on both axes.
#[must_use]
pub fn near(a: Coords, b: Coords) -> bool {
    within(a, b, NEAR_GAP)
}

/// Whether two rooms are under `max_gap` tiles apart on both axes.
#[must_use]
pub fn within(a: Coords, b: Coords, max_gap: i32) -> bool {
    gap(a.x, a.w, b.x, b.w) < max_gap && gap(a.y, a.h, b.y, b.h) < max_gap
}

/// `DRLGROOM_ReorderNearRoomList` (`0x0066BBC0`): bubble passes moving a room ahead of one it
/// lies wholly left of or above (room fields `+0x34..+0x40`: x, y, width, height).
pub fn reorder_near<T>(list: &mut [T], coords: impl Fn(&T) -> Coords) {
    let n = list.len();
    for _ in 1..n {
        for i in 0..n - 1 {
            let (cur, next) = (coords(&list[i]), coords(&list[i + 1]));
            if next.x + next.w <= cur.x || next.y + next.h <= cur.y {
                list.swap(i, i + 1);
            }
        }
    }
}

impl World {
    /// The walkable levels of `act`: every level its placement walk laid out and every
    /// preset or wilderness level depending on one, with the town's rooms taken from `town` and
    /// the wilderness generated for the act's seed, its rooms' tiles read through `sources`.
    /// Maze levels (reached through warps), levels overlapping one already taken and levels that
    /// cannot be generated (see [`World::unbuilt`]) are left out.
    #[must_use]
    pub fn build(data: &GameData, engine: &EngineData, act: &Act, town: Option<&PresetLevel>, sources: &TileSources) -> Self {
        let levels = data.levels();
        let placed = act.placed_levels();
        let mut ids = placed.clone();
        for depends_on in &placed {
            let mut extra: Vec<i32> = (1..200)
                .filter(|&id| levels.get(id).is_some_and(|d| d.act == act.act && d.depend == *depends_on))
                .collect();
            ids.append(&mut extra);
        }
        let mut world = Self::default();
        let outdoors = if act.act == 0 { Some(Act1Outdoors::new(data, engine, act, act.game_seed)) } else { None };
        for id in ids {
            let Some(def) = levels.get(id) else { continue };
            if !matches!(def.drlg_type, DrlgType::Preset | DrlgType::Wilderness) {
                continue;
            }
            let Some(area) = act.coords(levels, id) else { continue };
            if area.w <= 0 || area.h <= 0 || world.levels.iter().any(|l| overlaps(l.area, area)) {
                continue;
            }
            let (rooms, units, pieces, collision) = match (town, def.drlg_type, &outdoors) {
                (Some(t), _, _) if t.level_id == id => (t.rooms.clone(), t.units.clone(), Vec::new(), Vec::new()),
                (_, DrlgType::Wilderness, Some(Ok(outdoors))) => match outdoors.generate(id).and_then(|l| outdoors.build_rooms(sources, &l).map(|b| (l, b))) {
                    Ok((level, built)) => {
                        let units = built
                            .units
                            .iter()
                            .flatten()
                            .map(|u| PlacedUnit {
                                class: match u.kind {
                                    UnitKind::Object => UnitClass::Object(u.class),
                                    UnitKind::Monster => UnitClass::Other { kind: 1, id: u.class },
                                    UnitKind::Other(kind) => UnitClass::Other { kind, id: u.class },
                                },
                                x: u.x,
                                y: u.y,
                                path: Vec::new(),
                            })
                            .collect();
                        (level.rooms.iter().map(|r| r.area).collect(), units, level.rooms.iter().map(|r| r.preset).collect(), built.collision)
                    }
                    Err(e) => {
                        world.unbuilt.push((id, e.to_string()));
                        continue;
                    }
                },
                (_, DrlgType::Wilderness, Some(Err(e))) => {
                    world.unbuilt.push((id, e.to_string()));
                    continue;
                }
                (_, DrlgType::Wilderness, None) => {
                    world.unbuilt.push((id, format!("act {} wilderness is not ported", act.act + 1)));
                    continue;
                }
                _ => (grid_rooms(area, def.drlg_type), Vec::new(), Vec::new(), Vec::new()),
            };
            world.levels.push(WorldLevel { id, area, rooms, units, pieces, collision });
        }
        world
    }

    /// What [`World::build`] could not generate — levels it left out, or rooms whose units it
    /// could not place — with the reason.
    #[must_use]
    pub fn unbuilt(&self) -> &[(i32, String)] {
        &self.unbuilt
    }

    /// A world from levels already cut into rooms (tests, or a caller with its own generator).
    #[must_use]
    pub fn from_levels(levels: Vec<WorldLevel>) -> Self {
        Self { levels, unbuilt: Vec::new() }
    }

    /// The levels.
    #[must_use]
    pub fn levels(&self) -> &[WorldLevel] {
        &self.levels
    }

    /// A room's rectangle in tiles.
    #[must_use]
    pub fn room(&self, id: RoomId) -> Option<Coords> {
        self.level(id.level)?.rooms.get(id.index).copied()
    }

    fn level(&self, id: i32) -> Option<&WorldLevel> {
        self.levels.iter().find(|l| l.id == id)
    }

    /// The collision flags at a world subtile ([`crate::collision`] bits), `None` where no room
    /// has a map.
    #[must_use]
    pub fn collision_at(&self, x: i32, y: i32) -> Option<u8> {
        let id = self.room_at(x, y)?;
        self.level(id.level)?.collision.get(id.index)?.at(x, y)
    }

    /// The units standing in a room, in the order its level lists them.
    pub fn units_in(&self, id: RoomId) -> impl Iterator<Item = &PlacedUnit> {
        let level = self.level(id.level);
        level.into_iter().flat_map(move |l| {
            l.units.iter().filter(move |u| {
                let (tx, ty) = (u.x.div_euclid(SUBTILES), u.y.div_euclid(SUBTILES));
                l.rooms.iter().position(|c| tx >= c.x && tx < c.x + c.w && ty >= c.y && ty < c.y + c.h) == Some(id.index)
            })
        })
    }

    /// The room holding a world subtile position.
    #[must_use]
    pub fn room_at(&self, x: i32, y: i32) -> Option<RoomId> {
        let (tx, ty) = (x.div_euclid(SUBTILES), y.div_euclid(SUBTILES));
        let inside = |c: &Coords| tx >= c.x && tx < c.x + c.w && ty >= c.y && ty < c.y + c.h;
        let level = self.levels.iter().find(|l| inside(&l.area))?;
        let index = level.rooms.iter().position(inside)?;
        Some(RoomId { level: level.id, index })
    }

    /// The rooms near a room: its level's, reordered as the engine does, then other levels'.
    #[must_use]
    pub fn rooms_near(&self, id: RoomId) -> Vec<RoomId> {
        let Some(me) = self.room(id) else { return Vec::new() };
        let mut own: Vec<RoomId> = Vec::new();
        let mut others: Vec<RoomId> = Vec::new();
        for level in &self.levels {
            for (index, &room) in level.rooms.iter().enumerate() {
                if near(me, room) {
                    let rid = RoomId { level: level.id, index };
                    if level.id == id.level {
                        own.push(rid);
                    } else {
                        others.push(rid);
                    }
                }
            }
        }
        reorder_near(&mut own, |r| self.room(*r).unwrap_or_default());
        own.extend(others);
        own
    }
}

impl World {
    /// Whether two rooms (of any levels) are under `max_gap` tiles apart.
    #[must_use]
    pub fn rooms_within(&self, a: RoomId, b: RoomId, max_gap: i32) -> bool {
        match (self.room(a), self.room(b)) {
            (Some(a), Some(b)) => within(a, b, max_gap),
            _ => false,
        }
    }
}

fn overlaps(a: Coords, b: Coords) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16×8 town with Blood Moor's 24×16 below it, sharing the edge y = 108.
    fn world() -> World {
        let town = Coords { x: 100, y: 100, w: 16, h: 8 };
        let moor = Coords { x: 96, y: 108, w: 24, h: 16 };
        World::from_levels(vec![
            WorldLevel { id: 1, area: town, rooms: grid_rooms(town, DrlgType::Preset), units: Vec::new(), pieces: Vec::new(), collision: Vec::new() },
            WorldLevel { id: 2, area: moor, rooms: grid_rooms(moor, DrlgType::Wilderness), units: Vec::new(), pieces: Vec::new(), collision: Vec::new() },
        ])
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`): for a few seeds,
    /// Act I's walkable levels are the camp, the wilderness from Blood Moor to Tamoe Highland and
    /// the Monastery pieces, and the camp's rooms reach into Blood Moor.
    #[test]
    fn with_a_real_install_the_camp_borders_blood_moor() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = d2_data::GameData::load(&dir).unwrap();
        let engine = d2_data::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        for seed in [1u32, 2, 0x1234_5678, 0xBEEF_F00D] {
            let act = Act::build(data.levels(), 0, 0, seed);
            let town = PresetLevel::build(&data, &engine, &act, 1).unwrap();
            let world = World::build(&data, &engine, &act, Some(&town), &TileSources::new());
            assert!(world.unbuilt().is_empty(), "seed {seed:#x}: {:?}", world.unbuilt());
            let ids: Vec<i32> = world.levels().iter().map(|l| l.id).collect();
            for id in [1, 2, 3, 4, 5, 6, 7, 17, 26] {
                assert!(ids.contains(&id), "seed {seed:#x}: level {id} missing from {ids:?}");
            }
            let camp = &world.levels()[ids.iter().position(|&i| i == 1).unwrap()];
            assert_eq!(camp.rooms, town.rooms);
            let reaches_moor = (0..camp.rooms.len())
                .any(|index| world.rooms_near(RoomId { level: 1, index }).iter().any(|r| r.level == 2));
            assert!(reaches_moor, "seed {seed:#x}: the camp does not touch Blood Moor");
        }
    }

    /// With the operator's install and libd2's recordings (`LIBD2_DIR`): no room the world would
    /// send lies in a void of the engine's own layout, for the seeds whose rooms libd2 recorded.
    #[test]
    fn with_libd2_recordings_no_world_room_is_a_void() {
        let (Ok(dir), Ok(exe), Ok(libd2)) =
            (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE"), std::env::var("LIBD2_DIR"))
        else {
            return;
        };
        let data = d2_data::GameData::load(&dir).unwrap();
        let engine = d2_data::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let golden = std::path::Path::new(&libd2).join("packages/drlg/src/golden");
        let number = |line: &str, key: &str, from: usize| -> Option<(i32, usize)> {
            let at = line[from..].find(key)? + from + key.len();
            let end = line[at..].find(|c: char| !(c.is_ascii_digit() || c == '-'))? + at;
            Some((line[at..end].parse().ok()?, end))
        };
        let mut checked = 0;
        for (file, seed) in [("deep_seed_1.jsonl", 1u32), ("deep_seed_2.jsonl", 2), ("deep_seed_305419896.jsonl", 305_419_896)] {
            let text = std::fs::read_to_string(golden.join(file)).unwrap();
            let act = Act::build(data.levels(), 0, 0, seed);
            let town = PresetLevel::build(&data, &engine, &act, 1).unwrap();
            let world = World::build(&data, &engine, &act, Some(&town), &TileSources::new());
            for line in text.lines() {
                let Some((level, _)) = number(line, "\"levelId\":", 0) else { continue };
                let Some(ours) = world.levels().iter().find(|l| l.id == level) else { continue };
                let rooms_at = line.find("\"rooms\":").unwrap();
                let mut recorded = Vec::new();
                let mut at = rooms_at;
                while let Some((x, next)) = number(line, "{\"x\":", at) {
                    let (y, next) = number(line, "\"y\":", next).unwrap();
                    let (w, next) = number(line, "\"w\":", next).unwrap();
                    let (h, next) = number(line, "\"h\":", next).unwrap();
                    recorded.push(Coords { x, y, w, h });
                    at = next;
                }
                for room in &ours.rooms {
                    let covered = recorded.iter().any(|r| room.x >= r.x && room.x < r.x + r.w && room.y >= r.y && room.y < r.y + r.h);
                    assert!(covered, "seed {seed}: level {level} room {room:?} is a void in the engine's layout");
                    checked += 1;
                }
            }
        }
        assert!(checked > 500, "only {checked} rooms checked");
    }

    #[test]
    fn levels_are_cut_into_their_rooms() {
        let odd = Coords { x: 0, y: 0, w: 20, h: 9 };
        assert_eq!(grid_rooms(odd, DrlgType::Preset).len(), 3 * 2, "partial cells kept");
        assert_eq!(grid_rooms(odd, DrlgType::Preset)[2], Coords { x: 16, y: 0, w: 4, h: 8 });
        assert_eq!(grid_rooms(odd, DrlgType::Wilderness).len(), 2, "whole cells only");
    }

    #[test]
    fn near_rooms_cross_the_edge_between_levels() {
        let w = world();
        let at_edge = w.room_at(100 * 5 + 1, 107 * 5).unwrap();
        assert_eq!(at_edge, RoomId { level: 1, index: 0 });
        let near = w.rooms_near(at_edge);
        assert_eq!(&near[..2], &[RoomId { level: 1, index: 0 }, RoomId { level: 1, index: 1 }], "own level first");
        let moor: Vec<Coords> = near.iter().filter(|r| r.level == 2).map(|r| w.room(*r).unwrap()).collect();
        let row = |x| Coords { x, y: 108, w: 8, h: 8 };
        assert_eq!(moor, vec![row(96), row(104), row(112)], "the moor row below, 4 tiles of gap still near");
        assert_eq!(w.room_at(96 * 5, 123 * 5), Some(RoomId { level: 2, index: 3 }));
        assert_eq!(w.room_at(0, 0), None);
    }
}
