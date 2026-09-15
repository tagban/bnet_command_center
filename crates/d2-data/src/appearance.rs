//! How a character's equipment is drawn outside a game — on the character-select screen and in
//! chat, from sixteen graphics bytes and sixteen tint bytes.
//!
//! A save's header keeps them at `0x88` and `0x98`, one per body component (`HD TR LG RA LA RH
//! LH SH S1..S8`); a realm character's 33-byte portrait carries the first eleven of each
//! (`bnetcc_proto::d2`), which is how a chat client — or a bot — can draw a character.
//! `PLRSAVE2_WriteSaveHeader` (`0x00568F20`) fills them with `0x0063E510`, and this module
//! reproduces that.
//!
//! **Graphics values.** A value is not an item: it is a slot in one table of graphics codes the
//! engine builds the first time it needs one (`0x0063D710`). Slots 1..=3 are the body-armour
//! weights `lit`, `med` and `hvy`; the rest are filled in class id order with each weapon's,
//! helm's, shield's and body armour's `alternategfx` (its `code` when blank), once per code.
//! A code takes the next free slot, except that a weapon skips a slot `Game.exe`'s own list of
//! graphics (at `0x00744CA8`) gives a weapon, and armour skips one it gives armour; a code
//! that skipped is placed again by the next item drawn with it, so a few codes hold more than
//! one slot. Looking a code up (`0x0063D900`) takes the lowest slot holding the item's
//! graphics or its own code; 0 means none.
//!
//! **What each piece writes.** Items worn on the head or in a hand (`0x0063DA70`) write their
//! value into their `component` — except a circlet, which is not drawn — and a one-handed
//! weapon (`1hs`, `1ht`, `ht1`) goes to the right hand when it is the character's active
//! weapon and the left hand otherwise (`0x0063C050`). With a crossbow the left hand repeats
//! the right hand's value (`0x0063D930`). Body armour (`0x0063D690`) writes a weight to six
//! components — torso, legs, both arms, both shoulders — as the `lit`/`med`/`hvy` slot of
//! `ArmType.txt`'s token for its `Torso`/`Legs`/`rArm`/`lArm`/`rSPad`/`lSPad`.
//!
//! **Tints.** A tinted component's byte is `transform * 32 + colour + 1`, kept to a byte: the
//! item's `Transform` palette (1, 2, 5..=8) and a `Colors.txt` row; `0xFF` is untinted
//! (`0x0062C100`, `0x0062A250`).

use crate::items::{self, Code, ItemDef, Items};

/// Graphics table slots, 0 unused.
pub const SLOTS: usize = 255;

/// The sixteen body components, in save order.
pub const COMPONENTS: [&str; 16] = ["HD", "TR", "LG", "RA", "LA", "RH", "LH", "SH", "S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8"];

/// Components by index.
pub mod component {
    /// Head.
    pub const HEAD: usize = 0;
    /// Torso.
    pub const TORSO: usize = 1;
    /// Legs.
    pub const LEGS: usize = 2;
    /// Right arm.
    pub const RIGHT_ARM: usize = 3;
    /// Left arm.
    pub const LEFT_ARM: usize = 4;
    /// Right hand.
    pub const RIGHT_HAND: usize = 5;
    /// Left hand.
    pub const LEFT_HAND: usize = 6;
    /// Shield.
    pub const SHIELD: usize = 7;
    /// Right shoulder.
    pub const RIGHT_SHOULDER: usize = 8;
    /// Left shoulder.
    pub const LEFT_SHOULDER: usize = 9;
    /// Special 3: a Necromancer's shrunken head.
    pub const SPECIAL_3: usize = 10;
}

/// The weight slots body armour draws its parts with.
const WEIGHTS: [Code; 3] = [*b"lit ", *b"med ", *b"hvy "];
/// Weapon classes worn in either hand (`0x007446A0`, hand classes 2, 3 and 12).
const EITHER_HAND: [Code; 3] = [*b"1hs ", *b"1ht ", *b"ht1 "];
/// A crossbow's class.
const CROSSBOW: Code = *b"xbw ";

/// The engine's graphics table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graphics {
    /// Codes as little-endian integers, 0 for an empty slot.
    slots: [u32; SLOTS + 1],
    /// The class id of the item that filled each slot.
    sources: [Option<usize>; SLOTS + 1],
}

