//! Tile libraries: the DT1 files a room draws its tiles from, and the engine's pick among tiles
//! sharing an identity.
//!
//! A room loads the files of its level type whose `LvlTypes.txt` columns its DT1 mask selects,
//! then always `Blank.dt1`, `InvisWal.dt1` and `Warp.dt1` (`0x0066F240`). A grid cell names a
//! tile by orientation, main index and sub index; `DRLGROOMTILE_GetTileLibraryEntry`
//! (`0x0066D820`) collects the matching tiles of every file in load order — within one file in
//! reverse record order, as the engine's push-front hash chains return them (`0x0060CEA0`) —
//! and picks one by rarity on the room's seed.
//!
//! Ported from libd2 `packages/drlg/src/drlg/tilegen.zig` and `lib.zig` (MIT, © 2026 jaenster).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use d2_data::GameData;
use d2_formats::dt1::{Dt1, Tile};

use crate::rng::Seed;

/// The files every room loads after its level type's (`0x0066F240`).
pub const ALWAYS_LOADED: [&str; 3] = ["Act1/Outdoors/Blank.dt1", "Act1/Barracks/InvisWal.dt1", "Act1/Barracks/Warp.dt1"];

/// Candidates `FINDTILE_Lookup` collects at most.
const MAX_CANDIDATES: usize = 40;

/// One DT1 with its identity index.
#[derive(Debug)]
pub struct TileFile {
    tiles: Vec<Tile>,
    /// Tile indices by identity, in reverse record order.
    index: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl TileFile {
    /// Index a parsed DT1.
    #[must_use]
    pub fn new(dt1: Dt1) -> Self {
        let mut index: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        for (i, t) in dt1.tiles.iter().enumerate().rev() {
            index.entry((t.orientation, t.main, t.sub)).or_default().push(i as u32);
        }
        Self { tiles: dt1.tiles, index }
    }

    fn matching(&self, orientation: i32, main: i32, sub: i32) -> impl Iterator<Item = &Tile> {
        self.index.get(&(orientation, main, sub)).into_iter().flatten().map(|&i| &self.tiles[i as usize])
    }
}

/// DT1 files read from the install, each parsed once.
#[derive(Debug, Default)]
pub struct TileFiles {
    files: Mutex<HashMap<String, Option<Arc<TileFile>>>>,
}

impl TileFiles {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A file by its path under `data\global\tiles`; `None` if the install has no such file or it
    /// does not parse.
    pub fn get(&self, data: &GameData, path: &str) -> Option<Arc<TileFile>> {
        let key = path.to_ascii_lowercase().replace('/', "\\");
        if let Some(found) = self.files.lock().ok()?.get(&key) {
            return found.clone();
        }
        let member = format!("data\\global\\tiles\\{key}");
        let file = data.read_file(&member).ok().flatten().and_then(|b| Dt1::parse(&b).ok()).map(|d| Arc::new(TileFile::new(d)));
        self.files.lock().ok()?.insert(key, file.clone());
        file
    }

    /// A level type's files by `LvlTypes.txt` column, and the always-loaded ones.
    #[must_use]
    pub fn level_type(&self, data: &GameData, level_type: i32) -> TypeLibrary {
        let columns = data
            .lvl_types()
            .get(level_type)
            .map(|t| t.files.iter().map(|f| f.as_deref().and_then(|f| self.get(data, f))).collect())
            .unwrap_or_default();
        let always = ALWAYS_LOADED.iter().map(|f| self.get(data, f)).collect();
        TypeLibrary { columns, always }
    }
}

/// A level type's tile files.
#[derive(Debug, Clone, Default)]
pub struct TypeLibrary {
    columns: Vec<Option<Arc<TileFile>>>,
    always: Vec<Option<Arc<TileFile>>>,
}

impl TypeLibrary {
    /// The files a room with this DT1 mask loads, in load order.
    #[must_use]
    pub fn room(&self, dt1_mask: u32) -> Library {
        let picked = self.columns.iter().enumerate().filter(|(i, _)| *i < 32 && dt1_mask >> i & 1 != 0).map(|(_, f)| f);
        Library { files: picked.chain(self.always.iter()).flatten().cloned().collect() }
    }
}

/// A room's loaded tile files.
#[derive(Debug, Clone, Default)]
pub struct Library {
    files: Vec<Arc<TileFile>>,
}

/// The engine's stand-in when nothing matches: `(10, 0, 0)` with no collision.
pub static NO_COLLISION: Tile = Tile { orientation: 10, main: 0, sub: 0, rarity: 1, flags: [0; 25] };

impl Library {
    /// A library from files, in load order (tests, tools).
    #[must_use]
    pub fn from_files(files: Vec<Arc<TileFile>>) -> Self {
        Self { files }
    }

