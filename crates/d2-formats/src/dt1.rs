//! DT1 tile libraries: each tile's identity and its 5×5 block of subtile collision flags.
//!
//! A map cell names a tile by orientation, main index and sub index; several tiles can share
//! that identity and the engine picks one by rarity. The pixel blocks after the headers are not
//! read.
//!
//! Layout (version 7.6): `0x10C` tile count, `0x110` offset of the tile headers, 96 bytes each —
//! `+0x14` orientation, `+0x18` main index, `+0x1C` sub index, `+0x20` rarity, `+0x28` the 25
//! subtile flag bytes, row by row from the top. Written from the published DT1 format notes, as
//! libd2 `packages/formats/src/dt1.zig` (MIT) reads it.

use std::fmt;

/// Why a DT1 could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file ended early.
    Truncated,
    /// A count or offset is impossible.
    Corrupt,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "DT1: truncated",
            Self::Corrupt => "DT1: corrupt",
        })
    }
}

impl std::error::Error for Error {}

/// Size of a tile header.
const TILE_HEADER: usize = 96;

/// One tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// Orientation: 0 floor, 1–12 walls, 13 shadow, 15 roof, …
    pub orientation: i32,
    /// Main index.
    pub main: i32,
    /// Sub index.
    pub sub: i32,
    /// Weight among tiles sharing the identity.
    pub rarity: i32,
    /// Subtile flags, `row * 5 + column`, row 0 at the top.
    pub flags: [u8; 25],
}

impl Tile {
    /// The flags of subtile (`x`, `y`), 0 outside the tile.
    #[must_use]
    pub fn subtile(&self, x: usize, y: usize) -> u8 {
        if x < 5 && y < 5 {
            self.flags[y * 5 + x]
        } else {
            0
        }
    }
}

/// A parsed DT1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dt1 {
    /// Its tiles, in file order.
    pub tiles: Vec<Tile>,
}

fn i32_at(bytes: &[u8], at: usize) -> Result<i32, Error> {
    let b = bytes.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl Dt1 {
    /// Parse a DT1 file.
    ///
    /// # Errors
    ///
    /// [`Error`] for a truncated or corrupt file.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let count = usize::try_from(i32_at(bytes, 0x10C)?).map_err(|_| Error::Corrupt)?;
        let base = usize::try_from(i32_at(bytes, 0x110)?).map_err(|_| Error::Corrupt)?;
        let end = count.checked_mul(TILE_HEADER).and_then(|n| n.checked_add(base)).ok_or(Error::Corrupt)?;
        if end > bytes.len() {
            return Err(Error::Truncated);
        }
        let tiles = (0..count)
            .map(|i| {
                let at = base + i * TILE_HEADER;
                let mut flags = [0u8; 25];
                flags.copy_from_slice(&bytes[at + 0x28..at + 0x28 + 25]);
                Ok(Tile {
                    orientation: i32_at(bytes, at + 0x14)?,
                    main: i32_at(bytes, at + 0x18)?,
                    sub: i32_at(bytes, at + 0x1C)?,
                    rarity: i32_at(bytes, at + 0x20)?,
                    flags,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { tiles })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(tiles: &[(i32, i32, i32, i32, u8)]) -> Vec<u8> {
        let mut out = vec![0u8; 0x114];
        out[0..4].copy_from_slice(&7i32.to_le_bytes());
        out[4..8].copy_from_slice(&6i32.to_le_bytes());
        out[0x10C..0x110].copy_from_slice(&(tiles.len() as i32).to_le_bytes());
        out[0x110..0x114].copy_from_slice(&0x114i32.to_le_bytes());
        for &(orientation, main, sub, rarity, flag) in tiles {
            let mut h = [0u8; TILE_HEADER];
            h[0x14..0x18].copy_from_slice(&orientation.to_le_bytes());
            h[0x18..0x1C].copy_from_slice(&main.to_le_bytes());
            h[0x1C..0x20].copy_from_slice(&sub.to_le_bytes());
            h[0x20..0x24].copy_from_slice(&rarity.to_le_bytes());
            h[0x28] = flag;
            h[0x28 + 24] = flag << 1;
            out.extend_from_slice(&h);
        }
        out
    }

    #[test]
    fn headers_give_identity_rarity_and_flags() {
        let d = Dt1::parse(&file(&[(0, 5, 2, 1, 0x01), (3, 0, 7, 4, 0x04)])).unwrap();
        assert_eq!(d.tiles.len(), 2);
        let t = d.tiles[1];
        assert_eq!((t.orientation, t.main, t.sub, t.rarity), (3, 0, 7, 4));
        assert_eq!((t.subtile(0, 0), t.subtile(4, 4), t.subtile(2, 2), t.subtile(5, 0)), (0x04, 0x08, 0, 0));
    }

    #[test]
    fn a_short_file_is_refused() {
        let mut bytes = file(&[(0, 0, 0, 1, 0)]);
        bytes.truncate(bytes.len() - 1);
        assert_eq!(Dt1::parse(&bytes), Err(Error::Truncated));
        assert_eq!(Dt1::parse(&[0u8; 8]), Err(Error::Truncated));
    }
}
