//! DT1 tile libraries: each tile's identity, its 5×5 block of subtile collision flags and, for a
//! client, its picture.
//!
//! A map cell names a tile by orientation, main index and sub index; several tiles can share
//! that identity and the engine picks one by rarity. [`Dt1::parse`] reads what the servers need
//! (identity, rarity, collision); [`Dt1::drawings`] reads the pictures too.
//!
//! Layout (version 7.6): `0x10C` tile count, `0x110` offset of the tile headers, 96 bytes each:
//!
//! ```text
//! +0x00 i32 direction        +0x04 u16 roof height   +0x06 u8 sound   +0x07 u8 animated
//! +0x08 i32 height (walls negative)                  +0x0C i32 width
//! +0x14 i32 orientation      +0x18 i32 main index    +0x1C i32 sub index
//! +0x20 i32 rarity (an animated tile's frame)        +0x28 u8[25] subtile flags, top row first
//! +0x48 i32 offset of its block headers              +0x4C i32 length of its block data
//! +0x50 i32 block count
//! ```
//!
//! A block header is 20 bytes: `+0x00 i16 x`, `+0x02 i16 y` (pixels from the tile's origin),
//! `+0x06 u8 grid x`, `+0x07 u8 grid y`, `+0x08 u16 format`, `+0x0A i32 length`, `+0x10 i32`
//! offset of its data from the tile's block headers. Format 1 is an isometric block: a 32 × 15
//! diamond, 256 bytes, its rows `4, 8, … 32, … 8, 4` pixels wide and centred. Any other format is
//! run-length coded in a 32 × 32 box: pairs of bytes `(skip, count)`, each followed by `count`
//! pixels; `(0, 0)` ends a row.
//!
//! Written from the published DT1 format notes (Paul Siramy's "DT1 format", with the isometric
//! and RLE block decoders of his viewer), as libd2 `packages/formats/src/dt1.zig` (MIT) reads the
//! headers.

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

/// Size of a block header.
const BLOCK_HEADER: usize = 20;

/// Pixels per row of an isometric block, and where each row starts.
const ISO_WIDTHS: [usize; 15] = [4, 8, 12, 16, 20, 24, 28, 32, 28, 24, 20, 16, 12, 8, 4];
/// Bytes in an isometric block.
const ISO_BYTES: usize = 256;

/// One 32-pixel-wide piece of a tile's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Left edge, pixels right of the tile's origin.
    pub x: i32,
    /// Top edge, pixels below the tile's origin (above it for walls: negative).
    pub y: i32,
    /// Column in the tile's grid of blocks.
    pub grid_x: u8,
    /// Row in the tile's grid of blocks.
    pub grid_y: u8,
    /// 1 isometric, anything else run-length coded.
    pub format: u16,
    /// The coded pixels.
    pub data: Vec<u8>,
}

impl Block {
    /// Whether this is an isometric (floor-shaped) block.
    #[must_use]
    pub fn isometric(&self) -> bool {
        self.format == 1
    }

    /// Height of the box the block paints: 15 for an isometric block, 32 otherwise.
    #[must_use]
    pub fn height(&self) -> i32 {
        if self.isometric() {
            15
        } else {
            32
        }
    }

    /// Paint the block into `out`, a buffer `stride` pixels wide, with its top-left corner at
    /// (`left`, `top`). Pixels landing outside the buffer are dropped. Only the pixels the block
    /// codes are written; an RLE block's skipped pixels are left as they were.
    pub fn paint(&self, out: &mut [u8], stride: usize, left: i32, top: i32) {
        let rows = if stride == 0 { 0 } else { out.len() / stride };
        let mut put = |x: i32, y: i32, p: u8| {
            if x >= 0 && y >= 0 && (x as usize) < stride && (y as usize) < rows {
                out[y as usize * stride + x as usize] = p;
            }
        };
        if self.isometric() {
            let mut at = 0;
            for (row, &width) in ISO_WIDTHS.iter().enumerate() {
                let start = (32 - width) / 2;
                for col in 0..width {
                    let Some(&p) = self.data.get(at) else { return };
                    put(left + (start + col) as i32, top + row as i32, p);
                    at += 1;
                }
            }
            return;
        }
        let (mut x, mut y, mut at) = (0i32, 0i32, 0usize);
        while at + 1 < self.data.len() {
            let (skip, count) = (self.data[at], self.data[at + 1]);
            at += 2;
            if skip == 0 && count == 0 {
                x = 0;
                y += 1;
                continue;
            }
            x += i32::from(skip);
            for _ in 0..count {
                let Some(&p) = self.data.get(at) else { return };
                put(left + x, top + y, p);
                x += 1;
                at += 1;
            }
        }
    }
}

