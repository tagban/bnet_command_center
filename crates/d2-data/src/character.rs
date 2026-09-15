//! Drawing a realm character the way the character-select screen does — animated, from the
//! appearance bytes of its 33-byte portrait.
//!
//! The screen (`CharSel.cpp`, `0x00439210`) builds a front-end unit from the portrait's class,
//! status bits and sixteen graphics and tint bytes (`0x005066C0`) and draws it every frame
//! (`0x00503BA0`). Reproduced here:
//!
//! - **Stance.** Town neutral (`TN`); a living hardcore character stands in neutral (`NU`); a dead
//!   hardcore character is drawn as the front end's class 8 or 9 (`RH`).
//! - **Weapon stance** from the right-hand, left-hand and shield bytes (`0x00504AF0`): each hand's
//!   item class (its `wclass`, or `2handedwclass` when both hands hold something or a lone weapon
//!   has a different two-handed class), then the pair (`1hs` + `1ht` is `1js`, and so on). An
//!   impossible pair makes the screen draw its fallback, a bare-handed Rogue, and so does this.
//! - **Parts.** A graphics byte is a slot of the front end's own graphics table (the same builder
//!   as the game's, `0x00506000`, with its copy of the reserved list at `0x0072E1E0`); an empty or
//!   0xFF byte draws `lit`. Old values in the armour and helm bytes are remapped as `0x00504D60`
//!   does. Each COF layer's part is
//!   `data\global\chars\<class>\<part>\<class><part><graphics><stance><layer weapon class>.dcc`;
//!   a part with no file is not drawn.
//! - **Tints.** A tint byte less one packs `transform * 32 + colour`; transform 0 is 8 (its byte
//!   overflowed), 3 and 4 have no maps, colours past 20 are untinted (`0x005038D0`). The maps are
//!   `data\global\items\palette\{grey,grey2,gold,brown,greybrown,invgrey,invgrey2,invgreybrown}.dat`,
//!   21 of 256 bytes each (`0x00505550`).
//! - **Frames.** Facing 0, layers in the COF's order for each frame. The COF's draw order is
//!   indexed by the facing itself; a part's frames come from the file direction the facing maps
//!   to (`0x00600C70`) — for the 16-direction character files, direction 4, toward the viewer. A counter in 256ths of a
//!   frame advances by the COF's speed each drawn frame and restarts when it reaches the last
//!   frame, so the last frame is never shown.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use d2_formats::cof::{self, Cof};
use d2_formats::dcc::{self, Dcc};
use d2_formats::gif;
use d2_formats::mpq::{self, ArchiveSet, CHARACTER_ARCHIVES};

use crate::appearance::{Graphics, SLOTS};
use crate::engine::{EngineData, FrontEndTables};
use crate::items::{self, Code, ItemTypes};
use crate::GameData;

/// Milliseconds per drawn frame. The game draws at 25 frames a second; the character screen is
/// assumed to match.
pub const TICK_MS: u32 = 40;
/// The palette characters are drawn in.
pub const PALETTE: &str = "data\\global\\palette\\act1\\pal.dat";
/// Tint map files, by transform 1..=8.
const TINT_FILES: [&str; 8] = ["grey", "grey2", "gold", "brown", "greybrown", "invgrey", "invgrey2", "invgreybrown"];
/// Colours per tint file.
const COLOURS: usize = 21;
/// Front-end animation modes.
const MODE_NEUTRAL: usize = 1;
const MODE_TOWN_NEUTRAL: usize = 5;
/// The front end's fallback: a Rogue, bare-handed, in town.
const FALLBACK_CLASS: usize = 7;
const HAND_TO_HAND: usize = 1;

/// Why a character could not be drawn.
#[derive(Debug)]
pub enum Error {
    /// The archives could not be read.
    Mpq(mpq::Error),
    /// A file the drawing needs is not in the install.
    Missing(String),
    /// A COF could not be parsed.
    Cof(String, cof::Error),
    /// A DCC could not be decoded.
    Dcc(String, dcc::Error),
    /// Nothing of the character could be drawn.
    Empty,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mpq(e) => write!(f, "{e}"),
            Self::Missing(name) => write!(f, "{name} is not in the install"),
            Self::Cof(name, e) => write!(f, "{name}: {e}"),
            Self::Dcc(name, e) => write!(f, "{name}: {e}"),
            Self::Empty => f.write_str("no part of the character has graphics"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mpq::Error> for Error {
    fn from(e: mpq::Error) -> Self {
        Self::Mpq(e)
    }
}

/// The bytes of a portrait that decide how the character looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Appearance {
    /// Class, 0 Amazon … 6 Assassin.
    pub class: u8,
    /// `.d2s` status bits: 0x04 hardcore, 0x08 dead.
    pub status: u8,
    /// Graphics value per component; the portrait carries the first eleven, the rest are 0xFF.
    pub graphics: [u8; 16],
    /// Tint byte per component, likewise.
    pub tints: [u8; 16],
}

impl Appearance {
    /// Read the appearance from a 33-byte portrait (a chat statstring's tail, or a character
    /// list entry). `None` if it is too short or its class byte is not 1..=7.
    #[must_use]
    pub fn from_portrait(portrait: &[u8]) -> Option<Self> {
        if portrait.len() < 27 {
            return None;
        }
        let class = portrait[13].checked_sub(1).filter(|&c| c <= 6)?;
        let mut graphics = [0xFF; 16];
        graphics[..11].copy_from_slice(&portrait[2..13]);
        let mut tints = [0xFF; 16];
        tints[..11].copy_from_slice(&portrait[14..25]);
        Some(Self { class, status: portrait[26] & 0x7F, graphics, tints })
    }

