//! Diablo II file formats.
//!
//! The game's data lives in MPQ archives in the operator's own install
//! (`diablo2.data_dir`); nothing from them is in this repository. This crate reads them at
//! run time: [`mpq`] finds and decrypts members, [`pkware`] undoes the compression Diablo II
//! used for them, and [`excel`] splits the tab-separated tables most game rules live in.
//!
//! Ported from `jaenster/libd2` (MIT, © 2026 jaenster) `packages/formats`, as recorded in
//! `docs/LEGAL.md` §2; each module names its source.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cof;
pub mod dcc;
pub mod ds1;
pub mod dt1;
pub mod excel;
pub mod gif;
pub mod mpq;
pub mod pe;
pub mod pkware;
pub mod tbl;
pub mod zip;
