//! Diablo II game runtime.
//!
//! What lives in a running game — its units, their ids, modes and looks — reproduced from the
//! 1.14d `Game.exe` server code, each routine citing the address it follows
//! (`docs/D2GS-114D-WIRE.md`, `docs/LEGAL.md` §2). Rules come from the operator's install
//! (`d2_data::GameData`); the map from `d2_drlg`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod battle;
pub mod clock;
pub mod gear;
pub mod inventory;
pub mod loot;
pub mod path;
pub mod population;
pub mod spawn;
