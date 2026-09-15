//! Classic Battle.net wire protocol.
//!
//! Framing, wire-type readers/writers, and packet definitions — **no policy, no I/O and
//! no dependencies**. Everything here is a pure transformation between bytes and values,
//! which is what makes the decoders cheap to fuzz and lets the domain logic in
//! `bnetcc-core` be tested without a runtime.
//!
//! The socket glue (reading into a [`buf::RecvBuf`], draining with
//! [`bncs::decode_frame`] until it yields `None`, writing with [`bncs::encode_frame`])
//! lives in `bnetccd`, where the async runtime already is.
//!
//! Four incompatible framings live in this protocol family and mixing them up is the
//! classic implementation bug:
//!
//! ```text
//! BNCS   FF | id:u8 | len:u16le      len includes the header
//! MCP         len:u16le | id:u8      no magic byte, length FIRST
//! W3GS   F7 | id:u8 | len:u16le      len includes the header
//! Chat   line-oriented, CRLF
//! ```
//!
//! BNCS, MCP and the chat gateway are implemented; W3GS follows in a later phase.
//! See `docs/PROTOCOL-NOTES.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bncs;
pub mod bnftp;
pub mod bni;
pub mod buf;
pub mod chat;
pub mod d2;
pub mod d2gs;
pub mod error;
pub mod line;
pub mod mcp;
pub mod statstring;
pub mod w3general;

pub use error::{FourCc, ProtoError, Result};

/// Product identifiers, as sent in `SID_AUTH_INFO`.
pub mod product {
    use crate::error::FourCc;

    /// StarCraft.
    pub const STAR: FourCc = FourCc::from_ascii(b"STAR");
    /// StarCraft: Brood War.
    pub const SEXP: FourCc = FourCc::from_ascii(b"SEXP");
    /// StarCraft Shareware.
    pub const SSHR: FourCc = FourCc::from_ascii(b"SSHR");
    /// StarCraft Japanese.
    pub const JSTR: FourCc = FourCc::from_ascii(b"JSTR");
    /// Diablo (retail).
    pub const DRTL: FourCc = FourCc::from_ascii(b"DRTL");
    /// Diablo Shareware.
    pub const DSHR: FourCc = FourCc::from_ascii(b"DSHR");
    /// Warcraft II: Battle.net Edition.
    pub const W2BN: FourCc = FourCc::from_ascii(b"W2BN");
    /// Diablo II.
    pub const D2DV: FourCc = FourCc::from_ascii(b"D2DV");
    /// Diablo II: Lord of Destruction.
    pub const D2XP: FourCc = FourCc::from_ascii(b"D2XP");
    /// Warcraft III: Reign of Chaos.
    pub const WAR3: FourCc = FourCc::from_ascii(b"WAR3");
    /// Warcraft III: The Frozen Throne.
    pub const W3XP: FourCc = FourCc::from_ascii(b"W3XP");

    /// Which authentication family a product uses.
    ///
    /// This split is not cosmetic — it decides whether a federation node can verify a
    /// login at the edge or must proxy it to the hub. See `docs/FEDERATION.md` §4.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AuthFamily {
        /// `SID_LOGONRESPONSE` (0x29) — Diablo I, Warcraft II BNE, shareware StarCraft.
        LegacyXsha1,
        /// `SID_LOGONRESPONSE2` (0x3A) — StarCraft, Brood War, Diablo II.
        Xsha1,
        /// `SID_AUTH_ACCOUNTLOGON` (0x53/0x54) — Warcraft III.
        Srp,
    }

    /// Classify a product's authentication family.
    ///
    /// Returns `None` for products Command Center does not recognise; the caller should refuse
    /// the logon rather than guessing.
    #[must_use]
    pub fn auth_family(p: FourCc) -> Option<AuthFamily> {
        match p {
            STAR | SEXP | D2DV | D2XP => Some(AuthFamily::Xsha1),
            SSHR | JSTR | DRTL | DSHR | W2BN => Some(AuthFamily::LegacyXsha1),
            WAR3 | W3XP => Some(AuthFamily::Srp),
            _ => None,
        }
    }

    /// Whether the server should never answer UDP for this product.
    ///
    /// `DRTL`/`DSHR` (Diablo, Diablo Shareware) are kept here provisionally — we have no
    /// live client to test them and Diablo's game model differs.
    ///
    /// `W2BN` is deliberately **not** listed: a real Warcraft II BNE client (captured
    /// 2026-09-09) binds UDP `:6112`, shows a UDP warning, and greys Create/Join until the
    /// server's `PKT_SERVERPING` completes the round trip. The earlier "W2BN always
    /// No-UDP" note was a misreading; enabling games for it is a stated product goal.
    #[must_use]
    pub fn always_no_udp(p: FourCc) -> bool {
        matches!(p, DRTL | DSHR)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn auth_families_are_classified() {
            assert_eq!(auth_family(STAR), Some(AuthFamily::Xsha1));
            assert_eq!(auth_family(SEXP), Some(AuthFamily::Xsha1));
            assert_eq!(auth_family(W2BN), Some(AuthFamily::LegacyXsha1));
            assert_eq!(auth_family(WAR3), Some(AuthFamily::Srp));
            assert_eq!(auth_family(W3XP), Some(AuthFamily::Srp));
            assert_eq!(auth_family(FourCc::from_ascii(b"XXXX")), None);
        }

        #[test]
        fn no_udp_products() {
            // W2BN must NOT be here: it runs the UDP check and needs PKT_SERVERPING to
            // enable game hosting (confirmed against a real client, 2026-09-09).
            assert!(!always_no_udp(W2BN));
            assert!(always_no_udp(DRTL));
            assert!(always_no_udp(DSHR));
            assert!(!always_no_udp(STAR));
        }

        #[test]
        fn product_codes_render_readably() {
            assert_eq!(STAR.to_string(), "STAR");
            assert_eq!(W3XP.to_string(), "W3XP");
        }
    }
}
