//! PKWARE Data Compression Library "implode" decoder — how Diablo II compressed MPQ members.
//!
//! A stream is a two-byte header (whether literals are Huffman-coded; log2 of the dictionary
//! size) and then an LSB-first bit stream of literals and (length, distance) pairs. The three
//! Huffman tables are fixed by the format; the run lists below are them, in the compact
//! `(repeat - 1) << 4 | bit length` form of Mark Adler's `blast.c`.
//!
//! Ported from `jaenster/libd2` `packages/formats/src/pkware.zig` (MIT, © 2026 jaenster).

use std::fmt;

/// Why a stream could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The two-byte header is not one the format allows.
    BadHeader,
    /// The input ended mid-stream.
    Truncated,
    /// A bit sequence matched no code.
    BadCode,
    /// A back-reference points before the start of the output.
    DistanceTooFar,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadHeader => "PKWARE: bad header",
            Self::Truncated => "PKWARE: truncated stream",
            Self::BadCode => "PKWARE: invalid code",
            Self::DistanceTooFar => "PKWARE: back-reference before start of output",
        })
    }
}

impl std::error::Error for Error {}

const MAX_BITS: usize = 13;

/// Bit lengths of the 256 literal codes (used when the header says literals are coded).
const LIT_LEN: [u8; 98] = [
    11, 124, 8, 7, 28, 7, 188, 13, 76, 4, 10, 8, 12, 10, 12, 10, 8, 23, 8, 9, 7, 6, 7, 8, 7, 6, 55,
    8, 23, 24, 12, 11, 7, 9, 11, 12, 6, 7, 22, 5, 7, 24, 6, 11, 9, 6, 7, 22, 7, 11, 38, 7, 9, 8, 25,
    11, 8, 11, 9, 12, 8, 12, 5, 38, 5, 38, 5, 11, 7, 5, 6, 21, 6, 10, 53, 8, 7, 24, 10, 27, 44, 253,
    253, 253, 252, 252, 252, 13, 12, 45, 12, 45, 12, 61, 12, 45, 44, 173,
];
/// Bit lengths of the 16 length codes.
const LEN_LEN: [u8; 6] = [2, 35, 36, 53, 38, 23];
/// Bit lengths of the 64 distance codes.
const DIST_LEN: [u8; 7] = [2, 20, 53, 230, 247, 151, 248];

/// Base copy length per length code, and its extra bits. Code 15 with all eight extra bits set
/// is 519: the end of the stream.
const LEN_BASE: [u16; 16] = [3, 2, 4, 5, 6, 7, 8, 9, 10, 12, 16, 24, 40, 72, 136, 264];
const LEN_EXTRA: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
const END_OF_STREAM: u32 = 519;

struct Huffman {
    /// Codes of each bit length.
    count: [u16; MAX_BITS + 1],
    /// Symbols in canonical order: by code length, then symbol value.
    symbol: [u16; 256],
}

/// Expand a run list into a canonical decoding table.
const fn construct(rep: &[u8]) -> Huffman {
    let mut lengths = [0u8; 256];
    let mut n = 0;
    let mut r = 0;
    while r < rep.len() {
        let mut left = (rep[r] >> 4) as usize + 1;
        while left > 0 {
            lengths[n] = rep[r] & 15;
            n += 1;
            left -= 1;
        }
        r += 1;
    }
    let mut h = Huffman { count: [0; MAX_BITS + 1], symbol: [0; 256] };
    let mut i = 0;
    while i < n {
        h.count[lengths[i] as usize] += 1;
        i += 1;
    }
    let mut offs = [0u16; MAX_BITS + 2];
    let mut len = 1;
    while len <= MAX_BITS {
        offs[len + 1] = offs[len] + h.count[len];
        len += 1;
    }
    let mut sym = 0;
    while sym < n {
        let l = lengths[sym] as usize;
        if l != 0 {
            h.symbol[offs[l] as usize] = sym as u16;
            offs[l] += 1;
        }
        sym += 1;
    }
    h
}