impl Graphics {
    /// Build the table (`0x0063D710`). `reserved` is `Game.exe`'s list of `(code, item type)`
    /// by slot (`EngineData::reserved_graphics`).
    #[must_use]
    pub fn build(items: &Items, reserved: &[(Code, i32)]) -> Self {
        let types = items.types();
        let id = |code: &str| types.id(code).unwrap_or(-1);
        let (weapon_type, armor_type) = (id(items::types::WEAPON), id(items::types::ANY_ARMOR));
        let drawn_types = [weapon_type, id(items::types::ARMOR), id(items::types::ANY_SHIELD), id(items::types::HELM)];
        let circlet_type = id(items::types::CIRCLET);
        let mut slots = [0u32; SLOTS + 1];
        let mut sources = [None; SLOTS + 1];
        for (slot, weight) in slots[1..=3].iter_mut().zip(WEIGHTS) {
            *slot = u32::from_le_bytes(weight);
        }
        let reserved_type = |slot: usize| reserved.get(slot).map_or(0, |&(_, t)| t);
        let mut next = 4;
        for (class, item) in items.iter().enumerate() {
            let gfx = u32::from_le_bytes(item.alternate_gfx.unwrap_or(item.code));
            let t = item.item_type;
            let drawn = drawn_types.iter().any(|&p| types.is_a(t, p)) && !types.is_a(t, circlet_type);
            if slots[..next].contains(&gfx) || !drawn {
                continue;
            }
            let weapon = types.is_a(t, weapon_type);
            let armor = types.is_a(t, armor_type);
            let mut at = next;
            while at < SLOTS
                && ((weapon && types.is_a(reserved_type(at), weapon_type))
                    || (armor && types.is_a(reserved_type(at), armor_type))
                    || slots[at] != 0)
            {
                at += 1;
            }
            if at >= SLOTS {
                at = next;
            }
            slots[at] = gfx;
            sources[at] = Some(class);
            if at == next {
                next += 1;
            }
        }
        Self { slots, sources }
    }

    /// The class id of the item whose graphics filled a slot; `None` for the weights and empty
    /// slots.
    #[must_use]
    pub fn source(&self, value: u8) -> Option<usize> {
        self.sources.get(usize::from(value)).copied().flatten()
    }

    /// The value an item with graphics `gfx` (its code when blank) and code `code` is saved
    /// with: the lowest slot holding either (`0x0063D900`); 0 for none.
    #[must_use]
    pub fn value(&self, gfx: Option<Code>, code: Code) -> u8 {
        let gfx = gfx.map_or(0, u32::from_le_bytes);
        let code = u32::from_le_bytes(code);
        (1..SLOTS).find(|&i| self.slots[i] == gfx || self.slots[i] == code).map_or(0, |i| i as u8)
    }

    /// The code in a slot.
    #[must_use]
    pub fn code(&self, value: u8) -> Option<Code> {
        let slot = *self.slots.get(usize::from(value))?;
        (value != 0 && slot != 0).then(|| slot.to_le_bytes())
    }

    /// Every filled slot, in order.
    pub fn iter(&self) -> impl Iterator<Item = (u8, Code)> + '_ {
        (1..=SLOTS).filter_map(|i| self.code(i as u8).map(|c| (i as u8, c)))
    }
}

/// How one item is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Body armour: the weight values for torso, legs, right arm, left arm, right shoulder and
    /// left shoulder.
    BodyArmor([u8; 6]),
    /// One component's value.
    Part {
        /// The component written.
        component: usize,
        /// The graphics value.
        value: u8,
        /// A one-handed weapon: in the left hand instead when it is not the active weapon.
        either_hand: bool,
        /// A crossbow: the left hand repeats it.
        crossbow: bool,
    },
    /// Worn where the character is drawn, but drawn with nothing: a circlet, a graphic the table
    /// lacks, or a part past the sixteen components.
    Nothing,
}

/// The components body armour writes, in [`Look::BodyArmor`] order, with the `armor_pieces`
/// column each reads.
pub const BODY_ARMOR_PARTS: [(usize, usize); 6] = [
    (component::TORSO, 2),
    (component::LEGS, 3),
    (component::RIGHT_ARM, 0),
    (component::LEFT_ARM, 1),
    (component::RIGHT_SHOULDER, 4),
    (component::LEFT_SHOULDER, 5),
];

/// How `item` is drawn when worn, or `None` if it is not worn on the head, the torso or in a
/// hand. `armor_types` is `ArmType.txt`'s tokens in row order.
#[must_use]
pub fn look(items: &Items, graphics: &Graphics, armor_types: &[Code], item: &ItemDef) -> Option<Look> {
    let types = items.types();
    let locations = &types.get(item.item_type)?.body_locations;
    let worn = |at: &str| locations.iter().any(|l| l == at);
    if worn("tors") {
        let mut values = [0u8; 6];
        for (out, &(_, column)) in values.iter_mut().zip(BODY_ARMOR_PARTS.iter()) {
            let Some(&token) = armor_types.get(usize::from(item.armor_pieces[column])) else { return Some(Look::Nothing) };
            match graphics.value(Some(token), token) {
                0 => return Some(Look::Nothing),
                v => *out = v,
            }
        }
        return Some(Look::BodyArmor(values));
    }
    if !(worn("head") || worn("rarm") || worn("larm")) {
        return None;
    }
    if types.is(item.item_type, items::types::CIRCLET) {
        return Some(Look::Nothing);
    }
    let component = usize::try_from(item.component).unwrap_or(usize::MAX);
    let value = graphics.value(item.alternate_gfx, item.code);
    if component >= COMPONENTS.len() || value == 0 {
        return Some(Look::Nothing);
    }
    let class = item.weapon_class;
    Some(Look::Part {
        component,
        value,
        either_hand: class.is_some_and(|c| EITHER_HAND.contains(&c)),
        crossbow: class == Some(CROSSBOW),
    })
}

