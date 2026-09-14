//! DCC animations: the compressed, palette-indexed sprites Diablo II draws units with — one file
//! per body part, stance and weapon stance, holding every direction's frames.
//!
//! Written from "The Dcc File Format Description" by Bilian Belchev, with Paul Siramy's notes.
//! A direction is decoded on its own, from a bitstream read least significant bit first:
//!
//! ```text
//! u32 output size · 2 bits compression flags · 7 × 4-bit field widths (from a width table)
//! per frame: variable0, width, height, x offset (signed), y offset (signed),
//!            optional bytes, coded bytes, 1-bit bottom-up flag
//! [byte-aligned optional data] · 20-bit stream sizes · 256-bit key of the colours used
//! streams: equal cells, pixel masks, encoding types, raw pixel codes, codes and displacements
//! ```
//!
//! Frames are rebuilt inside the direction's bounding box, which is cut into cells of about 4×4
//! pixels. A first pass decodes up to four colours per cell (reusing the cell's previous colours
//! where its mask says so), a second pass paints each cell from those colours, or copies the cell
//! from the previous frame when the equal-cells stream says it did not change.
//!
//! One correction to the notes, found against the operator's files: a cell reads its
//! encoding-type bit only when its pixel mask is not zero. Read that way, every stream of every
//! character part is consumed to its last bit.

use std::fmt;

/// Why a DCC could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file or a direction ended early.
    Truncated,
    /// Not a DCC, or a size or count that cannot be.
    Corrupt,
    /// No such direction.
    NoDirection,
    /// A frame stored bottom-up, which no shipped file uses.
    BottomUp,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "DCC: truncated",
            Self::Corrupt => "DCC: corrupt",
            Self::NoDirection => "DCC: no such direction",
            Self::BottomUp => "DCC: bottom-up frames are not supported",
        })
    }
}

impl std::error::Error for Error {}

/// Field widths by 4-bit code.
const WIDTHS: [u32; 16] = [0, 1, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 26, 28, 30, 32];
/// Largest frame buffer the game accepts, in pixels.
const MAX_BUFFER: usize = 120_000;

/// A DCC file.
#[derive(Debug, Clone)]
pub struct Dcc<'a> {
    bytes: &'a [u8],
    directions: usize,
    frames: usize,
    offsets: Vec<usize>,
}

/// One direction's frames, all in the direction's bounding box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Direction {
    /// Left edge of the box, pixels right of the unit's base point.
    pub left: i32,
    /// Top edge of the box, pixels below the unit's base point.
    pub top: i32,
    /// Box width.
    pub width: usize,
    /// Box height.
    pub height: usize,
    /// Each frame: `width × height` palette indices, row by row; 0 is transparent.
    pub frames: Vec<Vec<u8>>,
}

impl<'a> Dcc<'a> {
    /// Read the header.
    ///
    /// # Errors
    ///
    /// [`Error`] if it is not a DCC.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 15 {
            return Err(Error::Truncated);
        }
        if bytes[0] != 0x74 {
            return Err(Error::Corrupt);
        }
        let directions = usize::from(bytes[2]);
        let frames = u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]) as usize;
        if directions == 0 || directions > 32 || frames > 256 {
            return Err(Error::Corrupt);
        }
        let table = bytes.get(15..15 + directions * 4).ok_or(Error::Truncated)?;
        let offsets: Vec<usize> =
            table.chunks_exact(4).map(|o| u32::from_le_bytes([o[0], o[1], o[2], o[3]]) as usize).collect();
        if offsets.iter().any(|&o| o > bytes.len()) {
            return Err(Error::Corrupt);
        }
        Ok(Self { bytes, directions, frames, offsets })
    }

    /// Directions in the file.
    #[must_use]
    pub fn directions(&self) -> usize {
        self.directions
    }

    /// Frames in each direction.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Decode one direction.
    ///
    /// # Errors
    ///
    /// [`Error`] if the direction's data is malformed.
    pub fn direction(&self, index: usize) -> Result<Direction, Error> {
        let start = *self.offsets.get(index).ok_or(Error::NoDirection)?;
        let end = self.offsets.get(index + 1).copied().unwrap_or(self.bytes.len());
        let data = self.bytes.get(start..end.max(start)).ok_or(Error::Truncated)?;
        decode_direction(data, self.frames)
    }
}

