//! COF files: how a unit's body parts are layered for one stance and weapon stance —
//! `data\global\chars\<class>\COF\<class><mode><weapon class>.cof`.
//!
//! ```text
//! 0x00 u8  layers        0x01 u8 frames per direction    0x02 u8 directions
//! 0x03 u8  version (20)  0x04 u32 flags
//! 0x08 i32 left, right, top, bottom — the animation's bounds around the base point
//! 0x18 u32 speed         frames advance speed/256 per drawn frame
//! 0x1c layers × 9 bytes: component, shadow, selectable, transparent, draw effect,
//!                        weapon class (4 bytes, NUL-padded)
//!      frames bytes:     per-frame events
//!      directions × frames × layers bytes: the components to draw, back to front
//! ```
//!
//! From the files and the 1.14d `Game.exe` front-end drawing (`0x005032B0` indexes the draw
//! order as `0x1c + 9 × layers + frames + (direction × frames + frame) × layers`; `0x00503740`
//! takes a part's weapon class from its layer).

use std::fmt;

/// Why a COF could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file ended early.
    Truncated,
    /// No layers, frames or directions.
    Empty,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "COF: truncated",
            Self::Empty => "COF: no layers, frames or directions",
        })
    }
}

impl std::error::Error for Error {}

const HEADER: usize = 0x1c;
const LAYER: usize = 9;

/// One layer: a body part and how it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// The body component, 0..16 (`HD TR LG RA LA RH LH SH S1..S8`).
    pub component: u8,
    /// Casts a shadow.
    pub shadow: bool,
    /// Can be clicked.
    pub selectable: bool,
    /// Drawn translucent.
    pub transparent: bool,
    /// Blend mode when translucent.
    pub draw_effect: u8,
    /// The weapon class its graphics are filed under (`hth`, `1hs`, …), lower case.
    pub weapon_class: String,
}

/// A parsed COF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cof {
    /// Frames in each direction.
    pub frames: usize,
    /// Directions.
    pub directions: usize,
    /// Bounds around the base point: left, right, top, bottom.
    pub bounds: [i32; 4],
    /// Frames advanced per drawn frame, in 256ths.
    pub speed: u32,
    /// The layers, in file order.
    pub layers: Vec<Layer>,
    priority: Vec<u8>,
}

impl Cof {
    /// Parse a COF.
    ///
    /// # Errors
    ///
    /// [`Error`] if it is truncated or empty.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER {
            return Err(Error::Truncated);
        }
        let (layer_count, frames, directions) = (usize::from(bytes[0]), usize::from(bytes[1]), usize::from(bytes[2]));
        if layer_count == 0 || frames == 0 || directions == 0 {
            return Err(Error::Empty);
        }
        let i32_at = |at: usize| i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let bounds = [i32_at(0x08), i32_at(0x0c), i32_at(0x10), i32_at(0x14)];
        let speed = i32_at(0x18) as u32;
        let layers_end = HEADER + layer_count * LAYER;
        let priority_start = layers_end + frames;
        let priority_end = priority_start + directions * frames * layer_count;
        if bytes.len() < priority_end {
            return Err(Error::Truncated);
        }
        let layers = bytes[HEADER..layers_end]
            .chunks_exact(LAYER)
            .map(|l| Layer {
                component: l[0],
                shadow: l[1] != 0,
                selectable: l[2] != 0,
                transparent: l[3] != 0,
                draw_effect: l[4],
                weapon_class: l[5..9].iter().take_while(|&&c| c != 0).map(|&c| char::from(c.to_ascii_lowercase())).collect(),
            })
            .collect();
        Ok(Self { frames, directions, bounds, speed, layers, priority: bytes[priority_start..priority_end].to_vec() })
    }

    /// The components drawn for `direction` and `frame`, back to front.
    #[must_use]
    pub fn draw_order(&self, direction: usize, frame: usize) -> &[u8] {
        let n = self.layers.len();
        let at = (direction.min(self.directions - 1) * self.frames + frame.min(self.frames - 1)) * n;
        &self.priority[at..at + n]
    }

    /// The layer drawing `component`, if any.
    #[must_use]
    pub fn layer(&self, component: u8) -> Option<&Layer> {
        self.layers.iter().find(|l| l.component == component)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_and_draw_order_parse() {
        let mut file = vec![2, 2, 1, 20, 0, 0, 0, 0];
        for v in [-10i32, 10, -40, 5, 80] {
            file.extend_from_slice(&v.to_le_bytes());
        }
        file.extend_from_slice(&[1, 1, 1, 0, 0, b'1', b'H', b'T', 0]);
        file.extend_from_slice(&[5, 1, 1, 0, 0, b'1', b'h', b's', 0]);
        file.extend_from_slice(&[0, 0]); // events
        file.extend_from_slice(&[1, 5, 5, 1]); // frame 0: TR then RH; frame 1: RH then TR
        let cof = Cof::parse(&file).unwrap();
        assert_eq!((cof.frames, cof.directions, cof.speed, cof.bounds), (2, 1, 80, [-10, 10, -40, 5]));
        assert_eq!(cof.layer(1).unwrap().weapon_class, "1ht");
        assert_eq!(cof.layer(5).unwrap().weapon_class, "1hs");
        assert_eq!(cof.draw_order(0, 0), [1, 5]);
        assert_eq!(cof.draw_order(0, 1), [5, 1]);
        assert!(Cof::parse(&file[..file.len() - 1]).is_err());
    }
}
