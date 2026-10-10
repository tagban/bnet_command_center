//! DC6 sprites: the menus, fonts, inventory pictures and loading screens.
//!
//! Each frame is run-length coded palette indices, index 0 left transparent. The decoder hands
//! back each frame as a plain `width × height` index buffer, top row first.
//!
//! ```text
//! 0x00 i32 version (6)      0x04 u32 flags      0x08 u32 encoding      0x0C u8[4] termination
//! 0x10 u32 directions       0x14 u32 frames per direction
//! 0x18 u32 frame offset[directions × frames]
//! frame: u32 flipped, u32 width, u32 height, i32 offset x, i32 offset y, u32 unknown,
//!        u32 next block, u32 length, u8 data[length], u8 terminator[3]
//! ```
//!
//! In the data a byte `0x80` ends a row, a byte with the high bit set skips `b & 0x7F`
//! transparent pixels, and any other byte copies that many index bytes that follow. Rows run
//! bottom to top unless the frame is flipped. Written from the published DC6 format notes.

use std::fmt;

/// Why a DC6 could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file ended early.
    Truncated,
    /// A count, size or offset is impossible.
    Corrupt,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "DC6: truncated",
            Self::Corrupt => "DC6: corrupt",
        })
    }
}

impl std::error::Error for Error {}

/// Largest side a frame may claim; the game's biggest are 256-pixel background tiles.
const MAX_SIDE: u32 = 4096;

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Horizontal offset of the frame's left edge from the sprite's anchor.
    pub offset_x: i32,
    /// Vertical offset of the frame's *bottom* edge from the sprite's anchor.
    pub offset_y: i32,
    /// Palette indices, `width × height`, top row first; 0 is transparent.
    pub pixels: Vec<u8>,
}

/// A whole sprite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dc6 {
    /// Number of directions.
    pub directions: u32,
    /// Frames in each direction.
    pub frames_per_direction: u32,
    /// Every frame, direction by direction.
    pub frames: Vec<Frame>,
}

impl Dc6 {
    /// Decode a sprite.
    ///
    /// # Errors
    /// [`Error`] if the file is short or its sizes do not fit.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let u32_at = |at: usize| -> Result<u32, Error> {
            Ok(u32::from_le_bytes(bytes.get(at..at + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
        };
        if u32_at(0)? != 6 {
            return Err(Error::Corrupt);
        }
        let directions = u32_at(0x10)?;
        let frames_per_direction = u32_at(0x14)?;
        let count = directions.checked_mul(frames_per_direction).filter(|&n| n <= 0x1_0000).ok_or(Error::Corrupt)?;
        let mut frames = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let at = u32_at(0x18 + i * 4)? as usize;
            frames.push(frame(bytes, at)?);
        }
        Ok(Self { directions, frames_per_direction, frames })
    }

    /// The frame `index` of direction `direction`, if both exist.
    #[must_use]
    pub fn frame(&self, direction: u32, index: u32) -> Option<&Frame> {
        if direction >= self.directions || index >= self.frames_per_direction {
            return None;
        }
        self.frames.get((direction * self.frames_per_direction + index) as usize)
    }
}

fn frame(bytes: &[u8], at: usize) -> Result<Frame, Error> {
    let head = bytes.get(at..at + 32).ok_or(Error::Truncated)?;
    let word = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    let (flipped, width, height) = (word(0) != 0, word(4), word(8));
    let (offset_x, offset_y) = (word(12) as i32, word(16) as i32);
    let length = word(28) as usize;
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Corrupt);
    }
    let data = bytes.get(at + 32..at + 32 + length).ok_or(Error::Truncated)?;
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0; w * h];
    if w > 0 && h > 0 {
        // Rows are counted from the first one the data writes.
        let mut row = 0usize;
        let mut x = 0usize;
        let mut i = 0usize;
        while i < data.len() && row < h {
            let b = data[i];
            i += 1;
            if b == 0x80 {
                row += 1;
                x = 0;
            } else if b & 0x80 != 0 {
                x += usize::from(b & 0x7F);
            } else {
                let n = usize::from(b);
                let run = data.get(i..i + n).ok_or(Error::Truncated)?;
                i += n;
                let y = if flipped { row } else { h - 1 - row };
                for (k, &p) in run.iter().enumerate() {
                    if x + k < w {
                        pixels[y * w + x + k] = p;
                    }
                }
                x += n;
            }
        }
    }
    Ok(Frame { width, height, offset_x, offset_y, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(flipped: u32, width: u32, height: u32, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [6u32, 1, 0, 0xEEEE_EEEE, 1, 1, 0x1C] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in [flipped, width, height, 0, 0, 0, 0, data.len() as u32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(data);
        b.extend_from_slice(&[0xEE; 3]);
        b
    }

    #[test]
    fn rows_run_bottom_up_unless_flipped() {
        // Row one: skip 1, copy [7]; row two: copy [1, 2].
        let data = [0x81, 1, 7, 0x80, 2, 1, 2, 0x80];
        let up = Dc6::parse(&file(0, 2, 2, &data)).unwrap();
        assert_eq!(up.frames[0].pixels, [1, 2, 0, 7]);
        let down = Dc6::parse(&file(1, 2, 2, &data)).unwrap();
        assert_eq!(down.frames[0].pixels, [0, 7, 1, 2]);
    }

    #[test]
    fn short_and_wrong_files_are_refused() {
        assert_eq!(Dc6::parse(&[6, 0, 0]), Err(Error::Truncated));
        let mut b = file(0, 1, 1, &[1, 5, 0x80]);
        b[0] = 5;
        assert_eq!(Dc6::parse(&b), Err(Error::Corrupt));
        let b = file(0, 1, 1, &[3, 5]);
        assert_eq!(Dc6::parse(&b), Err(Error::Truncated));
    }
}
