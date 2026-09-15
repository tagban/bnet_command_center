//! Tables the 1.14d engine keeps in `Game.exe` itself, read from the operator's copy.
//!
//! Only build 1.14.3.71 is accepted: the addresses are that build's. A few values every
//! client relies on are checked after reading as a second guard.

use d2_formats::pe::{self, Image};

use crate::Error;

/// Client-to-server opcodes the engine sizes (0x00..=0x70).
pub const CLIENT_OPCODES: usize = 0x71;
/// Server-to-client opcodes the engine sizes (0x00..=0xB4).
pub const SERVER_OPCODES: usize = 0xB5;
/// Preset object slots per act in `gpsPresetObjectTable`.
pub const PRESET_OBJECTS_PER_ACT: usize = 150;

/// Where the tables are in `Game.exe` 1.14.3.71.
mod address {
    pub const HUFFMAN_CODE_LENGTHS: u32 = 0x0070_76C0;
    pub const CLIENT_PACKET_SIZES: u32 = 0x0073_0DC0;
    pub const SERVER_PACKET_SIZES: u32 = 0x0073_0AE8;
    /// `gpsPresetObjectTable`, read by `DRLGPRESET_GetObjectIdFromActTable` (`0x006658E0`).
    pub const PRESET_OBJECTS: u32 = 0x0074_8AD8;
    /// The act environment's clock speeds, ticks per degree (`0x0061BE40`).
    pub const CLOCK_SPEEDS: u32 = 0x0074_43E4;
    /// The six day periods, `{angle, light phase, colour}` (`0x0061BEE0`, `0x0061C240`).
    pub const DAY_PERIODS: u32 = 0x0074_43F0;
    /// `gaOutdoorsLinkOffsets`: per level edge, the tile offset from a border cell into the
    /// neighbouring level (`GetOutLinkVisFlag`, `0x00675770` area).
    pub const OUTDOOR_LINK_OFFSETS: u32 = 0x006F_05D8;
    /// Border and corner preset ids by road type, one row per direction pair
    /// (`DRLGOUTPLACE_GetRoadPresetId` / `GetAdjacentRoadPresetId`); rows from 0.
    pub const OUTDOOR_ROAD_PRESETS: u32 = 0x006F_0620;
    /// Direction index by `dx + dy * 3`, from -4.
    pub const OUTDOOR_ROAD_DIRECTIONS: u32 = 0x006F_0FC0;
    /// Corner row by a pair of edge directions, from -40 (`PlaceAct1245OutdoorBorders`, `0x00675850`).
    pub const OUTDOOR_CORNERS: u32 = 0x006F_0FE8;
    /// Act I wilderness road flags: 15 rules of `{level, not level, not level, direction,
    /// next direction, flag}` (`0x00677180`).
    pub const OUTDOOR_ROAD_FLAGS: u32 = 0x006F_1258;
    /// Left/right variants of the vertical border pieces by preset id
    /// (`DRLGOUTROOM_SpawnVerticalBorderPresets`, `0x0067FE90`).
    pub const OUTDOOR_VERTICAL_BORDERS: u32 = 0x006F_2680;
    /// The eight neighbour offsets tried around a road cell, `y` then `x`, signed bytes
    /// (`SpawnRandomOutdoorDS1`, `0x006745E0`).
    pub const OUTDOOR_NEIGHBOURS_Y: u32 = 0x006F_060C;
    pub const OUTDOOR_NEIGHBOURS_X: u32 = 0x006F_0614;
    /// Link grid flag per shrine style (`SpawnAct12Shrines`, `0x00674E40`).
    pub const OUTDOOR_SHRINE_STYLES: u32 = 0x006F_061C;
    /// `DRLGPATH_GetPathDirection`'s table, three ints per 5×5 direction cell.
    pub const OUTDOOR_PATH_DIRECTIONS: u32 = 0x006F_1518;
    /// Road target search offsets, `y` then `x` (`DRLGOUTROOM_ComputeExitTargetPositions`, `0x00681000`).
    pub const OUTDOOR_SPIRAL_Y: u32 = 0x006F_2800;
    pub const OUTDOOR_SPIRAL_X: u32 = 0x006F_2810;
    /// Road vertex jitter directions, `x` then `y` (`DRLGOUTROOM_BuildVertexPathsWithJitter`, `0x00681240`).
    pub const OUTDOOR_JITTER_X: u32 = 0x006F_2820;
    pub const OUTDOOR_JITTER_Y: u32 = 0x006F_2830;
    /// The road search's direction cycles and step deltas, signed bytes (`0x006817D0`).
    pub const OUTDOOR_PATH_DELTAS: u32 = 0x006F_2840;
    /// `gaWallNeighborOrientTable`: a road edge cell's tile orientation by its eight
    /// neighbours (`DRLGOUTROOM_ComputeWallOrientations`, `0x00680B10`).
    pub const OUTDOOR_EDGE_ORIENTATIONS: u32 = 0x006F_2700;
    /// The graphics codes a save's appearance bytes were first laid out for, `{code, item type}`
    /// by slot; the graphics table builder keeps weapons and armour out of each other's slots
    /// with it (`0x0063D710`).
    pub const RESERVED_GRAPHICS: u32 = 0x0074_4CA8;
    /// The front end's own copy of that list, `{code, hand class, item type}` by slot, which the
    /// character-select screen draws from (`D2Comp.cpp`, `0x00506000`).
    pub const FRONT_END_GRAPHICS: u32 = 0x0072_E1E0;
    /// The front end's player class tokens, after their count (`0x00503740`).
    pub const FRONT_END_CLASSES: u32 = 0x0072_E04C;
    /// Its animation mode tokens, after their count.
    pub const FRONT_END_MODES: u32 = 0x0072_E0B4;
    /// Its body component tokens, followed by their count.
    pub const FRONT_END_COMPONENTS: u32 = 0x0072_E108;
    /// Its weapon class tokens, index 0 empty, followed by their count.
    pub const FRONT_END_WEAPON_CLASSES: u32 = 0x0072_E15C;
    /// An item's `wclass` code to a row of the hand-class list below, followed by the count.
    pub const FRONT_END_ITEM_WEAPON_CLASSES: u32 = 0x0072_EF68;
    /// Hand class (a weapon class token index) by that row; row 0 is anything unlisted.
    pub const FRONT_END_HAND_CLASSES: u32 = 0x0072_EF30;
    /// Which of a sprite file's directions a unit facing index draws, one row of 32 per
    /// direction count (row `log2(count) + 1`), read by `0x00600C70`.
    pub const FILE_DIRECTIONS: u32 = 0x006E_3A20;
    /// `DRLGPRESET_FindPresetTypeIndex` (`0x0066D960`): 37 `{level, first row, last row}`.
    pub const PRESET_TILE_LEVELS: u32 = 0x006E_EFC8;
    /// Its rows, `{main, orientation, sub flag, class, unit type, x offset, y offset}`.
    pub const PRESET_TILE_ROWS: u32 = 0x006E_F188;
    /// `gaWarpTileOffsetX/Y`: the four lit warp floor tiles' offsets, `x, y` pairs (`0x0066E360`).
    pub const WARP_TILE_OFFSETS: u32 = 0x006E_F554;
    /// `gnRoomTileMappingTransitionByType`: `[row * 7 + held tile type]` (`0x0066E740`).
    pub const TILE_MAPPING_TRANSITIONS: u32 = 0x006E_F574;
    /// `gnRoomTileMappingByTypeAndLayer`: a seam cell's tile type to a transition row.
    pub const TILE_MAPPING_BY_TYPE: u32 = 0x006E_F620;
    /// `VS_FIXEDFILEINFO` 1.14.3.71.
    pub const FILE_VERSION: (u32, u32) = (0x0001_000E, 0x0003_0047);
}

