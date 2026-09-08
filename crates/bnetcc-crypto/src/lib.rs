//! Cryptographic primitives for Classic Battle.net.
//!
//! Everything here is a pure function with no I/O and no runtime dependency, so it can
//! be tested exhaustively and fuzzed cheaply. Nothing in this crate is a modern secure
//! primitive — it exists to match what 1998–2003 game clients compute.
//!
//! See `docs/LEGAL.md` for the provenance of each algorithm; in short, everything here
//! comes from BNETDocs or permissively-licensed references, never from PvPGN's
//! AGPL-licensed SRP sources.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod xsha1;

pub use xsha1::{logon_proof, password_hash, proofs_match, xsha1_bytes};

/// Placeholder for Blizzard's NLS/SRP-6 variant, used by WarCraft III.
///
/// Not yet implemented. The parameters and the five deviations from stock SRP-6a are
/// documented in `docs/PROTOCOL-NOTES.md` section 3. Implement from the javaop write-up
/// and RFC 2945 — **not** from PvPGN's `bnetsrp3.cpp`, which is AGPL-3.0.
///
/// The important property for the federation design: the server stores `(salt, verifier)`
/// and the verifier does **not** permit client impersonation, so it is safe to cache at
/// a semi-trusted node. See `docs/FEDERATION.md` section 4.
pub mod nls {
    /// Blizzard's SRP generator.
    pub const G: u32 = 47;

    /// Blizzard's 256-bit SRP modulus, big-endian.
    ///
    /// NLS version 1 reverses this byte order relative to version 2.
    pub const N_BE: [u8; 32] = [
        0xF8, 0xFF, 0x1A, 0x8B, 0x61, 0x99, 0x18, 0x03, 0x21, 0x86, 0xB6, 0x8C, 0xA0, 0x92,
        0xB5, 0x55, 0x7E, 0x97, 0x6C, 0x78, 0xC7, 0x32, 0x12, 0xD9, 0x12, 0x16, 0xF6, 0x65,
        0x85, 0x23, 0xC7, 0x87,
    ];
}
