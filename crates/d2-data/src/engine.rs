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

        // GAMELOGON 37, ENTERGAME 1, ping 13; GameFlags 8, LoadAct 12, AssignPlayer 26.
        let sizes_ok = client_packet_sizes[0x68] == 37
            && client_packet_sizes[0x6B] == 1
            && client_packet_sizes[0x6D] == 13
            && server_packet_sizes[0x01] == 8
            && server_packet_sizes[0x03] == 12
            && server_packet_sizes[0x59] == 26;
        // Every preset slot is an object class, -1 for none; the first Act I slot is 0.
        let presets_ok = preset_objects.iter().all(|&c| (-1..1000).contains(&c));
        if !sizes_ok || !presets_ok {
            return Err(bad("tables do not look like 1.14d's".into()));
        }
        Ok(Self { huffman_code_lengths, client_packet_sizes, server_packet_sizes, preset_objects })
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
        if let Ok(libd2) = std::env::var("LIBD2_DIR") {
            let bin = std::fs::read(std::path::Path::new(&libd2).join("packages/drlg/src/excel/PresetObjectTable.bin")).unwrap();
            let theirs: Vec<i32> = bin.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            assert_eq!(engine.preset_objects, theirs);
        }
    }
}
