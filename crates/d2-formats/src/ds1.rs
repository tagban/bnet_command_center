//! DS1 map files: one preset area's tile layers, its preset units, and NPC walk paths.
//!
//! The generator needs the size and the units: which monsters (NPCs) and objects the map
//! places, and where, in subtiles relative to the map's corner. The tile layers are skipped
//! for now; they come back with collision.
//!
//! Ported from libd2 `packages/formats/src/ds1.zig` (MIT), which follows the engine's DS1
//! reader.

use std::fmt;

/// Why a DS1 could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file ended early.
    Truncated,
    /// A count or size is impossible.
    Corrupt,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "DS1: truncated",
            Self::Corrupt => "DS1: corrupt",
        })
    }
}

impl std::error::Error for Error {}

/// What a preset unit is (its DS1 `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// A monster or NPC (type 1): its id indexes the act's `MonPreset.txt` rows.
    Monster,
    /// An object (type 2): its id goes through the engine's preset object table.
    Object,
    /// Anything else, kept raw.
    Other(i32),
}

/// A preset unit placed by the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Its kind.
    pub kind: UnitKind,
    /// Its DS1 id.
    pub id: i32,
    /// Position in subtiles from the map's top-left corner.
    pub x: i32,
    /// Position in subtiles from the map's top-left corner.
    pub y: i32,
    /// Flags (version 6 and later).
    pub flags: i32,
    /// Walk path for an NPC, in subtiles, with each node's action.
    pub path: Vec<(i32, i32, i32)>,
}

/// A parsed DS1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ds1 {
    /// File version.
    pub version: i32,
    /// Width in tiles.
    pub width: i32,
    /// Height in tiles.
    pub height: i32,
    /// Act the map belongs to (0-based), version 8 and later.
    pub act: i32,
    /// Preset units, in file order.
    pub units: Vec<Unit>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn i32(&mut self) -> Result<i32, Error> {
        let b = self.bytes.get(self.at..self.at + 4).ok_or(Error::Truncated)?;
        self.at += 4;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn skip(&mut self, n: usize) -> Result<(), Error> {
        if self.at + n > self.bytes.len() {
            return Err(Error::Truncated);
        }
        self.at += n;
        Ok(())
    }

    fn skip_cstr(&mut self) -> Result<(), Error> {
        let len = self.bytes.get(self.at..).and_then(|b| b.iter().position(|&c| c == 0)).ok_or(Error::Truncated)?;
        self.at += len + 1;
        Ok(())
    }

    fn count(&mut self, limit: i32) -> Result<usize, Error> {
        match self.i32()? {
            n if (0..=limit).contains(&n) => Ok(n as usize),
            _ => Err(Error::Corrupt),
        }
    }
}

impl Ds1 {
    /// Parse a DS1 file.
    ///
    /// # Errors
    ///
    /// [`Error`] for a truncated or corrupt file.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader { bytes, at: 0 };
        let version = r.i32()?;
        let (raw_w, raw_h) = (r.i32()?, r.i32()?);
        if !(0..=4096).contains(&raw_w) || !(0..=4096).contains(&raw_h) {
            return Err(Error::Corrupt);
        }
        let act = if version > 7 { r.i32()?.min(4) } else { 0 };
        let tag_type = if version > 9 { r.i32()? } else { 0 };
        if version > 2 {
            for _ in 0..r.count(1024)? {
                r.skip_cstr()?;
            }
        }
        let (width, height) = (raw_w + 1, raw_h + 1);
        let block = width as usize * height as usize * 4;
        if (9..=13).contains(&version) {
            r.skip(8)?;
        }
        if version < 4 {
            r.skip(block * 4)?; // wall, orientation, floor, reserved
        } else {
            let walls = r.count(64)?;
            let floors = if version >= 16 { r.count(64)? } else { 1 };
            r.skip(block * (walls * 2 + floors))?;
        }
        r.skip(block)?; // shadow
        if (1..=2).contains(&tag_type) {
            r.skip(block)?; // substitution tags
        }

        let mut units = Vec::new();
        if version > 1 {
            for _ in 0..r.count(1 << 16)? {
                let kind = match r.i32()? {
                    1 => UnitKind::Monster,
                    2 => UnitKind::Object,
                    other => UnitKind::Other(other),
                };
                let (id, x, y) = (r.i32()?, r.i32()?, r.i32()?);
                let flags = if version > 5 { r.i32()? } else { 0 };
                units.push(Unit { kind, id, x, y, flags, path: Vec::new() });
            }
        }

        if version > 11 && (1..=2).contains(&tag_type) {
            if version > 17 {
                r.skip(4)?;
            }
            let groups = r.count(1 << 16)?;
            r.skip(groups * if version > 12 { 20 } else { 16 })?;
        }

        if version > 13 {
            for _ in 0..r.count(1 << 16)? {
                let count = r.i32()?;
                let (x, y) = (r.i32()?, r.i32()?);
                let count = usize::try_from(count).unwrap_or(0);
                let mut path = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    let (px, py) = (r.i32()?, r.i32()?);
                    let action = if version > 14 { r.i32()? } else { 1 };
                    path.push((px, py, action));
                }
                // The engine hangs a path off the preset unit at the same position, searching
                // from the end.
                if let Some(unit) = units.iter_mut().rev().find(|u| u.x == x && u.y == y) {
                    unit.path = path;
                }
            }
        }
        Ok(Self { version, width, height, act, units })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn units_and_their_paths_come_through_the_layers() {
        // version 18, 2x1 tiles (raw 1, 0), act 0, tag type 0, no files; 1 wall + 1 floor.
        let cells = 2 * 4;
        let mut bytes = le(&[18, 1, 0, 0, 0, 0, 1, 1]);
        bytes.extend(vec![0u8; cells * 3]); // wall, orientation, floor
        bytes.extend(vec![0u8; cells]); // shadow
        bytes.extend(le(&[2, 1, 7, 10, 12, 0, 2, 119, 20, 22, 0])); // two units
        bytes.extend(le(&[1, 2, 10, 12, 11, 13, 1, 14, 16, 2])); // one path of two nodes
        let ds1 = Ds1::parse(&bytes).unwrap();
        assert_eq!((ds1.width, ds1.height), (2, 1));
        assert_eq!(ds1.units.len(), 2);
        assert_eq!((ds1.units[1].kind, ds1.units[1].id, ds1.units[1].x), (UnitKind::Object, 119, 20));
        assert_eq!(ds1.units[0].path, vec![(11, 13, 1), (14, 16, 2)], "path attached by position");
        assert_eq!(Ds1::parse(&bytes[..bytes.len() - 3]), Err(Error::Truncated));
    }
}
