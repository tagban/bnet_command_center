//! Diablo II's tab-separated data tables (`data\global\excel\*.txt`).
//!
//! The first line names the columns; every later line is a row. Lines end in CRLF. Several
//! tables carry a marker row whose first cell is `Expansion`, splitting classic rows from
//! Lord of Destruction ones; the engine's compiled tables drop it, so [`Table::parse`] does too.

/// A parsed table: column names and rows of cells, all as text.
#[derive(Debug, Clone, Default)]
pub struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Parse a table. Blank lines and `Expansion` marker rows are skipped.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        let columns = lines.next().map(|h| h.split('\t').map(str::to_string).collect()).unwrap_or_default();
        let rows = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
            .filter(|cells| !cells.first().is_some_and(|c| c.eq_ignore_ascii_case("Expansion")))
            .collect();
        Self { columns, rows }
    }

    /// The column names.
    #[must_use]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A column's index, matched case-insensitively.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.eq_ignore_ascii_case(name))
    }

    /// The rows.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(move |cells| Row { table: self, cells })
    }

    /// One row by index.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<Row<'_>> {
        self.rows.get(index).map(|cells| Row { table: self, cells })
    }
}

/// A row of a [`Table`].
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    table: &'a Table,
    cells: &'a [String],
}

impl<'a> Row<'a> {
    /// A cell by column name; empty cells and missing columns are `None`.
    #[must_use]
    pub fn get(&self, column: &str) -> Option<&'a str> {
        let at = self.table.column(column)?;
        self.cells.get(at).map(String::as_str).filter(|s| !s.is_empty())
    }

    /// A cell as an integer, as the engine reads numbers: empty is 0.
    #[must_use]
    pub fn int(&self, column: &str) -> Option<i64> {
        match self.get(column) {
            None => self.table.column(column).map(|_| 0),
            Some(s) => s.trim().parse().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_skip_blank_lines_and_the_expansion_marker() {
        let t = Table::parse(b"class\tstr\thpadd\r\nAmazon\t20\t30\r\nExpansion\r\n\r\nDruid\t15\t\r\n");
        assert_eq!(t.columns(), &["class", "str", "hpadd"]);
        assert_eq!(t.len(), 2);
        let druid = t.row(1).unwrap();
        assert_eq!(druid.get("CLASS"), Some("Druid"));
        assert_eq!(druid.int("str"), Some(15));
        assert_eq!(druid.get("hpadd"), None);
        assert_eq!(druid.int("hpadd"), Some(0), "an empty number is 0");
        assert_eq!(druid.int("nope"), None, "a missing column is not 0");
    }
}
