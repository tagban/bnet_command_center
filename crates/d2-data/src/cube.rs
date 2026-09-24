//! `CubeMain.txt`: the Horadric Cube's recipes, read as the engine's loader reads them
//! (`0x00669130`, one 0x148-byte row each): the row's own tests, up to seven inputs (`0x00668600`
//! parses each) and up to three outputs (`0x00668A90`).
//!
//! An input names its item by `any`, an item type's code, an item's code (the type is tried
//! first), or a unique or set item's name; an output tries the item's code before the type's, and
//! also takes `usetype`, `useitem` and the three portals. Modifiers after the first token are
//! split on `=` and `,` and compared case-sensitively; an unknown one ends the list. A column
//! that names nothing leaves its row unusable [I: the engine's loader returns 0 for it].

use d2_formats::excel::Table;

use crate::affixes::{Affixes, Mod};
use crate::items::{class_index, code, Items};

/// An input's flags (`+0x00` of the 8-byte input).
pub mod input {
    /// `any` or an item's code: [`super::InputItem::Any`] or [`super::InputItem::Class`].
    pub const ITEM: u16 = 0x01;
    /// An item type's code.
    pub const TYPE: u16 = 0x02;
    /// `nos`: no sockets.
    pub const NO_SOCKETS: u16 = 0x04;
    /// `sock`: has sockets (no count is read).
    pub const SOCKETED: u16 = 0x08;
    /// `eth`.
    pub const ETHEREAL: u16 = 0x10;
    /// `noe`.
    pub const NOT_ETHEREAL: u16 = 0x20;
    /// A unique or set item by name.
    pub const NAMED: u16 = 0x40;
    /// `upg`: the listed base's family at its tier or higher.
    pub const UPGRADE: u16 = 0x80;
    /// `bas`: a normal-tier base.
    pub const BASIC: u16 = 0x100;
    /// `exc`: an exceptional base.
    pub const EXCEPTIONAL: u16 = 0x200;
    /// `eli`: an elite base.
    pub const ELITE: u16 = 0x400;
    /// `nru`: not a runeword.
    pub const NO_RUNEWORD: u16 = 0x800;
}

/// An output's flags (`+0x00` of the 0x54-byte output).
pub mod output {
    /// `mod`: the input itself, rebased (the upgrade recipes).
    pub const MOD: u16 = 0x01;
    /// `sock=N`.
    pub const SOCKETS: u16 = 0x02;
    /// `eth`.
    pub const ETHEREAL: u16 = 0x04;
    /// A unique or set item by name.
    pub const NAMED: u16 = 0x08;
    /// `uns`: what the input held in its sockets is lost.
    pub const UNSOCKET: u16 = 0x10;
    /// `rem`: what the input held in its sockets comes back.
    pub const REMOVE: u16 = 0x20;
    /// `reg`: the input made again.
    pub const REGENERATE: u16 = 0x40;
    /// `exc`: the input's exceptional base.
    pub const EXCEPTIONAL: u16 = 0x80;
    /// `eli`: the input's elite base.
    pub const ELITE: u16 = 0x100;
    /// `rep`: repaired.
    pub const REPAIR: u16 = 0x200;
    /// `rch`: recharged.
    pub const RECHARGE: u16 = 0x400;
}

/// What an input matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputItem {
    /// `any` (id `0xFFFF`).
    Any,
    /// An `Items` class (a unique or set by name matches its base's class).
    Class(i32),
    /// An `ItemTypes` row.
    Type(i32),
}

/// One input column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubeInput {
    /// The item.
    pub item: InputItem,
    /// [`input`] flags.
    pub flags: u16,
    /// A quality to match, 1–9; 0 any.
    pub quality: u8,
    /// `qty=N`, 0 when not given (counts as 1).
    pub quantity: u8,
    /// A named unique's or set item's row + 1, 0 when none.
    pub named_row: u16,
}

/// What an output makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// `Cow Portal` (kind 1).
    CowPortal,
    /// `Pandemonium Portal` (kind 2).
    PandemoniumPortal,
    /// `Pandemonium Finale Portal` (kind 3).
    PandemoniumFinalePortal,
    /// An item's code (kind 0xFC), with the `Items` class.
    Class(i32),
    /// An item type's code (kind 0xFD): a random item of the type.
    Type(i32),
    /// `usetype` (kind 0xFF), or `reg`: a new item of the first input's class.
    UseType,
    /// `useitem` (kind 0xFE): the first input itself.
    UseItem,
}