/// A tint byte: `transform * 32 + colour + 1`, or `0xFF` when the palette has no such tint
/// (`0x0062A250`, `0x00600C20`).
#[must_use]
pub fn tint(transform: i32, color: i32) -> u8 {
    let palette = transform < 9 && transform != 0 && !(3..=4).contains(&transform);
    if !palette || !(0..21).contains(&color) {
        return 0xFF;
    }
    ((transform * 32 + color + 1) & 0xFF) as u8
}

/// A tint byte's `(transform, colour)`; `None` for untinted. Transform 8 reads back as 0: its
/// byte overflows.
#[must_use]
pub fn untint(byte: u8) -> Option<(u8, u8)> {
    (byte != 0xFF && byte != 0).then(|| ((byte - 1) >> 5, (byte - 1) & 0x1F))
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_formats::excel::Table;

    /// Made-up items in the tables' real shape.
    fn items() -> Items {
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\tBodyLoc1\tBodyLoc2\r\n\
              None\t\t\t\t\t\r\n\
              Weapon\tweap\t\t\t\t\r\n\
              Axe\taxe\tweap\t\trarm\tlarm\r\n\
              Any Armor\tarmo\t\t\t\t\r\n\
              Helm\thelm\tarmo\t\thead\t\r\n\
              Armor\ttors\tarmo\t\ttors\t\r\n\
              Circlet\tcirc\thelm\t\thead\t\r\n\
              Bow\tbow\tweap\t\trarm\tlarm\r\n",
        );
        let weapons = Table::parse(
            b"name\tcode\ttype\talternateGfx\tcomponent\twclass\r\n\
              A\taaa\taxe\t\t5\t1hs\r\n\
              B\tbbb\taxe\taaa\t5\t1hs\r\n\
              C\tccc\tbow\t\t6\tbow\r\n",
        );
        let armor = Table::parse(
            b"name\tcode\ttype\talternategfx\tcomponent\trArm\tlArm\tTorso\tLegs\trSPad\tlSPad\r\n\
              H\thhh\thelm\t\t0\t\t\t\t\t\t\r\n\
              T\tttt\ttors\t\t1\t0\t1\t2\t0\t1\t1\r\n\
              R\trrr\tcirc\tlit\t0\t\t\t\t\t\t\r\n",
        );
        let misc = Table::parse(b"name\tcode\ttype\tcomponent\r\n");
        Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap()
    }

    fn slot(code: &[u8; 4], item_type: i32) -> (Code, i32) {
        (*code, item_type)
    }

    #[test]
    fn codes_fill_slots_once_skipping_slots_reserved_for_their_kind() {
        let items = items();
        // Slot 4 is reserved for a helm (type 4), slot 5 for a weapon (type 2).
        let reserved = [slot(b"    ", 1), slot(b"lit ", 1), slot(b"med ", 1), slot(b"hvy ", 1), slot(b"cap ", 4), slot(b"axe ", 2)];
        let g = Graphics::build(&items, &reserved);
        assert_eq!(g.code(1), Some(*b"lit "));
        assert_eq!(g.code(4), Some(*b"aaa "), "a weapon may take a helm's slot");
        assert_eq!(g.code(5), Some(*b"hhh "), "the bow skipped the weapon slot; the helm took it");
        assert_eq!(g.code(6), Some(*b"ccc "));
        assert_eq!(g.code(7), Some(*b"ttt "));
        assert_eq!(g.value(Some(*b"aaa "), *b"bbb "), 4, "B draws as A");
        assert_eq!(g.value(Some(*b"yyy "), *b"zzz "), 0);
        assert_eq!(g.value(None, *b"zzz "), 8, "blank graphics match the first empty slot, as in the engine");
        assert_eq!(g.iter().count(), 7, "the circlet adds nothing");
    }

    #[test]
    fn each_kind_of_item_writes_its_components() {
        let items = items();
        let g = Graphics::build(&items, &[]);
        let weights = [*b"lit ", *b"med ", *b"hvy "];
        let by_code = |c: &[u8; 4]| items.iter().find(|i| &i.code == c).unwrap();
        assert_eq!(
            look(&items, &g, &weights, by_code(b"bbb ")),
            Some(Look::Part { component: component::RIGHT_HAND, value: 4, either_hand: true, crossbow: false })
        );
        assert!(matches!(look(&items, &g, &weights, by_code(b"ccc ")), Some(Look::Part { component: 6, either_hand: false, .. })));
        // Torso 2 (hvy), legs 0 (lit), rArm 0, lArm 1, rSPad 1, lSPad 1.
        assert_eq!(look(&items, &g, &weights, by_code(b"ttt ")), Some(Look::BodyArmor([3, 1, 1, 2, 2, 2])));
        assert_eq!(look(&items, &g, &weights, by_code(b"rrr ")), Some(Look::Nothing), "circlets are not drawn");
    }

    /// With the operator's install and `Game.exe` (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`):
    /// the table has the values BNETDocs' "Chat Statstrings" lists for 1.14-era characters, and —
    /// given a libd2 checkout (`LIBD2_DIR`) — the appearance bytes of its sample save
    /// `EpicSorc.d2s` are what this module computes for the items that character wears.
    #[test]
    fn with_a_real_install_values_match_saved_characters() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = crate::GameData::load(dir).expect("rules");
        let engine = crate::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).expect("Game.exe");
        let items = data.items();
        let types = items.types();
        let ids = [items::types::ARMOR, items::types::HELM, items::types::WEAPON, items::types::ANY_ARMOR, items::types::ANY_SHIELD, items::types::CIRCLET]
            .map(|c| types.id(c));
        assert_eq!(ids, [Some(3), Some(37), Some(45), Some(50), Some(51), Some(75)], "the ids Game.exe tests");
        let g = Graphics::build(items, &engine.reserved_graphics);
        let value = |code: &[u8; 4]| g.value(Some(*code), *code);
        assert_eq!([value(b"hax "), value(b"ob1 "), value(b"am2 ")], [0x04, 0x33, 0x37], "weapons");
        assert_eq!([value(b"cap "), value(b"bhm "), value(b"dr1 "), value(b"ba5 ")], [0x39, 0x53, 0x56, 0x5B], "helms");
        assert_eq!([value(b"buc "), value(b"tow "), value(b"pa1 "), value(b"pa5 ")], [0x4F, 0x52, 0x5C, 0x5E], "shields");
        assert_eq!(g.iter().map(|(_, c)| c).filter(|c| c == b"lbb ").count(), 5, "a skipped code holds more than one slot");

        let Ok(libd2) = std::env::var("LIBD2_DIR") else {
            return;
        };
        let save = std::fs::read(std::path::Path::new(&libd2).join("packages/save/src/testdata/EpicSorc.d2s")).unwrap();
        let (gfx, tints) = (&save[0x88..0x98], &save[0x98..0xA8]);
        let worn = |code: &[u8; 4]| {
            let item = items.iter().find(|i| &i.code == code).unwrap();
            (item, look(items, &g, data.armor_types(), item).unwrap())
        };
        // Worn: a Diadem, a Dusk Shroud, an Eldritch Orb and a Monarch.
        assert_eq!((worn(b"ci3 ").1, gfx[component::HEAD]), (Look::Nothing, 0xFF), "circlets are not drawn");
        let (shroud, body) = worn(b"uui ");
        let Look::BodyArmor(parts) = body else { panic!("{body:?}") };
        for (&(c, _), v) in BODY_ARMOR_PARTS.iter().zip(parts) {
            assert_eq!(gfx[c], v, "{}", COMPONENTS[c]);
            assert_eq!(untint(tints[c]).map(|(t, _)| i32::from(t)), Some(shroud.transform));
        }
        assert!(matches!(worn(b"obc ").1, Look::Part { component: component::RIGHT_HAND, value: 0x33, either_hand: true, .. }));
        assert_eq!(gfx[component::RIGHT_HAND], 0x33, "the orb, worn in the left hand, is the active weapon");
        assert!(matches!(worn(b"uit ").1, Look::Part { component: component::SHIELD, value: 0x51, .. }));
        assert_eq!(gfx[component::SHIELD], 0x51);
    }

    #[test]
    fn tints_pack_palette_and_colour() {
        assert_eq!(tint(2, 3), 0x44);
        assert_eq!(tint(5, 9), 0xAA);
        assert_eq!(tint(3, 1), 0xFF, "no palette 3");
        assert_eq!(tint(0, 1), 0xFF);
        assert_eq!(tint(8, 0), 1, "transform 8 overflows the byte");
        assert_eq!(untint(0x44), Some((2, 3)));
        assert_eq!(untint(0xFF), None);
    }
}
