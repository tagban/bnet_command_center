//! String tables — `data\local\lng\<language>\*.tbl`.
//!
//! Every name the game shows is kept here, keyed both by number and by a short key the excel
//! tables use (an item's `namestr`, e.g. `cap`). This reader serves the key lookup, and a key's
//! number for the few places the game sends one (a runeword's name).
//!
//! ```text
//! 0x00 u16 crc
//! 0x02 u16 element count
//! 0x04 u32 hash table size     entries that follow the index
//! 0x08 u8  version
//! 0x09 u32 data start offset
//! 0x0d u32 hash max tries
//! 0x11 u32 file size           the file's real length, which checks a parse
//! 0x15     u16 index[count]
//!          entry[hash size]    17 bytes: u8 used, u16 index, u32 hash, u32 key offset,
//!                              u32 string offset, u16 length
//! ```
//!
//! Keys and strings are NUL-terminated, one byte per character (Windows-1252 for the English
//! tables). Layout from libd2 `packages/formats/src/strtbl.zig` (MIT).

use std::collections::HashMap;

const HEADER_LEN: usize = 0x15;
const ENTRY_LEN: usize = 17;

/// A parsed table: every used entry, key to its number and string.
#[derive(Debug, Clone, Default)]
pub struct StringTable {
    by_key: HashMap<String, (u16, String)>,
}

impl StringTable {
    /// Parse a table. `None` if the header's sizes do not fit the file.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize);
        let count = usize::from(u16_at(0x02)?);
        let hash_size = u32_at(0x04)?;
        if u32_at(0x11)? != bytes.len() {
            return None;
        }
        let entries = HEADER_LEN + count * 2;
        if entries + hash_size * ENTRY_LEN > bytes.len() {
            return None;
        }
        let cstr = |at: usize| -> Option<String> {
            let tail = bytes.get(at..)?;
            let end = tail.iter().position(|&b| b == 0)?;
            Some(tail[..end].iter().map(|&b| char::from(b)).collect())
        };
        let mut by_key = HashMap::new();
        for i in 0..hash_size {
            let at = entries + i * ENTRY_LEN;
            if bytes[at] == 0 {
                continue;
            }
            let (Some(key), Some(value)) = (cstr(u32_at(at + 7)?), cstr(u32_at(at + 11)?)) else { continue };
            let index = u16_at(at + 1)?;
            by_key.entry(key).or_insert((index, value));
        }
        Some(Self { by_key })
    }

    /// The string for a key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.by_key.get(key).map(|(_, s)| s.as_str())
    }

    /// A key's number within this table.
    #[must_use]
    pub fn index(&self, key: &str) -> Option<u16> {
        self.by_key.get(key).map(|&(i, _)| i)
    }

    /// Entries with a key.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether the table has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of `(key, value)` entries laid out as the game writes them.
    fn build(entries: &[(&str, &str)]) -> Vec<u8> {
        let count = entries.len();
        let strings_at = HEADER_LEN + count * 2 + count * ENTRY_LEN;
        let mut strings = Vec::new();
        let mut table = Vec::new();
        for (i, (key, value)) in entries.iter().enumerate() {
            let key_at = strings_at + strings.len();
            strings.extend_from_slice(key.as_bytes());
            strings.push(0);
            let value_at = strings_at + strings.len();
            strings.extend_from_slice(value.as_bytes());
            strings.push(0);
            table.push(1);
            table.extend_from_slice(&(i as u16).to_le_bytes());
            table.extend_from_slice(&0u32.to_le_bytes());
            table.extend_from_slice(&(key_at as u32).to_le_bytes());
            table.extend_from_slice(&(value_at as u32).to_le_bytes());
            table.extend_from_slice(&((value.len() + 1) as u16).to_le_bytes());
        }
        let total = strings_at + strings.len();
        let mut out = vec![0u8; HEADER_LEN];
        out[2..4].copy_from_slice(&(count as u16).to_le_bytes());
        out[4..8].copy_from_slice(&(count as u32).to_le_bytes());
        out[0x11..0x15].copy_from_slice(&(total as u32).to_le_bytes());
        for i in 0..count {
            out.extend_from_slice(&(i as u16).to_le_bytes());
        }
        out.extend(table);
        out.extend(strings);
        out
    }

    #[test]
    fn keys_find_their_strings() {
        let t = StringTable::parse(&build(&[("cap", "Cap"), ("skp", "Skull Cap")])).unwrap();
        assert_eq!(t.index("skp"), Some(1));
        assert_eq!(t.get("cap"), Some("Cap"));
        assert_eq!(t.get("skp"), Some("Skull Cap"));
        assert_eq!(t.get("hlm"), None);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn a_wrong_file_size_is_refused() {
        let mut bytes = build(&[("cap", "Cap")]);
        bytes.push(0);
        assert!(StringTable::parse(&bytes).is_none());
        assert!(StringTable::parse(&[0; 4]).is_none());
    }
}
