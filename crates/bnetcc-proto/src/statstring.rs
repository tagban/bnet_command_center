//! Chat statstrings — the per-user blob carried in `SID_CHATEVENT`.
//!
//! Every `EID_SHOWUSER` and `EID_JOIN` carries a statstring describing the user: their
//! product, and product-specific fields the *client* renders. The server stores and
//! echoes it; it does not need to understand most of it.
//!
//! Two parts of it we do need:
//!
//! - **The product tag**, so we know which icon file and which limits apply.
//! - **The WarCraft III icon code**, because that is what selects an entry from
//!   `icons.bni` for users whose icon is not chosen by chat flags. See [`crate::bni`].
//!
//! Everything else is passed through verbatim. Interpreting fields we have no use for is
//! how a server ends up coupled to a format it does not control.
//!
//! ⚠️ Field meanings below are from BNETDocs and are **reference data, not parsing
//! rules**. Real statstrings vary by patch: W3XP users have been observed with a level
//! and clan tag but **no icon field at all**, and users with no stats yet can send zero
//! fields. Every accessor here is defensive for that reason.

use crate::error::FourCc;

/// A statstring, split into its product tag and remaining fields.
///
/// Fields are space-delimited. The raw text is retained so it can be echoed byte for byte
/// — re-serialising from parsed parts would silently normalise a format we do not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statstring<'a> {
    raw: &'a [u8],
    product: Option<FourCc>,
    fields: Vec<&'a [u8]>,
}

impl<'a> Statstring<'a> {
    /// Split a statstring. Never fails: an unparseable one is still echoable.
    #[must_use]
    pub fn parse(raw: &'a [u8]) -> Self {
        let mut parts = raw.split(|&b| b == b' ').filter(|p| !p.is_empty());
        let product = parts.next().and_then(|p| {
            // The product appears as four characters. Because it is a u32 rendered as
            // text, it reads reversed: `STAR` appears as `RATS`.
            let b: [u8; 4] = p.try_into().ok()?;
            Some(FourCc(u32::from_be_bytes([b[3], b[2], b[1], b[0]])))
        });
        let fields: Vec<&[u8]> = if product.is_some() {
            parts.collect()
        } else {
            raw.split(|&b| b == b' ').filter(|p| !p.is_empty()).collect()
        };
        Self {
            raw,
            product,
            fields,
        }
    }

    /// The bytes exactly as received, for echoing.
    #[must_use]
    pub const fn raw(&self) -> &'a [u8] {
        self.raw
    }

    /// The product, if the first field looked like one.
    #[must_use]
    pub const fn product(&self) -> Option<FourCc> {
        self.product
    }

    /// Fields after the product.
    #[must_use]
    pub fn fields(&self) -> &[&'a [u8]] {
        &self.fields
    }

    /// One field, or `None` if the statstring is shorter than that.
    #[must_use]
    pub fn field(&self, index: usize) -> Option<&'a [u8]> {
        self.fields.get(index).copied()
    }

    /// A field parsed as an unsigned integer, or `None` if absent or not a number.
    #[must_use]
    pub fn field_u32(&self, index: usize) -> Option<u32> {
        std::str::from_utf8(self.field(index)?).ok()?.parse().ok()
    }

    /// The icon code a WarCraft III user should be shown.
    ///
    /// The first field of a WC3 statstring, when it is shaped like an icon code. Returns
    /// `None` for a user with no stats yet, or for the patches that omit it — both of
    /// which are normal and mean "fall back to the client's built-in icons".
    #[must_use]
    pub fn wc3_icon_code(&self) -> Option<FourCc> {
        let raw = self.field(0)?;
        let b: [u8; 4] = raw.try_into().ok()?;
        // Only accept something that actually parses as an icon code, so a level in
        // field 0 (seen on statstrings with no icon) is not mistaken for one.
        IconCode::parse(FourCc::from_ascii(&b))?;
        Some(FourCc::from_ascii(&b))
    }
}

