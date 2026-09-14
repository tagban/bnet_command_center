//! `ItemTypes.txt`, `Weapons.txt`, `Armor.txt` and `Misc.txt`: what items exist and the columns
//! that decide how a character wearing one is drawn.
//!
//! The engine loads the three item tables into one list — weapons, then armour, then misc — and
//! an item's class id is its place in that list. Item types are numbered by row, `Expansion`
//! markers skipped, and a type "is a" type it names in `Equiv1`/`Equiv2`, transitively
//! (`ITEMS_CheckItemTypeId`, `0x00629B50`, reads the matrix built from those columns).

use d2_formats::excel::Table;

use crate::Error;

/// A three-letter item or graphics code, space-padded to four bytes as the engine stores it.
pub type Code = [u8; 4];

/// A code as the engine stores it: up to four bytes, space-padded.
#[must_use]
pub fn code(s: &str) -> Code {
    let mut out = *b"    ";
    for (slot, b) in out.iter_mut().zip(s.bytes()) {
        *slot = b;
    }
    out
}

/// A code without its padding.
#[must_use]
pub fn code_str(c: &Code) -> String {
    String::from_utf8_lossy(c).trim_end().to_string()
}

/// Item types the appearance rules test, by code. `Game.exe` names them by id — in 1.14d's table
/// `tors` is 3, `helm` 37, `weap` 45, `armo` 50, `shld` 51 and `circ` 75.
pub mod types {
    /// Body armour.
    pub const ARMOR: &str = "tors";
    /// Helms.
    pub const HELM: &str = "helm";
    /// Weapons.
    pub const WEAPON: &str = "weap";
    /// Anything worn.
    pub const ANY_ARMOR: &str = "armo";
    /// Shields.
    pub const ANY_SHIELD: &str = "shld";
    /// Circlets, which the character is drawn without.
    pub const CIRCLET: &str = "circ";
}

/// One `ItemTypes.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemType {
    /// `ItemType`.
    pub name: String,
    /// `Code`.
    pub code: String,
    /// `BodyLoc1`/`BodyLoc2`: where an item of the type is worn (`head`, `tors`, `rarm`, …).
    pub body_locations: Vec<String>,
    /// Every type this one is, itself included.
    ancestors: Vec<i32>,
}

/// `ItemTypes.txt`, by type id.
#[derive(Debug, Clone, Default)]
pub struct ItemTypes {
    rows: Vec<ItemType>,
}

