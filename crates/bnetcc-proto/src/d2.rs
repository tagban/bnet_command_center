//! Diablo II closed-realm character data: names and the 33-byte character portrait.
//!
//! A realm character is drawn in two places from the same 33-byte blob:
//!
//! - **Character select** — each entry of `MCP_CHARLIST2` carries one.
//! - **Chat** — the user's statstring is `<product><realm>,<character>,<portrait>`, so the
//!   channel list can draw the character, its class and level.
//!
//! ```text
//! [0..2]    header         charlist: realm character count (14-bit); chat: 84 80
//! [2..13]   equipment      11 graphic codes (head, torso, legs, arms, weapons, shield, …)
//! [13]      class + 1      1 Amazon … 7 Assassin
//! [14..25]  colours        11 colour transforms for the equipment above
//! [25]      level
//! [26..28]  flags          14-bit: low byte = .d2s status bits, bits 8..12 = progression
//! [28..30]  unknown        14-bit, zero
//! [30]      ladder         FF = non-ladder
//! [31..33]  unknown        FF FF
//! ```
//!
//! Every byte is non-zero — the blob travels as a C string. "Nothing" is `0xFF`, and small
//! integers are sent 7 bits per byte with the high bit set ("14-bit" fields). An all-`0xFF`
//! equipment block draws a valid, unarmed character, which is what a new one is.
//!
//! Sources: BNETDocs "Chat Statstrings" (the byte table), cross-checked against the
//! MIT-licensed `jaenster/d2-dedicated-server` realm, which a retail 1.14d client renders
//! (see `docs/DIABLO2.md`). The two agree on every field.

use crate::error::FourCc;

/// Length of a character portrait.
pub const PORTRAIT_LEN: usize = 33;

/// Longest character name the client accepts.
pub const NAME_MAX: usize = 15;
/// Shortest character name the client accepts.
pub const NAME_MIN: usize = 2;

/// The `.d2s` status bits, as `MCP_CHARCREATE` sends them and the portrait carries them.
pub mod status {
    /// Hardcore.
    pub const HARDCORE: u8 = 0x04;
    /// A hardcore character that has died.
    pub const DEAD: u8 = 0x08;
    /// Lord of Destruction character.
    pub const EXPANSION: u8 = 0x20;
    /// Ladder character.
    pub const LADDER: u8 = 0x40;
    /// The bits a client may choose at creation (it cannot create a dead character).
    pub const CREATABLE: u8 = HARDCORE | EXPANSION | LADDER;
}

/// Character classes, as `MCP_CHARCREATE` numbers them.
pub mod class {
    /// Amazon.
    pub const AMAZON: u8 = 0;
    /// Sorceress.
    pub const SORCERESS: u8 = 1;
    /// Necromancer.
    pub const NECROMANCER: u8 = 2;
    /// Paladin.
    pub const PALADIN: u8 = 3;
    /// Barbarian.
    pub const BARBARIAN: u8 = 4;
    /// Druid — Lord of Destruction only.
    pub const DRUID: u8 = 5;
    /// Assassin — Lord of Destruction only.
    pub const ASSASSIN: u8 = 6;

    /// Whether a class exists at all.
    #[must_use]
    pub const fn is_valid(c: u8) -> bool {
        c <= ASSASSIN
    }

    /// Whether a class exists only in the expansion.
    #[must_use]
    pub const fn expansion_only(c: u8) -> bool {
        matches!(c, DRUID | ASSASSIN)
    }

    /// The class name, for logs and the admin panel.
    #[must_use]
    pub const fn name(c: u8) -> &'static str {
        match c {
            AMAZON => "Amazon",
            SORCERESS => "Sorceress",
            NECROMANCER => "Necromancer",
            PALADIN => "Paladin",
            BARBARIAN => "Barbarian",
            DRUID => "Druid",
            ASSASSIN => "Assassin",
            _ => "Unknown",
        }
    }
}

/// What a portrait is drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Portrait {
    /// Class, `0..=6`.
    pub class: u8,
    /// `.d2s` status bits.
    pub status: u8,
    /// Level, `1..=99`.
    pub level: u8,
    /// Difficulty/act progression.
    pub progression: u8,
}

/// Append a "14-bit" integer: 7 bits per byte, high bit always set, so it is never `0x00`.
fn push14(out: &mut Vec<u8>, v: u32) {
    out.push(((v & 0x7F) | 0x80) as u8);
    out.push((((v >> 7) & 0x7F) | 0x80) as u8);
}

impl Portrait {
    /// The 33 bytes, with `header` in the first two.
    fn encode(&self, header: [u8; 2]) -> Vec<u8> {
        let mut out = Vec::with_capacity(PORTRAIT_LEN);
        out.extend_from_slice(&header);
        out.extend_from_slice(&[0xFF; 11]); // equipment: none
        out.push(self.class.min(class::ASSASSIN) + 1);
        out.extend_from_slice(&[0xFF; 11]); // colours: default
        out.push(self.level.clamp(1, 99));
        // Low byte: the status bits the client tests (hardcore, dead, expansion, ladder).
        // Bits 8..12: progression, which picks the character's title.
        let flags = (u32::from(self.progression & 0x1F) << 8) | u32::from(self.status & 0x6C);
        push14(&mut out, flags);
        push14(&mut out, 0);
        out.push(0xFF); // ladder: none
        out.extend_from_slice(&[0xFF, 0xFF]);
        debug_assert_eq!(out.len(), PORTRAIT_LEN);
        out
    }