/// Build a minimal statstring for a user who has no stored stats yet.
///
/// The product tag appears first, reversed (a `u32` rendered as text — `SEXP` reads as
/// `PXES`), which is how the client identifies the user's product and icon. The zeroed
/// fields stand in for stats we do not track yet. This mirrors the shape a real server
/// sends for a fresh account and is enough for a client to render the user in its list.
///
/// **Diablo (`DRTL`/`DSHR`) is special-cased.** Its statstring is nine gameplay fields —
/// level, class, difficulty, the four attributes, gold, spawned — and a fresh character is
/// a level-1 Warrior (`1 0 0 30 10 20 25 0 0`), not the all-zero shape. A real Diablo server
/// sends this exact line for an empty statstring; the all-zero form makes other clients
/// render a level-0 character with no attributes. See `docs/PROTOCOL-NOTES.md` §8 and
/// [`layout::DIABLO_DEFAULT`] (which `build_default(DRTL)` reproduces).
///
/// **Diablo II (`D2DV`/`D2XP`) gets the bare tag**, for the reason [`d2_is_safe`] gives: the
/// 1.14d client's parser copies a D2 statstring's first two fields by scanning for commas with
/// no bound of any kind, so the nine-zero shape above walks off the packet and over its own
/// user list. The bare tag is the client's own "a Diablo II user with no character" form.
#[must_use]
pub fn build_default(product: FourCc) -> Vec<u8> {
    let a = product.as_ascii();
    let reversed = [a[3], a[2], a[1], a[0]];
    // WarCraft III's form is the reversed tag then up to three fields (icon, level, clan
    // tag), none of which a fresh account has — the bare tag is the safe minimum. ⚠️ The
    // exact bytes a real server returns are unconfirmed (docs/WARCRAFT3.md §4.3).
    if product == crate::product::WAR3 || product == crate::product::W3XP {
        return reversed.to_vec();
    }
    // Diablo II without a realm character: see this function's note and `d2_is_safe`.
    if product == crate::product::D2DV || product == crate::product::D2XP {
        return reversed.to_vec();
    }
    let mut s = Vec::with_capacity(29);
    s.extend_from_slice(&reversed);
    if product == crate::product::DRTL || product == crate::product::DSHR {
        s.extend_from_slice(b" 1 0 0 30 10 20 25 0 0");
    } else {
        s.extend_from_slice(b" 0 0 0 0 0 0 0 0 ");
        s.extend_from_slice(&reversed);
    }
    s
}

/// The reversed product tags that make a Diablo II client parse a statstring as one of its own:
/// `D2DV`, `D2XP` and the Japanese `D2ST`, each reversed.
const D2_TAGS: [&[u8; 4]; 3] = [b"VD2D", b"PX2D", b"TS2D"];

/// Whether a statstring is safe to put in front of a Diablo II 1.14d client.
///
/// **This is a memory-safety check, not a cosmetic one.** `Game.exe` 1.14d recognises the three
/// tags in [`D2_TAGS`] and, when a byte follows the tag, copies the next two comma-delimited
/// fields into a channel-user node at `+0x46` and `+0x36`. Both copy loops (`0x00446EE0` and
/// `0x00446F00`) end **only** on a comma — they test neither the source length nor the
/// destination. The node is `0xB4` bytes with its list `next` pointer at `+0xB0`, 106 bytes past
/// the first destination, so a D2-tagged statstring with fewer than two commas makes the client
/// overwrite its own user list — and then the channel's sort at `0x00447BB0` follows the wreckage
/// and dies at `0x00447BE1`. It is a server-controlled heap overflow in every client in the
/// channel, so nothing we emit may reach it.
///
/// Two shapes are safe, and they are the only two the client itself produces:
///
/// - **exactly the four tag bytes** — the "no character" form, which takes the NUL branch at
///   `0x00446E6A` and never reaches a copy loop;
/// - **tag + realm + `,` + name + `,` + portrait** — [`crate::d2::Portrait::chat_statstring`],
///   whose two commas bound both copies.
///
/// A statstring under any other tag is not this client's business and passes.
#[must_use]
pub fn d2_is_safe(statstring: &[u8]) -> bool {
    let Some(tag) = statstring.get(..4) else {
        return true; // too short to carry a tag at all
    };
    if !D2_TAGS.iter().any(|t| t.as_slice() == tag) {
        return true;
    }
    statstring.len() == 4 || statstring[4..].iter().filter(|&&b| b == b',').count() >= 2
}

/// A StarCraft or Warcraft II player's record, as its chat statstring shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Record {
    /// Ladder rating; 0 without ladder games.
    pub rating: u32,
    /// Ladder rank, 1 the best; 0 when unranked (below 500, the server's lowest rank).
    pub rank: u32,
    /// Normal-game wins.
    pub wins: u32,
    /// Highest ladder rating.
    pub high_rating: u32,
    /// Iron Man ladder rating (Warcraft II only).
    pub iron_rating: u32,
    /// Iron Man ladder rank (Warcraft II only), as `rank`.
    pub iron_rank: u32,
}

