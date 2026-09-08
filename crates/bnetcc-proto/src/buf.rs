//! Buffers, and checked readers and writers for Battle.net wire types.
//!
//! Every read is bounds-checked and returns a `Result`. There is no way to express an
//! unchecked read with this API, which is the whole point: the bug class that dominated
//! PvPGN's fifteen-year history is hand-rolled pointer arithmetic over attacker-supplied
//! bytes.

use crate::error::{FourCc, ProtoError, Result};

/// A growable receive buffer with a read cursor.
///
/// Bytes are appended at the tail and consumed from the head; the head offset is
/// reclaimed by compaction rather than by memmoving on every read, so draining N frames
/// from one socket read is O(total bytes), not O(N × remaining).
///
/// This exists instead of `bytes::BytesMut` to keep the crate dependency-free — see the
/// note in `Cargo.toml`. Swapping to `BytesMut` later is a mechanical change and would
/// buy zero-copy frame bodies; at classic-Battle.net packet sizes (a chat line is 224
/// bytes) that is not currently worth a dependency.
#[derive(Debug, Default, Clone)]
pub struct RecvBuf {
    buf: Vec<u8>,
    head: usize,
}

impl RecvBuf {
    /// Empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
        }
    }

    /// Empty buffer with a capacity hint.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
            head: 0,
        }
    }

    /// The unread bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[self.head..]
    }

    /// Number of unread bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len() - self.head
    }

    /// True if nothing is unread.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total allocated capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Append bytes to the tail.
    pub fn extend_from_slice(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Mark `n` bytes as read.
    ///
    /// Compacts once the consumed prefix is at least half the buffer, which bounds
    /// wasted space at 2× the live bytes without memmoving on every frame.
    pub fn consume(&mut self, n: usize) {
        self.head = (self.head + n).min(self.buf.len());
        if self.head >= self.buf.len() {
            self.buf.clear();
            self.head = 0;
        } else if self.head >= self.buf.len() / 2 && self.head >= 512 {
            self.buf.drain(..self.head);
            self.head = 0;
        }
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
    }

    /// Ensure at least `n` writable bytes are available and return the writable tail.
    ///
    /// Call [`Self::commit`] with the number of bytes actually written. This is the
    /// read-into-buffer path for a socket.
    pub fn writable_tail(&mut self, n: usize) -> &mut [u8] {
        if self.head > 0 && self.head == self.buf.len() {
            self.buf.clear();
            self.head = 0;
        }
        let start = self.buf.len();
        self.buf.resize(start + n, 0);
        &mut self.buf[start..]
    }

    /// Commit `written` bytes previously written into [`Self::writable_tail`].
    pub fn commit(&mut self, written: usize, requested: usize) {
        debug_assert!(written <= requested);
        let excess = requested - written;
        self.buf.truncate(self.buf.len() - excess);
    }

    /// A checked reader over the unread bytes.
    #[must_use]
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(self.as_slice())
    }
}

/// A cursor over a packet body that reads Battle.net wire types.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wrap a packet body.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// True if every byte has been consumed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(ProtoError::Truncated {
                needed: n,
                available: self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Read a `u8`.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Read a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a little-endian `i32` (used for time-zone bias).
    pub fn i32(&mut self) -> Result<i32> {
        self.u32().map(|v| v as i32)
    }

    /// Read a little-endian `u64`, also used for `FILETIME`.
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a four-character code, undoing the little-endian reversal.
    pub fn fourcc(&mut self) -> Result<FourCc> {
        let b = self.take(4)?;
        Ok(FourCc(u32::from_be_bytes([b[3], b[2], b[1], b[0]])))
    }

    /// Read `n` raw bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    /// Read a fixed-size byte array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// Read a NUL-terminated string as raw bytes, excluding the terminator.
    ///
    /// Returns bytes rather than `&str` deliberately: the encoding is product-dependent
    /// (UTF-8 for `STAR`/`SEXP`/`SSHR`/`JSTR`, ISO 8859-1 otherwise), so decoding is the
    /// caller's decision. `limit` enforces the protocol's documented ceiling.
    pub fn cstr(&mut self, limit: usize) -> Result<&'a [u8]> {
        let rest = &self.buf[self.pos..];
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(ProtoError::UnterminatedString)?;
        if end > limit {
            return Err(ProtoError::StringTooLong { len: end, limit });
        }
        self.pos += end + 1;
        Ok(&rest[..end])
    }

    /// Consume and return the rest of the body.
    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.buf[self.pos..];
        self.pos = self.buf.len();
        out
    }
}