    /// The bytes that decide the look, for naming and caching: class, status, then the eleven
    /// graphics and eleven tint bytes a portrait carries.
    #[must_use]
    pub fn key(&self) -> [u8; 24] {
        let mut k = [0u8; 24];
        k[0] = self.class;
        k[1] = self.status & 0x0C;
        k[2..13].copy_from_slice(&self.graphics[..11]);
        k[13..24].copy_from_slice(&self.tints[..11]);
        k
    }
}

/// What the character screen resolves an appearance to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    /// Front-end class (0..=6 the player classes, 7 the fallback, 8 and 9 dead hardcore).
    pub class: usize,
    /// Animation mode.
    pub mode: usize,
    /// Weapon class, as the front end numbers them (1 `hth`).
    pub weapon_class: usize,
    /// Graphics code per component; `None` draws `lit`.
    pub parts: [Option<Code>; 16],
    /// Tint per component: `(transform, colour)`.
    pub tints: [Option<(usize, usize)>; 16],
}

/// A drawn animation: palette-indexed frames on one canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    /// Canvas width.
    pub width: usize,
    /// Canvas height.
    pub height: usize,
    /// Each frame's `width × height` palette indices; 0 is transparent.
    pub frames: Vec<Vec<u8>>,
    /// Each frame's duration in hundredths of a second.
    pub delays: Vec<u16>,
    /// The palette, RGB.
    pub palette: [[u8; 3]; 256],
}

impl Animation {
    /// An animated GIF that loops forever.
    #[must_use]
    pub fn to_gif(&self) -> Vec<u8> {
        gif::encode(&gif::Animation {
            width: self.width as u16,
            height: self.height as u16,
            palette: &self.palette,
            transparent: 0,
            delays: &self.delays,
            frames: &self.frames,
        })
    }
}

/// Item type ids the rules test.
#[derive(Debug, Clone, Copy)]
struct TypeIds {
    any_armor: i32,
    helm: i32,
    armor: i32,
    gloves: i32,
    boots: i32,
    shield: i32,
}

/// Everything needed to draw characters, loaded once.
#[derive(Debug)]
pub struct CharacterArt {
    archives: ArchiveSet,
    palette: [[u8; 3]; 256],
    /// Tint maps by `transform * 21 + colour`, transforms 0..=8 (0 unused).
    tints: Vec<[u8; 256]>,
    tables: FrontEndTables,
    graphics: Graphics,
    slot_hand: [i32; SLOTS + 1],
    slot_two_handed: [i32; SLOTS + 1],
    slot_type: [i32; SLOTS + 1],
    types: ItemTypes,
    ids: TypeIds,
}

impl CharacterArt {
    /// Load the palette, tint maps and graphics tables from the install in `dir`, with the item
    /// tables from `data` and the front end's tables from `engine`.
    ///
    /// # Errors
    ///
    /// [`Error`] if the archives, the palette or a tint map cannot be read.
    pub fn load(dir: impl AsRef<Path>, data: &GameData, engine: &EngineData) -> Result<Self, Error> {
        let archives = ArchiveSet::open(dir, &CHARACTER_ARCHIVES)?;
        let read = |name: &str| -> Result<Vec<u8>, Error> { archives.read(name)?.ok_or_else(|| Error::Missing(name.to_string())) };
        let pal = read(PALETTE)?;
        if pal.len() < 768 {
            return Err(Error::Missing(PALETTE.into()));
        }
        let palette = std::array::from_fn(|i| [pal[i * 3 + 2], pal[i * 3 + 1], pal[i * 3]]);
        let mut tints = vec![[0u8; 256]; 9 * COLOURS];
        for (t, file) in TINT_FILES.iter().enumerate() {
            let name = format!("data\\global\\items\\palette\\{file}.dat");
            let bytes = read(&name)?;
            for colour in 0..COLOURS {
                if let Some(map) = bytes.get(colour * 256..(colour + 1) * 256) {
                    tints[(t + 1) * COLOURS + colour].copy_from_slice(map);
                }
            }
        }
        let items = data.items();
        let types = items.types().clone();
        let id = |code: &str| types.id(code).unwrap_or(-1);
        let ids = TypeIds {
            any_armor: id(items::types::ANY_ARMOR),
            helm: id(items::types::HELM),
            armor: id(items::types::ARMOR),
            gloves: id("glov"),
            boots: id("boot"),
            shield: id(items::types::ANY_SHIELD),
        };
        let tables = engine.front_end.clone();
        let reserved: Vec<(Code, i32)> = tables.graphics.iter().map(|&(code, _, t)| (code, t)).collect();
        let graphics = Graphics::build(items, &reserved);
        let hand_of = |class: Option<Code>| {
            let row = class
                .and_then(|c| tables.item_weapon_classes.iter().find(|&&(code, _)| code == c))
                .map_or(0, |&(_, row)| row as usize);
            tables.hand_classes.get(row).copied().unwrap_or(0)
        };
        let mut slot_hand = [0; SLOTS + 1];
        let mut slot_two_handed = [0; SLOTS + 1];
        let mut slot_type = [0; SLOTS + 1];
        for v in 0..=SLOTS {
            match graphics.source(v as u8).and_then(|class| items.get(class as i32)) {
                Some(item) => {
                    slot_type[v] = item.item_type;
                    slot_hand[v] = hand_of(item.weapon_class);
                    slot_two_handed[v] = hand_of(item.two_handed_class);
                }
                None if (1..=3).contains(&v) => {
                    slot_type[v] = ids.armor;
                    slot_hand[v] = tables.hand_classes.first().copied().unwrap_or(0);
                }
                None => {}
            }
        }
        Ok(Self { archives, palette, tints, tables, graphics, slot_hand, slot_two_handed, slot_type, types, ids })
    }