    /// The portrait for one `MCP_CHARLIST2` entry. `realm_count` is the account's total
    /// character count, which the character-select screen reads from the header.
    #[must_use]
    pub fn charlist_bytes(&self, realm_count: u32) -> Vec<u8> {
        let mut header = Vec::with_capacity(2);
        push14(&mut header, realm_count);
        self.encode([header[0], header[1]])
    }

    /// The full chat statstring for a realm character: `<product><realm>,<name>,<portrait>`.
    #[must_use]
    pub fn chat_statstring(&self, product: FourCc, realm: &str, name: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + realm.len() + name.len() + 2 + PORTRAIT_LEN);
        out.extend_from_slice(&product_tag(product));
        out.extend_from_slice(realm.as_bytes());
        out.push(b',');
        out.extend_from_slice(name.as_bytes());
        out.push(b',');
        out.extend(self.encode([0x84, 0x80]));
        out
    }
}

/// A product as it leads a statstring: the FourCC reversed (`D2XP` → `PX2D`).
#[must_use]
pub fn product_tag(product: FourCc) -> [u8; 4] {
    let a = product.as_ascii();
    [a[3], a[2], a[1], a[0]]
}

/// Parse the `Realm,CharacterName` a Diablo II client sends as its `SID_ENTERCHAT`
/// statstring. `None` for an Open Battle.net character (empty realm) or anything malformed.
#[must_use]
pub fn parse_enterchat_statstring(raw: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(raw).ok()?;
    let (realm, name) = text.split_once(',')?;
    (!realm.is_empty() && !name.is_empty()).then_some((realm, name))
}

/// Whether a character name is one the client itself would let a player type: 2–15
/// letters, with at most one `-` or `_` that is neither first nor last.
#[must_use]
pub fn valid_character_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if !(NAME_MIN..=NAME_MAX).contains(&bytes.len()) {
        return false;
    }
    let mut punctuation = 0;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'-' | b'_' if i != 0 && i != bytes.len() - 1 => punctuation += 1,
            _ => return false,
        }
    }
    punctuation <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product;

    const SORC: Portrait = Portrait { class: class::SORCERESS, status: status::EXPANSION, level: 1, progression: 0 };

    #[test]
    fn a_portrait_is_33_non_zero_bytes() {
        for p in [
            SORC,
            Portrait { class: class::ASSASSIN, status: 0x6C, level: 99, progression: 15 },
            Portrait { class: 0, status: 0, level: 0, progression: 0 },
        ] {
            for bytes in [p.charlist_bytes(0), p.charlist_bytes(18), p.chat_statstring(product::D2XP, "r", "n")] {
                assert!(!bytes.contains(&0), "a portrait travels as a C string: {bytes:02X?}");
            }
            assert_eq!(p.charlist_bytes(1).len(), PORTRAIT_LEN);
        }
    }

    #[test]
    fn the_fields_sit_where_the_client_reads_them() {
        let p = Portrait { class: class::PALADIN, status: status::HARDCORE | status::EXPANSION, level: 42, progression: 5 };
        let b = p.charlist_bytes(3);
        assert_eq!(&b[0..2], &[0x83, 0x80], "realm character count, 14-bit");
        assert!(b[2..13].iter().all(|&x| x == 0xFF), "no equipment");
        assert_eq!(b[13], 4, "class + 1");
        assert!(b[14..25].iter().all(|&x| x == 0xFF), "default colours");
        assert_eq!(b[25], 42, "level");
        assert_eq!(b[26], 0x80 | 0x24, "status bits in the low flag byte");
        assert_eq!(b[27], 0x80 | (5 << 1), "progression in bits 8..12 of the 14-bit flags");
        assert_eq!(&b[28..30], &[0x80, 0x80]);
        assert_eq!(&b[30..33], &[0xFF, 0xFF, 0xFF], "non-ladder");
    }

    #[test]
    fn the_chat_statstring_is_product_realm_name_portrait() {
        let s = SORC.chat_statstring(product::D2XP, "bncc", "Tyrael");
        let (head, portrait) = s.split_at(s.len() - PORTRAIT_LEN);
        assert_eq!(head, b"PX2Dbncc,Tyrael,");
        assert_eq!(&portrait[0..2], &[0x84, 0x80], "the chat header BNETDocs documents");
        assert_eq!(product_tag(product::D2DV), *b"VD2D");
    }

    #[test]
    fn enterchat_statstrings_name_the_realm_character() {
        assert_eq!(parse_enterchat_statstring(b"bncc,Tyrael"), Some(("bncc", "Tyrael")));
        assert_eq!(parse_enterchat_statstring(b",OpenChar"), None, "open characters have no realm");
        assert_eq!(parse_enterchat_statstring(b""), None);
        assert_eq!(parse_enterchat_statstring(b"PX2D"), None);
    }

    #[test]
    fn character_names_follow_the_client_rules() {
        for ok in ["Ty", "Tyrael", "Deckard-Cain", "Blood_Raven", "ABCDEFGHIJKLMNO"] {
            assert!(valid_character_name(ok), "{ok} should be allowed");
        }
        for bad in ["T", "ABCDEFGHIJKLMNOP", "-Tyrael", "Tyrael_", "Dec-kard-Cain", "a_b-c", "Tyrael2", "Ty rael", ""] {
            assert!(!valid_character_name(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn druids_and_assassins_are_expansion_only() {
        assert!(class::expansion_only(class::DRUID) && class::expansion_only(class::ASSASSIN));
        assert!(!class::expansion_only(class::NECROMANCER));
        assert!(!class::is_valid(7));
    }
}