/// The engine's own tables.
#[derive(Debug, Clone)]
pub struct EngineData {
    /// The D2GS wire's Huffman code lengths, one per byte value.
    pub huffman_code_lengths: [u8; 256],
    /// Client-to-server packet sizes by opcode: `>0` fixed, `-1` variable, `0` invalid.
    pub client_packet_sizes: [i32; CLIENT_OPCODES],
    /// Server-to-client packet sizes by opcode, same convention.
    pub server_packet_sizes: [i32; SERVER_OPCODES],
    /// Object class for each act's DS1 preset object ids below 150.
    preset_objects: Vec<i32>,
    /// The act clock's speeds, ticks per degree; a new act uses the first.
    pub clock_speeds: [i32; 3],
    /// The day's six periods: the clock angle each starts at and its light phase (0 day, 1 dusk,
    /// 2 night, 3 dawn).
    pub day_periods: [DayPeriod; 6],
    /// What the wilderness generator looks up.
    pub outdoor: OutdoorTables,
    /// The graphics slots appearance bytes were first laid out for, `(code, item type)` by
    /// slot — what [`crate::appearance::Graphics::build`] reads.
    pub reserved_graphics: Vec<(crate::items::Code, i32)>,
    /// What the front end draws characters with.
    pub front_end: FrontEndTables,
    /// What a room's tiles are built with.
    pub tiles: TileTables,
}

