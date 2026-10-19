//! The one TGA shape a BNI carries: type 10, run-length encoded true colour.
//!
//! Stored bottom-up unless the header says otherwise, and in BGR order rather than RGB —
//! both of which are ordinary for TGA and both of which produce upside-down, blue-faced
//! icons if missed.

/// A decoded image: `width * height * 3` bytes, RGB, top row first.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decode the RLE true-colour TGA a BNI embeds. `None` if it is some other kind.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Image> {
    let header = bytes.get(..18)?;
    let id_len = header[0] as usize;
    let image_type = header[2];
    let width = u32::from(u16::from_le_bytes([header[12], header[13]]));
    let height = u32::from(u16::from_le_bytes([header[14], header[15]]));
    let depth = header[16];
    let top_down = header[17] & 0x20 != 0;
    if image_type != 10 || (depth != 24 && depth != 32) || width == 0 || height == 0 {
        return None;
    }
    let bytes_per_pixel = (depth / 8) as usize;
    let mut data = bytes.get(18 + id_len..)?;

    let wanted = (width * height) as usize;
    let mut bgr: Vec<u8> = Vec::with_capacity(wanted * 3);
    while bgr.len() < wanted * 3 {
        let (&packet, rest) = data.split_first()?;
        data = rest;
        let count = usize::from(packet & 0x7F) + 1;
        if packet & 0x80 != 0 {
            // A run: one pixel repeated.
            let pixel = data.get(..bytes_per_pixel)?;
            data = &data[bytes_per_pixel..];
            for _ in 0..count {
                bgr.extend_from_slice(&pixel[..3]);
            }
        } else {
            // Literal pixels, one after another.
            let run = data.get(..count * bytes_per_pixel)?;
            data = &data[count * bytes_per_pixel..];
            for pixel in run.chunks_exact(bytes_per_pixel) {
                bgr.extend_from_slice(&pixel[..3]);
            }
        }
    }
    bgr.truncate(wanted * 3);

    // TGA stores blue first, and starts at the bottom unless told otherwise.
    let stride = (width * 3) as usize;
    let mut pixels = vec![0u8; wanted * 3];
    for y in 0..height as usize {
        let from = if top_down { y } else { height as usize - 1 - y };
        let row = &bgr[from * stride..from * stride + stride];
        let out = &mut pixels[y * stride..y * stride + stride];
        for (dst, src) in out.chunks_exact_mut(3).zip(row.chunks_exact(3)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
        }
    }
    Some(Image { width, height, pixels })
}

/// `height` rows starting at `top`, as RGB bytes. `None` if the image is not that tall.
#[must_use]
pub fn rows(image: &Image, top: u32, height: u32) -> Option<Vec<u8>> {
    if top.checked_add(height)? > image.height {
        return None;
    }
    let stride = (image.width * 3) as usize;
    let start = top as usize * stride;
    let end = start + height as usize * stride;
    image.pixels.get(start..end).map(<[u8]>::to_vec)
}

/// RGB pixels (top row first) as a TGA type 10: 24-bit, top-down, run-length encoded, with
/// no packet crossing a row, as the format asks.
#[must_use]
pub fn encode(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 18];
    out[2] = bnetcc_proto::bni::TGA_RLE_TRUECOLOR;
    out[12..14].copy_from_slice(&(width as u16).to_le_bytes());
    out[14..16].copy_from_slice(&(height as u16).to_le_bytes());
    out[16] = 24;
    out[17] = 0x20;
    let (w, h) = (width as usize, height as usize);
    for y in 0..h {
        let row: Vec<[u8; 3]> = (0..w).map(|x| {
            let i = (y * w + x) * 3;
            [rgb[i + 2], rgb[i + 1], rgb[i]]
        }).collect();
        let mut x = 0;
        while x < w {
            let mut run = 1;
            while x + run < w && run < 128 && row[x + run] == row[x] {
                run += 1;
            }
            if run > 1 {
                out.push(0x80 | (run - 1) as u8);
                out.extend_from_slice(&row[x]);
                x += run;
            } else {
                let start = x;
                while x < w && x - start < 128 && (x + 1 >= w || row[x + 1] != row[x]) {
                    x += 1;
                }
                out.push((x - start - 1) as u8);
                for p in &row[start..x] {
                    out.extend_from_slice(p);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×2 RLE TGA: a run of two blue pixels, then two literal pixels (red, green).
    fn sample(top_down: bool) -> Vec<u8> {
        let mut t = vec![0u8; 18];
        t[2] = 10;
        t[12] = 2;
        t[14] = 2;
        t[16] = 24;
        t[17] = if top_down { 0x20 } else { 0 };
        t.extend_from_slice(&[0x81, 0xFF, 0x00, 0x00]); // run of 2: BGR blue
        t.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00]); // red, green
        t
    }

    #[test]
    fn what_is_encoded_decodes_the_same() {
        // Runs, single pixels and a run that reaches the row's end.
        let mut rgb = Vec::new();
        for p in [[1, 2, 3], [1, 2, 3], [1, 2, 3], [9, 9, 9], [4, 5, 6], [7, 7, 7], [7, 7, 7], [7, 7, 7]] {
            rgb.extend_from_slice(&p);
        }
        let image = decode(&encode(4, 2, &rgb)).expect("decodes");
        assert_eq!((image.width, image.height), (4, 2));
        assert_eq!(image.pixels, rgb);
    }

    #[test]
    fn blue_first_becomes_red_first() {
        let image = decode(&sample(true)).expect("decodes");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(&image.pixels[..6], &[0, 0, 255, 0, 0, 255], "the run is blue, in RGB order");
        assert_eq!(&image.pixels[6..], &[255, 0, 0, 0, 255, 0], "then red and green");
    }

    #[test]
    fn a_bottom_up_image_is_turned_the_right_way_up() {
        let up = decode(&sample(false)).expect("decodes");
        let down = decode(&sample(true)).expect("decodes");
        assert_eq!(&up.pixels[..6], &down.pixels[6..], "its first row is the other's last");
    }

    #[test]
    fn icons_are_cut_out_by_row() {
        let image = decode(&sample(true)).unwrap();
        assert_eq!(rows(&image, 0, 1).unwrap(), vec![0, 0, 255, 0, 0, 255]);
        assert_eq!(rows(&image, 1, 1).unwrap(), vec![255, 0, 0, 0, 255, 0]);
        assert!(rows(&image, 1, 5).is_none(), "past the end is refused, not truncated");
    }

    #[test]
    fn anything_but_the_one_shape_a_bni_carries_is_declined() {
        let mut uncompressed = sample(true);
        uncompressed[2] = 2; // plain true-colour, not RLE
        assert!(decode(&uncompressed).is_none());
        assert!(decode(&[]).is_none());
    }
}