impl ItemTypes {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Code` or `Equiv1` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Code", "Equiv1"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "itemtypes.txt", problem: format!("no {column} column") });
            }
        }
        let raw: Vec<(String, String, [String; 2], Vec<String>)> = t
            .rows()
            .map(|row| {
                let text = |c: &str| row.get(c).unwrap_or_default().to_string();
                let locations = ["BodyLoc1", "BodyLoc2"].iter().map(|c| text(c)).filter(|s| !s.is_empty()).collect();
                (text("ItemType"), text("Code"), [text("Equiv1"), text("Equiv2")], locations)
            })
            .collect();
        let id_of = |code: &str| raw.iter().position(|r| !code.is_empty() && r.1 == code).map(|i| i as i32);
        let rows = raw
            .iter()
            .enumerate()
            .map(|(id, (name, code, _, locations))| {
                let mut ancestors = vec![id as i32];
                let mut at = 0;
                while at < ancestors.len() {
                    let (_, _, equiv, _) = &raw[ancestors[at] as usize];
                    for parent in equiv.iter().filter_map(|e| id_of(e)) {
                        if !ancestors.contains(&parent) {
                            ancestors.push(parent);
                        }
                    }
                    at += 1;
                }
                ItemType { name: name.clone(), code: code.clone(), body_locations: locations.clone(), ancestors }
            })
            .collect();
        Ok(Self { rows })
    }

    /// A type by id.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&ItemType> {
        usize::try_from(id).ok().and_then(|i| self.rows.get(i))
    }

    /// A type's id by its code.
    #[must_use]
    pub fn id(&self, code: &str) -> Option<i32> {
        self.rows.iter().position(|r| !code.is_empty() && r.code == code).map(|i| i as i32)
    }

    /// Whether `id` is `parent` or inherits from it (`0x00629B50`).
    #[must_use]
    pub fn is_a(&self, id: i32, parent: i32) -> bool {
        self.get(id).is_some_and(|t| t.ancestors.contains(&parent))
    }

    /// Whether `id` is the type coded `parent` or inherits from it.
    #[must_use]
    pub fn is(&self, id: i32, parent: &str) -> bool {
        self.id(parent).is_some_and(|p| self.is_a(id, p))
    }
}

/// Which table an item row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFile {
    /// `Weapons.txt`.
    Weapons,
    /// `Armor.txt`.
    Armor,
    /// `Misc.txt`.
    Misc,
}

/// One item row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDef {
    /// `name`: the designers' name, not what the game shows (that is [`Self::name_key`]).
    pub name: String,
    /// `namestr`: the string-table key of the shown name.
    pub name_key: String,
    /// `code`.
    pub code: Code,
    /// `alternategfx`: the graphics the item is drawn with; `None` when blank.
    pub alternate_gfx: Option<Code>,
    /// `type`, as a type id (-1 when unknown).
    pub item_type: i32,
    /// `component`: the body part it is drawn on (0 head, 5 right hand, 6 left hand, 7 shield,
    /// 10 special 3; 16 not drawn).
    pub component: i32,
    /// `rArm`, `lArm`, `Torso`, `Legs`, `rSPad`, `lSPad`: body armour's weight per part (0 light,
    /// 1 medium, 2 heavy).
    pub armor_pieces: [u8; 6],
    /// `wclass`: the weapon's animation class (`1hs`, `bow`, …).
    pub weapon_class: Option<Code>,
    /// `Transform`: which colour palette an item's tint picks from.
    pub transform: i32,
    /// The table it came from.
    pub file: ItemFile,
}

/// Every item, in class id order.
#[derive(Debug, Clone, Default)]
pub struct Items {
    types: ItemTypes,
    rows: Vec<ItemDef>,
}

impl Items {
    /// Parse the four tables.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if an item table lacks `code` or `type`, or [`ItemTypes::from_table`]'s.
    pub fn from_tables(itemtypes: &Table, weapons: &Table, armor: &Table, misc: &Table) -> Result<Self, Error> {
        let types = ItemTypes::from_table(itemtypes)?;
        let mut rows = Vec::new();
        for (table, file, name) in
            [(weapons, ItemFile::Weapons, "weapons.txt"), (armor, ItemFile::Armor, "armor.txt"), (misc, ItemFile::Misc, "misc.txt")]
        {
            for column in ["code", "type"] {
                if table.column(column).is_none() {
                    return Err(Error::BadTable { table: name, problem: format!("no {column} column") });
                }
            }
            rows.extend(table.rows().map(|row| {
                let text = |c: &str| row.get(c).unwrap_or_default();
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let piece = |c: &str| row.int(c).unwrap_or(0).clamp(0, 2) as u8;
                ItemDef {
                    name: text("name").trim().to_string(),
                    name_key: text("namestr").to_string(),
                    code: code(text("code")),
                    alternate_gfx: row.get("alternategfx").map(code),
                    item_type: types.id(text("type")).unwrap_or(-1),
                    component: row.get("component").map_or(16, |_| int("component")),
                    armor_pieces: [piece("rArm"), piece("lArm"), piece("Torso"), piece("Legs"), piece("rSPad"), piece("lSPad")],
                    weapon_class: row.get("wclass").map(code),
                    transform: int("Transform"),
                    file,
                }
            }));
        }
        Ok(Self { types, rows })
    }

    /// The item types.
    #[must_use]
    pub fn types(&self) -> &ItemTypes {
        &self.types
    }

    /// An item by class id.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&ItemDef> {
        usize::try_from(class).ok().and_then(|i| self.rows.get(i))
    }

    /// Every item, in class id order.
    pub fn iter(&self) -> impl Iterator<Item = &ItemDef> {
        self.rows.iter()
    }

    /// How many items there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> ItemTypes {
        ItemTypes::from_table(&Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\tBodyLoc1\tBodyLoc2\r\n\
              Weapon\tweap\t\t\t\t\r\n\
              Melee Weapon\tmele\tweap\t\t\t\r\n\
              Expansion\r\n\
              Hand to Hand\th2h\tmele\tassn\trarm\tlarm\r\n\
              Assassin Item\tassn\t\t\t\t\r\n",
        ))
        .unwrap()
    }

    #[test]
    fn types_inherit_through_both_equivalents() {
        let t = types();
        assert_eq!(t.id("h2h"), Some(2), "the Expansion marker takes no id");
        assert!(t.is_a(2, 0), "h2h → mele → weap");
        assert!(t.is_a(2, 3), "h2h → assn");
        assert!(t.is_a(1, 1));
        assert!(!t.is_a(0, 1));
        assert!(!t.is_a(-1, 0));
        assert_eq!(t.get(2).unwrap().body_locations, ["rarm", "larm"]);
    }

    #[test]
    fn items_keep_class_id_order_across_tables() {
        let itemtypes = Table::parse(b"ItemType\tCode\tEquiv1\r\nHelm\thelm\t\r\nAxe\taxe\t\r\n");
        let weapons = Table::parse(b"name\tcode\ttype\talternateGfx\tcomponent\twclass\r\nHand Axe\thax\taxe\t\t5\t1hs\r\n");
        let armor = Table::parse(b"name\tcode\ttype\talternategfx\tcomponent\tTorso\r\nWar Hat\txap\thelm\tcap\t0\t\r\n");
        let misc = Table::parse(b"name\tcode\ttype\r\nArrows\taqv\t\r\n");
        let items = Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap();
        assert_eq!(items.len(), 3);
        let hand_axe = items.get(0).unwrap();
        assert_eq!((hand_axe.code, hand_axe.alternate_gfx, hand_axe.item_type), (*b"hax ", None, 1));
        assert_eq!(hand_axe.weapon_class, Some(*b"1hs "));
        let war_hat = items.get(1).unwrap();
        assert_eq!((war_hat.alternate_gfx, war_hat.component, war_hat.file), (Some(*b"cap "), 0, ItemFile::Armor));
        assert_eq!(items.get(2).unwrap().component, 16, "no component column: not drawn");
        assert_eq!(code_str(b"cap "), "cap");
    }
}