static LIT_CODE: Huffman = construct(&LIT_LEN);
static LENGTH_CODE: Huffman = construct(&LEN_LEN);
static DIST_CODE: Huffman = construct(&DIST_LEN);

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl Bits<'_> {
    fn take(&mut self, need: u32) -> Result<u32, Error> {
        if need == 0 {
            return Ok(0);
        }
        while self.cnt < need {
            let &b = self.src.get(self.pos).ok_or(Error::Truncated)?;
            self.buf |= u32::from(b) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
        let v = self.buf & ((1 << need) - 1);
        self.buf >>= need;
        self.cnt -= need;
        Ok(v)
    }

    /// Codes arrive one bit at a time, most significant first, each bit inverted.
    fn decode(&mut self, h: &Huffman) -> Result<u16, Error> {
        let (mut code, mut first, mut index) = (0u32, 0u32, 0u32);
        for len in 1..=MAX_BITS {
            code |= self.take(1)? ^ 1;
            let count = u32::from(h.count[len]);
            if code < first + count {
                return Ok(h.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::BadCode)
    }
}

/// Decode `src` into `dst`, returning the bytes written. Stops at the end-of-stream code or when
/// `dst` is full: an MPQ sector knows its unpacked size, and some members stop short of the code.
///
/// # Errors
///
/// [`Error`] for a malformed stream.
pub fn explode(src: &[u8], dst: &mut [u8]) -> Result<usize, Error> {
    let mut bits = Bits { src, pos: 0, buf: 0, cnt: 0 };
    let coded_literals = bits.take(8)?;
    if coded_literals > 1 {
        return Err(Error::BadHeader);
    }
    let dict = bits.take(8)?;
    if !(4..=6).contains(&dict) {
        return Err(Error::BadHeader);
    }
    let mut n = 0;
    while n < dst.len() {
        if bits.take(1)? != 0 {
            let sym = usize::from(bits.decode(&LENGTH_CODE)?);
            let len = u32::from(LEN_BASE[sym]) + bits.take(u32::from(LEN_EXTRA[sym]))?;
            if len == END_OF_STREAM {
                break;
            }
            // A two-byte match carries two low distance bits; longer ones a dictionary's worth.
            let low = if len == 2 { 2 } else { dict };
            let dist = ((usize::from(bits.decode(&DIST_CODE)?) << low) | bits.take(low)? as usize) + 1;
            if dist > n {
                return Err(Error::DistanceTooFar);
            }
            for _ in 0..(len as usize).min(dst.len() - n) {
                dst[n] = dst[n - dist];
                n += 1;
            }
        } else {
            dst[n] = if coded_literals != 0 { bits.decode(&LIT_CODE)? as u8 } else { bits.take(8)? as u8 };
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte as an uncoded literal, then the end code: valid, maximally unhelpful.
    fn implode_literals(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8, 4];
        let mut bit = 0usize;
        let mut put = |out: &mut Vec<u8>, value: u32, n: u32| {
            for i in 0..n {
                if bit % 8 == 0 {
                    out.push(0);
                }
                if value >> i & 1 != 0 {
                    *out.last_mut().unwrap() |= 1 << (bit % 8);
                }
                bit += 1;
            }
        };
        for &c in data {
            put(&mut out, 0, 1);
            put(&mut out, u32::from(c), 8);
        }
        // Length symbol 15 is seven set code bits, sent inverted: seven zeros. 264 + 255 = 519.
        put(&mut out, 1, 1);
        put(&mut out, 0, 7);
        put(&mut out, 0xFF, 8);
        out
    }

    #[test]
    fn the_blast_reference_stream() {
        // Mark Adler's blast.c example: uncoded literals, 1024-byte dictionary, "AI" + a copy.
        let src = [0x00, 0x04, 0x82, 0x24, 0x25, 0x8f, 0x80, 0x7f];
        let mut dst = [0u8; 13];
        assert_eq!(explode(&src, &mut dst), Ok(13));
        assert_eq!(&dst, b"AIAIAIAIAIAIA");
    }

    #[test]
    fn literals_round_trip_and_the_end_code_stops_early() {
        let text = b"Hello, Diablo II!";
        let src = implode_literals(text);
        let mut dst = [0u8; 64];
        assert_eq!(explode(&src, &mut dst), Ok(text.len()));
        assert_eq!(&dst[..text.len()], text);
    }

    #[test]
    fn headers_it_cannot_be_are_refused() {
        let mut dst = [0u8; 4];
        assert_eq!(explode(&[0x02, 0x04, 0x00], &mut dst), Err(Error::BadHeader));
        assert_eq!(explode(&[0x00, 0x07, 0x00], &mut dst), Err(Error::BadHeader));
        assert_eq!(explode(&[0x00], &mut dst), Err(Error::Truncated));
    }

    #[test]
    fn the_static_tables_expand_to_their_alphabets() {
        assert_eq!(LENGTH_CODE.count, [0, 0, 1, 3, 3, 4, 3, 2, 0, 0, 0, 0, 0, 0]);
        assert_eq!(DIST_CODE.count, [0, 0, 1, 0, 2, 4, 15, 26, 16, 0, 0, 0, 0, 0]);
        assert_eq!(LIT_CODE.count, [0, 0, 0, 0, 1, 11, 20, 21, 16, 7, 5, 10, 91, 74]);
        assert_eq!(&LIT_CODE.symbol[..12], &[32, 69, 97, 101, 105, 108, 110, 111, 114, 115, 116, 117]);
    }
}
