//! The shown names of things, from the install's string tables.
//!
//! A key is looked up in `patchstring.tbl`, then `expansionstring.tbl`, then `string.tbl` — the
//! order the game's key lookup takes, so a patch's rename wins.

use d2_formats::tbl::StringTable;

/// The three tables of one language, most recent first.
#[derive(Debug, Clone, Default)]
pub struct Strings {
    tables: Vec<StringTable>,
}

/// The tables, in lookup order.
pub const TABLES: [&str; 3] = ["patchstring.tbl", "expansionstring.tbl", "string.tbl"];

impl Strings {
    /// Build from tables already in lookup order.
    #[must_use]
    pub fn from_tables(tables: Vec<StringTable>) -> Self {
        Self { tables }
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