    /// The front end's graphics table.
    #[must_use]
    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }

    fn static_code(&self, v: usize) -> Code {
        self.tables.graphics.get(v).map_or([0; 4], |g| g.0)
    }

    fn static_hand(&self, v: usize) -> i32 {
        self.tables.graphics.get(v).map_or(0, |g| g.1)
    }

    fn static_type(&self, v: usize) -> i32 {
        self.tables.graphics.get(v).map_or(0, |g| g.2)
    }

    /// The front end's slot holding `code`, searching from slot 0.
    fn find(&self, code: Code) -> Option<usize> {
        (0..SLOTS).find(|&i| self.graphics.code(i as u8).unwrap_or([0; 4]) == code)
    }

    /// `0x00504D60`: remap an out-of-date armour or helm value.
    fn remap(&self, component: usize, value: usize) -> usize {
        let mut v = value;
        if (1..=4).contains(&component) && v > 3 {
            for t in [self.ids.armor, self.ids.gloves, self.ids.boots] {
                if self.types.is_a(self.static_type(v), t) {
                    match self.find(self.static_code(v)) {
                        Some(i) => v = i,
                        None => v = v % 3 + 1,
                    }
                }
            }
            let shield = self.types.is_a(self.static_type(v), self.ids.shield).then(|| self.find(self.static_code(v))).flatten();
            match shield {
                Some(i) => v = i,
                None => v = v % 3 + 1,
            }
        }
        if component == 0 && !self.types.is_a(self.slot_type[v], self.ids.helm) {
            let helm = self.types.is_a(self.static_type(v), self.ids.helm).then(|| self.find(self.static_code(v))).flatten();
            v = helm.unwrap_or(1);
        }
        v
    }

    /// `0x00504AF0`: the weapon class the hands make; 0 when the pair is impossible.
    fn weapon_class(&self, class: usize, graphics: &[u8; 16]) -> usize {
        let (right, left, shield) = (graphics[5], graphics[6], graphics[7]);
        let both = right != 0xFF && left != 0xFF;
        let claws = |c: i32| (c == 13 || c == 14) && class != 6;
        let fix = |c: i32, v: usize| {
            if claws(c) || self.types.is_a(self.slot_type[v], self.ids.any_armor) {
                self.static_hand(v)
            } else {
                c
            }
        };
        let mut a = 0;
        if right != 0xFF {
            let v = usize::from(right);
            let lone_two_handed = left == 0xFF && shield == 0xFF && self.slot_two_handed[v] != self.slot_hand[v];
            a = if both || lone_two_handed { self.slot_two_handed[v] } else { self.slot_hand[v] };
            a = fix(a, v);
        }
        let mut b = 0;
        if left != 0xFF {
            let v = usize::from(left);
            b = if both { self.slot_two_handed[v] } else { self.slot_hand[v] };
            b = fix(b, v);
        }
        if claws(a) {
            a = 0;
        }
        if claws(b) {
            b = 0;
        }
        usize::try_from(combine_hands(a, b)).unwrap_or(0)
    }

    /// Resolve an appearance as the character screen does.
    #[must_use]
    pub fn look(&self, a: &Appearance) -> Look {
        let hardcore = a.status & 0x04 != 0;
        let dead = a.status & 0x08 != 0;
        let (class, mode) = match (hardcore, dead) {
            (true, true) => (if matches!(a.class, 0 | 1 | 6) { 8 } else { 9 }, MODE_TOWN_NEUTRAL),
            (true, false) => (usize::from(a.class), MODE_NEUTRAL),
            _ => (usize::from(a.class), MODE_TOWN_NEUTRAL),
        };
        let weapon_class = if class < 7 { self.weapon_class(class, &a.graphics) } else { HAND_TO_HAND };
        if weapon_class == 0 {
            let mut parts = [None; 16];
            parts[1] = self.graphics.code(1);
            return Look { class: FALLBACK_CLASS, mode: MODE_TOWN_NEUTRAL, weapon_class: HAND_TO_HAND, parts, tints: [None; 16] };
        }
        let parts = std::array::from_fn(|c| {
            let v = usize::from(a.graphics[c]);
            if v == 0 || v >= SLOTS {
                return None;
            }
            self.graphics.code(self.remap(c, v) as u8)
        });
        let tints = std::array::from_fn(|c| {
            let stored = a.tints[c].wrapping_sub(1);
            if a.tints[c] == 0xFF || stored == 0xFF {
                return None;
            }
            let (transform, colour) = (usize::from(stored >> 5), usize::from(stored & 0x1F));
            let transform = if transform == 0 { 8 } else { transform };
            (colour < COLOURS && !(3..=4).contains(&transform)).then_some((transform, colour))
        });
        Look { class, mode, weapon_class, parts, tints }
    }

    /// The file direction a facing index draws in a sprite file with `directions` directions
    /// (`0x00600C70`).
    #[must_use]
    pub fn file_direction(&self, directions: usize, facing: usize) -> usize {
        let row = if directions == 0 { 0 } else { directions.trailing_zeros() as usize + 1 };
        let index = self.tables.file_directions.get(row).and_then(|r| r.get(facing % 32)).copied().unwrap_or(0);
        usize::try_from(index).ok().filter(|&i| i < directions.max(1)).unwrap_or(0)
    }

    fn token(codes: &[Code], index: usize) -> String {
        codes.get(index).map_or_else(String::new, |c| String::from_utf8_lossy(c).trim_end_matches([' ', '\0']).to_string())
    }

    /// Draw an appearance: the character screen's animation, facing the viewer.
    ///
    /// # Errors
    ///
    /// [`Error`] if its COF is missing or a part's graphics are malformed.
    pub fn render(&self, a: &Appearance) -> Result<Animation, Error> {
        let look = self.look(a);
        let class = Self::token(&self.tables.classes, look.class);
        let mode = Self::token(&self.tables.modes, look.mode);
        let weapon = Self::token(&self.tables.weapon_classes, look.weapon_class);
        let cof_name = format!("data\\global\\chars\\{class}\\COF\\{class}{mode}{weapon}.cof");
        let cof_bytes = self.archives.read(&cof_name)?.ok_or_else(|| Error::Missing(cof_name.clone()))?;
        let cof = Cof::parse(&cof_bytes).map_err(|e| Error::Cof(cof_name.clone(), e))?;
        let direction = 0;

        let mut parts: HashMap<u8, (dcc::Direction, Option<usize>)> = HashMap::new();
        for layer in &cof.layers {
            let c = usize::from(layer.component);
            if c >= 16 {
                continue;
            }
            let component = Self::token(&self.tables.components, c);
            let graphics = look.parts[c].map_or_else(|| "lit".to_string(), |code| items::code_str(&code));
            let name = format!(
                "data\\global\\chars\\{class}\\{component}\\{class}{component}{graphics}{mode}{}.dcc",
                layer.weapon_class
            );
            let Some(bytes) = self.archives.read(&name)? else { continue };
            let file = Dcc::parse(&bytes).map_err(|e| Error::Dcc(name.clone(), e))?;
            let facing = direction * file.directions() / cof.directions.max(1);
            let decoded = file.direction(self.file_direction(file.directions(), facing)).map_err(|e| Error::Dcc(name.clone(), e))?;
            let tint = look.tints[c].map(|(t, colour)| t * COLOURS + colour);
            parts.insert(layer.component, (decoded, tint));
        }
        let drawn: Vec<&(dcc::Direction, Option<usize>)> = parts.values().filter(|(d, _)| d.width > 0).collect();
        if drawn.is_empty() {
            return Err(Error::Empty);
        }
        let left = drawn.iter().map(|(d, _)| d.left).min().unwrap_or(0);
        let top = drawn.iter().map(|(d, _)| d.top).min().unwrap_or(0);
        let right = drawn.iter().map(|(d, _)| d.left + d.width as i32).max().unwrap_or(0);
        let bottom = drawn.iter().map(|(d, _)| d.top + d.height as i32).max().unwrap_or(0);
        let (width, height) = ((right - left) as usize, (bottom - top) as usize);

        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut delays: Vec<u16> = Vec::new();
        let mut last_frame = usize::MAX;
        for frame in frame_sequence(cof.frames, cof.speed) {
            if frame == last_frame {
                if let Some(d) = delays.last_mut() {
                    *d += (TICK_MS / 10) as u16;
                }
                continue;
            }
            last_frame = frame;
            let mut canvas = vec![0u8; width * height];
            for component in cof.draw_order(direction, frame) {
                let Some((part, tint)) = parts.get(component) else { continue };
                let Some(pixels) = part.frames.get(frame.min(part.frames.len().saturating_sub(1))) else { continue };
                let map = tint.and_then(|t| self.tints.get(t));
                let (dx, dy) = ((part.left - left) as usize, (part.top - top) as usize);
                for y in 0..part.height {
                    for x in 0..part.width {
                        let p = pixels[y * part.width + x];
                        if p != 0 {
                            canvas[(dy + y) * width + dx + x] = map.map_or(p, |m| m[usize::from(p)]);
                        }
                    }
                }
            }
            frames.push(canvas);
            delays.push((TICK_MS / 10) as u16);
        }
        Ok(Animation { width, height, frames, delays, palette: self.palette })
    }
}

