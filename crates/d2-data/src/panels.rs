//! `Inventory.txt`: the grid each screen container gives an item to sit in.
//!
//! Only the grid sizes matter here — the many pixel columns place art on screen, which is the
//! client's business. The engine reads a container's width and height from its `Inventory.txt`
//! record at `+0x10F`/`+0x110` (`FUN_006286C0`, `.\INVENTORY\Inventory.cpp`), so the stash and
//! cube are as large as the data says and no larger.
//!
//! The rows are named for the panels: the backpack is per class but always `10 × 4`; `Bank Page
//! 1` is the pre-expansion stash, `Big Bank Page 1` the Lord of Destruction one, `Transmogrify
//! Box Page 1` the Horadric Cube.

use d2_formats::excel::Table;

/// A container's grid, in cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Grid {
    /// Cells across.
    pub width: u8,
    /// Cells down.
    pub height: u8,
}

impl Grid {
    /// Cells in the grid.
    #[must_use]
    pub fn cells(self) -> u16 {
        u16::from(self.width) * u16::from(self.height)
    }
}

/// The container grids `Inventory.txt` defines.
#[derive(Debug, Clone, Default)]
pub struct Panels {
    /// The backpack, `10 × 4`.
    pub inventory: Grid,
    /// The stash before Lord of Destruction, `6 × 4` (`Bank Page 1`).
    pub classic_stash: Grid,
    /// The stash with the expansion, `6 × 8` (`Big Bank Page 1`).
    pub expansion_stash: Grid,
    /// The Horadric Cube, `3 × 4` (`Transmogrify Box Page 1`).
    pub cube: Grid,
}

impl Panels {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let grid = |name: &str| -> Grid {
            t.rows()
                .find(|r| r.get("class").is_some_and(|c| c.eq_ignore_ascii_case(name)))
                .map(|r| Grid {
                    width: u8::try_from(r.int("gridX").unwrap_or(0).max(0)).unwrap_or(0),
                    height: u8::try_from(r.int("gridY").unwrap_or(0).max(0)).unwrap_or(0),
                })
                .unwrap_or_default()
        };
        Self {
            // The backpack is one row per class, all the same size; any class gives it.
            inventory: grid("Amazon"),
            classic_stash: grid("Bank Page 1"),
            expansion_stash: grid("Big Bank Page 1"),
            cube: grid("Transmogrify Box Page 1"),
        }
    }

    /// The stash a character of this game sees: the larger one comes with the expansion.
    #[must_use]
    pub fn stash(&self, expansion: bool) -> Grid {
        if expansion {
            self.expansion_stash
        } else {
            self.classic_stash
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_sizes_come_from_the_named_rows() {
        // A cut of the real Inventory.txt: the columns the engine reads, the rows that matter.
        let t = Table::parse(
            b"class\tgridX\tgridY\r\n\
              Amazon\t10\t4\r\n\
              Sorceress\t10\t4\r\n\
              Bank Page 1\t6\t4\r\n\
              Transmogrify Box Page 1\t3\t4\r\n\
              Big Bank Page 1\t6\t8\r\n\
              Hireling\t0\t0\r\n",
        );
        let panels = Panels::from_table(&t);
        assert_eq!(panels.inventory, Grid { width: 10, height: 4 });
        assert_eq!(panels.classic_stash, Grid { width: 6, height: 4 }, "the pre-expansion stash");
        assert_eq!(panels.expansion_stash, Grid { width: 6, height: 8 }, "Lord of Destruction's stash");
        assert_eq!(panels.cube, Grid { width: 3, height: 4 }, "the Horadric Cube");

        // The expansion gets the bigger stash; a classic game the smaller one.
        assert_eq!(panels.stash(true), panels.expansion_stash);
        assert_eq!(panels.stash(false).cells(), 24);
        assert_eq!(panels.stash(true).cells(), 48);
    }
}
