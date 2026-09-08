//! The BNI icon file format (`icons.bni`).
//!
//! A BNI is an index of icon entries plus **one embedded TGA** containing every icon
//! stacked vertically. The client fetches it over BNFTP after the server names it in
//! `SID_GETICONDATA`, then picks an icon per user by matching chat flags or a statstring
//! icon code.
//!
//! ```text
//! header  16 bytes, little-endian, no magic number:
//!   u32 header_size   always 16
//!   u16 version       always 1
//!   u16 padding
//!   u32 icon_count
//!   u32 data_offset   where the embedded TGA begins
//!
//! entry   repeated icon_count times:
//!   u32 flags         chat flags this icon applies to, or 0 to match by code
//!   u32 width
//!   u32 height
//!   u32[] codes       four-character codes, zero-terminated
//!
//! image   a complete TGA file: type 10 (RLE true-colour), 24 bpp,
//!         width = max(entry.width), height = sum(entry.height)
//! ```
//!
//! # Two traps
//!
//! **`icons-WAR3.bni` and `WAR3.bni` are not BNI files.** They are MPQ archives of `.blp`
//! images that happen to carry a `.bni` extension. A BNI parser will produce nonsense on
//! them, so [`parse`] detects the MPQ magic and says so rather than failing obscurely.
//!
//! **Some shipped files are malformed.** `icons_clan.bni` and `icons_lag.bni` put
//! `data_offset - 4` in the header-size field and `0xFFFFFFFF` in the data offset;
//! `classic_icons.bni` has entries whose code list starts with a NULL, which breaks a
//! naive zero-terminated read. The entry table must end exactly at `data_offset`, so
//! [`parse`] checks that and reports [`BniError::TableOverrun`] with both positions —
//! enough for an operator to recognise a known-bad file instead of guessing.

use crate::buf::Reader;
use crate::error::FourCc;

/// Fixed BNI header size, and the value the header-size field should hold.
pub const HEADER_LEN: usize = 16;

/// The only BNI version ever observed.
pub const VERSION: u16 = 1;

/// Maximum four-character codes in one entry, excluding the terminator.
pub const MAX_CODES: usize = 31;

/// TGA image type 10: run-length encoded true-colour.
pub const TGA_RLE_TRUECOLOR: u8 = 10;

/// Why a BNI could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BniError {
    /// The file is an MPQ archive, not a BNI.
    ///
    /// `icons-WAR3.bni` and `WAR3.bni` are like this: MPQ archives of `.blp` images with
    /// a misleading extension. Serving them over BNFTP is fine; parsing them as BNI is
    /// not.
    IsMpqArchive,
    /// Too short to contain a header.
    TooShort,
    /// The header-size field was not 16.
    BadHeaderSize(u32),
    /// The version field was not 1.
    UnsupportedVersion(u16),
    /// The image data offset lies outside the file.
    BadDataOffset {
        /// The declared offset.
        offset: u32,
        /// The file length.
        len: usize,
    },
    /// The icon count is implausible for the file size.
    ImplausibleIconCount(u32),
    /// The entry table did not end where the header said the image begins.
    ///
    /// This is the signature of a malformed file. Both positions are reported so an
    /// operator can recognise which one they have.
    TableOverrun {
        /// Where the entry table actually ended.
        table_end: usize,
        /// Where the header said the image begins.
        data_offset: usize,
    },
    /// An entry ran past the end of the file.
    TruncatedEntry {
        /// Index of the offending entry.
        index: u32,
    },
    /// An entry declared more codes than the format allows.
    TooManyCodes {
        /// Index of the offending entry.
        index: u32,
    },
}