/// The stances a pack covers: town neutral, and neutral for a living hardcore character.
pub const PACK_MODES: [usize; 2] = [MODE_TOWN_NEUTRAL, MODE_NEUTRAL];
/// Player classes a pack covers (the front end's 0..=6).
pub const PACK_CLASSES: usize = 7;

/// One animation the character screen can show: a class, stance and weapon class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackAnimation {
    /// `<class><mode><weapon class>`, e.g. `BATNHTH`.
    pub name: String,
    /// Class token.
    pub class: String,
    /// Mode token.
    pub mode: String,
    /// Weapon class token.
    pub weapon_class: String,
    /// Frames in the COF.
    pub frames: usize,
    /// COF speed, 256ths of a frame per tick.
    pub speed: u32,
    /// One loop: `(frame, ticks shown)`.
    pub sequence: Vec<(usize, u32)>,
    /// Each layer: component index and the weapon class its part files use.
    pub layers: Vec<(usize, String)>,
    /// Draw order per frame, back to front, as component indices.
    pub order: Vec<Vec<u8>>,
}

/// One part's frames, facing the viewer as the character screen draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPart {
    /// `<class><component><code><mode><layer weapon class>`, upper case, e.g. `BAHDCAPTNHTH`.
    pub name: String,
    /// Left edge, pixels right of the base point.
    pub left: i32,
    /// Top edge, pixels below the base point.
    pub top: i32,
    /// Width of every frame.
    pub width: usize,
    /// Height of every frame.
    pub height: usize,
    /// Frames.
    pub frames: usize,
    /// The frames as a GIF with the game palette; index 0 transparent.
    pub gif: Vec<u8>,
}

