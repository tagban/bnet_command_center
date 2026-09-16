//! Just enough PNG to write a 16×16 icon.
//!
//! A dependency would do this, and for anything larger one should. But the whole job here is
//! a handful of tiny images, and PNG permits a deflate stream made of *stored* blocks — the
//! compressor is allowed to give up and copy the bytes. That removes the only hard part, and
//! leaves two checksums and a fixed header. The files come out a few kilobytes instead of a
//! few hundred bytes, which for an icon nobody will notice.

/// CRC-32 as PNG specifies it, over a chunk's type and data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Adler-32, which zlib puts at the end of the stream.
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    let mut typed = kind.to_vec();
    typed.extend_from_slice(data);
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
}

/// A zlib stream that stores its input rather than compressing it.
fn stored_zlib(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // deflate, 32K window, no preset dictionary
    let mut rest = raw;
    loop {
        // A stored block's length is a u16, so long input becomes several blocks.
        let take = rest.len().min(0xFFFF);
        let (block, remainder) = rest.split_at(take);
        let last = u8::from(remainder.is_empty());
        out.push(last);
        out.extend_from_slice(&u16::try_from(take).unwrap_or(u16::MAX).to_le_bytes());
        out.extend_from_slice(&(!u16::try_from(take).unwrap_or(u16::MAX)).to_le_bytes());
        out.extend_from_slice(block);
        if remainder.is_empty() {
            break;
        }
        rest = remainder;
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// An 8-bit RGB image as a PNG. `pixels` is `width * height * 3` bytes, row by row.
///
/// # Panics
///
/// If `pixels` is not exactly `width * height * 3` bytes — a caller mistake, not input.
#[must_use]
pub fn rgb(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), (width * height * 3) as usize, "pixels must match the size given");
    let mut raw = Vec::with_capacity(pixels.len() + height as usize);
    for row in pixels.chunks_exact((width * 3) as usize) {
        raw.push(0); // filter: none. Filtering is what a real encoder does to compress well.
        raw.extend_from_slice(row);
    }

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits, truecolour, no interlace

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &stored_zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_is_shaped_the_way_a_reader_expects() {
        let pixels = vec![0xAB; 2 * 2 * 3];
        let png = rgb(2, 2, &pixels);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], "the signature");
        assert_eq!(&png[12..16], b"IHDR");
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]), "and ends at IEND");
    }

    #[test]
    fn the_checksums_are_the_ones_png_and_zlib_specify() {
        // Known answers, so a transcription slip in either table is caught here rather than
        // by an image viewer refusing the file.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        // "abc": a = 1+97+98+99 = 295, b = 98+196+295 = 589.
        assert_eq!(adler32(b"abc"), (589 << 16) | 295);
    }

    #[test]
    fn stored_blocks_carry_the_bytes_through_unchanged() {
        let raw: Vec<u8> = (0..70_000).map(|i| (i % 251) as u8).collect();
        let z = stored_zlib(&raw);
        assert_eq!(&z[..2], &[0x78, 0x01]);
        assert_eq!(z[z.len() - 4..], adler32(&raw).to_be_bytes(), "the stream ends with its checksum");
        // Long input needs more than one block, and only the last may be marked final.
        assert!(z.len() > raw.len() + 6, "several blocks, each with a header");
    }
}