impl std::fmt::Display for BniError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IsMpqArchive => write!(
                f,
                "this is an MPQ archive, not a BNI file (icons-WAR3.bni and WAR3.bni are \
                 MPQs of .blp images despite the extension); serve it verbatim over BNFTP \
                 instead of parsing it"
            ),
            Self::TooShort => write!(f, "shorter than a {HEADER_LEN}-byte BNI header"),
            Self::BadHeaderSize(v) => {
                write!(f, "header size field is {v}, expected {HEADER_LEN}")
            }
            Self::UnsupportedVersion(v) => write!(f, "BNI version {v} is not supported"),
            Self::BadDataOffset { offset, len } => write!(
                f,
                "image data offset {offset:#x} lies outside a {len}-byte file"
            ),
            Self::ImplausibleIconCount(n) => write!(f, "implausible icon count {n}"),
            Self::TableOverrun {
                table_end,
                data_offset,
            } => write!(
                f,
                "entry table ended at {table_end} but the image begins at {data_offset}; \
                 this file is malformed (known bad: icons_clan.bni, icons_lag.bni, \
                 classic_icons.bni)"
            ),
            Self::TruncatedEntry { index } => write!(f, "entry {index} runs past end of file"),
            Self::TooManyCodes { index } => {
                write!(f, "entry {index} declares more than {MAX_CODES} codes")
            }
        }
    }
}

impl std::error::Error for BniError {}

/// One icon entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BniIcon {
    /// Chat flags this icon applies to. Matched with a bitwise AND.
    ///
    /// Zero means "match by code instead".
    pub flags: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Four-character codes this icon serves, from a statstring's icon field.
    pub codes: Vec<FourCc>,
}

/// A parsed BNI file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BniFile {
    /// Format version.
    pub version: u16,
    /// Icon entries, in file order. **Order is significant: first match wins.**
    pub icons: Vec<BniIcon>,
    /// The embedded TGA, verbatim.
    pub image: Vec<u8>,
}

/// A TGA header, enough of it to validate a BNI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TgaHeader {
    /// Image type. BNI uses 10 (RLE true-colour).
    pub image_type: u8,
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Bits per pixel.
    pub depth: u8,
}

impl TgaHeader {
    /// Parse the 18-byte TGA header.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 18 {
            return None;
        }
        Some(Self {
            image_type: bytes[2],
            width: u16::from_le_bytes([bytes[12], bytes[13]]),
            height: u16::from_le_bytes([bytes[14], bytes[15]]),
            depth: bytes[16],
        })
    }
}

/// Whether a byte slice looks like an MPQ archive.
#[must_use]
pub fn is_mpq(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..3] == b"MPQ" && (bytes[3] == 0x1A || bytes[3] == 0x1B)
}

/// Parse a BNI file.
///
/// # Errors
///
/// [`BniError`], with enough context to tell a malformed file from a wrong one.
pub fn parse(bytes: &[u8]) -> Result<BniFile, BniError> {
    if is_mpq(bytes) {
        return Err(BniError::IsMpqArchive);
    }
    if bytes.len() < HEADER_LEN {
        return Err(BniError::TooShort);
    }

    let mut r = Reader::new(bytes);
    let header_size = r.u32().map_err(|_| BniError::TooShort)?;
    if header_size != HEADER_LEN as u32 {
        return Err(BniError::BadHeaderSize(header_size));
    }
    let version = r.u16().map_err(|_| BniError::TooShort)?;
    if version != VERSION {
        return Err(BniError::UnsupportedVersion(version));
    }
    let _padding = r.u16().map_err(|_| BniError::TooShort)?;
    let icon_count = r.u32().map_err(|_| BniError::TooShort)?;
    let data_offset = r.u32().map_err(|_| BniError::TooShort)?;

    // The smallest possible entry is 16 bytes (flags, w, h, terminator), so an icon
    // count that could not fit is rejected before allocating for it.
    if (icon_count as usize).saturating_mul(16) > bytes.len() {
        return Err(BniError::ImplausibleIconCount(icon_count));
    }
    let data_offset_usize = data_offset as usize;
    if data_offset_usize > bytes.len() || data_offset_usize < HEADER_LEN {
        return Err(BniError::BadDataOffset {
            offset: data_offset,
            len: bytes.len(),
        });
    }

    let mut icons = Vec::with_capacity(icon_count as usize);
    let mut pos = HEADER_LEN;
    for index in 0..icon_count {
        let mut r = Reader::new(&bytes[pos..data_offset_usize]);
        let flags = r.u32().map_err(|_| BniError::TruncatedEntry { index })?;
        let width = r.u32().map_err(|_| BniError::TruncatedEntry { index })?;
        let height = r.u32().map_err(|_| BniError::TruncatedEntry { index })?;

        let mut codes = Vec::new();
        loop {
            let raw = r.u32().map_err(|_| BniError::TruncatedEntry { index })?;
            if raw == 0 {
                break;
            }
            if codes.len() >= MAX_CODES {
                return Err(BniError::TooManyCodes { index });
            }
            // Codes are four ASCII bytes in a u32; the wire is little-endian, so the
            // human-readable order is the reverse of the stored bytes.
            let b = raw.to_le_bytes();
            codes.push(FourCc(u32::from_be_bytes([b[3], b[2], b[1], b[0]])));
        }

        pos += 12 + (codes.len() + 1) * 4;
        icons.push(BniIcon {
            flags,
            width,
            height,
            codes,
        });
    }

    if pos != data_offset_usize {
        return Err(BniError::TableOverrun {
            table_end: pos,
            data_offset: data_offset_usize,
        });
    }

    Ok(BniFile {
        version,
        icons,
        image: bytes[data_offset_usize..].to_vec(),
    })
}