/// The room tile builder's tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileTables {
    /// Levels whose preset wall tiles place units: `{level, first row, last row}` of
    /// [`Self::preset_rows`].
    pub preset_levels: [[i32; 3]; 37],
    /// `{main, orientation, sub flag, class, unit type, x offset, y offset}`.
    pub preset_rows: [[i32; 7]; 34],
    /// The four lit warp floor tiles, `(x, y)` from the warp's corner.
    pub warp_tile_offsets: [(i32, i32); 4],
    /// The tile type a seam cell's existing tile becomes: `[row * 7 + its type]`.
    pub mapping_transitions: [i32; 43],
    /// A visiting cell's tile type to a row of [`Self::mapping_transitions`]; -1 keep, -2 none.
    pub mapping_by_type: [i32; 20],
}

/// The front end's character-drawing tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontEndTables {
    /// `(code, hand class, item type)` by graphics slot, 255 of them.
    pub graphics: Vec<(crate::items::Code, i32, i32)>,
    /// Player class tokens (`AM`, `SO`, …, then the fallbacks `RO`, `RH`, …), space-padded.
    pub classes: Vec<crate::items::Code>,
    /// Animation mode tokens (`DT`, `NU`, …, `TN` is 5).
    pub modes: Vec<crate::items::Code>,
    /// Body component tokens (`HD`, `TR`, …).
    pub components: Vec<crate::items::Code>,
    /// Weapon class tokens by hand class (0 empty, 1 `hth`, 2 `1ht`, …).
    pub weapon_classes: Vec<crate::items::Code>,
    /// An item's `wclass` code and its row in [`Self::hand_classes`].
    pub item_weapon_classes: Vec<(crate::items::Code, u32)>,
    /// Hand class by row.
    pub hand_classes: Vec<i32>,
    /// The file direction for a facing index, by row `log2(directions) + 1` (0..=6) then index
    /// (`0x00600C70`): for 16 directions, facing 0 is file direction 4, toward the viewer.
    pub file_directions: Vec<[i32; 32]>,
}

