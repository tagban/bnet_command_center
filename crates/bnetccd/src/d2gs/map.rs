//! What the admin panel's live map reads from running games: each game's levels and players,
//! a level's collision map and marks, and what stands in it right now.
//!
//! Everything comes from the server's own copy of the act — the same rooms, collision maps and
//! spawned units it sends clients — so a difference from what a client draws is a server bug.

use serde::Serialize;

use d2_drlg::preset::UnitClass;
use d2_drlg::world::RoomId;
use d2_game::population::Spawned;

use super::{GameServer, SUBCLASS_WAYPOINT};

/// A grid cell no room's collision map covers.
pub const NO_MAP: u8 = 0xFF;

/// A running game.
#[derive(Debug, Serialize)]
pub struct MapGame {
    /// Game id.
    pub id: u16,
    /// Game name.
    pub name: String,
    /// 0 Normal, 1 Nightmare, 2 Hell.
    pub difficulty: u8,
    /// The act's map seed.
    pub map_seed: String,
    /// Seconds since it was created.
    pub age_secs: u64,
    /// Players in it and where they stand.
    pub players: Vec<MapPlayer>,
    /// Its walkable levels.
    pub levels: Vec<MapLevelInfo>,
}

/// A player's position.
#[derive(Debug, Clone, Serialize)]
pub struct MapPlayer {
    /// Character name.
    pub name: String,
    /// The level it stands in, if any.
    pub level: Option<i32>,
    /// World subtiles.
    pub x: i32,
    /// World subtiles.
    pub y: i32,
}

/// A level's name and rectangle.
#[derive(Debug, Serialize)]
pub struct MapLevelInfo {
    /// `Levels.txt` id.
    pub id: i32,
    /// `Levels.txt` name.
    pub name: String,
    /// Rectangle in tiles.
    pub x: i32,
    /// Rectangle in tiles.
    pub y: i32,
    /// Rectangle in tiles.
    pub w: i32,
    /// Rectangle in tiles.
    pub h: i32,
}

/// A level's static picture.
#[derive(Debug, Serialize)]
pub struct MapLevel {
    /// Its name and rectangle.
    pub info: MapLevelInfo,
    /// Collision flags per subtile over the level's rectangle (`w × 5` by `h × 5`), row by row,
    /// base64; [`NO_MAP`] where no room has a map.
    pub grid: String,
    /// Its rooms in tiles: `[x, y, w, h, LvlPrest def or 0]`.
    pub rooms: Vec<[i32; 5]>,
    /// Scanned pieces — cave mouths, towers, graveyards — at their room's middle.
    pub entrances: Vec<MapMark>,
    /// Map objects known before any room spawns them: waypoints, shrines, wells, and so on.
    pub marks: Vec<MapMark>,
}

/// A named spot.
#[derive(Debug, Clone, Serialize)]
pub struct MapMark {
    /// What it is.
    pub name: String,
    /// `waypoint`, `shrine`, `npc`, `object`, `monster` or `entrance`.
    pub kind: &'static str,
    /// World subtiles.
    pub x: i32,
    /// World subtiles.
    pub y: i32,
}

/// What stands in a level now.
#[derive(Debug, Serialize)]
pub struct MapLive {
    /// Players in the game.
    pub players: Vec<MapPlayer>,
    /// Spawned units of the level's populated rooms.
    pub units: Vec<MapMark>,
    /// Rooms of the level populated so far.
    pub populated_rooms: usize,
    /// Rooms in the level.
    pub rooms: usize,
}

