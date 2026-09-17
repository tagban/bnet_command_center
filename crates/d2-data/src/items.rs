//! `ItemTypes.txt`, `Weapons.txt`, `Armor.txt` and `Misc.txt`: what items exist and the columns
//! that decide how a character wearing one is drawn.
//!
//! The engine loads the three item tables into one list — weapons, then armour, then misc — and
//! an item's class id is its place in that list. Item types are numbered by row, `Expansion`
//! markers skipped, and a type "is a" type it names in `Equiv1`/`Equiv2`, transitively
//! (`ITEMS_CheckItemTypeId`, `0x00629B50`, reads the matrix built from those columns).

use std::collections::HashMap;

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

/// A class's index from its three-letter code (`ama` 0 … `ass` 6), as the tables name classes.
#[must_use]
pub fn class_index(code: &str) -> Option<u8> {
    ["ama", "sor", "nec", "pal", "bar", "dru", "ass"].iter().position(|c| *c == code).map(|i| i as u8)
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
    /// `Beltable`: whether an item of the type goes in a belt.
    pub beltable: bool,
    /// `Throwable` (`+0x10`).
    pub throwable: bool,
    /// `StaffMods` (`+0x1F`): the class whose skills a normal, superior, magic or rare item of the
    /// type may carry.
    pub staff_mods: Option<u8>,
    /// `Class` (`+0x21`): the class an item of the type is for.
    pub class: Option<u8>,
    /// `Magic`: an item of the type is always magic (`+0x14`).
    pub always_magic: bool,
    /// `Rare`: an item of the type can be rare (`+0x15`).
    pub can_be_rare: bool,
    /// `Normal`: an item of the type is always normal (`+0x16`).
    pub always_normal: bool,
    /// `TreasureClass`: the engine builds `<code><level>` treasure classes for it (`+0x1D`).
    pub treasure_class: bool,
    /// `Rarity`: an item's weight in those classes (`+0x1E`).
    pub rarity: i32,
    /// `MaxSock1`, `MaxSock25`, `MaxSock40`: most sockets by item level.
    pub max_sockets: [i32; 3],
    /// `VarInvGfx`: how many inventory pictures an item of the type picks from (`+0x23`).
    pub var_inv_gfx: i32,
    /// `Quiver` (`+0x0E`): a quiver's launcher type is named — arrows and bolts, priced by the
    /// piece.
    pub quiver: bool,
    /// `StorePage` (`+0x22`): the trade window tab an item of the type is shown on, as a
    /// `StorePage.txt` row ([`STORE_PAGES`]).
    pub store_page: Option<u8>,
    /// Every type this one is, itself included.
    ancestors: Vec<i32>,
}

/// `StorePage.txt`'s codes, by row: the tabs of a vendor's trade window.
pub const STORE_PAGES: [&str; 4] = ["armo", "weap", "mag", "misc"];

/// The vendors whose stock columns the item tables carry, in the engine's order of them
/// (`Misc.txt`'s `AkaraMin` is `+0x146`, `GheedMin` `+0x147`, `CharsiMin` `+0x148`, …), which is not
/// the tables' column order. `Hralti` is spelled as the column is.
pub const VENDORS: [&str; 17] =
    ["Akara", "Gheed", "Charsi", "Fara", "Lysander", "Drognan", "Hralti", "Alkor", "Ormus", "Elzix", "Asheara", "Cain", "Halbu", "Jamella", "Malah", "Larzuk", "Drehya"];