/// The outdoor (wilderness) generator's lookup tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutdoorTables {
    /// Per level edge (0..=3), `(x, y)` tile offsets from a border cell's corner into the
    /// neighbouring level.
    pub link_offsets: [(i32, i32); 4],
    /// Preset ids, 13 rows of 4 road types: row `direction + 1` for an edge's border pieces, the
    /// corner row for a corner.
    pub road_presets: [[i32; 4]; 13],
    /// Direction index (0..=3, -1 none) by `dx + dy * 3 + 4`.
    pub road_directions: [i32; 9],
    /// Corner row (-1 none) by the pair index `+ 40`.
    pub corners: [i32; 81],
    /// Act I road flag rules: `[level, not level, not level, direction, next direction, flag]`.
    pub road_flags: [[i32; 6]; 15],
    /// Vertical border piece variants by preset id: `[left, right]`.
    pub vertical_borders: [[i32; 2]; 16],
    /// The eight `(x, y)` neighbours tried around a road cell.
    pub neighbours: [(i32, i32); 8],
    /// Link grid flag per shrine style.
    pub shrine_styles: [i32; 4],
    /// Direction (0..=7) toward a target by 5×5 direction cell.
    pub path_directions: [i32; 25],
    /// Road target search offsets `(x, y)` by step.
    pub spiral: [(i32, i32); 4],
    /// Road vertex jitter `(x, y)` by step.
    pub jitter: [(i32, i32); 4],
    /// Four 4-entry direction cycles, then y deltas and x deltas by direction.
    pub path_deltas: [i32; 24],
    /// A road edge cell's tile orientation (0 none) by the mask of its set neighbours: bit 7 NE,
    /// 6 E, 5 SE, 4 N, 3 S, 2 NW, 1 W, 0 SW.
    pub edge_orientations: [u8; 256],
}

/// One period of an act's day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DayPeriod {
    /// Clock angle, in degrees, the period starts at.
    pub angle: i32,
    /// Light phase: 0 day, 1 dusk, 2 night, 3 dawn.
    pub phase: i32,
}

impl EngineData {
    /// Read the tables from a whole `Game.exe` file.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] naming what did not match 1.14d.
    pub fn from_game_exe(file: &[u8]) -> Result<Self, Error> {
        let bad = |problem: String| Error::BadTable { table: "Game.exe", problem };
        let image = Image::parse(file).ok_or_else(|| bad("not a PE image".into()))?;
        match pe::file_version(file) {
            Some(v) if v == address::FILE_VERSION => {}
            Some((ms, ls)) => {
                return Err(bad(format!(
                    "version {}.{}.{}.{} (need 1.14.3.71)",
                    ms >> 16,
                    ms & 0xFFFF,
                    ls >> 16,
                    ls & 0xFFFF
                )))
            }
            None => return Err(bad("no version resource".into())),
        }
        let out_of_range = |what: &str| bad(format!("{what} out of range"));
        let huffman_code_lengths: [u8; 256] = image
            .bytes(address::HUFFMAN_CODE_LENGTHS, 256)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| out_of_range("Huffman table"))?;
        let mut client_packet_sizes = [0i32; CLIENT_OPCODES];
        image.i32s(address::CLIENT_PACKET_SIZES, &mut client_packet_sizes).ok_or_else(|| out_of_range("client size table"))?;
        let mut server_packet_sizes = [0i32; SERVER_OPCODES];
        image.i32s(address::SERVER_PACKET_SIZES, &mut server_packet_sizes).ok_or_else(|| out_of_range("server size table"))?;
        let mut preset_objects = vec![0i32; 5 * PRESET_OBJECTS_PER_ACT];
        image.i32s(address::PRESET_OBJECTS, &mut preset_objects).ok_or_else(|| out_of_range("preset object table"))?;
        let mut clock_speeds = [0i32; 3];
        image.i32s(address::CLOCK_SPEEDS, &mut clock_speeds).ok_or_else(|| out_of_range("clock speeds"))?;
        let mut periods = [0i32; 18];
        image.i32s(address::DAY_PERIODS, &mut periods).ok_or_else(|| out_of_range("day periods"))?;
        let day_periods: [DayPeriod; 6] = std::array::from_fn(|i| DayPeriod { angle: periods[i * 3], phase: periods[i * 3 + 1] });
        let outdoor = OutdoorTables::read(&image).ok_or_else(|| out_of_range("outdoor tables"))?;
        let reserved_graphics: Vec<(crate::items::Code, i32)> = image
            .bytes(address::RESERVED_GRAPHICS, crate::appearance::SLOTS * 8)
            .ok_or_else(|| out_of_range("reserved graphics"))?
            .chunks_exact(8)
            .map(|e| ([e[0], e[1], e[2], e[3]], i32::from_le_bytes([e[4], e[5], e[6], e[7]])))
            .collect();
        let front_end = FrontEndTables::read(&image).ok_or_else(|| out_of_range("front-end tables"))?;
        let tiles = TileTables::read(&image).ok_or_else(|| out_of_range("room tile tables"))?;