/// An output's mod: a property and the percent chance it is given (0 always).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubeMod {
    /// The property, as an affix carries one.
    pub property: Mod,
    /// `mod n chance`: 1–99 rolls, anything else always.
    pub chance: u8,
}

/// One output column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubeOutput {
    /// What it makes.
    pub kind: OutputKind,
    /// [`output`] flags.
    pub flags: u16,
    /// A named unique's or set item's row + 1, 0 when none.
    pub named_row: u16,
    /// Quality 1–9, 0 when not given.
    pub quality: u8,
    /// `qty=N` or `sock=N` — one field, the last written wins.
    pub quantity: u8,
    /// `lvl`: the item level, when not 0.
    pub level: u8,
    /// `plvl`: percent of the character's level.
    pub player_level: u8,
    /// `ilvl`: percent of the first input's item level.
    pub item_level: u8,
    /// `pre=N`: up to three `MagicPrefix.txt` ids (row + 1).
    pub prefixes: [u16; 3],
    /// `suf=N`: up to three `MagicSuffix.txt` ids.
    pub suffixes: [u16; 3],
    /// `mod 1`–`mod 5`.
    pub mods: Vec<CubeMod>,
}

/// One recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubeRecipe {
    /// `description`, for logs.
    pub description: String,
    /// `enabled`: a disabled row never matches.
    pub enabled: bool,
    /// `ladder`: only in a ladder game.
    pub ladder: bool,
    /// `min diff`.
    pub min_difficulty: u8,
    /// `class`: a character class, `None` for any.
    pub class: Option<u8>,
    /// `op`.
    pub op: u8,
    /// `param`.
    pub param: i32,
    /// `value`.
    pub value: i32,
    /// `numinputs`: how many items the cube must hold, exactly.
    pub num_inputs: u8,
    /// `version`: 100 and up only in an expansion game.
    pub version: u16,
    /// `input 1`–`7`, empty columns left out.
    pub inputs: Vec<CubeInput>,
    /// `output`, `output b`, `output c`.
    pub outputs: [Option<CubeOutput>; 3],
    /// Every column named something the tables have [I: the loader fails the row otherwise].
    pub usable: bool,
}

/// `CubeMain.txt`, in row order.
#[derive(Debug, Clone, Default)]
pub struct CubeRecipes {
    rows: Vec<CubeRecipe>,
}

/// A quality token (`low` … `tmp` → 1 … 9).
fn quality(token: &str) -> Option<u8> {
    ["low", "nor", "hiq", "mag", "set", "rar", "uni", "crf", "tmp"].iter().position(|q| *q == token).map(|i| i as u8 + 1)
}

/// The column's text without its quotes, and its first token.
fn split(text: &str) -> (String, Vec<String>) {
    let text: String = text.chars().filter(|&c| c != '"').collect();
    let mut parts = text.splitn(2, ',');
    let first = parts.next().unwrap_or_default().trim().to_string();
    let rest = parts.next().map_or_else(Vec::new, |r| r.split([',', '=']).map(|t| t.trim().to_string()).collect());
    (first, rest)
}

/// A number token.
fn number(token: Option<&String>) -> u16 {
    token.and_then(|t| t.parse::<i64>().ok()).map_or(0, |n| n.clamp(0, 0xFFFF) as u16)
}

/// A unique's or set item's row by its name, with its base's class: `(row + 1, class, set)`.
fn named(name: &str, items: &Items, affixes: &Affixes) -> Option<(u16, i32, bool)> {
    if let Some((row, u)) = affixes.uniques.iter().enumerate().find(|(_, u)| u.name == name) {
        return Some((row as u16 + 1, items.class_of(&u.code)?, false));
    }
    let (row, s) = affixes.set_items.iter().enumerate().find(|(_, s)| s.name == name)?;
    Some((row as u16 + 1, items.class_of(&s.code)?, true))
}