/// A tile with its picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawing {
    /// Identity, rarity and collision, as [`Dt1::parse`] reads them.
    pub tile: Tile,
    /// Direction (the editor's; the engine draws by orientation).
    pub direction: i32,
    /// How far above its floor a roof is drawn, in pixels.
    pub roof_height: u16,
    /// Footstep sound index.
    pub sound: u8,
    /// Animated: tiles sharing the identity are its frames, by [`Tile::rarity`].
    pub animated: bool,
    /// Height in pixels; negative for walls, which rise above their origin.
    pub height: i32,
    /// Width in pixels (160).
    pub width: i32,
    /// Its blocks.
    pub blocks: Vec<Block>,
}

/// A tile's picture decoded: palette indices in the box its blocks cover, 0 transparent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Left edge, pixels right of the tile's origin.
    pub x: i32,
    /// Top edge, pixels below the tile's origin.
    pub y: i32,
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// `width × height` indices, row by row.
    pub pixels: Vec<u8>,
}

impl Drawing {
    /// Decode every block into one picture covering them all; `None` for a tile without blocks.
    #[must_use]
    pub fn picture(&self) -> Option<Picture> {
        let x0 = self.blocks.iter().map(|b| b.x).min()?;
        let y0 = self.blocks.iter().map(|b| b.y).min()?;
        let x1 = self.blocks.iter().map(|b| b.x + 32).max()?;
        let y1 = self.blocks.iter().map(|b| b.y + b.height()).max()?;
        let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut pixels = vec![0u8; width * height];
        for b in &self.blocks {
            b.paint(&mut pixels, width, b.x - x0, b.y - y0);
        }
        Some(Picture { x: x0, y: y0, width, height, pixels })
    }
}

fn i16_at(bytes: &[u8], at: usize) -> Result<i32, Error> {
    let b = bytes.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(i32::from(i16::from_le_bytes([b[0], b[1]])))
}