        // GAMELOGON 37, ENTERGAME 1, ping 13; GameFlags 8, LoadAct 12, AssignPlayer 26.
        let sizes_ok = client_packet_sizes[0x68] == 37
            && client_packet_sizes[0x6B] == 1
            && client_packet_sizes[0x6D] == 13
            && server_packet_sizes[0x01] == 8
            && server_packet_sizes[0x03] == 12
            && server_packet_sizes[0x59] == 26;
        // Every preset slot is an object class, -1 for none; the first Act I slot is 0.
        let presets_ok = preset_objects.iter().all(|&c| (-1..1000).contains(&c));
        // Angles within a circle, phases 0..=3, a positive speed.
        let clock_ok = clock_speeds[0] > 0
            && day_periods.iter().all(|p| (0..360).contains(&p.angle) && (0..=3).contains(&p.phase));
        // Border pieces 4..=15 on an edge facing the first direction; the corner rows are 1..=12.
        let outdoor_ok = outdoor.road_presets[1] == [0, 4, 0x16C, 0x31F]
            && outdoor.road_directions.iter().all(|d| (-1..=3).contains(d))
            && outdoor.corners.iter().all(|c| (-1..=12).contains(c))
            && outdoor.road_flags.iter().all(|r| r[5] > 0 && r[5] & (r[5] - 1) == 0)
            && outdoor.path_directions.iter().all(|d| (0..8).contains(d))
            && outdoor.path_deltas[16..].iter().all(|d| (-1..=1).contains(d));
        // The body armour weights, then the first helm.
        let graphics_ok = reserved_graphics[1] == (*b"lit ", 1)
            && reserved_graphics[4] == (*b"cap ", 37)
            && front_end.graphics[4] == (*b"cap ", 0, 37)
            && front_end.classes.first() == Some(b"AM  ")
            && front_end.modes.get(5) == Some(b"TN  ")
            && front_end.weapon_classes.get(1) == Some(b"hth ")
            && front_end.file_directions[5][0] == 4
            && front_end.file_directions[4][..8] == [4, 5, 6, 7, 0, 2, 1, 3];
        // The Barracks' rows come first; the lit warp tiles fill a 2×2 block; a floor seam keeps
        // its type.
        let tiles_ok = tiles.preset_levels[0] == [28, 0, 3]
            && tiles.preset_levels.iter().all(|l| (0..=l[2]).contains(&l[1]) && l[2] < 34)
            && tiles.warp_tile_offsets == [(0, 0), (1, 0), (0, 1), (1, 1)]
            && tiles.mapping_by_type[0] == -1
            && tiles.mapping_transitions[42] == 7;
        if !sizes_ok || !presets_ok || !clock_ok || !outdoor_ok || !graphics_ok || !tiles_ok {
            return Err(bad("tables do not look like 1.14d's".into()));
        }
        Ok(Self {
            huffman_code_lengths,
            client_packet_sizes,
            server_packet_sizes,
            preset_objects,
            clock_speeds,
            day_periods,
            outdoor,
            reserved_graphics,
            front_end,
            tiles,
        })
    }

    /// The object class a DS1 preset object (unit type 2) becomes: ids below 150 go through the
    /// act's slots, larger ids are `id - 150` (`DRLGPRESET_GetObjectIdFromActTable`).
    #[must_use]
    pub fn preset_object_class(&self, act: u8, ds1_id: i32) -> Option<i32> {
        let per_act = PRESET_OBJECTS_PER_ACT as i32;
        if ds1_id < 0 {
            return None;
        }
        if ds1_id >= per_act {
            return Some(ds1_id - per_act);
        }
        let class = *self.preset_objects.get(usize::from(act.min(4)) * PRESET_OBJECTS_PER_ACT + ds1_id as usize)?;
        (class >= 0).then_some(class)
    }
}

