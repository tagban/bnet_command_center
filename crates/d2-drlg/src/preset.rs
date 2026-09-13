//! Preset levels: an area built from one fixed DS1 map — the towns and set pieces.
//!
//! The level's rectangle comes from the act layout; the DS1 is the `LvlPrest.txt` file the
//! engine picks; the preset units (NPCs, objects) sit at the DS1's subtile positions offset by
//! the level's origin. Rooms are the level cut into 8×8-tile cells, as the engine's preset area
//! builder (`DRLGPRESET_BuildPresetArea`) lays them out for a single-map level.

use std::fmt;

use d2_data::engine::EngineData;
use d2_data::levels::DrlgType;
use d2_data::presets::PresetMonster;
use d2_data::GameData;
use d2_formats::ds1::{Ds1, UnitKind};

use crate::act::Act;
use crate::Coords;

/// Tiles per room edge in a preset area.
pub const ROOM_TILES: i32 = 8;
/// Subtiles per tile.
pub const SUBTILES: i32 = 5;

/// Why a preset level could not be built.
#[derive(Debug)]
pub enum Error {
    /// The level is not a preset level, or has no `LvlPrest.txt` row.
    NotPreset(i32),
    /// Which of several map files the engine picks for this level is not ported yet.
    PickNotPorted(i32),
    /// The map file could not be read.
    Data(d2_data::Error),
    /// The map file is missing.
    MissingMap(String),
    /// The map file is malformed.
    Ds1(d2_formats::ds1::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPreset(l) => write!(f, "level {l} is not a preset level"),
            Self::PickNotPorted(l) => write!(f, "level {l}: map file pick not ported"),
            Self::Data(e) => write!(f, "{e}"),
            Self::MissingMap(m) => write!(f, "{m} is not in the install"),
            Self::Ds1(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// What a placed unit is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitClass {
    /// An object, by `objects.txt` class.
    Object(i32),
    /// A monster or NPC, as `MonPreset.txt` resolves it.
    Monster(PresetMonster),
    /// A DS1 unit the engine does not spawn from (items, unknown types).
    Other {
        /// DS1 type.
        kind: i32,
        /// DS1 id.
        id: i32,
    },
}

/// A preset unit in world coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedUnit {
    /// What it is.
    pub class: UnitClass,
    /// World position in subtiles.
    pub x: i32,
    /// World position in subtiles.
    pub y: i32,
    /// An NPC's walk path, world subtiles, with each node's action.
    pub path: Vec<(i32, i32, i32)>,
}

/// A built preset level.
#[derive(Debug, Clone)]
pub struct PresetLevel {
    /// Level id.
    pub level_id: i32,
    /// The level's rectangle in tiles.
    pub area: Coords,
    /// The DS1 used, as an install member path.
    pub map: String,
    /// Rooms, in tiles.
    pub rooms: Vec<Coords>,
    /// Preset units, in DS1 order.
    pub units: Vec<PlacedUnit>,
}

impl PresetLevel {
    /// Build `level_id` from its act layout.
    ///
    /// # Errors
    ///
    /// [`Error`] if the level is not a preset level, its file pick is not ported, or its map
    /// cannot be read.
    pub fn build(data: &GameData, engine: &EngineData, act: &Act, level_id: i32) -> Result<Self, Error> {
        let levels = data.levels();
        let def = levels.get(level_id).filter(|d| d.drlg_type == DrlgType::Preset).ok_or(Error::NotPreset(level_id))?;
        let prest = data.lvl_prests().for_level(level_id).ok_or(Error::NotPreset(level_id))?;
        let area = act.coords(levels, level_id).ok_or(Error::NotPreset(level_id))?;

        let pick = match (level_id, prest.files.len()) {
            (1, _) => act.rogue_encampment_pick().ok_or(Error::PickNotPorted(level_id))?,
            (_, 1) => 0,
            _ => return Err(Error::PickNotPorted(level_id)),
        };
        let file = prest.files.get(pick as usize).ok_or(Error::PickNotPorted(level_id))?;
        let map = format!("data\\global\\tiles\\{}", file.replace('/', "\\"));
        let bytes = data.read_file(&map).map_err(Error::Data)?.ok_or_else(|| Error::MissingMap(map.clone()))?;
        let ds1 = Ds1::parse(&bytes).map_err(Error::Ds1)?;

        let (ox, oy) = (area.x * SUBTILES, area.y * SUBTILES);
        let units = ds1
            .units
            .iter()
            .map(|u| PlacedUnit {
                class: match u.kind {
                    UnitKind::Object => engine
                        .preset_object_class(def.act, u.id)
                        .map_or(UnitClass::Other { kind: 2, id: u.id }, UnitClass::Object),
                    UnitKind::Monster => data
                        .mon_presets()
                        .get(def.act, u.id)
                        .cloned()
                        .map_or(UnitClass::Other { kind: 1, id: u.id }, UnitClass::Monster),
                    UnitKind::Other(kind) => UnitClass::Other { kind, id: u.id },
                },
                x: ox + u.x,
                y: oy + u.y,
                path: u.path.iter().map(|&(x, y, action)| (ox + x, oy + y, action)).collect(),
            })
            .collect();

        let mut rooms = Vec::new();
        for y in (0..area.h).step_by(ROOM_TILES as usize) {
            for x in (0..area.w).step_by(ROOM_TILES as usize) {
                rooms.push(Coords {
                    x: area.x + x,
                    y: area.y + y,
                    w: ROOM_TILES.min(area.w - x),
                    h: ROOM_TILES.min(area.h - y),
                });
            }
        }
        Ok(Self { level_id, area, map, rooms, units })
    }