/// What one vendor stocks of an item: `<vendor>Min`, `Max`, `MagicMin`, `MagicMax`, `MagicLvl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VendorStock {
    /// `…Min`: fewest normal ones.
    pub min: u8,
    /// `…Max`: most normal ones.
    pub max: u8,
    /// `…MagicMin`: fewest magic ones.
    pub magic_min: u8,
    /// `…MagicMax`: most magic ones.
    pub magic_max: u8,
    /// `…MagicLvl`: the store level magic ones need.
    pub magic_level: u8,
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
        // Each row with its two equivalents, its ancestors filled in below.
        let mut rows: Vec<(ItemType, [String; 2])> = t
            .rows()
            .map(|row| {
                let text = |c: &str| row.get(c).unwrap_or_default().to_string();
                let body_locations = ["BodyLoc1", "BodyLoc2"].iter().map(|c| text(c)).filter(|s| !s.is_empty()).collect();
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let row = ItemType {
                    name: text("ItemType"),
                    code: text("Code"),
                    body_locations,
                    beltable: int("Beltable") != 0,
                    throwable: int("Throwable") != 0,
                    staff_mods: row.get("StaffMods").and_then(class_index),
                    class: row.get("Class").and_then(class_index),
                    always_magic: int("Magic") != 0,
                    can_be_rare: int("Rare") != 0,
                    always_normal: int("Normal") != 0,
                    treasure_class: int("TreasureClass") != 0,
                    rarity: int("Rarity"),
                    max_sockets: [int("MaxSock1"), int("MaxSock25"), int("MaxSock40")],
                    var_inv_gfx: int("VarInvGfx"),
                    quiver: row.get("Quiver").is_some_and(|q| !q.is_empty()),
                    store_page: row.get("StorePage").and_then(|p| STORE_PAGES.iter().position(|c| *c == p)).map(|i| i as u8),
                    ancestors: Vec::new(),
                };
                (row, [text("Equiv1"), text("Equiv2")])
            })
            .collect();
        let id_of = |rows: &[(ItemType, [String; 2])], code: &str| rows.iter().position(|r| !code.is_empty() && r.0.code == code).map(|i| i as i32);
        for id in 0..rows.len() {
            let mut ancestors = vec![id as i32];
            let mut at = 0;
            while at < ancestors.len() {
                for parent in rows[ancestors[at] as usize].1.iter().filter_map(|e| id_of(&rows, e)) {
                    if !ancestors.contains(&parent) {
                        ancestors.push(parent);
                    }
                }
                at += 1;
            }
            rows[id].0.ancestors = ancestors;
        }
        let rows = rows.into_iter().map(|(row, _)| row).collect();
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

    /// Every type, by id.
    pub fn iter(&self) -> impl Iterator<Item = &ItemType> {
        self.rows.iter()
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
    /// `type2`, as a type id (-1 when blank).
    pub item_type2: i32,
    /// `component`: the body part it is drawn on (0 head, 5 right hand, 6 left hand, 7 shield,
    /// 10 special 3; 16 not drawn).
    pub component: i32,
    /// `rArm`, `lArm`, `Torso`, `Legs`, `rSPad`, `lSPad`: body armour's weight per part (0 light,
    /// 1 medium, 2 heavy).
    pub armor_pieces: [u8; 6],
    /// `wclass`: the weapon's animation class (`1hs`, `bow`, …).
    pub weapon_class: Option<Code>,
    /// `2handedwclass`: its class when a Barbarian holds it in both hands.
    pub two_handed_class: Option<Code>,
    /// `Transform`: which colour palette an item's tint picks from.
    pub transform: i32,
    /// The table it came from.
    pub file: ItemFile,
    /// `invwidth`/`invheight`: inventory cells.
    pub inv_size: (u8, u8),
    /// `compactsave`: saved and sent as a simple item — no quality, stats or sockets
    /// (Game.exe sets item flag `0x200000` from it, `0x006312B0`).
    pub compact: bool,
    /// `autobelt`: picked up straight into a free belt slot (`0x0063C600`).
    pub auto_belt: bool,
    /// `stackable`: carries a quantity.
    pub stackable: bool,
    /// `useable`: right-clicking it uses it.
    pub useable: bool,
    /// `quest`: a quest item.
    pub quest: bool,
    /// `pSpell`: what using it does (3 a healing or mana potion, 5 a rejuvenation potion).
    pub spell: i32,
    /// `len`: frames its effect lasts, 0 for at once.
    pub duration: i32,
    /// `stat1`–`stat3` with `calc1`–`calc3`, where both are given and the calc is a number.
    pub effects: Vec<(String, i32)>,
    /// `level`: the item's quality level, which places it in the `<type><level>` treasure
    /// classes and against the monster's level in the quality roll (`+0xFD`).
    pub level: i32,
    /// `levelreq`.
    pub level_req: i32,
    /// `version`: 0 for classic, 100 for an item only in Lord of Destruction (`+0xF6`).
    pub version: i32,
    /// `spawnable` (`+0x133`).
    pub spawnable: bool,
    /// `unique`: only ever unique (`+0x129`).
    pub unique_only: bool,
    /// `durability`: its maximum (`+0x112`); 0 with [`Self::no_durability`].
    pub durability: i32,
    /// `nodurability`.
    pub no_durability: bool,
    /// `minac`, `maxac` (`+0xCC`, `+0xD0`).
    pub defense: (i32, i32),
    /// `block` (`+0x111`).
    pub block: i32,
    /// `speed` (`+0xD8`).
    pub speed: i32,
    /// `mindam`, `maxdam` (`+0xFE`, `+0xFF`).
    pub damage: (i32, i32),
    /// `2handmindam`, `2handmaxdam` (`+0x102`, `+0x103`).
    pub two_hand_damage: (i32, i32),
    /// `minmisdam`, `maxmisdam` (`+0x100`, `+0x101`).
    pub missile_damage: (i32, i32),
    /// `reqstr`, `reqdex`.
    pub requirements: (i32, i32),
    /// `gemsockets`: most sockets the base item takes.
    pub gem_sockets: i32,
    /// `minstack`, `maxstack`, `spawnstack`.
    pub stack: (i32, i32, i32),
    /// `normcode`, `ubercode`, `ultracode`: the item's normal, exceptional and elite tiers.
    pub tiers: [Code; 3],
    /// `auto prefix`: the `AutoMagic.txt` group an expansion item may get (`+0xF8`).
    pub auto_prefix: i32,
    /// `2handed`.
    pub two_handed: bool,
    /// `belt`: the `Belts.txt` row a belt uses.
    pub belt: i32,
    /// `hasinv`: the item can have sockets (`+0x137`).
    pub has_inv: bool,
    /// `magic lvl`: added to the item level for its affix level (`+0x140`).
    pub magic_level: i32,
    /// `StrBonus`, `DexBonus` (`+0x106`, `+0x108`): hundredths of a percent of damage per point
    /// of strength or dexterity.
    pub str_bonus: i32,
    /// See [`Self::str_bonus`].
    pub dex_bonus: i32,
    /// `cost` (`+0xE0`): the base of every price.
    pub cost: i32,
    /// `gamble cost` (`+0xD4`).
    pub gamble_cost: i32,
    /// `bitfield1` (`+0xDC`): bit 0 lets a vendor stock magic ones.
    pub bitfield1: i32,
    /// Each vendor's stock of the item, in [`VENDORS`] order.
    pub vendors: [VendorStock; 17],
    /// `PermStoreItem` (`+0x1A4`): always in a vendor's stock, never sold out.
    pub perm_store_item: bool,
    /// `NightmareUpgrade`, `HellUpgrade` (`+0x19C`, `+0x1A0`): what a vendor stocks in its place on
    /// those difficulties; `None` for `xxx`.
    pub upgrades: [Option<Code>; 2],
}