/// Serialise a BNI file.
#[must_use]
pub fn build(file: &BniFile) -> Vec<u8> {
    let table_len: usize = file
        .icons
        .iter()
        .map(|i| 12 + (i.codes.len() + 1) * 4)
        .sum();
    let data_offset = HEADER_LEN + table_len;

    let mut out = Vec::with_capacity(data_offset + file.image.len());
    out.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    out.extend_from_slice(&file.version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(file.icons.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data_offset as u32).to_le_bytes());

    for icon in &file.icons {
        out.extend_from_slice(&icon.flags.to_le_bytes());
        out.extend_from_slice(&icon.width.to_le_bytes());
        out.extend_from_slice(&icon.height.to_le_bytes());
        for code in &icon.codes {
            let b = code.as_ascii();
            out.extend_from_slice(&[b[3], b[2], b[1], b[0]]);
        }
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    out.extend_from_slice(&file.image);
    out
}

/// A problem found while checking a BNI against its embedded image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BniWarning {
    /// The embedded image is not a readable TGA header.
    ImageNotTga,
    /// The image is not RLE true-colour.
    UnexpectedImageType(u8),
    /// The image is not 24- or 32-bit.
    UnexpectedDepth(u8),
    /// TGA width does not equal the widest icon.
    WidthMismatch {
        /// From the TGA header.
        tga: u16,
        /// The widest entry.
        widest: u32,
    },
    /// TGA height does not equal the sum of icon heights.
    HeightMismatch {
        /// From the TGA header.
        tga: u16,
        /// Sum of entry heights.
        total: u32,
    },
    /// An entry has no flags and no codes, so nothing can ever select it.
    UnreachableIcon {
        /// Index of the entry.
        index: usize,
    },
}

/// Check a parsed BNI for the problems that make a client render nothing.
///
/// Returns every problem rather than the first, so an operator fixing an icon pack gets
/// the whole list in one pass.
#[must_use]
pub fn validate(file: &BniFile) -> Vec<BniWarning> {
    let mut out = Vec::new();

    for (index, icon) in file.icons.iter().enumerate() {
        if icon.flags == 0 && icon.codes.is_empty() {
            out.push(BniWarning::UnreachableIcon { index });
        }
    }

    let Some(tga) = TgaHeader::parse(&file.image) else {
        out.push(BniWarning::ImageNotTga);
        return out;
    };
    if tga.image_type != TGA_RLE_TRUECOLOR {
        out.push(BniWarning::UnexpectedImageType(tga.image_type));
    }
    if tga.depth != 24 && tga.depth != 32 {
        out.push(BniWarning::UnexpectedDepth(tga.depth));
    }

    let widest = file.icons.iter().map(|i| i.width).max().unwrap_or(0);
    let total: u32 = file.icons.iter().map(|i| i.height).sum();
    if u32::from(tga.width) != widest {
        out.push(BniWarning::WidthMismatch {
            tga: tga.width,
            widest,
        });
    }
    if u32::from(tga.height) != total {
        out.push(BniWarning::HeightMismatch {
            tga: tga.height,
            total,
        });
    }
    out
}