    /// `TILEPROJECT_LookupTilesInAllProjects` (`0x00604AE0`): matching tiles in load order.
    fn candidates(&self, orientation: i32, main: i32, sub: i32) -> Vec<&Tile> {
        self.files.iter().flat_map(|f| f.matching(orientation, main, sub)).take(MAX_CANDIDATES).collect()
    }

    /// `DRLGROOMTILE_GetTileLibraryEntry` (`0x0066D820`): the tile for a cell of `tile_type` with
    /// grid flags `grid`, rolling `seed` when the matches have a rarity. With no match, the first
    /// `(10, 0, 0)` tile, without a roll; `None` if there is none either (the engine halts).
    pub fn pick(&self, seed: &mut Seed, tile_type: i32, grid: u32) -> Option<&Tile> {
        let (main, sub) = if grid == 0 { (0, 0) } else { ((grid >> 20 & 0x3F) as i32, (grid >> 8 & 0xFF) as i32) };
        let found = self.candidates(tile_type, main, sub);
        if found.is_empty() {
            return self.candidates(10, 0, 0).first().copied();
        }
        let sum = found.iter().fold(0u32, |s, t| s.wrapping_add(t.rarity as u32));
        // The low word, masked for a power of two (0x0066D8F4 divides the 32-bit word).
        let roll = seed.pick(sum);
        if sum == 0 {
            return Some(found[0]);
        }
        let mut left = i64::from(roll) + 1;
        let mut i = 0;
        while found.len() > 1 && left > 0 {
            left -= i64::from(found[i].rarity);
            i += 1;
            if i >= found.len() {
                break;
            }
        }
        Some(found[i.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(orientation: i32, main: i32, sub: i32, rarity: i32, flag: u8) -> Tile {
        Tile { orientation, main, sub, rarity, flags: [flag; 25] }
    }

    #[test]
    fn candidates_run_file_by_file_each_newest_record_first() {
        let a = Arc::new(TileFile::new(Dt1 { tiles: vec![tile(0, 1, 2, 1, 1), tile(0, 1, 2, 1, 2)] }));
        let b = Arc::new(TileFile::new(Dt1 { tiles: vec![tile(0, 1, 2, 1, 3), tile(10, 0, 0, 1, 9)] }));
        let lib = Library::from_files(vec![a, b]);
        let flags: Vec<u8> = lib.candidates(0, 1, 2).iter().map(|t| t.flags[0]).collect();
        assert_eq!(flags, [2, 1, 3]);
    }

    #[test]
    fn a_pick_walks_the_rarity_ladder_on_the_rooms_seed() {
        let file = Arc::new(TileFile::new(Dt1 { tiles: vec![tile(0, 1, 2, 3, 1), tile(0, 1, 2, 1, 2), tile(10, 0, 0, 1, 9)] }));
        let lib = Library::from_files(vec![file]);
        let grid = (1 << 20) | (2 << 8) | 2;
        // Candidates are [rarity 1 (flag 2), rarity 3 (flag 1)], sum 4: roll 0 takes the first,
        // 1..=3 the second.
        for low in 0..64u32 {
            let mut seed = Seed::new(low, 0x29A);
            let roll = Seed::new(low, 0x29A).pick(4);
            let picked = lib.pick(&mut seed, 0, grid).unwrap();
            assert_eq!(picked.flags[0], if roll == 0 { 2 } else { 1 }, "low {low}");
            assert_ne!(seed, Seed::new(low, 0x29A), "rolled");
        }
        let mut seed = Seed::new(5, 0x29A);
        assert_eq!(lib.pick(&mut seed, 3, grid).unwrap().flags[0], 9, "no match: the (10,0,0) tile");
        assert_eq!(seed, Seed::new(5, 0x29A), "without a roll");
    }

    #[test]
    fn a_mask_selects_columns_before_the_always_loaded_files() {
        let f = |flag| Some(Arc::new(TileFile::new(Dt1 { tiles: vec![tile(0, 0, 0, 1, flag)] })));
        let types = TypeLibrary { columns: vec![f(1), None, f(3)], always: vec![f(7)] };
        let lib = types.room(0b101);
        let flags: Vec<u8> = lib.candidates(0, 0, 0).iter().map(|t| t.flags[0]).collect();
        assert_eq!(flags, [1, 3, 7]);
        assert_eq!(types.room(0b010).candidates(0, 0, 0).len(), 1);
    }
}