/// An input column (`0x00668600`). `None` for one that names nothing.
fn parse_input(text: &str, items: &Items, affixes: &Affixes) -> Option<CubeInput> {
    let (first, tokens) = split(text);
    let mut input = CubeInput { item: InputItem::Any, flags: 0, quality: 0, quantity: 0, named_row: 0 };
    if first.eq_ignore_ascii_case("any") {
        input.flags |= input::ITEM;
    } else if let Some(t) = (first.len() <= 4).then(|| items.types().id(&first)).flatten() {
        input.flags |= input::TYPE;
        input.item = InputItem::Type(t);
    } else if let Some(c) = (first.len() <= 4).then(|| items.class_of(&code(&first))).flatten() {
        input.flags |= input::ITEM;
        input.item = InputItem::Class(c);
    } else {
        let (row, class, set) = named(&first, items, affixes)?;
        input.flags |= input::ITEM | input::NAMED;
        input.quality = if set { 5 } else { 7 };
        input.named_row = row;
        input.item = InputItem::Class(class);
    }
    let mut it = tokens.iter();
    while let Some(token) = it.next() {
        let flag = match token.as_str() {
            "qty" => {
                input.quantity = number(it.next()).min(255) as u8;
                continue;
            }
            "nos" => input::NO_SOCKETS,
            "sock" => input::SOCKETED,
            "eth" => input::ETHEREAL,
            "noe" => input::NOT_ETHEREAL,
            "upg" => input::UPGRADE,
            "bas" => input::BASIC,
            "exc" => input::EXCEPTIONAL,
            "eli" => input::ELITE,
            "nru" => input::NO_RUNEWORD,
            q => match quality(q) {
                Some(q) => {
                    input.quality = q;
                    continue;
                }
                None => break,
            },
        };
        input.flags |= flag;
    }
    Some(input)
}

/// An output column (`0x00668A90`). `None` for one that names nothing.
fn parse_output(text: &str, items: &Items, affixes: &Affixes) -> Option<CubeOutput> {
    let (first, tokens) = split(text);
    let mut out = CubeOutput {
        kind: OutputKind::UseType,
        flags: 0,
        named_row: 0,
        quality: 0,
        quantity: 0,
        level: 0,
        player_level: 0,
        item_level: 0,
        prefixes: [0; 3],
        suffixes: [0; 3],
        mods: Vec::new(),
    };
    let portal = [("Cow Portal", OutputKind::CowPortal), ("Pandemonium Portal", OutputKind::PandemoniumPortal), ("Pandemonium Finale Portal", OutputKind::PandemoniumFinalePortal)];
    if let Some(&(_, kind)) = portal.iter().find(|(n, _)| first.eq_ignore_ascii_case(n)) {
        out.kind = kind;
    } else if first.eq_ignore_ascii_case("usetype") {
        out.kind = OutputKind::UseType;
    } else if first.eq_ignore_ascii_case("useitem") {
        out.kind = OutputKind::UseItem;
    } else if let Some(c) = (first.len() <= 4).then(|| items.class_of(&code(&first))).flatten() {
        out.kind = OutputKind::Class(c);
    } else if let Some(t) = (first.len() <= 4).then(|| items.types().id(&first)).flatten() {
        out.kind = OutputKind::Type(t);
    } else {
        let (row, class, set) = named(&first, items, affixes)?;
        out.kind = OutputKind::Class(class);
        out.flags |= output::NAMED;
        out.quality = if set { 5 } else { 7 };
        out.named_row = row;
        let level = if set { affixes.set_items.get(usize::from(row) - 1).map(|s| s.level) } else { affixes.uniques.get(usize::from(row) - 1).map(|u| u.level) };
        out.item_level = level.unwrap_or(0).clamp(0, 255) as u8;
    }
    let mut it = tokens.iter();
    while let Some(token) = it.next() {
        let flag = match token.as_str() {
            "qty" => {
                out.quantity = number(it.next()).min(255) as u8;
                continue;
            }
            "pre" | "suf" => {
                let slots = if token == "pre" { &mut out.prefixes } else { &mut out.suffixes };
                let id = number(it.next());
                if let Some(slot) = slots.iter_mut().find(|s| **s == 0) {
                    *slot = id;
                }
                continue;
            }
            "sock" => {
                out.quantity = number(it.next()).min(255) as u8;
                output::SOCKETS
            }
            "eth" => output::ETHEREAL,
            "mod" => output::MOD,
            "uns" => output::UNSOCKET,
            "rem" => output::REMOVE,
            "reg" => {
                out.kind = OutputKind::UseType;
                output::REGENERATE
            }
            "exc" => output::EXCEPTIONAL,
            "eli" => output::ELITE,
            "rep" => output::REPAIR,
            "rch" => output::RECHARGE,
            q => match quality(q) {
                Some(q) => {
                    out.quality = q;
                    continue;
                }
                None => break,
            },
        };
        out.flags |= flag;
    }
    Some(out)
}

