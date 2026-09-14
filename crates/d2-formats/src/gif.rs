//! A GIF89a writer for palette-indexed animations — Diablo II's 256-colour sprites map onto a
//! GIF's colour table one to one, so frames need no quantising.
//!
//! Written from the GIF89a specification: a global colour table, the NETSCAPE2.0 looping
//! extension, and per frame a graphic control extension (delay, transparent index, restore to
//! background) and an LZW-compressed image.

use std::collections::HashMap;

/// An animation to write.
#[derive(Debug, Clone)]
pub struct Animation<'a> {
    /// Canvas width.
    pub width: u16,
    /// Canvas height.
    pub height: u16,
    /// 256 RGB triples.
    pub palette: &'a [[u8; 3]; 256],
    /// The palette index drawn as transparent.
    pub transparent: u8,
    /// Each frame's delay before the next, in hundredths of a second; the last delay repeats for
    /// frames past the end.
    pub delays: &'a [u16],
    /// Each frame's `width × height` palette indices, row by row.
    pub frames: &'a [Vec<u8>],
}

/// Encode an animation that loops forever.
///
/// # Panics
///
/// If a frame's length is not `width × height`.
#[must_use]
pub fn encode(a: &Animation<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"GIF89a");
    out.extend_from_slice(&a.width.to_le_bytes());
    out.extend_from_slice(&a.height.to_le_bytes());
    out.extend_from_slice(&[0xF7, a.transparent, 0]); // global table of 256, background index
    for rgb in a.palette {
        out.extend_from_slice(rgb);
    }
    // Loop forever.
    out.extend_from_slice(&[0x21, 0xFF, 0x0B]);
    out.extend_from_slice(b"NETSCAPE2.0");
    out.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x00]);
    for (i, frame) in a.frames.iter().enumerate() {
        let delay = a.delays.get(i).or(a.delays.last()).copied().unwrap_or(10);
        assert_eq!(frame.len(), usize::from(a.width) * usize::from(a.height), "frame size");
        // Graphic control: restore to background, transparent colour.
        out.extend_from_slice(&[0x21, 0xF9, 0x04, 0x09]);
        out.extend_from_slice(&delay.to_le_bytes());
        out.extend_from_slice(&[a.transparent, 0x00]);
        out.push(0x2C);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&a.width.to_le_bytes());
        out.extend_from_slice(&a.height.to_le_bytes());
        out.push(0);
        out.push(8); // minimum code size
        let data = lzw(frame);
        for chunk in data.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
    }
    out.push(0x3B);
    out
}

/// GIF-flavoured LZW with an 8-bit alphabet: codes grow from 9 to 12 bits, and the table is
/// cleared when it fills.
fn lzw(pixels: &[u8]) -> Vec<u8> {
    const CLEAR: u32 = 256;
    const END: u32 = 257;
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut acc_bits = 0u32;
    let mut emit = |code: u32, size: u32, out: &mut Vec<u8>| {
        acc |= code << acc_bits;
        acc_bits += size;
        while acc_bits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            acc_bits -= 8;
        }
    };
    let mut table: HashMap<(u32, u8), u32> = HashMap::new();
    let mut next = END + 1;
    let mut size = 9;
    emit(CLEAR, size, &mut out);
    let Some((&first, rest)) = pixels.split_first() else {
        emit(END, size, &mut out);
        if acc_bits > 0 {
            out.push(acc as u8);
        }
        return out;
    };
    let mut prefix = u32::from(first);
    for &p in rest {
        if let Some(&code) = table.get(&(prefix, p)) {
            prefix = code;
            continue;
        }
        emit(prefix, size, &mut out);
        if next < 4096 {
            table.insert((prefix, p), next);
            next += 1;
            if next > (1 << size) && size < 12 {
                size += 1;
            }
        } else {
            emit(CLEAR, size, &mut out);
            table.clear();
            next = END + 1;
            size = 9;
        }
        prefix = u32::from(p);
    }
    emit(prefix, size, &mut out);
    emit(END, size, &mut out);
    if acc_bits > 0 {
        out.push(acc as u8);
    }
    out
}

