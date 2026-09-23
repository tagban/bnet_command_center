//! The shown names of things, from the install's string tables.
//!
//! A key is looked up in `patchstring.tbl`, then `expansionstring.tbl`, then `string.tbl` — the
//! order the game's key lookup takes, so a patch's rename wins.

use d2_formats::tbl::StringTable;

/// The three tables of one language, most recent first, each with the number its keys' ids
/// start at.
#[derive(Debug, Clone, Default)]
pub struct Strings {
    tables: Vec<StringTable>,
    firsts: Vec<u16>,
}

/// The tables, in lookup order.
pub const TABLES: [&str; 3] = ["patchstring.tbl", "expansionstring.tbl", "string.tbl"];

/// Where each of [`TABLES`]' ids start: a string's id is its number in its table plus this — so
/// an expansion string's is 20000 and up.
pub const FIRST_IDS: [u16; 3] = [10000, 20000, 0];

impl Strings {
    /// Build from tables already in lookup order (their ids unknown).
    #[must_use]
    pub fn from_tables(tables: Vec<StringTable>) -> Self {
        Self { tables, firsts: Vec::new() }
    }

    /// Build from `(name, table)` pairs, `name` one of [`TABLES`], in lookup order.
    #[must_use]
    pub fn from_named(tables: Vec<(&str, StringTable)>) -> Self {
        let firsts = tables.iter().map(|(name, _)| TABLES.iter().position(|t| t == name).map_or(0, |i| FIRST_IDS[i])).collect();
        Self { tables: tables.into_iter().map(|(_, t)| t).collect(), firsts }
    }

    /// A key's id, as the game sends it: looked up in the same order as [`Self::get`].
    #[must_use]
    pub fn id(&self, key: &str) -> Option<u16> {
        self.tables.iter().zip(&self.firsts).find_map(|(t, &first)| t.index(key).map(|i| first + i))
    }

    /// The string for a key, trimmed.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.tables.iter().find_map(|t| t.get(key)).map(str::trim)
    }

    /// Whether no table was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.iter().all(StringTable::is_empty)
    }
}