/// The statstring for a StarCraft-family or Warcraft II user with a [`Record`]: the reversed
/// tag then the nine fields of [`layout::STARCRAFT_FIELDS`] — rating, rank, wins, spawned
/// (0), league (0), high rating, Iron Man rating and rank, and the reversed tag as the icon.
/// An empty record is exactly [`build_default`]'s line.
#[must_use]
pub fn build_starcraft(product: FourCc, record: Record) -> Vec<u8> {
    let a = product.as_ascii();
    let tag = String::from_utf8_lossy(&[a[3], a[2], a[1], a[0]]).into_owned();
    let Record { rating, rank, wins, high_rating, iron_rating, iron_rank } = record;
    format!("{tag} {rating} {rank} {wins} 0 0 {high_rating} {iron_rating} {iron_rank} {tag}").into_bytes()
}

/// A WarCraft III race tier, the middle character of an icon code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Random.
    Random,
    /// Human.
    Human,
    /// Undead.
    Undead,
    /// Night Elf.
    NightElf,
    /// Orc.
    Orc,
    /// Tournament / Frozen Throne.
    Tournament,
}

impl Tier {
    /// The letter used in an icon code.
    #[must_use]
    pub const fn letter(self) -> u8 {
        match self {
            Self::Random => b'R',
            Self::Human => b'H',
            Self::Undead => b'U',
            Self::NightElf => b'N',
            Self::Orc => b'O',
            Self::Tournament => b'D',
        }
    }

    /// Parse a tier letter.
    #[must_use]
    pub const fn from_letter(b: u8) -> Option<Self> {
        match b {
            b'R' => Some(Self::Random),
            b'H' => Some(Self::Human),
            b'U' => Some(Self::Undead),
            b'N' => Some(Self::NightElf),
            b'O' => Some(Self::Orc),
            b'D' => Some(Self::Tournament),
            _ => None,
        }
    }
}

/// A WarCraft III icon code: `Level + Tier + "3W"`, e.g. `2H3W` for level 2 Human.
///
/// Levels run 1–5 in Reign of Chaos and 1–6 in The Frozen Throne. The tier sub-field
/// arrived in patch 1.03, so very old statstrings will not parse — which is correct, and
/// means "use the client's own icon".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconCode {
    /// Ladder level.
    pub level: u8,
    /// Race tier.
    pub tier: Tier,
}

impl IconCode {
    /// Parse a four-character code.
    #[must_use]
    pub fn parse(code: FourCc) -> Option<Self> {
        let b = code.as_ascii();
        if &b[2..4] != b"3W" {
            return None;
        }
        let level = b[0].checked_sub(b'0').filter(|n| (1..=6).contains(n))?;
        Some(Self {
            level,
            tier: Tier::from_letter(b[1])?,
        })
    }

    /// Render back to a four-character code.
    #[must_use]
    pub fn to_fourcc(self) -> FourCc {
        FourCc::from_ascii(&[b'0' + self.level, self.tier.letter(), b'3', b'W'])
    }
}

/// Field meanings, for reference. **Not used for parsing.**
///
/// Recorded here so the next person does not have to re-derive them, and marked so nobody
/// mistakes them for a contract. Sources in `docs/PROTOCOL-NOTES.md`.
pub mod layout {
    /// `STAR`, `SEXP`, `W2BN` — nine space-delimited fields.
    ///
    /// Ladder rating, ladder rank, wins, spawned (0/1), league id, high ladder rating,
    /// IronMan rating (W2BN only), IronMan rank (W2BN only), icon code (StarCraft only).
    /// The extended StarCraft form arrived in patch 1.10.
    pub const STARCRAFT_FIELDS: usize = 9;

    /// `WAR3`, `W3XP` — icon code, level, and an optional reversed clan tag.
    ///
    /// May be as few as zero fields for a user with no stats yet.
    pub const WARCRAFT3_FIELDS: usize = 3;

    /// `DRTL` — level, class, difficulty dots, strength, magic, dexterity, vitality,
    /// gold, spawned. **Client-supplied**, so a bot may send anything at all.
    pub const DIABLO_FIELDS: usize = 9;

    /// The default a Diablo client gets when it sends an empty statstring.
    pub const DIABLO_DEFAULT: &[u8] = b"LTRD 1 0 0 30 10 20 25 0 0";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product;

    /// Every product a user can log in with. The safety test below has to cover all of them,
    /// so a product added later is caught rather than quietly skipped.
    const EVERY_PRODUCT: &[FourCc] = &[
        product::STAR,
        product::SEXP,
        product::SSHR,
        product::JSTR,
        product::W2BN,
        product::DRTL,
        product::DSHR,
        product::D2DV,
        product::D2XP,
        product::WAR3,
        product::W3XP,
    ];

