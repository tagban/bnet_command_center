//! Line framing for the telnet / chat gateway (protocol bytes `0x03`, `0x43`, `0x63`).
//!
//! Deliberately a *buffered line* decoder rather than a byte-at-a-time reader. PvPGN
//! reads its text protocols one character per event-loop pass — `packet_set_size(packet,
//! 1)`, then "no end of line, get another char" — so a 200-character line costs 200
//! `recv()` syscalls and 200 full trips through the loop. On a warnet, where the gateway
//! is the primary interface rather than an afterthought, that is the difference between
//! working and not.

use crate::buf::RecvBuf;
use crate::error::{ProtoError, Result};

/// Default ceiling on one line.
///
/// An unbounded line buffer on an unauthenticated socket is a memory-exhaustion
/// primitive, so this is enforced rather than advisory.
pub const DEFAULT_MAX_LINE: usize = 1024;

/// Decode one CRLF- or LF-terminated line from `src`.
///
/// Returns `Ok(None)` if no complete line is buffered yet. Empty lines decode to empty
/// vectors; the caller decides whether to ignore them (it should).
///
/// # Errors
///
/// [`ProtoError::LineTooLong`] once the buffer exceeds `max_line` without a terminator.
/// Fatal to the connection.
pub fn decode_line(src: &mut RecvBuf, max_line: usize) -> Result<Option<Vec<u8>>> {
    let buf = src.as_slice();
    if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
        if idx > max_line {
            src.clear();
            return Err(ProtoError::LineTooLong(max_line));
        }
        let mut end = idx;
        if end > 0 && buf[end - 1] == b'\r' {
            end -= 1;
        }
        let line = buf[..end].to_vec();
        src.consume(idx + 1);
        return Ok(Some(line));
    }
    if buf.len() > max_line {
        // No terminator in sight and already over budget: this peer is not going to
        // send one. Release the bytes rather than holding them while we unwind.
        src.clear();
        return Err(ProtoError::LineTooLong(max_line));
    }
    Ok(None)
}

/// Append a CRLF-terminated line to `dst`.
pub fn encode_line(line: &[u8], dst: &mut Vec<u8>) {
    dst.reserve(line.len() + 2);
    dst.extend_from_slice(line);
    dst.extend_from_slice(b"\r\n");
}

/// Format a gateway message: a four-digit zero-padded id, a name, and optional data.
///
/// The gateway's wire format is `<4-digit-id> <NAME> [data]`, CRLF-terminated, with user
/// flags rendered as four-character zero-padded hex.
#[must_use]
pub fn gateway_message(id: u16, name: &str, data: Option<&str>) -> String {
    match data {
        Some(d) => format!("{id:04} {name} {d}"),
        None => format!("{id:04} {name}"),
    }
}

/// Render user flags the way the gateway does: four zero-padded hex digits.
#[must_use]
pub fn gateway_flags(flags: u32) -> String {
    format!("{flags:04X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_of(bytes: &[u8]) -> RecvBuf {
        let mut b = RecvBuf::new();
        b.extend_from_slice(bytes);
        b
    }

    #[test]
    fn decodes_crlf_lines() {
        let mut buf = buf_of(b"hello\r\nworld\r\n");
        assert_eq!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().unwrap(), b"hello");
        assert_eq!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().unwrap(), b"world");
        assert_eq!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap(), None);
    }

    #[test]
    fn decodes_bare_lf_lines() {
        // Bots written against a Unix stack routinely send bare LF.
        let mut buf = buf_of(b"login bot\n");
        assert_eq!(
            decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().unwrap(),
            b"login bot"
        );
    }

    #[test]
    fn waits_for_a_partial_line() {
        let mut buf = buf_of(b"part");
        assert_eq!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap(), None);
        assert_eq!(buf.len(), 4);
    }

    #[test]
    fn empty_lines_decode_to_empty() {
        let mut buf = buf_of(b"\r\n\n");
        assert!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().unwrap().is_empty());
        assert!(decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().unwrap().is_empty());
    }

    #[test]
    fn rejects_an_overlong_line_and_frees_the_buffer() {
        let mut buf = buf_of(&vec![b'x'; 2048]);
        assert!(matches!(
            decode_line(&mut buf, 1024),
            Err(ProtoError::LineTooLong(1024))
        ));
        assert!(buf.is_empty(), "buffered bytes must be released");
    }

    #[test]
    fn a_whole_batch_decodes_in_one_pass() {
        // The property that matters for a warnet: N lines arriving together cost one
        // decode loop, not N event-loop wakeups.
        let mut wire = Vec::new();
        for i in 0..500 {
            encode_line(format!("line {i}").as_bytes(), &mut wire);
        }
        let mut buf = buf_of(&wire);
        let mut count = 0;
        while decode_line(&mut buf, DEFAULT_MAX_LINE).unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 500);
        assert!(buf.is_empty());
    }

    #[test]
    fn gateway_message_pads_the_id() {
        assert_eq!(gateway_message(1001, "ZEALOT", None), "1001 ZEALOT");
        assert_eq!(gateway_message(7, "X", Some("hi")), "0007 X hi");
    }

    #[test]
    fn gateway_flags_are_four_hex_digits() {
        assert_eq!(gateway_flags(0x02), "0002");
        assert_eq!(gateway_flags(0x10), "0010");
    }

    #[test]
    fn line_decoder_never_panics_on_arbitrary_input() {
        let mut seed = 0xABCD_1234u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let n = (next() % 128) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let mut buf = buf_of(&bytes);
            for _ in 0..8 {
                match decode_line(&mut buf, DEFAULT_MAX_LINE) {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
}
