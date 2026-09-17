//! `Belts.txt`: how many boxes a worn belt gives.
//!
//! The belt is one grid of sixteen slots (`0x007447B4` is `16 × 1`), of which a player sees as
//! many as its belt allows: slot `column + 4 × row`, row 0 the bottom, hotkey, row. The engine
//! takes the worn belt from body location 8 and its `Armor.txt` `belt` column as the row of this
//! table ([`NO_BELT_KIND`] with nothing worn), then reads `numboxes` from that row —
//! `0x00660CB0(kind, 0, &record)` copies the `0x108`-byte record at `(0 * 7 + kind)` and
//! `0x0063C600` reads its byte `+4`.
//!
//! The table holds each of the seven belts twice, once per screen layout; only the box count
//! matters to a server, the rest of a row places boxes on screen.

use d2_formats::excel::Table;

/// The row a player with no belt on uses (`default`, four boxes).
pub const NO_BELT_KIND: i32 = 2;
/// Slots in a belt row.
pub const ROW_WIDTH: u8 = 4;
/// Slots the belt grid holds at most, four rows of four.
pub const MAX_BOXES: u8 = 16;

/// `Belts.txt`: the box count of each kind of belt.
#[derive(Debug, Clone, Default)]
pub struct Belts {
    boxes: Vec<u8>,
}

impl Belts {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        Self {
            boxes: t.rows().map(|r| u8::try_from(r.int("numboxes").unwrap_or(0).clamp(0, i64::from(MAX_BOXES))).unwrap_or(0)).collect(),
        }
    }

    /// The boxes a belt of `kind` gives; `None` for a kind the table does not have.
    #[must_use]
    pub fn boxes(&self, kind: i32) -> Option<u8> {
        usize::try_from(kind).ok().and_then(|row| self.boxes.get(row).copied()).filter(|&n| n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real table's first rows, as `Belts.txt` has them.
    const TABLE: &[u8] = b"name\tnumboxes\tboxwidth\r\nbelt\t12\t29\r\nsash\t8\t29\r\ndefault\t4\t29\r\ngirdle\t16\t29\r\n";

    #[test]
    fn a_belt_gives_the_boxes_its_row_names() {
        let belts = Belts::from_table(&Table::parse(TABLE));
        assert_eq!(belts.boxes(0), Some(12), "a belt");
        assert_eq!(belts.boxes(1), Some(8), "a sash");
        assert_eq!(belts.boxes(NO_BELT_KIND), Some(4), "no belt worn");
        assert_eq!(belts.boxes(3), Some(16), "a girdle");
        assert_eq!(belts.boxes(9), None, "no such row");
        assert_eq!(belts.boxes(-1), None);
    }
}
