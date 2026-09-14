//! `Levels.txt`: each area's act, size, fixed offset and generator type.

use d2_formats::excel::Table;

use crate::Error;

/// How an area is generated (`DrlgType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrlgType {
    /// No generator (the `Null` row).
    None,
    /// A grid of preset rooms (`LvlMaze.txt`).
    Maze,
    /// One fixed map file (`LvlPrest.txt`): towns and set pieces.
    Preset,
    /// Outdoor wilderness.
    Wilderness,
}

/// One `Levels.txt` row: the geometry the level generator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelDef {
    /// `Id`.
    pub id: i32,
    /// `Name`.
    pub name: String,
    /// `LevelName`: the string key the client shows, e.g. `Blood Moor`.
    pub level_name: String,
    /// `Act`, 0-based.
    pub act: u8,
    /// `SizeX`/`SizeY` per difficulty (Normal, Nightmare, Hell), in tiles.
    pub size: [(i32, i32); 3],
    /// `OffsetX`/`OffsetY`, in tiles.
    pub offset: (i32, i32),
    /// `Depend`: the level this one's offset is relative to (0 = none).
    pub depend: i32,
    /// `DrlgType`.
    pub drlg_type: DrlgType,
    /// `LevelType`: the `LvlTypes.txt` row naming its tile set.
    pub level_type: i32,
    /// `Waypoint`: the level's bit in a player's waypoint flags (record `+0xE4`), `None` for
    /// a level without a waypoint (255).
    pub waypoint: Option<u8>,
    /// `Vis0`..`Vis7`: levels seen from this one (0 = none).
    pub vis: [i32; 8],
    /// `Warp0`..`Warp7`: the `LvlWarp.txt` id reaching each `Vis` level, -1 for none (an open
    /// edge rather than a door).
    pub warp: [i32; 8],
    /// `SubType`: the `LvlSub.txt` group of terrain pieces its outdoor rooms roll, -1 for none.
    pub sub_type: i32,
    /// `SubTheme`: which probability column of that group, -1 for none.
    pub sub_theme: i32,
    /// `SubWaypoint`: the `LvlSub.txt` group its waypoint comes from, -1 for none.
    pub sub_waypoint: i32,
    /// `SubShrine`: the `LvlSub.txt` group its shrines come from, -1 for none.
    pub sub_shrine: i32,
    /// `WarpDist`: monsters do not spawn closer than its square root, in subtiles, to where
    /// players arrive (`0x0054DB50`).
    pub warp_dist: i32,
    /// What monsters its rooms spawn.
    pub monsters: LevelMonsters,
}

/// A level's `Levels.txt` monster columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LevelMonsters {
    /// `NumMon`: monster types a game picks for the level.
    pub types: i32,
    /// `rangedspawn`: the first type picked should be ranged.
    pub ranged_first: bool,
    /// `MonLvl1`..`MonLvl3`: the level of monsters on Nightmare and Hell in a classic game
    /// (Normal takes each class's own `Level`).
    pub area_level: [i32; 3],
    /// `MonLvl1Ex`..`MonLvl3Ex`: the same in an expansion game.
    pub area_level_expansion: [i32; 3],
    /// `MonDen`, per difficulty: spawn density, in 100000ths per 3×3-subtile slot.
    pub density: [i32; 3],
    /// `MonUMin`/`MonUMax`, per difficulty: unique packs.
    pub uniques: [(i32, i32); 3],
    /// `mon1`..`mon10`: Normal's candidates (`MonStats.txt` ids).
    pub normal: Vec<String>,
    /// `nmon1`..`nmon10`: Nightmare's and Hell's.
    pub nightmare: Vec<String>,
    /// `umon1`..`umon10`: unique pack leaders on Normal.
    pub unique: Vec<String>,
}

/// All levels, by id.
#[derive(Debug, Clone, Default)]
pub struct Levels {
    by_id: Vec<Option<LevelDef>>,
}

impl Levels {
    /// Parse `Levels.txt`.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if a column the generator needs is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Id", "Act", "SizeX", "SizeY", "SizeX(N)", "SizeY(N)", "SizeX(H)", "SizeY(H)", "OffsetX", "OffsetY", "Depend", "DrlgType", "LevelType"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "levels.txt", problem: format!("no {column} column") });
            }
        }
        let mut by_id: Vec<Option<LevelDef>> = Vec::new();
        for row in t.rows() {
            let int = |c: &str| row.int(c).unwrap_or(0) as i32;
            let names = |prefix: &str| -> Vec<String> {
                (1..=25).filter_map(|i| row.get(&format!("{prefix}{i}"))).filter(|n| !n.is_empty()).map(str::to_string).collect()
            };
            let id = int("Id");
            if id <= 0 {
                continue; // the Null row
            }
            let def = LevelDef {
                id,
                name: row.get("Name").unwrap_or_default().to_string(),
                level_name: row.get("LevelName").unwrap_or_default().to_string(),
                act: int("Act") as u8,
                size: [(int("SizeX"), int("SizeY")), (int("SizeX(N)"), int("SizeY(N)")), (int("SizeX(H)"), int("SizeY(H)"))],
                offset: (int("OffsetX"), int("OffsetY")),
                depend: int("Depend"),
                drlg_type: match int("DrlgType") {
                    1 => DrlgType::Maze,
                    2 => DrlgType::Preset,
                    3 => DrlgType::Wilderness,
                    _ => DrlgType::None,
                },
                level_type: int("LevelType"),
                waypoint: row.int("Waypoint").and_then(|w| u8::try_from(w).ok()).filter(|&w| w != 255),
                vis: std::array::from_fn(|i| int(&format!("Vis{i}"))),
                warp: std::array::from_fn(|i| row.int(&format!("Warp{i}")).map_or(-1, |w| w as i32)),
                sub_type: row.int("SubType").map_or(-1, |v| v as i32),
                sub_theme: row.int("SubTheme").map_or(-1, |v| v as i32),
                sub_waypoint: row.int("SubWaypoint").map_or(-1, |v| v as i32),
                sub_shrine: row.int("SubShrine").map_or(-1, |v| v as i32),
                warp_dist: int("WarpDist"),
                monsters: LevelMonsters {
                    types: int("NumMon"),
                    ranged_first: int("rangedspawn") != 0,
                    area_level: [int("MonLvl1"), int("MonLvl2"), int("MonLvl3")],
                    area_level_expansion: [int("MonLvl1Ex"), int("MonLvl2Ex"), int("MonLvl3Ex")],
                    density: [int("MonDen"), int("MonDen(N)"), int("MonDen(H)")],
                    uniques: [
                        (int("MonUMin"), int("MonUMax")),
                        (int("MonUMin(N)"), int("MonUMax(N)")),
                        (int("MonUMin(H)"), int("MonUMax(H)")),
                    ],
                    normal: names("mon"),
                    nightmare: names("nmon"),
                    unique: names("umon"),
                },
            };
            let at = id as usize;
            if by_id.len() <= at {
                by_id.resize(at + 1, None);
            }
            by_id[at] = Some(def);
        }
        Ok(Self { by_id })
    }

    /// A level by id.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&LevelDef> {
        usize::try_from(id).ok().and_then(|i| self.by_id.get(i)).and_then(Option::as_ref)
    }
}
