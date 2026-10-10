//! Palette transform tables — `data\global\palette\<act>\pal.pl2`, beside the act's `pal.dat`.
//!
//! The game draws into an 8-bit buffer, so light, transparency and coloured text are all lookups
//! from one palette index to another. These are those lookups.
//!
//! ```text
//! 0x00000 u8[256][4]       the palette, RGBA
//! 0x00400 u8[32][256]      light levels, darkest first
//! 0x02400 u8[16][256]      inverted colours
//! 0x03400 u8[256]          selected unit
//! 0x03500 u8[3][256][256]  alpha blends 25 %, 50 %, 75 %: [level][source][destination]
//! 0x33500 u8[256][256]     additive blend
//! 0x43500 u8[256][256]     multiplicative blend
//! 0x53500 u8[111][256]     hue variations
//! 0x5A400 u8[3][256]       red, green, blue tones
//! 0x5A700 u8[14][256]      unknown
//! 0x5B500 u8[256][256]     max-component blend
//! 0x6B500 u8[256]          darkened
//! 0x6B600 u8[13][3]        text colours, RGB
//! 0x6B627 u8[13][256]      text colour shifts
//! ```
//!
//! 443 175 bytes in all. Written from the published PL2 format notes.

/// Length of a `pal.pl2`.
pub const LEN: usize = 0x6B627 + 13 * 256;

const LIGHT: usize = 0x400;
const ALPHA: usize = 0x3500;
const ADDITIVE: usize = 0x33500;
const MULTIPLY: usize = 0x43500;
const TEXT_SHIFTS: usize = 0x6B627;

/// The transforms of one palette.
#[derive(Clone, PartialEq, Eq)]
pub struct Pl2(Vec<u8>);

impl std::fmt::Debug for Pl2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pl2")
    }
}

/// How a source pixel is laid over what is already drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    /// Replace it.
    Opaque,
    /// Mix 25 %, 50 % or 75 % of the source (`0`, `1`, `2`).
    Alpha(u8),
    /// Add the colours.
    Additive,
    /// Multiply the colours.
    Multiply,
}

impl Pl2 {
    /// Take a `pal.pl2`. `None` if it is not the right length.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        (bytes.len() == LEN).then(|| Self(bytes.to_vec()))
    }

    /// The index that `src` drawn over `dst` leaves.
    #[must_use]
    pub fn blend(&self, blend: Blend, src: u8, dst: u8) -> u8 {
        let cell = usize::from(src) * 256 + usize::from(dst);
        match blend {
            Blend::Opaque => src,
            Blend::Alpha(level) => self.0[ALPHA + usize::from(level.min(2)) * 0x10000 + cell],
            Blend::Additive => self.0[ADDITIVE + cell],
            Blend::Multiply => self.0[MULTIPLY + cell],
        }
    }

    /// The 256-entry table that lights a pixel at `level` (0 darkest … 31).
    #[must_use]
    pub fn light(&self, level: u8) -> &[u8] {
        let at = LIGHT + usize::from(level.min(31)) * 256;
        &self.0[at..at + 256]
    }

    /// The 256-entry table that tints a font to text colour `colour` (0 white … 12).
    #[must_use]
    pub fn text(&self, colour: u8) -> &[u8] {
        let at = TEXT_SHIFTS + usize::from(colour.min(12)) * 256;
        &self.0[at..at + 256]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_found_at_their_offsets() {
        let mut b = vec![0u8; LEN];
        b[ALPHA + 0x10000 + 3 * 256 + 4] = 9;
        b[ADDITIVE + 5 * 256 + 6] = 7;
        b[TEXT_SHIFTS + 2 * 256 + 1] = 8;
        b[LIGHT + 31 * 256] = 6;
        let p = Pl2::parse(&b).unwrap();
        assert_eq!(p.blend(Blend::Alpha(1), 3, 4), 9);
        assert_eq!(p.blend(Blend::Additive, 5, 6), 7);
        assert_eq!(p.blend(Blend::Opaque, 5, 6), 5);
        assert_eq!(p.text(2)[1], 8);
        assert_eq!(p.light(40)[0], 6);
        assert!(Pl2::parse(&b[1..]).is_none());
    }
}