impl Dt1 {
    /// Parse a DT1 file with every tile's picture.
    ///
    /// # Errors
    ///
    /// [`Error`] for a truncated or corrupt file.
    pub fn drawings(bytes: &[u8]) -> Result<Vec<Drawing>, Error> {
        let headers = Self::parse(bytes)?;
        let base = usize::try_from(i32_at(bytes, 0x110)?).map_err(|_| Error::Corrupt)?;
        headers
            .tiles
            .into_iter()
            .enumerate()
            .map(|(i, tile)| {
                let at = base + i * TILE_HEADER;
                let blocks_at = usize::try_from(i32_at(bytes, at + 0x48)?).map_err(|_| Error::Corrupt)?;
                let count = usize::try_from(i32_at(bytes, at + 0x50)?).map_err(|_| Error::Corrupt)?;
                if count > 4096 {
                    return Err(Error::Corrupt);
                }
                let blocks = (0..count)
                    .map(|b| {
                        let h = blocks_at + b * BLOCK_HEADER;
                        let header = bytes.get(h..h + BLOCK_HEADER).ok_or(Error::Truncated)?;
                        let length = usize::try_from(i32_at(header, 0x0A)?).map_err(|_| Error::Corrupt)?;
                        let offset = usize::try_from(i32_at(header, 0x10)?).map_err(|_| Error::Corrupt)?;
                        let start = blocks_at.checked_add(offset).ok_or(Error::Corrupt)?;
                        let data = bytes.get(start..start.checked_add(length).ok_or(Error::Corrupt)?).ok_or(Error::Truncated)?;
                        let format = u16::from_le_bytes([header[8], header[9]]);
                        if format == 1 && length < ISO_BYTES {
                            return Err(Error::Corrupt);
                        }
                        Ok(Block { x: i16_at(header, 0)?, y: i16_at(header, 2)?, grid_x: header[6], grid_y: header[7], format, data: data.to_vec() })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Ok(Drawing {
                    tile,
                    direction: i32_at(bytes, at)?,
                    roof_height: u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]),
                    sound: bytes[at + 6],
                    animated: bytes[at + 7] != 0,
                    height: i32_at(bytes, at + 8)?,
                    width: i32_at(bytes, at + 0x0C)?,
                    blocks,
                })
            })
            .collect()
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

    /// One tile with the given blocks: `(x, y, format, data)`.
    fn file_with_blocks(blocks: &[(i16, i16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = file(&[(1, 2, 3, 1, 0)]);
        let tile_at = 0x114;
        let headers_at = out.len();
        out[tile_at + 4..tile_at + 6].copy_from_slice(&80u16.to_le_bytes());
        out[tile_at + 7] = 1;
        out[tile_at + 8..tile_at + 12].copy_from_slice(&(-96i32).to_le_bytes());
        out[tile_at + 0x0C..tile_at + 0x10].copy_from_slice(&160i32.to_le_bytes());
        out[tile_at + 0x48..tile_at + 0x4C].copy_from_slice(&(headers_at as i32).to_le_bytes());
        out[tile_at + 0x50..tile_at + 0x54].copy_from_slice(&(blocks.len() as i32).to_le_bytes());
        let mut data_at = blocks.len() * BLOCK_HEADER;
        let mut data = Vec::new();
        for (i, (x, y, format, bytes)) in blocks.iter().enumerate() {
            let mut h = [0u8; BLOCK_HEADER];
            h[0..2].copy_from_slice(&x.to_le_bytes());
            h[2..4].copy_from_slice(&y.to_le_bytes());
            h[6] = i as u8;
            h[8..10].copy_from_slice(&format.to_le_bytes());
            h[0x0A..0x0E].copy_from_slice(&(bytes.len() as i32).to_le_bytes());
            h[0x10..0x14].copy_from_slice(&(data_at as i32).to_le_bytes());
            data_at += bytes.len();
            data.extend_from_slice(bytes);
            out.extend_from_slice(&h);
        }
        out.extend(data);
        out
    }

    #[test]
    fn an_isometric_block_fills_a_centred_diamond() {
        let data: Vec<u8> = (0..ISO_BYTES).map(|i| (i % 250 + 1) as u8).collect();
        let d = Dt1::drawings(&file_with_blocks(&[(32, 16, 1, data)])).unwrap();
        assert_eq!(d.len(), 1);
        let t = &d[0];
        assert_eq!((t.roof_height, t.animated, t.height, t.width), (80, true, -96, 160));
        assert_eq!((t.tile.orientation, t.tile.main, t.tile.sub), (1, 2, 3));
        let p = t.picture().unwrap();
        assert_eq!((p.x, p.y, p.width, p.height), (32, 16, 32, 15));
        // Row 0: 4 pixels from column 14; row 7: the full 32; row 14: 4 again.
        let row = |y: usize| &p.pixels[y * 32..(y + 1) * 32];
        assert_eq!(row(0)[13..19], [0, 1, 2, 3, 4, 0]);
        assert!(row(7).iter().all(|&c| c != 0));
        assert_eq!(row(14).iter().filter(|&&c| c != 0).count(), 4);
        assert_eq!(row(14)[14], ((ISO_BYTES - 4) % 250 + 1) as u8);
    }

    #[test]
    fn an_rle_block_skips_runs_and_ends_rows() {
        // Row 0: skip 2, draw 3; skip 1, draw 1. Row 1: draw 2. Row 2 untouched.
        let data = vec![2, 3, 7, 8, 9, 1, 1, 5, 0, 0, 0, 2, 6, 6, 0, 0];
        let d = Dt1::drawings(&file_with_blocks(&[(0, -64, 0, data), (32, -32, 0, vec![0, 1, 4])])).unwrap();
        let p = d[0].picture().unwrap();
        assert_eq!((p.x, p.y, p.width, p.height), (0, -64, 64, 64));
        assert_eq!(p.pixels[..8], [0, 0, 7, 8, 9, 0, 5, 0]);
        assert_eq!(p.pixels[64..67], [6, 6, 0]);
        assert!(p.pixels[128..192].iter().all(|&c| c == 0));
        // The second block starts 32 right and 32 down: one pixel at its row 0, column 0.
        assert_eq!(p.pixels[32 * 64 + 32], 4);
    }

    #[test]
    fn a_block_past_the_end_is_refused() {
        let mut bytes = file_with_blocks(&[(0, 0, 0, vec![0, 1, 4])]);
        bytes.truncate(bytes.len() - 1);
        assert_eq!(Dt1::drawings(&bytes), Err(Error::Truncated));
        assert!(Dt1::parse(&bytes).is_ok(), "the headers alone still read");
    }
}
