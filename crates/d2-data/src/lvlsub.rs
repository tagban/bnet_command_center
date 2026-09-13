//! `LvlSub.txt`: the substitution pieces the outdoor generator stamps onto wilderness levels —
//! the cliff and border shapes that make a level's edge ragged.

use d2_formats::excel::Table;

use crate::Error;

/// One `LvlSub.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvlSub {
    /// `Type`: the group the generator asks for; rows of one group are consecutive.
    pub sub_type: i32,
    /// `File`, relative to `data\global\tiles`.
    pub file: String,
    /// `BordType`: 0 places one of the file's groups and stops, 1 stops after each group's first
    /// placement, 2 keeps going.
    pub bord_type: i32,
    /// `GridSize`: level cells per map tile.
    pub grid_size: i32,
    /// `CheckAll`: try every position rather than a roll's worth.
    pub check_all: bool,
    /// `Dt1Mask`: tile files the piece needs.
    pub dt1_mask: i32,
    /// `Prob0`..`Prob4`: percent chance per sub theme that a room uses this row.
    pub prob: [i32; 5],
    /// `Trials0`..`Trials4`: placement tries per sub theme, -1 for every position.
    pub trials: [i32; 5],
    /// `Max0`..`Max4`: placements per sub theme.
    pub max: [i32; 5],
}

/// `LvlSub.txt`, in file order.
#[derive(Debug, Clone, Default)]
pub struct LvlSubs {
    rows: Vec<LvlSub>,
}

impl LvlSubs {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Type`, `File`, `BordType` or `GridSize` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Type", "File", "BordType", "GridSize"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "lvlsub.txt", problem: format!("no {column} column") });
            }
        }
        let rows = t
            .rows()
            .map(|row| LvlSub {
                sub_type: row.int("Type").unwrap_or(-1) as i32,
                file: row.get("File").unwrap_or_default().to_string(),
                bord_type: row.int("BordType").unwrap_or(0) as i32,
                grid_size: row.int("GridSize").unwrap_or(0) as i32,
                check_all: row.int("CheckAll").unwrap_or(0) != 0,
                dt1_mask: row.int("Dt1Mask").unwrap_or(0) as i32,
                prob: std::array::from_fn(|i| row.int(&format!("Prob{i}")).unwrap_or(0) as i32),
                trials: std::array::from_fn(|i| row.int(&format!("Trials{i}")).unwrap_or(0) as i32),
                max: std::array::from_fn(|i| row.int(&format!("Max{i}")).unwrap_or(0) as i32),
            })
            .collect();
        Ok(Self { rows })
    }

    /// The consecutive rows of group `sub_type`, from its first row
    /// (`TXT_LvlSub_GetLineFromSubType` and the walk `TILESUB_AddSecondaryBorder` makes, `0x00670750`).
    #[must_use]
    pub fn group(&self, sub_type: i32) -> &[LvlSub] {
        let Some(start) = self.rows.iter().position(|r| r.sub_type == sub_type) else { return &[] };
        let len = self.rows[start..].iter().take_while(|r| r.sub_type == sub_type).count();
        &self.rows[start..start + len]
    }
}