/// Builder for a packet body.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// New empty body.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(64),
        }
    }

    /// New empty body with a capacity hint.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
        }
    }

    /// Append a `u8`.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// Append a little-endian `u16`.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append a little-endian `u32`.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append a little-endian `u64`.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append a four-character code in wire (reversed) order.
    pub fn fourcc(&mut self, v: FourCc) -> &mut Self {
        let b = v.as_ascii();
        self.buf.extend_from_slice(&[b[3], b[2], b[1], b[0]]);
        self
    }

    /// Append raw bytes.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    /// Append a NUL-terminated string.
    ///
    /// Interior NUL bytes are stripped rather than rejected. They can only arrive from a
    /// peer that already violated the protocol, and truncating one user's nickname is a
    /// better outcome than failing a whole channel broadcast.
    pub fn cstr(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend(v.iter().copied().filter(|&b| b != 0));
        self.buf.push(0);
        self
    }

    /// Current body length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True if nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Finish, yielding the body bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounds_checked() {
        let mut r = Reader::new(&[1, 2]);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 2);
        assert!(matches!(
            r.u8(),
            Err(ProtoError::Truncated {
                needed: 1,
                available: 0
            })
        ));
    }

    #[test]
    fn truncated_u32_does_not_advance_the_cursor() {
        let mut r = Reader::new(&[0xAA, 0xBB]);
        assert!(r.u32().is_err());
        assert_eq!(r.remaining(), 2, "a failed read must not consume");
    }

    #[test]
    fn fourcc_roundtrips_through_wire_order() {
        let star = FourCc::from_ascii(b"STAR");
        let mut w = Writer::new();
        w.fourcc(star);
        let body = w.finish();
        assert_eq!(&body[..], &[0x52, 0x41, 0x54, 0x53], "wire order is reversed");
        assert_eq!(Reader::new(&body).fourcc().unwrap(), star);
        assert_eq!(star.to_string(), "STAR");
    }

    #[test]
    fn cstr_requires_a_terminator() {
        let mut r = Reader::new(b"abc");
        assert!(matches!(r.cstr(64), Err(ProtoError::UnterminatedString)));
    }

    #[test]
    fn cstr_enforces_the_limit() {
        let mut r = Reader::new(b"abcdefghij\0");
        assert!(matches!(
            r.cstr(4),
            Err(ProtoError::StringTooLong { len: 10, limit: 4 })
        ));
    }

    #[test]
    fn cstr_reads_and_advances() {
        let mut r = Reader::new(b"Zealot\0rest");
        assert_eq!(r.cstr(15).unwrap(), b"Zealot");
        assert_eq!(r.rest(), b"rest");
    }

    #[test]
    fn writer_strips_interior_nuls() {
        let mut w = Writer::new();
        w.cstr(b"ab\0cd");
        assert_eq!(w.finish(), b"abcd\0");
    }

    #[test]
    fn empty_cstr_is_a_single_nul() {
        let mut w = Writer::new();
        w.cstr(b"");
        assert_eq!(w.finish(), b"\0");
    }

    #[test]
    fn recvbuf_consumes_and_compacts() {
        let mut b = RecvBuf::new();
        b.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(b.len(), 5);
        b.consume(2);
        assert_eq!(b.as_slice(), &[3, 4, 5]);
        b.consume(3);
        assert!(b.is_empty());
        // Fully drained buffers reset the head so capacity is reused.
        b.extend_from_slice(&[9]);
        assert_eq!(b.as_slice(), &[9]);
    }

    #[test]
    fn recvbuf_does_not_grow_without_bound_under_streaming() {
        // Simulates a long-lived chat connection: append a frame, consume it, forever.
        // Without compaction the Vec would grow linearly and never shrink.
        let mut b = RecvBuf::new();
        for _ in 0..10_000 {
            b.extend_from_slice(&[0u8; 64]);
            b.consume(64);
        }
        assert!(b.is_empty());
        assert!(
            b.capacity() < 8192,
            "capacity grew to {} — compaction is not working",
            b.capacity()
        );
    }

    #[test]
    fn recvbuf_writable_tail_and_commit() {
        let mut b = RecvBuf::new();
        let want = 128;
        let tail = b.writable_tail(want);
        tail[..3].copy_from_slice(&[7, 8, 9]);
        b.commit(3, want);
        assert_eq!(b.as_slice(), &[7, 8, 9]);
    }
}