/// A least-significant-bit-first reader over part of a direction.
#[derive(Debug, Clone, Copy)]
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize, len: usize) -> Self {
        Self { data, pos, end: pos + len }
    }

    fn bits(&mut self, n: u32) -> Result<u32, Error> {
        let mut value = 0u64;
        for i in 0..n {
            if self.pos >= self.end {
                return Err(Error::Truncated);
            }
            let byte = *self.data.get(self.pos / 8).ok_or(Error::Truncated)?;
            value |= u64::from((byte >> (self.pos % 8)) & 1) << i;
            self.pos += 1;
        }
        Ok(value as u32)
    }

    fn signed(&mut self, n: u32) -> Result<i32, Error> {
        let v = self.bits(n)?;
        if n == 0 {
            return Ok(0);
        }
        if n < 32 && v & (1 << (n - 1)) != 0 {
            Ok((v | (!0u32 << n)) as i32)
        } else {
            Ok(v as i32)
        }
    }
}

/// A frame's header and where its cells are.
#[derive(Debug, Clone)]
struct FrameCells {
    /// The frame's box, in direction-box pixels.
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    cells: Vec<Cell>,
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    /// The frame-buffer cell it stands for.
    buffer: usize,
    /// Unchanged from the buffer cell's last frame (stage 1's equal-cells bit).
    equal: bool,
    /// Its pixel-buffer entry, when it has one.
    entry: usize,
}

/// Cell sizes along one axis for a frame starting `start` pixels into the buffer and `len` long.
fn cell_sizes(start: usize, len: usize) -> Vec<usize> {
    let first = 4 - start % 4;
    if len <= first + 1 {
        return vec![len];
    }
    let rest = len - first - 1;
    let mut count = 2 + rest / 4;
    if rest % 4 == 0 {
        count -= 1;
    }
    let mut sizes = vec![4; count];
    sizes[0] = first;
    sizes[count - 1] = len - first - 4 * (count - 2);
    sizes
}