/// What a graphics value means to the weapon-stance and helm rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSlot {
    /// The value.
    pub value: u8,
    /// Its graphics code, if the slot is filled.
    pub code: Option<Code>,
    /// Hand class held in one hand.
    pub hand: i32,
    /// Hand class held with both hands.
    pub two_handed: i32,
    /// The reserved list's hand class, used for claws off an Assassin and armour in a hand.
    pub reserved_hand: i32,
    /// The item is armour (its hand class is replaced by the reserved one).
    pub armor: bool,
    /// The item is a helm (a head byte that is not draws `lit`).
    pub helm: bool,
}

/// Everything a client needs to draw characters itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// Palette, RGB.
    pub palette: [[u8; 3]; 256],
    /// Tint maps: transforms 1..=8, 21 colours each, 256 bytes each.
    pub tints: Vec<[u8; 256]>,
    /// Class tokens by front-end class.
    pub classes: Vec<String>,
    /// Mode tokens by front-end mode.
    pub modes: Vec<String>,
    /// Component tokens.
    pub components: Vec<String>,
    /// Weapon class tokens by front-end number.
    pub weapon_classes: Vec<String>,
    /// Weapon class for each pair of hand classes, `[right][left]`.
    pub hand_pairs: Vec<Vec<i32>>,
    /// Every graphics value.
    pub slots: Vec<PackSlot>,
    /// Every animation found.
    pub animations: Vec<PackAnimation>,
    /// Every part file found for them.
    pub parts: Vec<PackPart>,
}

impl CharacterArt {
    /// Collect every animation and part the character screen can draw, facing the viewer.
    ///
    /// # Errors
    ///
    /// [`Error`] if a file is there but malformed.
    pub fn pack(&self) -> Result<Pack, Error> {
        let token = |codes: &[Code], i: usize| Self::token(codes, i);
        let mut codes: Vec<String> = vec!["lit".into()];
        for (_, code) in self.graphics.iter() {
            let c = items::code_str(&code);
            if !codes.contains(&c) {
                codes.push(c);
            }
        }
        let mut animations = Vec::new();
        let mut parts: Vec<PackPart> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for class_index in 0..PACK_CLASSES {
            let class = token(&self.tables.classes, class_index);
            for &mode_index in &PACK_MODES {
                let mode = token(&self.tables.modes, mode_index);
                for weapon_index in 1..self.tables.weapon_classes.len() {
                    let weapon = token(&self.tables.weapon_classes, weapon_index);
                    let cof_name = format!("data\\global\\chars\\{class}\\COF\\{class}{mode}{weapon}.cof");
                    let Some(cof_bytes) = self.archives.read(&cof_name)? else { continue };
                    let cof = Cof::parse(&cof_bytes).map_err(|e| Error::Cof(cof_name.clone(), e))?;
                    let mut sequence: Vec<(usize, u32)> = Vec::new();
                    for frame in frame_sequence(cof.frames, cof.speed) {
                        match sequence.last_mut() {
                            Some((f, ticks)) if *f == frame => *ticks += 1,
                            _ => sequence.push((frame, 1)),
                        }
                    }
                    for layer in cof.layers.iter().filter(|l| l.component < 16) {
                        let component = token(&self.tables.components, usize::from(layer.component));
                        for code in &codes {
                            let name = format!("{class}{component}{code}{mode}{}", layer.weapon_class).to_ascii_uppercase();
                            if !seen.insert(name.clone()) {
                                continue;
                            }
                            let path = format!("data\\global\\chars\\{class}\\{component}\\{name}.dcc");
                            let Some(bytes) = self.archives.read(&path)? else { continue };
                            let file = Dcc::parse(&bytes).map_err(|e| Error::Dcc(path.clone(), e))?;
                            let facing = self.file_direction(file.directions(), 0);
                            let d = file.direction(facing).map_err(|e| Error::Dcc(path.clone(), e))?;
                            if d.width == 0 || d.frames.is_empty() {
                                continue;
                            }
                            let gif = gif::encode(&gif::Animation {
                                width: d.width as u16,
                                height: d.height as u16,
                                palette: &self.palette,
                                transparent: 0,
                                delays: &[(TICK_MS / 10) as u16],
                                frames: &d.frames,
                            });
                            parts.push(PackPart { name, left: d.left, top: d.top, width: d.width, height: d.height, frames: d.frames.len(), gif });
                        }
                    }
                    animations.push(PackAnimation {
                        name: format!("{class}{mode}{weapon}").to_ascii_uppercase(),
                        class: class.clone(),
                        mode: mode.clone(),
                        weapon_class: weapon,
                        frames: cof.frames,
                        speed: cof.speed,
                        sequence,
                        layers: cof.layers.iter().map(|l| (usize::from(l.component), l.weapon_class.clone())).collect(),
                        order: (0..cof.frames).map(|f| cof.draw_order(0, f).to_vec()).collect(),
                    });
                }
            }
        }
        let slots = (0..=SLOTS)
            .map(|v| PackSlot {
                value: v as u8,
                code: self.graphics.code(v as u8),
                hand: self.slot_hand[v],
                two_handed: self.slot_two_handed[v],
                reserved_hand: self.static_hand(v),
                armor: self.types.is_a(self.slot_type[v], self.ids.any_armor),
                helm: self.types.is_a(self.slot_type[v], self.ids.helm),
            })
            .collect();
        let n = self.tables.weapon_classes.len() as i32;
        Ok(Pack {
            palette: self.palette,
            tints: self.tints[COLOURS..].to_vec(),
            classes: (0..self.tables.classes.len()).map(|i| token(&self.tables.classes, i)).collect(),
            modes: (0..self.tables.modes.len()).map(|i| token(&self.tables.modes, i)).collect(),
            components: (0..self.tables.components.len()).map(|i| token(&self.tables.components, i)).collect(),
            weapon_classes: (0..self.tables.weapon_classes.len()).map(|i| token(&self.tables.weapon_classes, i)).collect(),
            hand_pairs: (0..n).map(|a| (0..n).map(|b| combine_hands(a, b)).collect()).collect(),
            slots,
            animations,
            parts,
        })
    }
}

