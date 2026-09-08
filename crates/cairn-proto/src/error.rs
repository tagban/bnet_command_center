//! Protocol errors.
//!
//! Every one of these is a **connection-scoped** event: log it, close that connection,
//! carry on. None is ever a reason to disturb another session, and none may panic.
//! PvPGN accumulated thirty-plus "crash on malformed packet" fix commits over fifteen
//! years precisely because its parse failures were not modelled as values.

use std::fmt;

/// A protocol-level failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    /// First byte of a BNCS frame was not `0xFF`.
    BadMagic(u8),
    /// Declared frame length was smaller than the header itself.
    ShortFrame {
        /// Declared length.
        len: usize,
        /// Header size that length must at least cover.
        header: usize,
    },
    /// Declared frame length exceeded the configured maximum.
    FrameTooLarge {
        /// Declared length.
        len: usize,
        /// Configured ceiling.
        max: usize,
    },
    /// Ran out of bytes while reading a field.
    Truncated {
        /// Bytes the read required.
        needed: usize,
        /// Bytes actually left.
        available: usize,
    },
    /// A NUL-terminated string ran to the end of the buffer without a terminator.
    UnterminatedString,
    /// A string field exceeded the protocol's documented limit.
    StringTooLong {
        /// Actual length.
        len: usize,
        /// Documented ceiling.
        limit: usize,
    },
    /// A line-oriented protocol sent a line past the configured ceiling.
    ///
    /// An unbounded line buffer on an unauthenticated socket is a memory-exhaustion
    /// primitive, so this is enforced, not advisory.
    LineTooLong(usize),
    /// A packet arrived that the state machine does not accept in its current state.
    UnexpectedPacket {
        /// Packet identifier.
        id: u8,
        /// Session state name.
        state: &'static str,
    },
    /// A field held a value outside its documented domain.
    InvalidValue {
        /// Field name.
        field: &'static str,
        /// Offending value, rendered.
        value: String,
    },
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(b) => write!(f, "bad BNCS magic: expected 0xFF, got {b:#04x}"),
            Self::ShortFrame { len, header } => {
                write!(f, "declared frame length {len} is shorter than the {header}-byte header")
            }
            Self::FrameTooLarge { len, max } => {
                write!(f, "frame length {len} exceeds maximum {max}")
            }
            Self::Truncated { needed, available } => {
                write!(f, "truncated field: needed {needed} more bytes, {available} available")
            }
            Self::UnterminatedString => write!(f, "unterminated string"),
            Self::StringTooLong { len, limit } => {
                write!(f, "string field too long: {len} bytes, limit {limit}")
            }
            Self::LineTooLong(max) => write!(f, "line too long: exceeded {max} bytes"),
            Self::UnexpectedPacket { id, state } => {
                write!(f, "unexpected packet {id:#04x} in state {state}")
            }
            Self::InvalidValue { field, value } => {
                write!(f, "invalid value for {field}: {value}")
            }
        }
    }
}

impl std::error::Error for ProtoError {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, ProtoError>;

/// A four-character code (`STAR`, `W2BN`, `IX86`, …).
///
/// Stored as the value in human reading order. Because all Battle.net numerics are
/// little-endian, the ASCII appears **reversed** in the byte stream: `STAR` is
/// `52 41 54 53`. [`crate::buf::Reader::fourcc`] and [`crate::buf::Writer::fourcc`]
/// handle that, so nothing outside this module needs to think about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCc(pub u32);

impl FourCc {
    /// Build from four ASCII bytes in human reading order.
    #[must_use]
    pub const fn from_ascii(s: &[u8; 4]) -> Self {
        Self(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// The four ASCII characters in human reading order.
    #[must_use]
    pub const fn as_ascii(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.as_ascii();
        if b.iter().all(u8::is_ascii_graphic) {
            write!(f, "{}", String::from_utf8_lossy(&b))
        } else {
            write!(f, "{:#010x}", self.0)
        }
    }
}
