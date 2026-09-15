//! Diablo II ladder seasons.
//!
//! A season is a number and the day it began. Every ladder character belongs to the current
//! season. Ending one — only ever from the admin panel, since how long a season should run
//! depends on how many people play (tagban, 2026-09-14) — turns every ladder character, softcore
//! and hardcore alike, into a normal character that keeps everything it has, which empties the
//! ladder, and starts the next season for new ladder characters.
//!
//! The season is kept in a small JSON file next to the account database, as staff bans are; with
//! in-memory storage it lives in memory only.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// A ladder season.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Season {
    /// 1 for the first.
    pub number: u32,
    /// When it began, seconds since the Unix epoch.
    pub started: u64,
}

/// The current season, persisted.
pub struct LadderSeasons {
    path: Option<PathBuf>,
    current: Mutex<Season>,
}

impl LadderSeasons {
    /// Load the current season from `path`. Without a file — a new server, or in-memory storage —
    /// season 1 begins now (and is written). A malformed file is logged and starts season 1 too,
    /// but is left alone for the operator to look at.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let now = crate::now_ms() / 1000;
        let first = Season { number: 1, started: now };
        let (current, write) = match path.as_ref().map(std::fs::read) {
            None => (first, false),
            Some(Ok(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(season) => (season, false),
                Err(e) => {
                    warn!(path = ?path, error = %e, "ladder season file is malformed; showing season 1");
                    (first, false)
                }
            },
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => (first, true),
            Some(Err(e)) => {
                warn!(path = ?path, error = %e, "cannot read the ladder season file; showing season 1");
                (first, false)
            }
        };
        let seasons = Self { path, current: Mutex::new(current) };
        if write {
            seasons.write(current);
        }
        seasons
    }

    /// The season now.
    #[must_use]
    pub fn current(&self) -> Season {
        *self.current.lock().expect("season lock")
    }

    /// Start the season after the current one, now; the new season.
    pub fn begin_next(&self) -> Season {
        let mut current = self.current.lock().expect("season lock");
        *current = Season { number: current.number + 1, started: crate::now_ms() / 1000 };
        self.write(*current);
        *current
    }

    fn write(&self, season: Season) {
        let Some(path) = &self.path else { return };
        match serde_json::to_vec_pretty(&season) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, bytes) {
                    warn!(path = %path.display(), error = %e, "could not save the ladder season");
                }
            }
            Err(e) => warn!(error = %e, "could not serialise the ladder season"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_server_starts_season_one_and_seasons_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("bnetcc-season-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bnetccd-d2-season.json");
        let _ = std::fs::remove_file(&path);

        let seasons = LadderSeasons::load(Some(path.clone()));
        let first = seasons.current();
        assert_eq!(first.number, 1);
        assert!(path.exists(), "written at once");
        let second = seasons.begin_next();
        assert_eq!(second.number, 2);
        assert!(second.started >= first.started);

        assert_eq!(LadderSeasons::load(Some(path.clone())).current(), second, "read back after a restart");
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(LadderSeasons::load(Some(path.clone())).current().number, 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"not json", "a malformed file is left for the operator");
        let _ = std::fs::remove_dir_all(&dir);

        let memory = LadderSeasons::load(None);
        assert_eq!(memory.begin_next().number, 2);
    }
}
