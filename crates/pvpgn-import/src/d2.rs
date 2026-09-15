//! Diablo II closed-realm characters from PvPGN's character server (d2cs).
//!
//! d2cs keeps each character twice: a small index entry under `charinfo/<account>/<character>`
//! and the game's own `.d2s` save under `charsave/<character>`. Only the directory layout of
//! `charinfo` is used — it says which account owns which character. Everything else (class,
//! level, hardcore, expansion, ladder, progression, the whole save) is read from the `.d2s`,
//! which Command Center stores as it is.

use std::path::{Path, PathBuf};

use d2_formats::d2s::Save;

/// A character to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCharacter {
    /// The owning account's name, as the `charinfo` directory spells it (lowercase).
    pub account: String,
    /// The name, from the save.
    pub name: String,
    /// Class, 0 Amazon … 6 Assassin.
    pub class: u8,
    /// Status bits.
    pub status: u8,
    /// Level.
    pub level: u8,
    /// Progression (`.d2s` `0x25`).
    pub progression: u8,
    /// Last played, from the save (`0x30`).
    pub last_played: u64,
    /// The save.
    pub save: Vec<u8>,
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).collect();
    out.sort();
    out
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Every character with both a `charinfo` entry and a readable 1.10–1.14 save, and a note for
/// each one left out.
#[must_use]
pub fn read_characters(charinfo: &Path, charsave: &Path) -> (Vec<PlannedCharacter>, Vec<String>) {
    let saves = files(charsave);
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for account_dir in files(charinfo).into_iter().filter(|p| p.is_dir()) {
        let account = file_name(&account_dir);
        for entry in files(&account_dir).into_iter().filter(|p| p.is_file()) {
            let character = file_name(&entry);
            let Some(save_path) = saves.iter().find(|s| file_name(s).eq_ignore_ascii_case(&character)) else {
                notes.push(format!("{character} (account {account}): no save in charsave"));
                continue;
            };
            let bytes = match std::fs::read(save_path) {
                Ok(b) => b,
                Err(e) => {
                    notes.push(format!("{character} (account {account}): {e}"));
                    continue;
                }
            };
            let save = match Save::parse(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    notes.push(format!("{character} (account {account}): not a Diablo II 1.10–1.14 save ({e})"));
                    continue;
                }
            };
            let name = save.name();
            if !name.eq_ignore_ascii_case(&character) {
                notes.push(format!("{character} (account {account}): its save is named {name}"));
                continue;
            }
            let at = |i: usize| save.header.get(i).copied().unwrap_or(0);
            let last_played = u32::from_le_bytes([at(0x30), at(0x31), at(0x32), at(0x33)]);
            out.push(PlannedCharacter {
                account: account.clone(),
                name,
                class: save.class(),
                status: save.status(),
                level: save.level().max(1),
                progression: at(0x25),
                last_played: u64::from(last_played),
                save: bytes,
            });
        }
    }
    (out, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_are_found_by_account_directory_and_read_from_their_saves() {
        let root = std::env::temp_dir().join(format!("pvpgn-d2-{}", std::process::id()));
        let (info, saves) = (root.join("charinfo"), root.join("charsave"));
        std::fs::create_dir_all(info.join("raynor")).unwrap();
        std::fs::create_dir_all(info.join("kerrigan")).unwrap();
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(info.join("raynor").join("marshal"), b"index").unwrap();
        std::fs::write(info.join("kerrigan").join("queen"), b"index").unwrap();
        let mut save = Save::new("Marshal", 4, 0x20 | 0x04, 1_136_073_600, &[]);
        save.set_level(17, 1_136_080_000);
        save.header[0x25] = 5;
        std::fs::write(saves.join("marshal"), save.to_bytes()).unwrap();
        std::fs::write(saves.join("queen"), b"not a save").unwrap();
        let (found, notes) = read_characters(&info, &saves);
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(found.len(), 1, "{notes:?}");
        let c = &found[0];
        assert_eq!((c.account.as_str(), c.name.as_str(), c.class, c.level, c.progression, c.last_played), ("raynor", "Marshal", 4, 17, 5, 1_136_080_000));
        assert_eq!(c.status & 0x24, 0x24, "expansion and hardcore kept");
        assert_eq!(notes.len(), 1);
    }
}
