//! Font tables — `data\local\font\<language>\<font>.tbl`, beside the font's DC6.
//!
//! The table maps each character to a frame of the DC6 and gives the advance to use.
//!
//! ```text
//! 0x00 u8[4] "Woo!"     0x04 u16 version     0x06 u32 locale
//! 0x0A u8 line height   0x0B u8 cap height
//! 0x0C glyph[…]         14 bytes: u16 character, u8 ?, u8 width, u8 height, u8[3] ?,
//!                                 u16 frame, u8[4] ?
//! ```
//!
//! Written from the published font table notes.

use std::collections::HashMap;

/// Bytes before the first glyph.
const HEADER_LEN: usize = 12;
/// Bytes per glyph.
const GLYPH_LEN: usize = 14;

/// One character's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// Pixels to advance after drawing it.
    pub width: u8,
    /// Its height.
    pub height: u8,
    /// The DC6 frame that draws it.
    pub frame: u16,
}

/// A parsed table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontTable {
    /// Distance between lines.
    pub line_height: u8,
    /// Height of a capital.
    pub cap_height: u8,
    glyphs: HashMap<u16, Glyph>,
}

impl FontTable {
    /// Parse a table. `None` unless it starts with the `Woo!` signature.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"Woo!" || bytes.len() < HEADER_LEN {
            return None;
        }
        let glyphs = bytes[HEADER_LEN..]
            .chunks_exact(GLYPH_LEN)
            .map(|g| {
                (u16::from_le_bytes([g[0], g[1]]), Glyph { width: g[3], height: g[4], frame: u16::from_le_bytes([g[8], g[9]]) })
            })
            .collect();
        Some(Self { line_height: bytes[0x0A], cap_height: bytes[0x0B], glyphs })
    }

    /// The entry for `c`, if the font has one.
    #[must_use]
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        u16::try_from(u32::from(c)).ok().and_then(|code| self.glyphs.get(&code).copied())
    }

    /// Width of `text` in pixels; characters the font lacks count as nothing.
    #[must_use]
    pub fn width(&self, text: &str) -> u32 {
        text.chars().filter_map(|c| self.glyph(c)).map(|g| u32::from(g.width)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_found_by_character() {
        let mut b = b"Woo!".to_vec();
        b.extend_from_slice(&[1, 0, 0, 0, 0, 0, 16, 11]);
        b.extend_from_slice(&[b'A', 0, 0, 9, 11, 0, 0, 0, 33, 0, 0, 0, 0, 0]);
        let t = FontTable::parse(&b).unwrap();
        assert_eq!((t.line_height, t.cap_height), (16, 11));
        assert_eq!(t.glyph('A'), Some(Glyph { width: 9, height: 11, frame: 33 }));
        assert_eq!(t.width("AAz"), 18);
        assert!(FontTable::parse(b"Wow!").is_none());
    }
}