    #[test]
    fn nothing_we_advertise_can_overflow_a_diablo_two_client() {
        // The invariant `d2_is_safe` documents, held over everything we build. A D2-tagged
        // statstring that is neither the bare tag nor twice-comma'd is a heap overflow in
        // every 1.14d client in the channel, so this is the test that must never be relaxed.
        for &p in EVERY_PRODUCT {
            let s = build_default(p);
            assert!(d2_is_safe(&s), "build_default({p}) = {:?} would overflow a D2 client", String::from_utf8_lossy(&s));
        }
        // And over a real realm character, whose two commas are what bound the client's copies.
        let portrait = crate::d2::Portrait { class: crate::d2::class::SORCERESS, status: crate::d2::status::EXPANSION, level: 1, progression: 0 };
        assert!(d2_is_safe(&portrait.chat_statstring(product::D2XP, "bncc", "Tyrael")));
    }

    #[test]
    fn a_diablo_two_user_with_no_character_is_just_the_tag() {
        // The client's own "no character" form: the NUL right after the tag takes the branch
        // at 0x00446E6A, which never reaches a copy loop.
        assert_eq!(build_default(product::D2DV), b"VD2D");
        assert_eq!(build_default(product::D2XP), b"PX2D");
    }

    #[test]
    fn the_shape_that_crashed_a_real_client_is_refused() {
        // What we used to send. It reached a real 1.14d client on 2026-09-18 and took it down
        // on sight: a D2 tag, a byte after it, and not one comma to stop the copy.
        assert!(!d2_is_safe(b"VD2D 0 0 0 0 0 0 0 0 VD2D"));
        assert!(!d2_is_safe(b"PX2D 0 0 0 0 0 0 0 0 PX2D"));
        // One comma is not enough — the client runs two copy loops, and the second one is
        // what walks the furthest.
        assert!(!d2_is_safe(b"PX2Dbncc,Tyrael"));
        assert!(d2_is_safe(b"PX2Dbncc,Tyrael,anything"));
        // The Japanese tag parses down the same branch, so it is held to the same rule.
        assert!(!d2_is_safe(b"TS2D 0 0"));
        // Other products never enter that parser and are none of this check's business.
        assert!(d2_is_safe(b"RATS 0 0 0 0 0 0 0 0 RATS"));
        assert!(d2_is_safe(build_default(product::DRTL).as_slice()));
        assert!(d2_is_safe(b""), "too short to carry a tag");
    }

    #[test]
    fn diablo_gets_its_documented_default_statstring() {
        // DRTL's default must be the level-1 Warrior line, and must match the documented
        // constant exactly so the two never drift.
        assert_eq!(build_default(product::DRTL), layout::DIABLO_DEFAULT);
        // Shareware carries the same fresh-character stats under its own (reversed) tag.
        assert_eq!(build_default(product::DSHR), b"RHSD 1 0 0 30 10 20 25 0 0");
        // The default parses back as a Diablo statstring: product tag + nine fields.
        let default = build_default(product::DRTL);
        let s = Statstring::parse(&default);
        assert_eq!(s.product(), Some(product::DRTL));
        assert_eq!(s.fields().len(), layout::DIABLO_FIELDS);
        assert_eq!(s.field_u32(0), Some(1), "level 1");
        assert_eq!(s.field_u32(3), Some(30), "warrior strength");
    }

    #[test]
    fn a_starcraft_record_fills_the_ladder_fields() {
        assert_eq!(build_starcraft(product::SEXP, Record::default()), build_default(product::SEXP));
        let record = Record { rating: 1016, rank: 3, wins: 12, high_rating: 1040, iron_rating: 990, iron_rank: 7 };
        let line = build_starcraft(product::W2BN, record);
        assert_eq!(line, b"NB2W 1016 3 12 0 0 1040 990 7 NB2W");
        let s = Statstring::parse(&line);
        assert_eq!(s.product(), Some(product::W2BN));
        assert_eq!(s.fields().len(), layout::STARCRAFT_FIELDS);
    }

    #[test]
    fn a_non_diablo_default_keeps_the_generic_shape() {
        // Unchanged for everything else: reversed tag, eight zeros, reversed tag.
        assert_eq!(build_default(product::SEXP), b"PXES 0 0 0 0 0 0 0 0 PXES");
    }

    #[test]
    fn the_product_tag_is_read_reversed() {
        // It is a u32 rendered as text, so STAR reads as RATS.
        let s = Statstring::parse(b"RATS 0 0 0 0 0 0 0 0");
        assert_eq!(s.product(), Some(product::STAR));
        assert_eq!(s.fields().len(), 8);
    }