/// Every item, in class id order.
#[derive(Debug, Clone, Default)]
pub struct Items {
    types: ItemTypes,
    rows: Vec<ItemDef>,
    by_code: HashMap<Code, i32>,
}

/// An inventory size from its column: 1 to 15 cells (the item bits' grid fields are four bits).
fn piece_size(v: Option<i64>) -> u8 {
    v.unwrap_or(1).clamp(1, 15) as u8
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
                    item_type2: types.id(text("type2")).unwrap_or(-1),
                    component: row.get("component").map_or(16, |_| int("component")),
                    armor_pieces: [piece("rArm"), piece("lArm"), piece("Torso"), piece("Legs"), piece("rSPad"), piece("lSPad")],
                    weapon_class: row.get("wclass").map(code),
                    two_handed_class: row.get("2handedwclass").map(code),
                    transform: int("Transform"),
                    file,
                    inv_size: (piece_size(row.int("invwidth")), piece_size(row.int("invheight"))),
                    compact: int("compactsave") != 0,
                    auto_belt: int("autobelt") != 0,
                    stackable: int("stackable") != 0,
                    useable: int("useable") != 0,
                    quest: int("quest") != 0,
                    spell: int("pSpell"),
                    duration: int("len"),
                    effects: (1..=3)
                        .filter_map(|i| {
                            let stat = row.get(&format!("stat{i}")).filter(|s| !s.is_empty())?;
                            Some((stat.to_string(), row.get(&format!("calc{i}"))?.trim().parse().ok()?))
                        })
                        .collect(),
                    level: int("level"),
                    level_req: int("levelreq"),
                    version: int("version"),
                    spawnable: int("spawnable") != 0,
                    unique_only: int("unique") != 0,
                    durability: int("durability"),
                    no_durability: int("nodurability") != 0,
                    defense: (int("minac"), int("maxac")),
                    block: int("block"),
                    speed: int("speed"),
                    damage: (int("mindam"), int("maxdam")),
                    two_hand_damage: (int("2handmindam"), int("2handmaxdam")),
                    missile_damage: (int("minmisdam"), int("maxmisdam")),
                    requirements: (int("reqstr"), int("reqdex")),
                    gem_sockets: int("gemsockets"),
                    stack: (int("minstack"), int("maxstack"), int("spawnstack")),
                    tiers: [code(text("normcode")), code(text("ubercode")), code(text("ultracode"))],
                    auto_prefix: int("auto prefix"),
                    two_handed: int("2handed") != 0,
                    belt: int("belt"),
                    has_inv: int("hasinv") != 0,
                    magic_level: int("magic lvl"),
                    str_bonus: int("StrBonus"),
                    dex_bonus: int("DexBonus"),
                    cost: int("cost"),
                    gamble_cost: int("gamble cost"),
                    bitfield1: int("bitfield1"),
                    vendors: VENDORS.map(|v| {
                        let byte = |c: &str| row.int(&format!("{v}{c}")).unwrap_or(0).clamp(0, 255) as u8;
                        VendorStock { min: byte("Min"), max: byte("Max"), magic_min: byte("MagicMin"), magic_max: byte("MagicMax"), magic_level: byte("MagicLvl") }
                    }),
                    perm_store_item: int("PermStoreItem") != 0,
                    upgrades: ["NightmareUpgrade", "HellUpgrade"].map(|c| row.get(c).filter(|u| !u.is_empty() && *u != "xxx").map(code)),
                }
            }));
        }
        let mut by_code = HashMap::new();
        for (class, row) in rows.iter().enumerate() {
            by_code.entry(row.code).or_insert(class as i32);
        }
        Ok(Self { types, rows, by_code })
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

    /// An item's class id by its code (the first row with it).
    #[must_use]
    pub fn class_of(&self, code: &Code) -> Option<i32> {
        self.by_code.get(code).copied()
    }

    /// Whether item `class` — its `type` or `type2` — is the type coded `parent` or inherits
    /// from it (`0x00629BB0`).
    #[must_use]
    pub fn is(&self, class: i32, parent: &str) -> bool {
        self.types.id(parent).is_some_and(|p| self.is_type(class, p))
    }

    /// Whether item `class` — its `type` or `type2` — is type `parent` or inherits from it.
    #[must_use]
    pub fn is_type(&self, class: i32, parent: i32) -> bool {
        self.get(class).is_some_and(|d| self.types.is_a(d.item_type, parent) || (d.item_type2 >= 0 && self.types.is_a(d.item_type2, parent)))
    }

    /// Whether item `class` is an exceptional or elite tier (`0x00629F70`).
    #[must_use]
    pub fn is_uber(&self, class: i32) -> bool {
        self.get(class).is_some_and(|d| d.tiers[0] != *b"    " && d.code != d.tiers[0])
    }

    /// The potion families a belt column stacks, as the lists at `DAT_00744660` name them.
    const BELT_FAMILIES: [&[&[u8; 4]]; 3] =
        [&[b"hp1 ", b"hp2 ", b"hp3 ", b"hp4 ", b"hp5 "], &[b"mp1 ", b"mp2 ", b"mp3 ", b"mp4 ", b"mp5 "], &[b"rvs ", b"rvl "]];

    /// Whether a picked-up item of class `new` stacks on top of one of class `belted` in a belt
    /// column (`0x00628A40`): the same item, or two potions of one family — any two healing
    /// potions, any two mana potions, or the two rejuvenation potions. Anything else, an antidote
    /// or a scroll among them, only matches its own kind.
    #[must_use]
    pub fn same_belt_kind(&self, new: i32, belted: i32) -> bool {
        if new == belted {
            return true;
        }
        let (Some(a), Some(b)) = (self.get(new), self.get(belted)) else { return false };
        Self::BELT_FAMILIES.iter().any(|family| family.contains(&&a.code) && family.contains(&&b.code))
    }

    /// Whether item `class` is of a beltable type.
    #[must_use]
    pub fn beltable(&self, class: i32) -> bool {
        self.get(class).and_then(|d| self.types.get(d.item_type)).is_some_and(|t| t.beltable)
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

    #[test]
    fn potions_carry_their_belt_and_use_columns() {
        let itemtypes = Table::parse(b"ItemType\tCode\tEquiv1\tBeltable\r\nPotion\tpoti\t\t1\r\nHealing Potion\thpot\tpoti\t1\r\nGem\tgem\t\t0\r\n");
        let empty = Table::parse(b"name\tcode\ttype\r\n");
        let misc = Table::parse(
            b"name\tcode\ttype\tinvwidth\tinvheight\tcompactsave\tautobelt\tuseable\tpSpell\tlen\tstat1\tcalc1\tstat2\tcalc2\r\n\
              Minor Healing Potion\thp1\thpot\t1\t1\t1\t1\t1\t3\t192\thpregen\t30\t\t\r\n\
              Chipped Ruby\tgcr\tgem\t1\t1\t1\t0\t0\t\t\t\t\t\t\r\n\
              Skeleton Key\tkey\tgem\t1\t1\t0\t0\t0\t\t\t\t\t\t\r\n",
        );
        let items = Items::from_tables(&itemtypes, &empty, &empty, &misc).unwrap();
        let hp1 = items.class_of(&code("hp1")).unwrap();
        let def = items.get(hp1).unwrap();
        assert_eq!((def.inv_size, def.compact, def.auto_belt, def.useable, def.spell, def.duration), ((1, 1), true, true, true, 3, 192));
        assert_eq!(def.effects, [("hpregen".to_string(), 30)]);
        assert!(items.beltable(hp1), "the type's own column");
        let ruby = items.class_of(&code("gcr")).unwrap();
        assert!(!items.beltable(ruby) && items.get(ruby).unwrap().effects.is_empty());
        assert!(!items.get(items.class_of(&code("key")).unwrap()).unwrap().compact);
        assert_eq!(items.class_of(&code("zzz")), None);
    }

    #[test]
    fn a_belt_column_stacks_potions_of_one_family() {
        let itemtypes = Table::parse(b"ItemType\tCode\tEquiv1\tBeltable\r\nPotion\tpoti\t\t1\r\n");
        let empty = Table::parse(b"name\tcode\ttype\r\n");
        let misc = Table::parse(
            b"name\tcode\ttype\r\nMinor Healing Potion\thp1\tpoti\r\nGreater Healing Potion\thp4\tpoti\r\n\
              Minor Mana Potion\tmp1\tpoti\r\nRejuvenation Potion\trvs\tpoti\r\n\
              Full Rejuvenation Potion\trvl\tpoti\r\nAntidote Potion\typs\tpoti\r\n",
        );
        let items = Items::from_tables(&itemtypes, &empty, &empty, &misc).unwrap();
        let class = |c: &str| items.class_of(&code(c)).unwrap();
        assert!(items.same_belt_kind(class("hp1"), class("hp4")), "any two healing potions");
        assert!(items.same_belt_kind(class("rvs"), class("rvl")), "both rejuvenations");
        assert!(items.same_belt_kind(class("yps"), class("yps")), "an antidote on its own kind");
        assert!(!items.same_belt_kind(class("hp1"), class("mp1")), "healing does not stack on mana");
        assert!(!items.same_belt_kind(class("rvs"), class("hp1")), "nor rejuvenation on healing");
        assert!(!items.same_belt_kind(class("yps"), class("hp1")));
        assert!(!items.same_belt_kind(-1, class("hp1")), "no such item");
    }
}