impl CubeRecipes {
    /// Read `CubeMain.txt`, resolving its codes and names against the item and affix tables.
    #[must_use]
    pub fn from_table(table: &Table, items: &Items, affixes: &Affixes) -> Self {
        let rows = table
            .rows()
            .map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0);
                let text = |c: &str| row.get(c).map(str::trim).filter(|s| !s.is_empty());
                let mut usable = true;
                let inputs = (1..=7)
                    .filter_map(|i| text(&format!("input {i}")))
                    .filter_map(|t| {
                        let parsed = parse_input(t, items, affixes);
                        usable &= parsed.is_some();
                        parsed
                    })
                    .collect();
                let mut outputs: [Option<CubeOutput>; 3] = [None, None, None];
                for (n, (col, pre)) in [("output", ""), ("output b", "b "), ("output c", "c ")].into_iter().enumerate() {
                    let Some(t) = text(col) else { continue };
                    let Some(mut out) = parse_output(t, items, affixes) else {
                        usable = false;
                        continue;
                    };
                    let byte = |c: &str| row.int(&format!("{pre}{c}")).unwrap_or(0).clamp(0, 255) as u8;
                    out.level = byte("lvl");
                    out.player_level = byte("plvl");
                    // A named unique's or set item's level stands unless the column gives one.
                    if byte("ilvl") != 0 || out.flags & output::NAMED == 0 {
                        out.item_level = byte("ilvl");
                    }
                    out.mods = (1..=5)
                        .filter_map(|m| {
                            let code = text(&format!("{pre}mod {m}"))?.to_string();
                            let n = |c: &str| row.int(&format!("{pre}mod {m} {c}")).unwrap_or(0) as i32;
                            Some(CubeMod { property: Mod { code, param: n("param"), min: n("min"), max: n("max") }, chance: n("chance").clamp(0, 255) as u8 })
                        })
                        .collect();
                    outputs[n] = Some(out);
                }
                CubeRecipe {
                    description: row.get("description").unwrap_or_default().to_string(),
                    enabled: int("enabled") != 0,
                    ladder: int("ladder") != 0,
                    min_difficulty: int("min diff").clamp(0, 255) as u8,
                    class: text("class").and_then(class_index),
                    op: int("op").clamp(0, 255) as u8,
                    param: int("param") as i32,
                    value: int("value") as i32,
                    num_inputs: int("numinputs").clamp(0, 255) as u8,
                    version: int("version").clamp(0, 0xFFFF) as u16,
                    inputs,
                    outputs,
                    usable,
                }
            })
            .collect();
        Self { rows }
    }

    /// Every recipe, in row order — the order the matcher tries them.
    #[must_use]
    pub fn rows(&self) -> &[CubeRecipe] {
        &self.rows
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
    use crate::affixes::UniqueItem;

    fn items() -> Items {
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tEquiv2\r\nAny\tallx\t\t\r\nWeapon\tweap\tallx\t\r\nAxe\taxe\tweap\t\r\nAmulet\tamul\tallx\t\r\nQuest\tques\tallx\t\r\nRing\tring\tallx\t\r\n",
        );
        let weapons = Table::parse(b"name\tcode\ttype\r\nHand Axe\thax\taxe\r\nAxe\taxe\taxe\r\nStaff of Kings\tmsf\tques\r\nHoradric Staff\thst\tques\r\n");
        let armor = Table::parse(b"name\tcode\ttype\r\n");
        let misc = Table::parse(b"name\tcode\ttype\r\nAmulet of the Viper\tvip\tques\r\nAmulet\tamu\tamul\r\nRing\trin\tring\r\n");
        Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap()
    }

    #[test]
    fn inputs_try_the_type_first_and_outputs_the_item() {
        let (items, affixes) = (items(), Affixes::default());
        let axe_type = items.types().id("axe").unwrap();
        let axe_item = items.class_of(&code("axe")).unwrap();
        let input = parse_input("axe", &items, &affixes).unwrap();
        assert_eq!(input.item, InputItem::Type(axe_type));
        let out = parse_output("axe", &items, &affixes).unwrap();
        assert_eq!(out.kind, OutputKind::Class(axe_item));
    }

    #[test]
    fn modifiers_are_split_on_equals_and_commas_and_stop_at_an_unknown_one() {
        let (items, affixes) = (items(), Affixes::default());
        let input = parse_input("\"rin,mag,qty=3\"", &items, &affixes).unwrap();
        assert_eq!((input.quality, input.quantity), (4, 3));
        // A bare `sock` reads no count: the number that follows ends the list.
        let input = parse_input("any,sock,3,eth", &items, &affixes).unwrap();
        assert_eq!(input.flags, input::ITEM | input::SOCKETED, "eth after the number is never read");
        // Case matters.
        let input = parse_input("any,MAG", &items, &affixes).unwrap();
        assert_eq!(input.quality, 0);
        let out = parse_output("\"amu,mag,pre=331,suf=2,sock=1\"", &items, &affixes).unwrap();
        assert_eq!((out.quality, out.prefixes[0], out.suffixes[0], out.quantity), (4, 331, 2, 1));
        assert_eq!(out.flags, output::SOCKETS);
        let out = parse_output("useitem,mod,exc", &items, &affixes).unwrap();
        assert_eq!((out.kind, out.flags), (OutputKind::UseItem, output::MOD | output::EXCEPTIONAL));
    }

    #[test]
    fn a_unique_by_name_matches_its_base_and_row() {
        let items = items();
        let mut affixes = Affixes::default();
        affixes.uniques.push(UniqueItem {
            name: "The Ring".into(),
            version: 0,
            enabled: true,
            ladder: false,
            rarity: 1,
            no_limit: false,
            level: 29,
            level_req: 29,
            code: code("rin"),
            props: Vec::new(),
            cost: (0, 0),
        });
        let input = parse_input("The Ring", &items, &affixes).unwrap();
        assert_eq!((input.item, input.quality, input.named_row), (InputItem::Class(items.class_of(&code("rin")).unwrap()), 7, 1));
        assert!(parse_input("Nothing Like It", &items, &affixes).is_none());
    }

    #[test]
    fn a_row_reads_its_tests_inputs_outputs_and_mods() {
        let (items, affixes) = (items(), Affixes::default());
        let table = Table::parse(
            b"description\tenabled\tladder\tmin diff\tversion\top\tparam\tvalue\tclass\tnuminputs\tinput 1\tinput 2\tinput 3\tinput 4\tinput 5\tinput 6\tinput 7\toutput\tlvl\tplvl\tilvl\tmod 1\tmod 1 chance\tmod 1 param\tmod 1 min\tmod 1 max\toutput b\tb lvl\r\n\
Staff\t1\t\t\t\t28\t\t\t\t2\tmsf\tvip\t\t\t\t\t\thst\t\t\t\t\t\t\t\t\t\t\r\n\
Broken\t1\t\t\t\t\t\t\t\t1\tnothing\t\t\t\t\t\t\tamu\t\t50\t50\tdex\t40\t\t1\t3\t\t\r\n",
        );
        let cube = CubeRecipes::from_table(&table, &items, &affixes);
        let staff = &cube.rows()[0];
        assert!(staff.enabled && staff.usable);
        assert_eq!((staff.op, staff.num_inputs, staff.inputs.len()), (28, 2, 2));
        assert_eq!(staff.outputs[0].as_ref().unwrap().kind, OutputKind::Class(items.class_of(&code("hst")).unwrap()));
        assert!(staff.outputs[1].is_none());
        let broken = &cube.rows()[1];
        assert!(!broken.usable, "an input that names nothing");
        let out = broken.outputs[0].as_ref().unwrap();
        assert_eq!((out.player_level, out.item_level), (50, 50));
        assert_eq!(out.mods, vec![CubeMod { property: Mod { code: "dex".into(), param: 0, min: 1, max: 3 }, chance: 40 }]);
    }
}