/// The frame drawn at each tick of one loop (`0x00503DDD`): a counter in 256ths of a frame starts
/// at 0, advances by `speed` after each draw, and restarts once it reaches the last frame.
#[must_use]
pub fn frame_sequence(frames: usize, speed: u32) -> Vec<usize> {
    let last = frames.saturating_sub(1) as u32;
    let mut sequence = vec![0];
    if speed == 0 || last == 0 {
        return sequence;
    }
    let mut counter = speed;
    while counter >> 8 < last {
        sequence.push((counter >> 8) as usize);
        counter += speed;
    }
    sequence
}

/// The front end's weapon class for a right-hand and left-hand class (`0x00504AF0`'s tail); its
/// numbering: 1 `hth`, 2 `1ht`, 3 `2ht`, 4 `1hs`, 5 `2hs`, 6 `bow`, 7 `xbw`, 8 `stf`, 9 `1js`,
/// 10 `1jt`, 11 `1ss`, 12 `1st`, 13 `ht1`, 14 `ht2`.
#[must_use]
pub fn combine_hands(a: i32, b: i32) -> i32 {
    if a == 0 {
        return if b == 0 { 1 } else { b };
    }
    if a == b && (a == 6 || a == 7) {
        return a;
    }
    if a == 8 {
        return 8;
    }
    if b == 0 {
        return a;
    }
    let reaches_second_test = match a {
        4 => {
            if b == 4 {
                return 11;
            }
            if b == 2 {
                return 9;
            }
            true
        }
        2 => {
            if b == 4 {
                return 12;
            }
            true
        }
        _ => {
            if a == 5 && b == 2 {
                return 11;
            }
            b != 4
        }
    };
    if reaches_second_test && b != 5 {
        if a == 2 {
            return if b == 2 { 10 } else { 0 };
        }
    } else if a == 2 {
        return 11;
    }
    match a {
        5 => {
            if b == 4 || b == 5 {
                11
            } else {
                0
            }
        }
        4 => {
            if b == 5 {
                11
            } else {
                0
            }
        }
        13 => {
            if b == 13 {
                13
            } else {
                0
            }
        }
        14 => {
            if b == 14 {
                13
            } else {
                0
            }
        }
        1 if b == 1 => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_combine_as_the_front_end_does() {
        assert_eq!(combine_hands(0, 0), 1, "bare hands");
        assert_eq!(combine_hands(0, 6), 6, "a bow in the left hand");
        assert_eq!(combine_hands(4, 0), 4, "a one-handed swing weapon");
        assert_eq!(combine_hands(7, 7), 7, "a crossbow, repeated in the left hand");
        assert_eq!(combine_hands(8, 3), 8, "a staff wins");
        assert_eq!(combine_hands(4, 4), 11, "two swing weapons: 1ss");
        assert_eq!(combine_hands(4, 2), 9, "1js");
        assert_eq!(combine_hands(2, 4), 12, "1st");
        assert_eq!(combine_hands(2, 2), 10, "1jt");
        assert_eq!(combine_hands(2, 5), 11);
        assert_eq!(combine_hands(5, 2), 11);
        assert_eq!(combine_hands(5, 5), 11);
        assert_eq!(combine_hands(4, 5), 11);
        assert_eq!(combine_hands(13, 13), 13);
        assert_eq!(combine_hands(14, 14), 13);
        assert_eq!(combine_hands(4, 6), 0, "impossible");
        assert_eq!(combine_hands(6, 4), 0);
    }

    #[test]
    fn a_loop_skips_the_last_frame() {
        let s = frame_sequence(16, 80);
        assert_eq!(s.len(), 48, "3840 / 80 ticks");
        assert_eq!((s[0], s[3], s[4], *s.last().unwrap()), (0, 0, 1, 14));
        assert_eq!(frame_sequence(1, 80), [0]);
        assert_eq!(frame_sequence(16, 0), [0]);
        assert_eq!(frame_sequence(2, 256), [0]);
    }

    #[test]
    fn a_portrait_gives_its_appearance() {
        let mut p = [0xFFu8; 33];
        p[..2].copy_from_slice(&[0x84, 0x80]);
        p[2] = 0x39; // a cap
        p[13] = 5; // Barbarian
        p[14] = 0x44;
        p[25] = 50;
        p[26] = 0x80 | 0x24; // hardcore expansion
        let a = Appearance::from_portrait(&p).unwrap();
        assert_eq!((a.class, a.status, a.graphics[0], a.graphics[11], a.tints[0]), (4, 0x24, 0x39, 0xFF, 0x44));
        assert_eq!(a.key()[..3], [4, 0x04, 0x39]);
        p[13] = 9;
        assert!(Appearance::from_portrait(&p).is_none());
        assert!(Appearance::from_portrait(&p[..20]).is_none());
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`): the front end's
    /// table matches the game's, and a character in gear draws.
    #[test]
    fn with_a_real_install_characters_draw() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let art = CharacterArt::load(&dir, &data, &engine).unwrap();
        let game = Graphics::build(data.items(), &engine.reserved_graphics);
        assert_eq!(game, art.graphics, "the front end's table is the game's");

        // A naked Barbarian.
        let mut a = Appearance { class: 4, status: 0x20, graphics: [0xFF; 16], tints: [0xFF; 16] };
        let look = art.look(&a);
        assert_eq!((look.class, look.mode, look.weapon_class), (4, MODE_TOWN_NEUTRAL, HAND_TO_HAND));
        assert_eq!(art.file_direction(16, 0), 4, "the screen's facing is file direction 4, toward the viewer");
        assert_eq!(art.file_direction(8, 0), 4);
        assert_eq!(art.file_direction(1, 0), 0);
        let naked = art.render(&a).unwrap();
        assert_eq!(naked.frames.len(), 15);
        assert!(naked.frames.iter().all(|f| f.iter().any(|&p| p != 0)));

        // EpicSorc's gear: a Dusk Shroud (tinted), an Eldritch Orb and a Monarch.
        a = Appearance { class: 1, status: 0x20, graphics: [0xFF; 16], tints: [0xFF; 16] };
        a.graphics[..10].copy_from_slice(&[0xFF, 1, 1, 1, 1, 0x33, 0xFF, 0x51, 2, 2]);
        a.tints[..10].copy_from_slice(&[0xFF, 0x44, 0x44, 0x44, 0x44, 0xAA, 0xFF, 0xFF, 0x44, 0x44]);
        let look = art.look(&a);
        assert_eq!(art.token_of(look.weapon_class), "1hs", "an orb swings");
        assert_eq!(look.tints[1], Some((2, 3)));
        assert_eq!(look.parts[5], Some(*b"ob1 "));
        let sorc = art.render(&a).unwrap();
        assert!(sorc.width > naked.width / 2 && sorc.frames.len() > 1);
        assert!(sorc.to_gif().starts_with(b"GIF89a"));
    }

    /// A client that knows only the pack and its published rules (docs/D2-CHARACTER-PACK.md).
    fn draw_from_pack(pack: &Pack, frames_of: &HashMap<String, Vec<Vec<u8>>>, a: &Appearance) -> Option<Animation> {
        let (hardcore, dead) = (a.status & 4 != 0, a.status & 8 != 0);
        if hardcore && dead {
            return None;
        }
        let mode = if hardcore { "NU" } else { "TN" };
        let class = usize::from(a.class);
        let g = a.graphics;
        let (rh, lh, sh) = (g[5], g[6], g[7]);
        let both = rh != 255 && lh != 255;
        let claws = |h: i32| (h == 13 || h == 14) && class != 6;
        let hand = |v: u8, is_right: bool| {
            let s = pack.slots[usize::from(v)];
            let lone_two_handed = is_right && lh == 255 && sh == 255 && s.two_handed != s.hand;
            let mut h = if both || lone_two_handed { s.two_handed } else { s.hand };
            if claws(h) || s.armor {
                h = s.reserved_hand;
            }
            if claws(h) {
                h = 0;
            }
            h
        };
        let right = if rh == 255 { 0 } else { hand(rh, true) };
        let left = if lh == 255 { 0 } else { hand(lh, false) };
        let w = pack.hand_pairs[right as usize][left as usize];
        if w == 0 {
            return None;
        }
        let name = format!("{}{mode}{}", pack.classes[class], pack.weapon_classes[w as usize]).to_ascii_uppercase();
        let anim = pack.animations.iter().find(|x| x.name == name)?;
        type Drawn<'a> = (&'a PackPart, &'a Vec<Vec<u8>>, Option<usize>);
        let mut drawn: HashMap<usize, Drawn> = HashMap::new();
        for (c, layer_class) in &anim.layers {
            let v = usize::from(g[*c]);
            let code = if v == 0 || v == 255 || (*c == 0 && !pack.slots[v].helm) {
                "lit".to_string()
            } else {
                pack.slots[v].code.map_or_else(|| "lit".into(), |code| items::code_str(&code))
            };
            let part_name = format!("{}{}{code}{mode}{layer_class}", pack.classes[class], pack.components[*c]).to_ascii_uppercase();
            let Some(part) = pack.parts.iter().find(|p| p.name == part_name) else { continue };
            let t = a.tints[*c];
            let tint = (t != 255 && t != 0)
                .then(|| {
                    let s = t - 1;
                    let (tr, colour) = (usize::from(s >> 5), usize::from(s & 31));
                    let tr = if tr == 0 { 8 } else { tr };
                    (colour <= 20 && tr != 3 && tr != 4).then_some((tr - 1) * 21 + colour)
                })
                .flatten();
            drawn.insert(*c, (part, &frames_of[&part_name], tint));
        }
        let left_edge = drawn.values().map(|d| d.0.left).min()?;
        let top_edge = drawn.values().map(|d| d.0.top).min()?;
        let width = (drawn.values().map(|d| d.0.left + d.0.width as i32).max()? - left_edge) as usize;
        let height = (drawn.values().map(|d| d.0.top + d.0.height as i32).max()? - top_edge) as usize;
        let mut frames = Vec::new();
        let mut delays = Vec::new();
        for &(f, ticks) in &anim.sequence {
            let mut canvas = vec![0u8; width * height];
            for &c in &anim.order[f] {
                let Some((part, pixels, tint)) = drawn.get(&usize::from(c)) else { continue };
                let px = &pixels[f.min(pixels.len() - 1)];
                for y in 0..part.height {
                    for x in 0..part.width {
                        let p = px[y * part.width + x];
                        if p != 0 {
                            let at = (y + (part.top - top_edge) as usize) * width + x + (part.left - left_edge) as usize;
                            canvas[at] = tint.map_or(p, |t| pack.tints[t][usize::from(p)]);
                        }
                    }
                }
            }
            frames.push(canvas);
            delays.push((ticks * TICK_MS / 10) as u16);
        }
        Some(Animation { width, height, frames, delays, palette: pack.palette })
    }

    /// With the operator's install: the pack plus its rules draw exactly what the renderer draws.
    #[test]
    fn with_a_real_install_the_pack_draws_what_the_screen_draws() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let art = CharacterArt::load(&dir, &data, &engine).unwrap();
        let pack = art.pack().unwrap();
        let frames_of: HashMap<String, Vec<Vec<u8>>> =
            pack.parts.iter().map(|p| (p.name.clone(), gif::decode_frames(&p.gif).unwrap().2)).collect();
        let looks: [(u8, u8, &[u8], &[u8]); 9] = [
            (4, 0x20, &[], &[]),
            (1, 0x20, &[0xFF, 1, 1, 1, 1, 0x33, 0xFF, 0x51, 2, 2], &[0xFF, 0x44, 0x44, 0x44, 0x44, 0xAA, 0xFF, 0xFF, 0x44, 0x44]),
            (3, 0x24, &[0x3D, 3, 3, 3, 3, 0x16, 0xFF, 0x5C, 2, 2], &[0x21, 0x44, 0x44, 0x44, 0x44, 0x08]),
            (4, 0x20, &[0x5A, 3, 3, 3, 3, 0x17, 0x13, 0xFF, 3, 3], &[]),
            (6, 0x20, &[0x5B, 1, 1, 1, 1, 0x2D, 0x2D, 0xFF, 1, 1], &[]),
            (0, 0x20, &[0x39, 2, 2, 1, 1, 0xFF, 0x29, 0xFF, 2, 2], &[0x86]),
            (2, 0x20, &[0xFF, 1, 1, 1, 1, 0x0B, 0xFF, 0xFF, 1, 1, 0x60], &[]),
            (5, 0x20, &[0x56, 2, 2, 2, 2, 0x0D, 0xFF, 0x55, 2, 2], &[0xE5]),
            (4, 0x20, &[0xFF, 1, 1, 1, 1, 0x31, 0x31], &[]),
        ];
        for (class, status, g, t) in looks {
            let mut a = Appearance { class, status, graphics: [0xFF; 16], tints: [0xFF; 16] };
            a.graphics[..g.len()].copy_from_slice(g);
            a.tints[..t.len()].copy_from_slice(t);
            let expected = art.render(&a).unwrap();
            let got = draw_from_pack(&pack, &frames_of, &a).unwrap();
            assert_eq!((got.width, got.height, got.frames.len()), (expected.width, expected.height, expected.frames.len()), "{a:?}");
            assert_eq!(got.delays, expected.delays, "{a:?}");
            assert!(got.frames == expected.frames, "{a:?}: pixels differ");
        }
    }

    impl CharacterArt {
        fn token_of(&self, weapon_class: usize) -> String {
            Self::token(&self.tables.weapon_classes, weapon_class)
        }
    }
}