/// Pick the icon a user should be shown.
///
/// Matching order is the client's: **file order, first match wins.** An entry with
/// non-zero flags matches when any of them are set on the user; an entry with zero flags
/// matches by statstring icon code.
///
/// Returns the index into [`BniFile::icons`], or `None` if nothing matched — in which
/// case the client falls back to its own built-in icons.
#[must_use]
pub fn select_icon(file: &BniFile, user_flags: u32, code: Option<FourCc>) -> Option<usize> {
    file.icons.iter().position(|icon| {
        if icon.flags != 0 {
            icon.flags & user_flags != 0
        } else {
            code.is_some_and(|c| icon.codes.contains(&c))
        }
    })
}

/// The vertical offset of an icon within the embedded image, in pixels.
///
/// Icons are stacked top to bottom in file order.
#[must_use]
pub fn icon_y_offset(file: &BniFile, index: usize) -> u32 {
    file.icons.iter().take(index).map(|i| i.height).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::user_flags;

    /// A minimal but valid TGA header for `w` x `h`, RLE true-colour, 24 bpp.
    fn tga(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0u8; 18];
        v[2] = TGA_RLE_TRUECOLOR;
        v[12..14].copy_from_slice(&w.to_le_bytes());
        v[14..16].copy_from_slice(&h.to_le_bytes());
        v[16] = 24;
        v.extend_from_slice(b"pixels");
        v
    }

    fn sample() -> BniFile {
        BniFile {
            version: VERSION,
            icons: vec![
                BniIcon {
                    flags: user_flags::ADMIN,
                    width: 14,
                    height: 14,
                    codes: vec![],
                },
                BniIcon {
                    flags: user_flags::OPERATOR,
                    width: 14,
                    height: 14,
                    codes: vec![],
                },
                BniIcon {
                    flags: 0,
                    width: 14,
                    height: 14,
                    codes: vec![FourCc::from_ascii(b"2H3W"), FourCc::from_ascii(b"3H3W")],
                },
            ],
            image: tga(14, 42),
        }
    }

    #[test]
    fn build_then_parse_round_trips() {
        let original = sample();
        let bytes = build(&original);
        assert_eq!(parse(&bytes).unwrap(), original);
    }

    #[test]
    fn the_header_is_the_documented_shape() {
        let bytes = build(&sample());
        assert_eq!(&bytes[0..4], &16u32.to_le_bytes(), "header size");
        assert_eq!(&bytes[4..6], &1u16.to_le_bytes(), "version");
        assert_eq!(&bytes[8..12], &3u32.to_le_bytes(), "icon count");
        // Entry table: two entries with no codes (16 bytes each) and one with two
        // codes (12 + 3*4 = 24), so the image starts at 16 + 16 + 16 + 24 = 72.
        assert_eq!(&bytes[12..16], &72u32.to_le_bytes(), "data offset");
    }

    #[test]
    fn codes_are_stored_reversed_like_every_other_fourcc() {
        let file = BniFile {
            version: VERSION,
            icons: vec![BniIcon {
                flags: 0,
                width: 1,
                height: 1,
                codes: vec![FourCc::from_ascii(b"2H3W")],
            }],
            image: tga(1, 1),
        };
        let bytes = build(&file);
        // Entry begins at offset 16: flags, width, height, then the code.
        assert_eq!(&bytes[28..32], b"W3H2", "little-endian reverses the ASCII");
        assert_eq!(parse(&bytes).unwrap().icons[0].codes[0].to_string(), "2H3W");
    }

    #[test]
    fn an_mpq_is_identified_rather_than_misparsed() {
        // icons-WAR3.bni and WAR3.bni are MPQ archives of .blp images. Saying so is far
        // more useful than failing on a nonsense header size.
        let mut mpq = b"MPQ\x1A".to_vec();
        mpq.extend_from_slice(&[0u8; 64]);
        assert_eq!(parse(&mpq), Err(BniError::IsMpqArchive));
        assert!(is_mpq(&mpq));
        assert!(!is_mpq(&build(&sample())));
    }

    #[test]
    fn a_bad_header_size_is_rejected() {
        let mut bytes = build(&sample());
        bytes[0] = 0x20;
        assert_eq!(parse(&bytes), Err(BniError::BadHeaderSize(0x20)));
    }

    #[test]
    fn an_unsupported_version_is_rejected() {
        let mut bytes = build(&sample());
        bytes[4] = 2;
        assert_eq!(parse(&bytes), Err(BniError::UnsupportedVersion(2)));
    }

    #[test]
    fn a_data_offset_past_the_end_is_rejected() {
        let mut bytes = build(&sample());
        bytes[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        // icons_clan.bni and icons_lag.bni are exactly this shape.
        assert!(matches!(parse(&bytes), Err(BniError::BadDataOffset { .. })));
    }

    #[test]
    fn a_table_that_does_not_meet_the_image_is_reported_with_both_positions() {
        let mut bytes = build(&sample());
        // Claim the image starts four bytes later than the table actually ends.
        bytes[12..16].copy_from_slice(&76u32.to_le_bytes());
        match parse(&bytes) {
            Err(BniError::TableOverrun {
                table_end,
                data_offset,
            }) => {
                assert_eq!(table_end, 72);
                assert_eq!(data_offset, 76);
            }
            other => panic!("expected TableOverrun, got {other:?}"),
        }
    }

    #[test]
    fn an_implausible_icon_count_does_not_allocate() {
        let mut bytes = build(&sample());
        bytes[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(BniError::ImplausibleIconCount(_))
        ));
    }

    #[test]
    fn a_short_file_is_rejected() {
        assert_eq!(parse(&[]), Err(BniError::TooShort));
        assert_eq!(parse(&[0u8; 8]), Err(BniError::TooShort));
    }

    #[test]
    fn flags_match_before_codes_and_in_file_order() {
        let f = sample();
        assert_eq!(select_icon(&f, user_flags::ADMIN, None), Some(0));
        assert_eq!(select_icon(&f, user_flags::OPERATOR, None), Some(1));
        // An admin who is also an operator gets the first matching entry, not the most
        // specific — that is the client's rule and we must match it.
        assert_eq!(
            select_icon(&f, user_flags::ADMIN | user_flags::OPERATOR, None),
            Some(0)
        );
    }

    #[test]
    fn code_matching_applies_only_to_zero_flag_entries() {
        let f = sample();
        assert_eq!(
            select_icon(&f, 0, Some(FourCc::from_ascii(b"3H3W"))),
            Some(2)
        );
        assert_eq!(select_icon(&f, 0, Some(FourCc::from_ascii(b"9Z9Z"))), None);
        assert_eq!(select_icon(&f, 0, None), None);
    }

    #[test]
    fn icon_offsets_stack_vertically() {
        let f = sample();
        assert_eq!(icon_y_offset(&f, 0), 0);
        assert_eq!(icon_y_offset(&f, 1), 14);
        assert_eq!(icon_y_offset(&f, 2), 28);
    }

    #[test]
    fn validate_accepts_a_well_formed_file() {
        assert!(validate(&sample()).is_empty());
    }

    #[test]
    fn validate_catches_image_dimension_mismatches() {
        let mut f = sample();
        f.image = tga(10, 10);
        let w = validate(&f);
        assert!(w.contains(&BniWarning::WidthMismatch { tga: 10, widest: 14 }));
        assert!(w.contains(&BniWarning::HeightMismatch { tga: 10, total: 42 }));
    }

    #[test]
    fn validate_catches_an_icon_nothing_can_select() {
        let mut f = sample();
        f.icons.push(BniIcon {
            flags: 0,
            width: 14,
            height: 14,
            codes: vec![],
        });
        f.image = tga(14, 56);
        assert!(validate(&f).contains(&BniWarning::UnreachableIcon { index: 3 }));
    }

    #[test]
    fn validate_catches_a_wrong_image_encoding() {
        let mut f = sample();
        f.image[2] = 2; // uncompressed true-colour
        f.image[16] = 8; // paletted depth
        let w = validate(&f);
        assert!(w.contains(&BniWarning::UnexpectedImageType(2)));
        assert!(w.contains(&BniWarning::UnexpectedDepth(8)));
    }

    #[test]
    fn parser_never_panics_on_arbitrary_input() {
        let mut seed = 0x5EED_1234u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let n = (next() % 160) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let _ = parse(&bytes);
        }
        // And well-formed headers with garbage tables must also terminate.
        for _ in 0..20_000 {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&(next() % 8).to_le_bytes());
            bytes.extend_from_slice(&(16 + next() % 64).to_le_bytes());
            for _ in 0..64 {
                bytes.extend_from_slice(&next().to_le_bytes());
            }
            let _ = parse(&bytes);
        }
    }
}