/// A level's shown name (`LevelName`), or its table name when that is blank.
fn level_name(def: &d2_data::levels::LevelDef) -> String {
    if def.level_name.is_empty() { def.name.clone() } else { def.level_name.clone() }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl GameServer {
    fn map_players(&self, game: &super::Game) -> Vec<MapPlayer> {
        game.positions
            .iter()
            .map(|(name, &(x, y))| MapPlayer {
                name: name.clone(),
                level: game.world.as_ref().and_then(|w| w.room_at(x, y)).map(|r| r.level),
                x,
                y,
            })
            .collect()
    }

    /// Every running game as the public feed shows it: name, whether it has a password,
    /// difficulty, players connected, seconds since it was made.
    #[must_use]
    pub fn public_games(&self) -> Vec<(String, bool, u8, usize, u64)> {
        let g = self.lock();
        let mut games: Vec<_> = g
            .by_id
            .values()
            .map(|game| (game.name.clone(), !game.password.is_empty(), game.difficulty, game.connected.len(), game.created.elapsed().as_secs()))
            .collect();
        games.sort_by(|a, b| a.4.cmp(&b.4));
        games
    }

    /// Every running game.
    #[must_use]
    pub fn map_games(&self) -> Vec<MapGame> {
        let g = self.lock();
        let mut games: Vec<MapGame> = g
            .by_id
            .iter()
            .map(|(&id, game)| MapGame {
                id,
                name: game.name.clone(),
                difficulty: game.difficulty,
                map_seed: format!("{:#010x}", game.map_seed),
                age_secs: game.created.elapsed().as_secs(),
                players: self.map_players(game),
                levels: game.world.as_ref().map_or_else(Vec::new, |w| {
                    w.levels()
                        .iter()
                        .map(|l| MapLevelInfo {
                            id: l.id,
                            name: self.rules.as_ref().and_then(|r| r.levels().get(l.id)).map_or_else(|| format!("Level {}", l.id), level_name),
                            x: l.area.x,
                            y: l.area.y,
                            w: l.area.w,
                            h: l.area.h,
                        })
                        .collect()
                }),
            })
            .collect();
        games.sort_by_key(|g| g.id);
        games
    }

    /// A level's collision map, rooms and known marks.
    #[must_use]
    pub fn map_level(&self, game_id: u16, level_id: i32) -> Option<MapLevel> {
        let g = self.lock();
        let world = g.by_id.get(&game_id)?.world.as_ref()?;
        let level = world.levels().iter().find(|l| l.id == level_id)?;
        let rules = self.rules.as_ref();
        let (gw, gh) = (level.area.w * 5, level.area.h * 5);
        let mut grid = vec![NO_MAP; (gw.max(0) * gh.max(0)) as usize];
        for map in &level.collision {
            let (ox, oy) = ((map.area.x - level.area.x) * 5, (map.area.y - level.area.y) * 5);
            let w = map.area.w * 5;
            for (i, &cell) in map.cells.iter().enumerate() {
                let (x, y) = (ox + i as i32 % w, oy + i as i32 / w);
                if x >= 0 && y >= 0 && x < gw && y < gh {
                    grid[(y * gw + x) as usize] = cell & 0x7F;
                }
            }
        }
        let rooms = level.rooms.iter().enumerate().map(|(i, r)| [r.x, r.y, r.w, r.h, level.pieces.get(i).copied().unwrap_or(0)]).collect();
        let entrances = level
            .pieces
            .iter()
            .zip(&level.rooms)
            .filter_map(|(&piece, room)| {
                let row = rules?.lvl_prests().by_def(piece).filter(|r| piece != 0 && r.scan)?;
                let name = row.name.trim_start_matches("Act 1 - ").to_string();
                Some(MapMark { name, kind: "entrance", x: room.x * 5 + room.w * 5 / 2, y: room.y * 5 + room.h * 5 / 2 })
            })
            .chain(level.warps.iter().filter_map(|w| {
                let (to, _) = world.warp_destination(rules?, level.id, w.slot)?;
                let name = rules?.levels().get(to).map_or_else(|| format!("level {to}"), level_name);
                Some(MapMark { name: format!("To {name}"), kind: "entrance", x: w.x, y: w.y })
            }))
            .collect();
        let marks = level
            .units
            .iter()
            .filter_map(|u| {
                let (name, kind) = match &u.class {
                    UnitClass::Object(class) => {
                        let def = rules.and_then(|r| r.objects().get(*class));
                        let name = def.map_or_else(|| format!("object {class}"), |d| d.name.clone());
                        let kind = match def {
                            Some(d) if d.sub_class & SUBCLASS_WAYPOINT != 0 => "waypoint",
                            Some(d) if d.init_fn == 1 => "shrine",
                            _ => "object",
                        };
                        (name, kind)
                    }
                    UnitClass::Monster(m) => (format!("{m:?}"), "npc"),
                    UnitClass::Other { .. } => return None,
                };
                Some(MapMark { name, kind, x: u.x, y: u.y })
            })
            .collect();
        let info = MapLevelInfo {
            id: level.id,
            name: rules.and_then(|r| r.levels().get(level.id)).map_or_else(|| format!("Level {}", level.id), level_name),
            x: level.area.x,
            y: level.area.y,
            w: level.area.w,
            h: level.area.h,
        };
        Some(MapLevel { info, grid: base64(&grid), rooms, entrances, marks })
    }

    /// The players, and the units spawned in a level's rooms so far.
    #[must_use]
    pub fn map_live(&self, game_id: u16, level_id: i32) -> Option<MapLive> {
        let g = self.lock();
        let game = g.by_id.get(&game_id)?;
        let world = game.world.as_ref()?;
        let level = world.levels().iter().find(|l| l.id == level_id)?;
        let mut units = Vec::new();
        let mut populated_rooms = 0;
        if let Some(population) = &game.population {
            for index in 0..level.rooms.len() {
                let Some(spawned) = population.units(RoomId { level: level_id, index }) else { continue };
                populated_rooms += 1;
                for unit in spawned {
                    units.push(match *unit {
                        Spawned::Monster { class, x, y, .. } => {
                            let m = self.rules.as_ref().and_then(|r| r.monsters().get(i32::from(class)));
                            let name = m.map_or_else(|| format!("monster {class}"), |m| m.id.clone());
                            let kind = if m.is_some_and(|m| m.npc || m.alignment() != 0) { "npc" } else { "monster" };
                            MapMark { name, kind, x: i32::from(x), y: i32::from(y) }
                        }
                        Spawned::Object { class, x, y, .. } => {
                            let name = self.rules.as_ref().and_then(|r| r.objects().name(i32::from(class))).unwrap_or("object").to_string();
                            MapMark { name, kind: "object", x: i32::from(x), y: i32::from(y) }
                        }
                    });
                }
            }
        }
        Some(MapLive { players: self.map_players(game), units, populated_rooms, rooms: level.rooms.len() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_like_the_browser_expects() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(&[0xFF, 0x00, 0x7F, 0x01]), "/wB/AQ==");
    }
}