    /// The room containing a world subtile position.
    #[must_use]
    pub fn room_at(&self, x: i32, y: i32) -> Option<Coords> {
        self.room_index_at(x, y).map(|i| self.rooms[i])
    }

    /// Index into [`PresetLevel::rooms`] of the room containing a world subtile position.
    #[must_use]
    pub fn room_index_at(&self, x: i32, y: i32) -> Option<usize> {
        let (tx, ty) = (x.div_euclid(SUBTILES), y.div_euclid(SUBTILES));
        self.rooms.iter().position(|r| tx >= r.x && tx < r.x + r.w && ty >= r.y && ty < r.y + r.h)
    }

    /// The rooms "near" a room — itself and every room of the level less than 6 tiles away on
    /// both axes, which for 8×8 rooms is its 3×3 neighbourhood (`DRLGROOM_DefineRoomsNear`,
    /// `0x0066BC20`). A client gets `0x07` and the units of each of these rooms when its player
    /// enters the room. Rooms of neighbouring levels are not included yet.
    #[must_use]
    pub fn rooms_near(&self, room: usize) -> Vec<usize> {
        let Some(&a) = self.rooms.get(room) else { return Vec::new() };
        let gap = |a0: i32, aw: i32, b0: i32, bw: i32| if a0 < b0 { b0 - aw - a0 } else { a0 - bw - b0 };
        let mut near: Vec<usize> = (0..self.rooms.len())
            .filter(|&i| {
                let b = self.rooms[i];
                gap(a.x, a.w, b.x, b.w) < 6 && gap(a.y, a.h, b.y, b.h) < 6
            })
            .collect();
        // DRLGROOM_ReorderNearRoomList (0x0066BBC0): a bubble pass that moves a room ahead of one
        // it lies wholly left of or above. The fields it compares are taken as x/y/w/h.
        let n = near.len();
        for _ in 1..n {
            for i in 0..n - 1 {
                let (cur, next) = (self.rooms[near[i]], self.rooms[near[i + 1]]);
                if next.x + next.w <= cur.x || next.y + next.h <= cur.y {
                    near.swap(i, i + 1);
                }
            }
        }
        near
    }