impl FrontEndTables {
    fn read(image: &Image) -> Option<Self> {
        let u32_at = |at: u32| image.bytes(at, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let codes = |at: u32, n: u32| -> Option<Vec<crate::items::Code>> {
            let bytes = image.bytes(at, n as usize * 4)?;
            Some(bytes.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect())
        };
        let graphics = image
            .bytes(address::FRONT_END_GRAPHICS, crate::appearance::SLOTS * 12)?
            .chunks_exact(12)
            .map(|e| {
                let int = |o: usize| i32::from_le_bytes([e[o], e[o + 1], e[o + 2], e[o + 3]]);
                ([e[0], e[1], e[2], e[3]], int(4), int(8))
            })
            .collect();
        let limit = |n: u32| (n <= 64).then_some(n);
        let classes = codes(address::FRONT_END_CLASSES + 4, limit(u32_at(address::FRONT_END_CLASSES)?)?)?;
        let modes = codes(address::FRONT_END_MODES + 4, limit(u32_at(address::FRONT_END_MODES)?)?)?;
        let components = codes(address::FRONT_END_COMPONENTS, 16)?;
        let weapon_classes = codes(address::FRONT_END_WEAPON_CLASSES, limit(u32_at(address::FRONT_END_WEAPON_CLASSES + 15 * 4)?)?)?;
        let item_count = limit(u32_at(address::FRONT_END_ITEM_WEAPON_CLASSES + 13 * 8)?)?;
        let item_weapon_classes = image
            .bytes(address::FRONT_END_ITEM_WEAPON_CLASSES, item_count as usize * 8)?
            .chunks_exact(8)
            .map(|e| ([e[0], e[1], e[2], e[3]], u32::from_le_bytes([e[4], e[5], e[6], e[7]])))
            .collect();
        let hand_classes = image
            .bytes(address::FRONT_END_HAND_CLASSES, (item_count as usize + 1) * 4)?
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let mut file_directions = Vec::with_capacity(7);
        for row in 0..7u32 {
            let mut ints = [0i32; 32];
            image.i32s(address::FILE_DIRECTIONS + row * 32 * 4, &mut ints)?;
            file_directions.push(ints);
        }
        Some(Self { graphics, classes, modes, components, weapon_classes, item_weapon_classes, hand_classes, file_directions })
    }
}

impl TileTables {
    fn read(image: &Image) -> Option<Self> {
        let mut levels = [0i32; 37 * 3];
        image.i32s(address::PRESET_TILE_LEVELS, &mut levels)?;
        let mut rows = [0i32; 34 * 7];
        image.i32s(address::PRESET_TILE_ROWS, &mut rows)?;
        let mut offsets = [0i32; 8];
        image.i32s(address::WARP_TILE_OFFSETS, &mut offsets)?;
        let mut mapping_transitions = [0i32; 43];
        image.i32s(address::TILE_MAPPING_TRANSITIONS, &mut mapping_transitions)?;
        let mut mapping_by_type = [0i32; 20];
        image.i32s(address::TILE_MAPPING_BY_TYPE, &mut mapping_by_type)?;
        Some(Self {
            preset_levels: std::array::from_fn(|i| std::array::from_fn(|c| levels[i * 3 + c])),
            preset_rows: std::array::from_fn(|i| std::array::from_fn(|c| rows[i * 7 + c])),
            warp_tile_offsets: std::array::from_fn(|i| (offsets[i * 2], offsets[i * 2 + 1])),
            mapping_transitions,
            mapping_by_type,
        })
    }
}

impl OutdoorTables {
    fn read(image: &Image) -> Option<Self> {
        fn ints<const N: usize>(image: &Image, at: u32) -> Option<[i32; N]> {
            let mut out = [0i32; N];
            image.i32s(at, &mut out)?;
            Some(out)
        }
        let links: [i32; 8] = ints(image, address::OUTDOOR_LINK_OFFSETS)?;
        let presets: [i32; 52] = ints(image, address::OUTDOOR_ROAD_PRESETS)?;
        let flags: [i32; 90] = ints(image, address::OUTDOOR_ROAD_FLAGS)?;
        let vertical: [i32; 32] = ints(image, address::OUTDOOR_VERTICAL_BORDERS)?;
        let signed = |at: u32, n: usize| image.bytes(at, n).map(|b| b.iter().map(|&v| i32::from(v as i8)).collect::<Vec<i32>>());
        let (ny, nx) = (signed(address::OUTDOOR_NEIGHBOURS_Y, 8)?, signed(address::OUTDOOR_NEIGHBOURS_X, 8)?);
        let directions: [i32; 75] = ints(image, address::OUTDOOR_PATH_DIRECTIONS)?;
        let (sy, sx): ([i32; 4], [i32; 4]) = (ints(image, address::OUTDOOR_SPIRAL_Y)?, ints(image, address::OUTDOOR_SPIRAL_X)?);
        let (jx, jy): ([i32; 4], [i32; 4]) = (ints(image, address::OUTDOOR_JITTER_X)?, ints(image, address::OUTDOOR_JITTER_Y)?);
        let deltas = signed(address::OUTDOOR_PATH_DELTAS, 24)?;
        Some(Self {
            link_offsets: std::array::from_fn(|i| (links[i * 2], links[i * 2 + 1])),
            road_presets: std::array::from_fn(|r| std::array::from_fn(|c| presets[r * 4 + c])),
            road_directions: ints(image, address::OUTDOOR_ROAD_DIRECTIONS)?,
            corners: ints(image, address::OUTDOOR_CORNERS)?,
            road_flags: std::array::from_fn(|r| std::array::from_fn(|c| flags[r * 6 + c])),
            vertical_borders: std::array::from_fn(|i| [vertical[i * 2], vertical[i * 2 + 1]]),
            neighbours: std::array::from_fn(|i| (nx[i], ny[i])),
            shrine_styles: ints(image, address::OUTDOOR_SHRINE_STYLES)?,
            path_directions: std::array::from_fn(|i| directions[i * 3]),
            spiral: std::array::from_fn(|i| (sx[i], sy[i])),
            jitter: std::array::from_fn(|i| (jx[i], jy[i])),
            path_deltas: std::array::from_fn(|i| deltas[i]),
            edge_orientations: image.bytes(address::OUTDOOR_EDGE_ORIENTATIONS, 256)?.try_into().ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the operator's `Game.exe` (`BNETCC_D2_GAME_EXE`): the tables load, and — given a
    /// libd2 checkout (`LIBD2_DIR`) — the preset object table equals libd2's extraction of the
    /// same address, which pins the address rather than just the shape.
    #[test]
    fn with_a_real_game_exe_the_tables_load() {
        let Ok(path) = std::env::var("BNETCC_D2_GAME_EXE") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(path).unwrap()).expect("1.14d tables");
        assert_eq!(engine.huffman_code_lengths[0], 1, "byte 0 costs one bit");
        assert_eq!(engine.day_periods[2], DayPeriod { angle: 0, phase: 0 }, "a new act's period: day, at angle 0");
        assert_eq!(engine.outdoor.road_flags[0], [0, 2, 3, 1, 0, 4], "a river flag, not for Blood Moor or Cold Plains");
        assert_eq!(engine.outdoor.corners[40], -1, "no corner without a turn");
        assert_eq!(engine.outdoor.shrine_styles, [0x1000, 0x2000, 0x4000, 0x8000]);
        assert_eq!(engine.outdoor.neighbours[0], (-1, 0));
        assert_eq!(&engine.outdoor.path_deltas[16..], &[0, 1, 0, -1, 1, 0, -1, 0], "y then x steps by direction");
        if let Ok(libd2) = std::env::var("LIBD2_DIR") {
            let bin = std::fs::read(std::path::Path::new(&libd2).join("packages/drlg/src/excel/PresetObjectTable.bin")).unwrap();
            let theirs: Vec<i32> = bin.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            assert_eq!(engine.preset_objects, theirs);
        }
    }
}
