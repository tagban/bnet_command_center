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

pub mod nls;
