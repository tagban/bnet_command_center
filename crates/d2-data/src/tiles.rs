//! The tables behind a room's tiles: `LvlTypes.txt` (which DT1 tile libraries a level type
//! loads) and `LvlWarp.txt` (the stairs and cave mouths that join levels).

use d2_formats::excel::Table;

use crate::Error;

/// Columns `File 1`..`File 32`.
pub const TYPE_FILES: usize = 32;

/// One `LvlTypes.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvlType {
    /// `Id`, the `Levels.txt` `LevelType`.
    pub id: i32,
    /// `File 1`..`File 32`, relative to `data\global\tiles`, `None` for `0` or blank. A room's
    /// DT1 mask picks these by column.
    pub files: Vec<Option<String>>,
}

/// `LvlTypes.txt`.
#[derive(Debug, Clone, Default)]
pub struct LvlTypes {
    rows: Vec<LvlType>,
}

impl LvlTypes {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Id` or `File 1` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Id", "File 1"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "lvltypes.txt", problem: format!("no {column} column") });
            }
        }
        let rows = t
            .rows()
            .filter_map(|row| {
                let id = row.int("Id")? as i32;
                let files = (1..=TYPE_FILES)
                    .map(|i| row.get(&format!("File {i}")).filter(|f| !f.is_empty() && *f != "0").map(str::to_string))
                    .collect();
                Some(LvlType { id, files })
            })
            .collect();
        Ok(Self { rows })
    }

    /// The row for a level type.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&LvlType> {
        self.rows.iter().find(|r| r.id == id)
    }
}

/// One `LvlWarp.txt` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LvlWarp {
    /// `Id`, as `Levels.txt` `Warp0`..`Warp7` name it.
    pub id: i32,
    /// `LitVersion`: the warp has a lit tile set drawn over it.
    pub lit_version: bool,
    /// `Tiles`: the sub index the lit tiles add.
    pub tiles: i32,
    /// `Direction`: `b`oth, `l`eft or `r`ight, as its byte.
    pub direction: u8,
    /// `ExitWalkX`/`ExitWalkY`: how far, in subtiles, a player arriving through the warp walks
    /// on from its tile (`0x005550B0` reads them at `+0x14`/`+0x18`).
    pub exit_walk: (i32, i32),
}

/// One `LvlMaze.txt` row: how a maze level grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LvlMaze {
    /// `Level`.
    pub level: i32,
    /// `Rooms`, `Rooms(N)`, `Rooms(H)`: cells to grow to.
    pub rooms: [i32; 3],
    /// `SizeX`/`SizeY`: a cell's size in tiles.
    pub size: (i32, i32),
    /// `Merge`: per-mille chance two touching cells join.
    pub merge: i32,
}

/// `LvlMaze.txt`.
#[derive(Debug, Clone, Default)]
pub struct LvlMazes {
    rows: Vec<LvlMaze>,
}

impl LvlMazes {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Level` or `SizeX` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Level", "SizeX"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "lvlmaze.txt", problem: format!("no {column} column") });
            }
        }
        let rows = t
            .rows()
            .filter_map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                Some(LvlMaze {
                    level: row.int("Level")? as i32,
                    rooms: [int("Rooms"), int("Rooms(N)"), int("Rooms(H)")],
                    size: (int("SizeX"), int("SizeY")),
                    merge: int("Merge"),
                })
            })
            .collect();
        Ok(Self { rows })
    }

    /// `TXT_LvlMaze_FindLineByLevelId`: the first row for a level.
    #[must_use]
    pub fn for_level(&self, level: i32) -> Option<&LvlMaze> {
        self.rows.iter().find(|r| r.level == level && level != 0)
    }
}

/// `LvlWarp.txt`, in file order.
#[derive(Debug, Clone, Default)]
pub struct LvlWarps {
    rows: Vec<LvlWarp>,
}

impl LvlWarps {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Id` or `Direction` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Id", "Direction"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "lvlwarp.txt", problem: format!("no {column} column") });
            }
        }
        let rows = t
            .rows()
            .filter_map(|row| {
                Some(LvlWarp {
                    id: row.int("Id")? as i32,
                    lit_version: row.int("LitVersion").unwrap_or(0) != 0,
                    tiles: row.int("Tiles").unwrap_or(0) as i32,
                    direction: row.get("Direction").and_then(|d| d.bytes().next()).unwrap_or(0),
                    exit_walk: (row.int("ExitWalkX").unwrap_or(0) as i32, row.int("ExitWalkY").unwrap_or(0) as i32),
                })
            })
            .collect();
        Ok(Self { rows })
    }

    /// `TXT_LvlWarp_Setup` (`0x0061F310`): the first row with this id whose direction is
    /// `wanted`, or either one when `wanted` or the row's direction is `b`. An index into
    /// [`LvlWarps::row`].
    #[must_use]
    pub fn setup(&self, id: i32, wanted: u8) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id && (wanted == b'b' || r.direction == b'b' || r.direction == wanted))
    }

    /// A row by index.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&LvlWarp> {
        self.rows.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_keeps_its_file_columns_and_a_warp_is_found_by_direction() {
        let types = Table::parse(b"Name\tId\tFile 1\tFile 2\tFile 3\r\nWild\t2\tA/Floor.dt1\t0\tA/Trees.dt1\r\n");
        let types = LvlTypes::from_table(&types).unwrap();
        let wild = types.get(2).unwrap();
        assert_eq!(wild.files[..3], [Some("A/Floor.dt1".into()), None, Some("A/Trees.dt1".into())]);
        assert_eq!(wild.files.len(), TYPE_FILES);

        let warps = Table::parse(b"Name\tId\tLitVersion\tTiles\tDirection\r\nL\t5\t1\t2\tl\r\nR\t5\t0\t3\tr\r\n");
        let warps = LvlWarps::from_table(&warps).unwrap();
        assert_eq!(warps.setup(5, b'r'), Some(1));
        assert_eq!(warps.setup(5, b'b'), Some(0));
        assert_eq!(warps.setup(6, b'b'), None);
        assert_eq!(warps.row(0).map(|r| (r.lit_version, r.tiles)), Some((true, 2)));
    }
}