    /// The preset units standing in a room, in map order.
    pub fn units_in(&self, room: usize) -> impl Iterator<Item = &PlacedUnit> {
        self.units.iter().filter(move |u| self.room_index_at(u.x, u.y) == Some(room))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: i32, h: i32) -> PresetLevel {
        let rooms = (0..h)
            .flat_map(|y| (0..w).map(move |x| Coords { x: 100 + x * ROOM_TILES, y: 50 + y * ROOM_TILES, w: ROOM_TILES, h: ROOM_TILES }))
            .collect();
        PresetLevel { level_id: 1, area: Coords::default(), map: String::new(), rooms, units: Vec::new() }
    }

    #[test]
    fn a_room_is_near_itself_and_its_neighbours_only() {
        let level = grid(4, 4);
        let mut near = level.rooms_near(5); // column 1, row 1
        near.sort_unstable();
        assert_eq!(near, vec![0, 1, 2, 4, 5, 6, 8, 9, 10]);
        let mut corner = level.rooms_near(15);
        corner.sort_unstable();
        assert_eq!(corner, vec![10, 11, 14, 15]);
        assert!(level.rooms_near(99).is_empty());

        let ordered = level.rooms_near(5);
        for (i, &a) in ordered.iter().enumerate() {
            for &b in &ordered[i + 1..] {
                let (ra, rb) = (level.rooms[a], level.rooms[b]);
                assert!(!(rb.x + rb.w <= ra.x && rb.y + rb.h <= ra.y), "{rb:?} is left of and above {ra:?} but comes later");
            }
        }
        assert_eq!(level.room_index_at(100 * SUBTILES + 8 * SUBTILES, 50 * SUBTILES + 7 * SUBTILES - 1), Some(1));
    }

    /// The Rogue Encampment for seed 0x12345678 against libd2's engine dump of the same town's
    /// objects (`LIBD2_DIR`), using the operator's install (`BNETCC_D2_DATA_DIR`) and `Game.exe`
    /// (`BNETCC_D2_GAME_EXE`).
    #[test]
    fn with_libd2_the_town_objects_match_the_engine() {
        let (Ok(libd2), Ok(dir), Ok(exe)) =
            (std::env::var("LIBD2_DIR"), std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE"))
        else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let act = Act::build(data.levels(), 0, 0, 305_419_896);
        let town = PresetLevel::build(&data, &engine, &act, 1).unwrap();
        assert!(town.map.ends_with("TownS1.ds1"), "{}", town.map);
        assert_eq!(town.rooms.len(), 35, "56x40 tiles in 8x8 rooms");

        let dump = std::fs::read_to_string(std::path::Path::new(&libd2).join("packages/drlg/src/golden/obj_seed305_act1town.jsonl")).unwrap();
        let line = dump.lines().find(|l| l.contains("\"drlg_obj\"")).unwrap();
        let recorded: Vec<(i32, i32, i32)> = line
            .split("{\"cls\":")
            .skip(1)
            .map(|o| {
                let num = |key: &str| -> i32 {
                    let at = o.find(key).unwrap() + key.len();
                    o[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect::<String>().parse().unwrap()
                };
                (num(""), num("\"x\":"), num("\"y\":"))
            })
            .collect();
        let ours: Vec<(i32, i32, i32)> = town
            .units
            .iter()
            .filter_map(|u| match u.class {
                UnitClass::Object(c) => Some((c, u.x, u.y)),
                _ => None,
            })
            .collect();
        let missing: Vec<_> = recorded.iter().filter(|r| !ours.contains(r)).collect();
        let extra: Vec<_> = ours.iter().filter(|o| !recorded.contains(o)).collect();
        eprintln!("town objects: {} recorded, {} ours; extra (not in the dump): {extra:?}", recorded.len(), ours.len());
        assert!(missing.is_empty(), "recorded objects we do not place: {missing:?}");
    }
}
