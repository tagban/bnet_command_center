//! Palettes — `data\global\palette\<act>\pal.dat`: 256 colours, three bytes each, stored
//! blue, green, red.

/// 256 colours as `[r, g, b]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette(pub [[u8; 3]; 256]);

impl Palette {
    /// Parse a `pal.dat`. `None` if it is shorter than 768 bytes.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let body = bytes.get(..768)?;
        let mut colours = [[0; 3]; 256];
        for (c, bgr) in colours.iter_mut().zip(body.chunks_exact(3)) {
            *c = [bgr[2], bgr[1], bgr[0]];
        }
        Some(Self(colours))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_blue_first() {
        let mut b = vec![0; 768];
        b[3..6].copy_from_slice(&[1, 2, 3]);
        assert_eq!(Palette::parse(&b).unwrap().0[1], [3, 2, 1]);
        assert!(Palette::parse(&b[..767]).is_none());
    }
}
