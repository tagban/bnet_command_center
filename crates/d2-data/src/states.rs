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
}

impl States {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t.rows().map(|r| State { name: r.get("state").unwrap_or_default().to_string(), no_send: r.int("nosend").unwrap_or(0) != 0 }).collect();
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
