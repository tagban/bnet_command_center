//! Diablo II level generation (DRLG).
//!
//! The retail client builds the map itself from the game's seed, so a server has to build the
//! same one, cell for cell, to know where rooms, objects and monsters are. This crate reproduces
//! the 1.14d engine's generator, reading its rules from the operator's tables
//! (`d2_data::GameData`).
//!
//! Ported from `jaenster/libd2` `packages/drlg` (MIT, © 2026 jaenster), itself built from the
//! 1.14d `Game.exe`; each function cites the engine address it reproduces. See `docs/LEGAL.md`
//! §2 and `docs/D2GS-RUST.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod act;
pub mod collision;
pub mod outdoor;
pub mod preset;
pub mod rng;
pub mod room_tiles;
pub mod tiles;
pub mod world;

/// A rectangle in tiles: origin and size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coords {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}