fn decode_direction(data: &[u8], frame_count: usize) -> Result<Direction, Error> {
    let mut b = Bits::new(data, 0, data.len() * 8);
    let _output_size = b.bits(32)?;
    let flags = b.bits(2)?;
    let mut width_of = || -> Result<u32, Error> { Ok(WIDTHS[b.bits(4)? as usize]) };
    let widths = [width_of()?, width_of()?, width_of()?, width_of()?, width_of()?, width_of()?, width_of()?];
    let [variable0_bits, width_bits, height_bits, x_bits, y_bits, optional_bits, coded_bits] = widths;

    // Frame headers: (left, top, width, height) in pixels from the base point.
    let mut boxes = Vec::with_capacity(frame_count);
    let mut optional_total = 0usize;
    for _ in 0..frame_count {
        b.bits(variable0_bits)?;
        let width = b.bits(width_bits)? as usize;
        let height = b.bits(height_bits)? as usize;
        let x = b.signed(x_bits)?;
        let y = b.signed(y_bits)?;
        optional_total += b.bits(optional_bits)? as usize;
        b.bits(coded_bits)?;
        if b.bits(1)? == 1 {
            return Err(Error::BottomUp);
        }
        let height_i = i32::try_from(height).map_err(|_| Error::Corrupt)?;
        boxes.push((x, y - height_i + 1, width, height));
    }
    if optional_total > 0 {
        b.pos = b.pos.div_ceil(8) * 8 + optional_total * 8;
    }
    let equal_size = if flags & 2 != 0 { b.bits(20)? as usize } else { 0 };
    let mask_size = b.bits(20)? as usize;
    let (encoding_size, raw_size) = if flags & 1 != 0 { (b.bits(20)? as usize, b.bits(20)? as usize) } else { (0, 0) };
    let mut key = Vec::with_capacity(256);
    for colour in 0..=255u8 {
        if b.bits(1)? == 1 {
            key.push(colour);
        }
    }
    let total = data.len() * 8;
    let mut equal_stream = Bits::new(data, b.pos, equal_size);
    let mut mask_stream = Bits::new(data, b.pos + equal_size, mask_size);
    let mut encoding_stream = Bits::new(data, b.pos + equal_size + mask_size, encoding_size);
    let mut raw_stream = Bits::new(data, b.pos + equal_size + mask_size + encoding_size, raw_size);
    let codes_start = b.pos + equal_size + mask_size + encoding_size + raw_size;
    if codes_start > total {
        return Err(Error::Truncated);
    }
    let mut codes = Bits::new(data, codes_start, total - codes_start);

    // The direction's box.
    let (left, top, right, bottom) = boxes.iter().filter(|b| b.2 > 0 && b.3 > 0).fold(
        (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
        |(l, t, r, bt), &(x, y, w, h)| (l.min(x), t.min(y), r.max(x + w as i32), bt.max(y + h as i32)),
    );
    if left > right {
        return Ok(Direction { left: 0, top: 0, width: 0, height: 0, frames: vec![Vec::new(); frame_count] });
    }
    let width = (right - left) as usize;
    let height = (bottom - top) as usize;
    if width * height > MAX_BUFFER {
        return Err(Error::Corrupt);
    }
    let buffer_columns = 1 + (width - 1) / 4;
    let buffer_rows = 1 + (height - 1) / 4;

    let mut frames: Vec<FrameCells> = boxes
        .iter()
        .map(|&(x, y, w, h)| {
            let (fx, fy) = ((x - left) as usize, (y - top) as usize);
            let mut cells = Vec::new();
            if w > 0 && h > 0 {
                let (columns, rows) = (cell_sizes(fx, w), cell_sizes(fy, h));
                let mut cy = fy;
                for (j, &ch) in rows.iter().enumerate() {
                    let mut cx = fx;
                    for (i, &cw) in columns.iter().enumerate() {
                        let buffer = (fy / 4 + j) * buffer_columns + fx / 4 + i;
                        cells.push(Cell { x: cx, y: cy, w: cw, h: ch, buffer, equal: false, entry: usize::MAX });
                        cx += cw;
                    }
                    cy += ch;
                }
            }
            FrameCells { x: fx, y: fy, width: w, height: h, cells }
        })
        .collect();
    let buffer_cells = buffer_columns * buffer_rows;
    if frames.iter().flat_map(|f| &f.cells).any(|c| c.buffer >= buffer_cells) {
        return Err(Error::Corrupt);
    }

    // Stage 1: each cell's colours.
    let mut entries: Vec<[u8; 4]> = Vec::new();
    let mut last_entry: Vec<Option<usize>> = vec![None; buffer_cells];
    for frame in &mut frames {
        for cell in &mut frame.cells {
            let previous = last_entry[cell.buffer];
            let mut mask = 0xF;
            if previous.is_some() {
                if flags & 2 != 0 && equal_stream.bits(1)? == 1 {
                    cell.equal = true;
                    continue;
                }
                mask = mask_stream.bits(4)?;
            }
            // The encoding-type bit is only there for a cell with pixels to decode; the format
            // notes read it for every cell, which leaves the raw and type streams short.
            let raw = mask != 0 && flags & 1 != 0 && encoding_stream.bits(1)? == 1;
            let mut decoded = [0u32; 4];
            let mut count = 0;
            let mut last = 0u32;
            for _ in 0..mask.count_ones() {
                let pixel = if raw {
                    raw_stream.bits(8)?
                } else {
                    let mut p = last;
                    loop {
                        let step = codes.bits(4)?;
                        p += step;
                        if step != 15 {
                            break;
                        }
                    }
                    p
                };
                if pixel == last {
                    break;
                }
                last = pixel;
                decoded[count] = pixel;
                count += 1;
            }
            let old = previous.map_or([0; 4], |i| entries[i]);
            let mut entry = [0u8; 4];
            let mut taken = 0;
            for (k, slot) in entry.iter_mut().enumerate() {
                if mask & (1 << k) == 0 {
                    *slot = old[k];
                    continue;
                }
                if taken < count {
                    let code = decoded[count - 1 - taken] as usize;
                    *slot = *key.get(code).ok_or(Error::Corrupt)?;
                }
                taken += 1;
            }
            cell.entry = entries.len();
            last_entry[cell.buffer] = Some(entries.len());
            entries.push(entry);
        }
    }

    // Stage 2: paint the cells.
    let mut canvas = vec![0u8; width * height];
    let mut last_cell: Vec<Option<(usize, usize, usize, usize)>> = vec![None; buffer_cells];
    let mut out = Vec::with_capacity(frame_count);
    for frame in &frames {
        for cell in &frame.cells {
            match last_cell[cell.buffer] {
                Some((px, py, pw, ph)) if cell.equal => {
                    if (pw, ph) == (cell.w, cell.h) {
                        let mut copy = vec![0u8; pw * ph];
                        for row in 0..ph {
                            let from = (py + row) * width + px;
                            copy[row * pw..(row + 1) * pw].copy_from_slice(&canvas[from..from + pw]);
                        }
                        for row in 0..ph {
                            let to = (cell.y + row) * width + cell.x;
                            canvas[to..to + pw].copy_from_slice(&copy[row * pw..(row + 1) * pw]);
                        }
                    } else {
                        for row in 0..cell.h {
                            let to = (cell.y + row) * width + cell.x;
                            canvas[to..to + cell.w].fill(0);
                        }
                    }
                }
                _ => {
                    let entry = *entries.get(cell.entry).ok_or(Error::Corrupt)?;
                    let bits = if entry[0] == entry[1] {
                        0
                    } else if entry[1] == entry[2] {
                        1
                    } else {
                        2
                    };
                    for row in 0..cell.h {
                        for column in 0..cell.w {
                            let index = if bits == 0 { 0 } else { codes.bits(bits)? as usize };
                            canvas[(cell.y + row) * width + cell.x + column] = entry[index];
                        }
                    }
                }
            }
            last_cell[cell.buffer] = Some((cell.x, cell.y, cell.w, cell.h));
        }
        let mut pixels = vec![0u8; width * height];
        for row in frame.y..frame.y + frame.height {
            let at = row * width;
            pixels[at + frame.x..at + frame.x + frame.width].copy_from_slice(&canvas[at + frame.x..at + frame.x + frame.width]);
        }
        out.push(pixels);
    }
    Ok(Direction { left, top, width, height, frames: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits least significant first.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        len: usize,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, n: u32) {
            for i in 0..n {
                if self.len % 8 == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= (((value >> i) & 1) as u8) << (self.len % 8);
                self.len += 1;
            }
        }
    }

    #[test]
    fn cells_are_four_wide_with_no_one_pixel_edges() {
        assert_eq!(cell_sizes(0, 9), [4, 5], "a last 1-pixel column joins its neighbour");
        assert_eq!(cell_sizes(0, 10), [4, 4, 2]);
        assert_eq!(cell_sizes(0, 5), [5]);
        assert_eq!(cell_sizes(2, 5), [2, 3], "the first cell ends on the buffer's grid");
        assert_eq!(cell_sizes(3, 2), [2], "a frame one pixel past a cell edge is one cell");
        assert_eq!(cell_sizes(1, 3), [3]);
    }

    #[test]
    fn signed_fields_extend_their_top_bit() {
        let data = [0b0000_0111u8, 0];
        let mut b = Bits::new(&data, 0, 16);
        assert_eq!(b.signed(3).unwrap(), -1);
        let mut b = Bits::new(&data, 0, 16);
        assert_eq!(b.signed(1).unwrap(), -1, "a 1-bit offset can only be 0 or -1");
        assert_eq!(b.bits(2).unwrap(), 3);
    }

    /// A one-direction, one-frame 2×2 image with colours 0 (transparent) and 9.
    #[test]
    fn a_tiny_frame_decodes() {
        let mut w = BitWriter::default();
        w.put(0, 32); // output size
        w.put(0, 2); // no optional streams
        for code in [0, 2, 2, 2, 2, 0, 0] {
            w.put(code, 4); // variable0 0 bits; width/height/x/y 2 bits; no optional/coded bits
        }
        w.put(2, 2); // width
        w.put(2, 2); // height
        w.put(0, 2); // x
        w.put(1, 2); // y: bottom row at 1, so top at 0
        w.put(0, 1); // top-down
        w.put(0, 20); // pixel mask stream: unused (first frame)
        for colour in 0..256 {
            w.put(u32::from(colour == 0 || colour == 9), 1);
        }
        // Stage 1, one cell (mask 0xF): codes 1 then a repeat to stop.
        w.put(1, 4); // 0 + 1 = code 1 (colour 9)
        w.put(0, 4); // 1 + 0 = repeat: stop
        // Stage 2: entry [9, 0, 0, 0] needs 1 bit per pixel: 0 → 9, 1 → 0.
        for bit in [0, 1, 1, 0] {
            w.put(bit, 1);
        }
        let mut file = vec![0x74, 6, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        file.extend_from_slice(&19u32.to_le_bytes());
        file.extend_from_slice(&w.bytes);
        let dcc = Dcc::parse(&file).unwrap();
        let d = dcc.direction(0).unwrap();
        assert_eq!((d.left, d.top, d.width, d.height), (0, 0, 2, 2));
        assert_eq!(d.frames[0], [9, 0, 0, 9]);
    }
}
