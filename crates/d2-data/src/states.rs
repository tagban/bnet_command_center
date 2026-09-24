//! `States.txt`: the states a unit can be in — a warcry's, a curse's, a passive's — by name. A
//! state's id is its row, and the id is what goes to the client (`0xA7`–`0xAA`).

use d2_formats::excel::Table;

/// Every state, by id.
#[derive(Debug, Clone, Default)]
pub struct States {
    rows: Vec<State>,
}

/// The `States.txt` columns read so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// `state`.
    pub name: String,
    /// `nosend`: the client is never told of it.
    pub no_send: bool,
    /// `group` (`+0x1E`): a state that takes over one of the same group removes it — one armour
    /// at a time, one of Quickness and Fade (`0x0056C740`). 0 for none.
    pub group: i32,
    /// `restrict`: a state that bars the skills with `restrict` 0 (a Druid's wolf and bear forms).
    pub restrict: bool,
    /// `disguise`: a state that changes a unit's look (`+0x12` bit 0) — while one is on the unit
    /// counts as transformed.
    pub disguise: bool,
}

impl States {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|r| State {
                name: r.get("state").unwrap_or_default().to_string(),
                no_send: r.int("nosend").unwrap_or(0) != 0,
                group: r.int("group").unwrap_or(0) as i32,
                restrict: r.int("restrict").unwrap_or(0) != 0,
                disguise: r.int("disguise").unwrap_or(0) != 0,
            })
            .collect();
        Self { rows }
    }

    /// A state's id by name, any case.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<u16> {
        self.rows.iter().position(|s| s.name.eq_ignore_ascii_case(name)).and_then(|i| u16::try_from(i).ok())
    }

    /// A state by id.
    #[must_use]
    pub fn get(&self, id: u16) -> Option<&State> {
        self.rows.get(usize::from(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_their_rows() {
        let t = Table::parse(b"state\tid\tnosend\r\nnone\t0\t\r\nfreeze\t1\t\r\nhidden\t2\t1\r\n");
        let s = States::from_table(&t);
        assert_eq!((s.id("Freeze"), s.id("hidden"), s.id("missing")), (Some(1), Some(2), None));
        assert!(s.get(2).is_some_and(|x| x.no_send));
    }
}