/// Decode the frames of a GIF this module wrote (one global table, full-canvas frames,
/// 8-bit codes): each frame's palette indices. `None` for anything else.
#[must_use]
pub fn decode_frames(bytes: &[u8]) -> Option<(u16, u16, Vec<Vec<u8>>)> {
    if !bytes.starts_with(b"GIF89a") || bytes.len() < 13 {
        return None;
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]);
    let height = u16::from_le_bytes([bytes[8], bytes[9]]);
    let mut at = 13 + if bytes[10] & 0x80 != 0 { 3 << ((bytes[10] & 7) + 1) } else { 0 };
    let mut frames = Vec::new();
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let len = usize::from(*bytes.get(at)?);
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
        }
    };
    loop {
        match *bytes.get(at)? {
            0x21 => at = skip_blocks(at + 2)?,
            0x2C => {
                if bytes.get(at + 9)? & 0x80 != 0 {
                    return None;
                }
                at += 11; // descriptor and the minimum code size
                let mut data = Vec::new();
                loop {
                    let len = usize::from(*bytes.get(at)?);
                    at += 1;
                    if len == 0 {
                        break;
                    }
                    data.extend_from_slice(bytes.get(at..at + len)?);
                    at += len;
                }
                let pixels = unlzw(&data)?;
                if pixels.len() != usize::from(width) * usize::from(height) {
                    return None;
                }
                frames.push(pixels);
            }
            0x3B => return Some((width, height, frames)),
            _ => return None,
        }
    }
}

/// Undo [`lzw`].
fn unlzw(data: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut read = |size: u32| -> Option<u32> {
        let mut v = 0u32;
        for i in 0..size {
            let bit = (data.get(pos / 8)? >> (pos % 8)) & 1;
            v |= u32::from(bit) << i;
            pos += 1;
        }
        Some(v)
    };
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();
    let reset = |table: &mut Vec<Vec<u8>>| {
        table.clear();
        table.extend((0..=255u8).map(|b| vec![b]));
        table.push(Vec::new());
        table.push(Vec::new());
    };
    reset(&mut table);
    let mut size = 9;
    let mut previous: Option<Vec<u8>> = None;
    loop {
        let code = read(size)? as usize;
        if code == 256 {
            reset(&mut table);
            size = 9;
            previous = None;
            continue;
        }
        if code == 257 {
            return Some(out);
        }
        let entry = if code < table.len() {
            table[code].clone()
        } else {
            let mut e = previous.clone()?;
            e.push(e[0]);
            e
        };
        out.extend_from_slice(&entry);
        if let Some(mut p) = previous.take() {
            p.push(entry[0]);
            if table.len() < 4096 {
                table.push(p);
            }
            if table.len() == (1 << size) && size < 12 {
                size += 1;
            }
        }
        previous = Some(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference LZW decoder, to check the encoder against.
    fn unlzw_reference(data: &[u8]) -> Vec<u8> {
        let mut pos = 0usize;
        let mut read = |size: u32| {
            let mut v = 0u32;
            for i in 0..size {
                let bit = (data[pos / 8] >> (pos % 8)) & 1;
                v |= u32::from(bit) << i;
                pos += 1;
            }
            v
        };
        let mut out = Vec::new();
        let mut table: Vec<Vec<u8>> = Vec::new();
        let reset = |table: &mut Vec<Vec<u8>>| {
            table.clear();
            table.extend((0..=255u8).map(|b| vec![b]));
            table.push(Vec::new());
            table.push(Vec::new());
        };
        reset(&mut table);
        let mut size = 9;
        let mut previous: Option<Vec<u8>> = None;
        loop {
            let code = read(size) as usize;
            if code == 256 {
                reset(&mut table);
                size = 9;
                previous = None;
                continue;
            }
            if code == 257 {
                break;
            }
            let entry = if code < table.len() {
                table[code].clone()
            } else {
                let mut e = previous.clone().unwrap();
                e.push(e[0]);
                e
            };
            out.extend_from_slice(&entry);
            if let Some(mut p) = previous.take() {
                p.push(entry[0]);
                table.push(p);
                if table.len() == (1 << size) && size < 12 {
                    size += 1;
                }
            }
            previous = Some(entry);
        }
        out
    }

    #[test]
    fn lzw_round_trips_through_table_resets() {
        let mut pixels = Vec::new();
        let mut x = 12345u32;
        for _ in 0..40_000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            pixels.push(((x >> 16) % 7) as u8 * if x & 0x100 == 0 { 1 } else { 30 });
        }
        assert_eq!(unlzw_reference(&lzw(&pixels)), pixels);
        assert_eq!(unlzw(&lzw(&pixels)).unwrap(), pixels);
        assert_eq!(unlzw_reference(&lzw(&[5])), [5]);
        assert_eq!(unlzw_reference(&lzw(&[0; 5000])), vec![0; 5000]);
    }

    #[test]
    fn the_file_has_its_parts() {
        let palette = [[0u8; 3]; 256];
        let frames = vec![vec![0u8, 1, 2, 3], vec![3u8, 2, 1, 0]];
        let gif = encode(&Animation { width: 2, height: 2, palette: &palette, transparent: 0, delays: &[8], frames: &frames });
        assert!(gif.starts_with(b"GIF89a\x02\x00\x02\x00\xF7"));
        assert!(gif.iter().filter(|&&b| b == 0x2C).count() >= 2);
        assert_eq!(*gif.last().unwrap(), 0x3B);
        assert_eq!(decode_frames(&gif), Some((2, 2, frames)));
    }
}