    #[test]
    fn fields_are_addressable_and_defensive() {
        let s = Statstring::parse(b"RATS 1200 5 42");
        assert_eq!(s.field_u32(0), Some(1200));
        assert_eq!(s.field_u32(1), Some(5));
        assert_eq!(s.field_u32(2), Some(42));
        assert_eq!(s.field_u32(3), None, "past the end is None, not a panic");
        assert_eq!(s.field(99), None);
    }

    #[test]
    fn a_non_numeric_field_is_none_rather_than_zero() {
        // Returning 0 would silently turn a malformed statstring into a plausible one.
        let s = Statstring::parse(b"RATS abc");
        assert_eq!(s.field_u32(0), None);
    }

    #[test]
    fn an_empty_statstring_parses_to_nothing() {
        let s = Statstring::parse(b"");
        assert_eq!(s.product(), None);
        assert!(s.fields().is_empty());
        assert_eq!(s.wc3_icon_code(), None);
    }

    #[test]
    fn the_raw_bytes_are_preserved_for_echoing() {
        // The server stores and echoes; re-serialising would normalise a format we do
        // not own.
        let raw = b"RATS  1200   5  ";
        assert_eq!(Statstring::parse(raw).raw(), raw);
    }

    #[test]
    fn warcraft_three_icon_codes_round_trip() {
        let code = IconCode {
            level: 2,
            tier: Tier::Human,
        };
        assert_eq!(code.to_fourcc().to_string(), "2H3W");
        assert_eq!(IconCode::parse(code.to_fourcc()), Some(code));

        for (text, level, tier) in [
            ("1R3W", 1, Tier::Random),
            ("3U3W", 3, Tier::Undead),
            ("4N3W", 4, Tier::NightElf),
            ("5O3W", 5, Tier::Orc),
            ("6D3W", 6, Tier::Tournament),
        ] {
            let parsed = IconCode::parse(FourCc::from_ascii(
                text.as_bytes().try_into().unwrap(),
            ))
            .unwrap_or_else(|| panic!("{text} did not parse"));
            assert_eq!(parsed.level, level);
            assert_eq!(parsed.tier, tier);
        }
    }

    #[test]
    fn malformed_icon_codes_are_refused() {
        for text in ["2H3X", "0H3W", "7H3W", "2Z3W", "ABCD", "2H3w"] {
            assert_eq!(
                IconCode::parse(FourCc::from_ascii(text.as_bytes().try_into().unwrap())),
                None,
                "{text} should not parse"
            );
        }
    }

    #[test]
    fn a_warcraft_three_statstring_yields_its_icon() {
        let s = Statstring::parse(b"3WAR 2H3W 14");
        assert_eq!(s.wc3_icon_code().map(|c| c.to_string()), Some("2H3W".into()));
    }

    #[test]
    fn a_statstring_with_no_icon_field_does_not_invent_one() {
        // W3XP users have been observed with a level and clan tag but no icon field.
        // Mistaking the level for an icon code would select the wrong icon for everyone.
        let s = Statstring::parse(b"3WAR 14 XYZ");
        assert_eq!(s.wc3_icon_code(), None);

        // And a user with no stats at all sends nothing.
        let s = Statstring::parse(b"3WAR");
        assert_eq!(s.wc3_icon_code(), None);
    }

    #[test]
    fn icon_selection_works_end_to_end_from_a_statstring() {
        // The join this module exists for: statstring -> icon code -> icons.bni entry.
        use crate::bni::{select_icon, BniFile, BniIcon};

        let file = BniFile {
            version: crate::bni::VERSION,
            icons: vec![
                BniIcon {
                    flags: crate::chat::user_flags::OPERATOR,
                    width: 14,
                    height: 14,
                    codes: vec![],
                },
                BniIcon {
                    flags: 0,
                    width: 14,
                    height: 14,
                    codes: vec![FourCc::from_ascii(b"2H3W")],
                },
            ],
            image: Vec::new(),
        };

        let s = Statstring::parse(b"3WAR 2H3W 14");
        assert_eq!(select_icon(&file, 0, s.wc3_icon_code()), Some(1));
        // An operator matches by flag first, whatever their statstring says.
        assert_eq!(
            select_icon(&file, crate::chat::user_flags::OPERATOR, s.wc3_icon_code()),
            Some(0)
        );
    }

    #[test]
    fn parser_never_panics_on_arbitrary_input() {
        let mut seed = 0x0BAD_F00Du32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let n = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let s = Statstring::parse(&bytes);
            let _ = s.product();
            let _ = s.wc3_icon_code();
            let _ = s.field_u32(0);
            let _ = s.field(3);
        }
    }
}
