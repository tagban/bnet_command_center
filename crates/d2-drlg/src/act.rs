//! Where each level of an act sits: world origin and size in tiles, from the game seed.
//!
//! Two paths, as the engine has them:
//! - Levels in an act's **placement graph** are placed by a backtracking walk
//!   (`DRLGLEVEL_ParseLevelData`, `0x006774xx`): each node goes against its predecessor in an
//!   RNG-chosen direction, re-rolled on overlap until the whole chain fits.
//! - Every other level rides its `Levels.txt` `Depend` chain: `Offset + depend's origin`
//!   (`DRLG_SetLevelPositionAndSize`, `0x00642D10`).
//!
//! The placement RNG starts from the act seed after the act-specific pre-rolls
//! (`DRLG_AllocDrlgActMisc`, `0x00642DA0`). Ported from libd2 `packages/drlg/src/act.zig` (MIT).

use std::collections::HashMap;

use d2_data::levels::Levels;

use crate::rng::Seed;
use crate::Coords;

/// Fixed arrays in `D2DrlgLevelPlacementStrc`.
const MAX_NODES: usize = 15;

/// Placement callbacks (engine addresses in the comments).
#[derive(Clone, Copy)]
enum Place {
    /// `fpLDE1` `0x006760F0`: Levels.txt Offset, no roll.
    Fixed,
    /// `DRLGPLACE_Adjacent2DirRandom` `0x006769A0`.
    Adj2,
    /// `fpLevelDataEntry` `0x00676280`: 8 directions, half-size + 8.
    Dir8,
    /// `DRLGPLACE_Adjacent8DirAligned` `0x00676AE0`.
    Dir8Aligned,
    /// `fpLDE2` `0x00676150`: 4 directions, ±0x10 nudge.
    Lde2,
    /// `fpLDE3` `0x00676650`: 4 directions + flip, resizes (Blood Moor).
    Lde3,
    /// `fpLDE4` `0x00676450`: 4 directions + flip, ±8 nudge (Rogue Encampment).
    Lde4,
    /// `fpLDE5` `0x006768C0`: direction 0, no roll.
    Lde5,
    /// `DRLGPLACE_Adjacent4DirMirrored`.
    Mirror4,
    /// `DRLGPLACE_OrientedFixedOffset` `0x0067D830`.
    OrientFixed,
    /// `DRLGPLACE_OrientedAbsolutePos` `0x0067D8E0`.
    OrientAbsolute,
    /// `DRLGPLACE_OrientedTableLookup` `0x0067D980`.
    OrientTable,
}

/// The validation gate passed to `ParseLevelData`.
#[derive(Clone, Copy)]
enum Validate {
    /// No gate.
    None,
    /// No overlap with any placed node but the predecessor.
    Overlap,
    /// `DRLGPLACE_ACT1_ValidatePlacementList1`.
    Act1List1,
    /// `DRLGPLACE_ACT1_ValidatePlacementList2WithGap`.
    Act1List2,
}

struct Node {
    level: i32,
    place: Place,
    prev: i32,
}

const fn node(level: i32, place: Place, prev: i32) -> Node {
    Node { level, place, prev }
}

/// Act I list 1: the wilderness trunk anchored at Stony Field.
const ACT1_LIST1: [Node; 5] = [
    node(4, Place::Fixed, -1),  // Stony Field
    node(3, Place::Lde2, 0),    // Cold Plains
    node(2, Place::Lde3, 1),    // Blood Moor (resizes)
    node(1, Place::Lde4, 2),    // Rogue Encampment
    node(17, Place::Lde2, 1),   // Burial Grounds
];
/// Act I list 2: the Monastery approach and the cow level.
const ACT1_LIST2: [Node; 5] = [
    node(39, Place::Fixed, -1), // Moo Moo Farm
    node(26, Place::Fixed, -1), // Monastery Gate
    node(7, Place::Lde5, 1),    // Tamoe Highland
    node(6, Place::Lde2, 2),    // Black Marsh
    node(5, Place::Lde2, 3),    // Dark Wood
];
const ACT2_LIST1: [Node; 6] = [
    node(40, Place::Fixed, -1),      // Lut Gholein
    node(41, Place::Adj2, 0),        // Rocky Waste
    node(42, Place::Dir8, 1),        // Dry Hills
    node(43, Place::Dir8, 2),        // Far Oasis
    node(44, Place::Dir8, 3),        // Lost City
    node(45, Place::Dir8Aligned, 4), // Valley of Snakes
];
const ACT2_LIST2: [Node; 1] = [node(46, Place::Fixed, -1)]; // Canyon of the Magi
const ACT4_LIST1: [Node; 4] = [
    node(103, Place::Fixed, -1),  // Pandemonium Fortress
    node(104, Place::Mirror4, 0), // Outer Steppes
    node(105, Place::Lde2, 1),    // Plains of Despair
    node(106, Place::Lde2, 2),    // City of the Damned
];
const ACT4_LIST2: [Node; 1] = [node(108, Place::Fixed, -1)]; // Chaos Sanctuary
const ACT5_LIST1: [Node; 4] = [
    node(109, Place::Fixed, -1),      // Harrogath
    node(110, Place::Fixed, 0),       // Bloody Foothills
    node(111, Place::OrientFixed, 1), // Frigid Highlands
    node(112, Place::OrientTable, 2), // Arreat Plateau
];
const ACT5_LIST2: [Node; 1] = [node(117, Place::OrientAbsolute, -1)]; // Frozen Tundra
const ACT5_LIST3: [Node; 2] = [
    node(134, Place::Fixed, -1), // Forgotten Sands
    node(136, Place::Fixed, -1), // Uber Tristram
];

/// `DRLGPLACE_OrientedTableLookup` offsets (`.data` `0x006F1F7C`), by `dir + prev_dir * 2`.
const ORIENT_TABLE_X: [i32; 4] = [0, -96, -64, -160];
const ORIENT_TABLE_Y: [i32; 4] = [-160, -64, -96, 0];

/// `RogueEncampentLayout[64]`: allowed (dir, flip) of the Rogue Encampment against (dir, flip)
/// of Blood Moor. Index `dir + 4 * (flip + 2 * (prev_dir + 4 * prev_flip))`.
const ROGUE_LAYOUT: [u8; 64] = [
    1, 1, 0, 0, 0, 0, 0, 0, //
    0, 1, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 1, 0, 1, 0, 0, //
    1, 0, 0, 0, 0, 0, 1, 1, //
    0, 1, 0, 0, 0, 0, 0, 1, //
    0, 1, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 1, 1, 0, //
    1, 0, 0, 0, 0, 0, 1, 1, //
];

/// The placement walk's state for one list.
struct Placement<'a> {
    seed: Seed,
    nodes: &'a [Node],
    coords: [Coords; MAX_NODES],
    initial_dir: [i32; MAX_NODES],
    cur_dir: [i32; MAX_NODES],
    /// `aFlipState`: rolled once.
    flip: [i32; MAX_NODES],
    /// `aStepCounter`: alternates on each re-dispatch.
    step: [i32; MAX_NODES],
    cur: usize,
}

impl<'a> Placement<'a> {
    fn prev(&self, node: usize) -> Coords {
        self.coords[self.nodes[node].prev as usize]
    }

    fn fixed(&mut self, levels: &Levels) -> bool {
        let def = levels.get(self.nodes[self.cur].level).expect("placement level in Levels.txt");
        self.coords[self.cur].x = def.offset.0;
        self.coords[self.cur].y = def.offset.1;
        true
    }

    fn adj2(&mut self) -> bool {
        let n = self.cur;
        if self.initial_dir[n] == -1 {
            self.seed.next();
            self.initial_dir[n] = (self.seed.low & 1) as i32 + 1;
            self.cur_dir[n] = self.initial_dir[n];
        } else {
            let next = 2 - i32::from(self.cur_dir[n] != 1);
            if next == self.initial_dir[n] {
                return false;
            }
            self.cur_dir[n] = next;
        }
        let p = self.prev(n);
        let s = &mut self.coords[n];
        match self.cur_dir[n] {
            1 => {
                s.x = p.x - s.w;
                s.y = p.y;
            }
            0 => {
                s.x = p.w - s.w + p.x;
                s.y = p.h + p.y;
            }
            2 => {
                s.x = p.x;
                s.y = p.y - s.h;
            }
            3 => {
                s.x = p.w + p.x;
                s.y = p.y;
            }
            _ => {}
        }
        true
    }

    fn roll_dir8(&mut self) -> bool {
        let n = self.cur;
        if self.initial_dir[n] == -1 {
            self.seed.next();
            self.initial_dir[n] = (self.seed.low & 7) as i32;
            self.cur_dir[n] = self.initial_dir[n];
        } else {
            let next = (self.cur_dir[n] + 1) & 7;
            if next == self.initial_dir[n] {
                return false;
            }
            self.cur_dir[n] = next;
        }
        true
    }

    fn dir8(&mut self) -> bool {
        if !self.roll_dir8() {
            return false;
        }
        let n = self.cur;
        let p = self.prev(n);
        let s = &mut self.coords[n];
        let (x, y) = match self.cur_dir[n] {
            0 => (p.x - 8 - s.w / 2, p.y + p.h),
            1 => (p.x + s.w / 2 + 8, p.y + p.h),
            2 => (p.x - s.w, p.y - s.h / 2 - 8),
            3 => (p.x - s.w, p.y + s.h / 2 + 8),
            4 => (p.x - 8 - s.w / 2, p.y - s.h),
            5 => (p.x + s.w / 2 + 8, p.y - s.h),
            6 => (p.x + p.w, p.y - s.h / 2 - 8),
            7 => (p.x + p.w, p.y + s.h / 2 + 8),
            _ => (s.x, s.y),
        };
        (s.x, s.y) = (x, y);
        true
    }

    fn dir8_aligned(&mut self) -> bool {
        if !self.roll_dir8() {
            return false;
        }
        let n = self.cur;
        let p = self.prev(n);
        let s = &mut self.coords[n];
        let (x, y) = match self.cur_dir[n] {
            0 | 1 => (p.x, p.h + p.y),
            2 | 3 => (p.x - s.w, p.y),
            4 | 5 => (p.x, p.y - s.h),
            6 | 7 => (p.w + p.x, p.y),
            _ => (s.x, s.y),
        };
        (s.x, s.y) = (x, y);
        true
    }

    fn lde2(&mut self) -> bool {
        let n = self.cur;
        if self.initial_dir[n] == -1 {
            self.seed.next();
            self.initial_dir[n] = (self.seed.low & 3) as i32;
            self.cur_dir[n] = self.initial_dir[n];
        } else {
            let next = (self.cur_dir[n] + 1) & 3;
            if next == self.initial_dir[n] {
                return false;
            }
            self.cur_dir[n] = next;
        }
        let p = self.prev(n);
        let s = &mut self.coords[n];
        let (x, y) = match self.cur_dir[n] {
            0 => (p.x - 16, p.y + p.h),
            1 => (p.x - s.w, p.y - 16),
            2 => (p.x + p.w - s.w + 16, p.y - s.h),
            3 => (p.x + p.w, p.y + p.h - s.h + 16),
            _ => (s.x, s.y),
        };
        (s.x, s.y) = (x, y);
        true
    }

    fn lde5(&mut self) -> bool {
        let n = self.cur;
        self.initial_dir[n] = 0;
        self.cur_dir[n] = 0;
        let p = self.prev(n);
        let s = &mut self.coords[n];
        (s.x, s.y) = (p.x, p.y + p.h);
        true
    }

    /// Shared by `fpLDE3`/`fpLDE4`: a direction and a flip on first entry; afterwards the
    /// direction advances by the step, which toggles, until all eight pairs are spent.
    fn roll_dir4_flip(&mut self) -> bool {
        let n = self.cur;
        if self.initial_dir[n] == -1 {
            self.seed.next();
            self.initial_dir[n] = (self.seed.low & 3) as i32;
            self.cur_dir[n] = self.initial_dir[n];
            self.seed.next();
            self.flip[n] = (self.seed.low & 1) as i32;
            self.step[n] = self.flip[n];
        } else {
            let next_dir = (self.cur_dir[n] + self.step[n]) & 3;
            let next_flip = (self.step[n] + 1) & 1;
            if next_dir == self.initial_dir[n] && next_flip == self.flip[n] {
                return false;
            }
            self.cur_dir[n] = next_dir;
            self.step[n] = next_flip;
        }
        true
    }

    fn lde4(&mut self) -> bool {
        if !self.roll_dir4_flip() {
            return false;
        }
        let n = self.cur;
        let p = self.prev(n);
        let stepped = self.step[n] != 0;
        let s = &mut self.coords[n];
        let (x, y) = match (stepped, self.cur_dir[n]) {
            (true, 0) => (p.x, p.y + p.h),
            (true, 1) => (p.x - s.w, p.y + 8),
            (true, 2) => (p.x + p.w - s.w, p.y - s.h),
            (true, 3) => (p.x + p.w, p.y + p.h - s.h - 8),
            (false, 0) => (p.x + p.w - s.w, p.y + p.h),
            (false, 1) => (p.x - s.w, p.y + p.h - s.h - 8),
            (false, 2) => (p.x, p.y - s.h),
            (false, 3) => (p.x + p.w, p.y + 8),
            _ => (s.x, s.y),
        };
        (s.x, s.y) = (x, y);
        true
    }

    fn lde3(&mut self) -> bool {
        if !self.roll_dir4_flip() {
            return false;
        }
        let n = self.cur;
        let odd = self.cur_dir[n] & 1 != 0;
        let stepped = self.step[n] != 0;
        let p = self.prev(n);
        let s = &mut self.coords[n];
        (s.w, s.h) = if odd { (0x60, 0x38) } else { (0x38, 0x60) };
        let (x, y) = match (stepped, self.cur_dir[n]) {
            (true, 0) => (p.x - 16, p.y + p.h),
            (true, 1) => (p.x - s.w, p.y - 16),
            (true, 2) => (p.x + p.w - s.w + 16, p.y - s.h),
            (true, 3) => (p.x + p.w, p.y + p.h - s.h + 16),
            (false, 0) => (p.x + p.w - s.w + 16, p.y + p.h),
            (false, 1) => (p.x - s.w, p.y + p.h - s.h + 16),
            (false, 2) => (p.x - 16, p.y - s.h),
            (false, 3) => (p.x + p.w, p.y - 16),
            _ => (s.x, s.y),
        };
        (s.x, s.y) = (x, y);
        true
    }

    fn mirror4(&mut self) -> bool {
        let n = self.cur;
        self.initial_dir[n] = 3;
        self.cur_dir[n] = 3;
        self.seed.next();
        let p = self.prev(n);
        let low_bit = self.seed.low & 1 != 0;
        let s = &mut self.coords[n];
        s.x = p.x + p.w;
        s.y = if low_bit { p.y + p.h - s.h + 8 } else { p.y - 8 };
        true
    }

    fn orient_size(&mut self, n: usize, dir: i32) {
        let s = &mut self.coords[n];
        if self.seed.low & 1 == 0 {
            (s.w, s.h) = (0x40, 0xA0);
        } else if dir != 0 {
            (s.w, s.h) = (0xA0, 0x40);
        }
    }

    fn orient_fixed(&mut self) -> bool {
        let n = self.cur;
        self.seed.next();
        let d = (self.seed.low & 1) as i32;
        self.cur_dir[n] = d;
        self.initial_dir[n] = d;
        self.orient_size(n, d);
        let p = self.prev(n);
        let s = &mut self.coords[n];
        (s.x, s.y) = (p.x - s.w, p.y + p.h - s.h - 16);
        true
    }

    fn orient_absolute(&mut self, levels: &Levels) -> bool {
        let n = self.cur;
        self.seed.next();
        let d = (self.seed.low & 1) as i32;
        self.cur_dir[n] = d;
        self.initial_dir[n] = d;
        self.orient_size(n, d);
        let def = levels.get(self.nodes[n].level).expect("placement level in Levels.txt");
        let s = &mut self.coords[n];
        (s.x, s.y) = def.offset;
        true
    }

    fn orient_table(&mut self) -> bool {
        let n = self.cur;
        if self.initial_dir[n] == -1 {
            self.seed.next();
            self.initial_dir[n] = (self.seed.low & 1) as i32;
            self.cur_dir[n] = self.initial_dir[n];
        } else {
            let next = i32::from(self.cur_dir[n] == 0);
            if next == self.initial_dir[n] {
                return false;
            }
            self.cur_dir[n] = next;
        }
        let dir = self.cur_dir[n];
        let prev_dir = self.cur_dir[self.nodes[n].prev as usize];
        let at = (dir + prev_dir * 2) as usize;
        let p = self.prev(n);
        let s = &mut self.coords[n];
        (s.w, s.h) = if dir == 0 { (0x40, 0xA0) } else { (0xA0, 0x40) };
        (s.x, s.y) = (ORIENT_TABLE_X[at] + p.x, ORIENT_TABLE_Y[at] + p.y);
        true
    }

    fn dispatch(&mut self, levels: &Levels) -> bool {
        match self.nodes[self.cur].place {
            Place::Fixed => self.fixed(levels),
            Place::Adj2 => self.adj2(),
            Place::Dir8 => self.dir8(),
            Place::Dir8Aligned => self.dir8_aligned(),
            Place::Lde2 => self.lde2(),
            Place::Lde3 => self.lde3(),
            Place::Lde4 => self.lde4(),
            Place::Lde5 => self.lde5(),
            Place::Mirror4 => self.mirror4(),
            Place::OrientFixed => self.orient_fixed(),
            Place::OrientAbsolute => self.orient_absolute(levels),
            Place::OrientTable => self.orient_table(),
        }
    }

    /// No overlap with any placed node other than the predecessor.
    fn no_overlap(&self, n: usize) -> bool {
        (0..n).all(|i| i as i32 == self.nodes[n].prev || free_room_ex(self.coords[n], self.coords[i]))
    }

    fn validate(&self, n: usize, gate: Validate) -> bool {
        match gate {
            Validate::None => true,
            Validate::Overlap => self.no_overlap(n),
            Validate::Act1List1 => {
                if !self.no_overlap(n) {
                    return false;
                }
                match self.nodes[n].level {
                    1 => {
                        let prev = self.nodes[n].prev as usize;
                        let i = self.cur_dir[n] + 4 * (self.step[n] + 2 * (self.cur_dir[prev] + 4 * self.step[prev]));
                        ROGUE_LAYOUT[i as usize] != 0
                    }
                    17 => {
                        let prev = self.nodes[n].prev;
                        !self.nodes.iter().enumerate().any(|(i, other)| i != n && other.prev == prev && self.cur_dir[n] == self.cur_dir[i])
                    }
                    _ => true,
                }
            }
            Validate::Act1List2 => {
                if !self.no_overlap(n) {
                    return false;
                }
                if n == 0 {
                    return true;
                }
                let mut cow = self.coords[0];
                cow.h += 200;
                cow.y -= 200;
                free_room_ex(cow, self.coords[n])
            }
        }
    }
}

/// `FreeRoomEx` (`0x0066B860`): free unless the rectangles overlap on both axes.
fn free_room_ex(a: Coords, b: Coords) -> bool {
    let dx = if a.x < b.x { b.x - a.w - a.x } else { a.x - b.w - b.x };
    let dy = if a.y < b.y { b.y - a.h - a.y } else { a.y - b.h - b.y };
    !(dx < 0 && dy < 0)
}

/// Run one list: size every node, then walk, backtracking when a node's placements run out.
fn run_placement<'a>(levels: &Levels, difficulty: usize, nodes: &'a [Node], seed: Seed, gate: Validate) -> Placement<'a> {
    let mut p = Placement {
        seed,
        nodes,
        coords: [Coords::default(); MAX_NODES],
        initial_dir: [-1; MAX_NODES],
        cur_dir: [-1; MAX_NODES],
        flip: [-1; MAX_NODES],
        step: [-1; MAX_NODES],
        cur: 0,
    };
    for (i, n) in nodes.iter().enumerate() {
        let def = levels.get(n.level).expect("placement level in Levels.txt");
        (p.coords[i].w, p.coords[i].h) = def.size[difficulty];
    }
    let mut n: i32 = 0;
    while n >= 0 && (n as usize) < nodes.len() {
        p.cur = n as usize;
        if !p.dispatch(levels) {
            let i = n as usize;
            (p.initial_dir[i], p.cur_dir[i], p.flip[i], p.step[i]) = (-1, -1, -1, -1);
            n -= 1;
        } else if p.validate(n as usize, gate) {
            n += 1;
        }
    }
    p
}

/// The seed the placement walk rolls: the game seed stepped once, then the act's pre-rolls.
fn placement_seed(act: u8, game_seed: u32) -> Seed {
    let mut s = Seed::new(game_seed, 0x29A);
    s.next();
    match act {
        1 => loop {
            // Act II: re-roll the staff and boss tombs until they differ.
            s.next();
            let staff = s.low % 7;
            s.next();
            if staff != s.low % 7 {
                break s;
            }
        },
        2 => {
            s.next(); // Act III: jungle interlink
            s
        }
        _ => s,
    }
}

/// An act's level layout for one game.
#[derive(Debug, Clone)]
pub struct Act {
    /// 0-based act.
    pub act: u8,
    /// Difficulty the sizes were taken for (0 Normal, 1 Nightmare, 2 Hell).
    pub difficulty: u8,
    placed: HashMap<i32, Coords>,
    /// `aCurrentDir` of each placed level, for the file picks that read it.
    directions: HashMap<i32, i32>,
}

impl Act {
    /// Lay out `act` (0-based) for `game_seed` at `difficulty`.
    #[must_use]
    pub fn build(levels: &Levels, act: u8, difficulty: u8, game_seed: u32) -> Self {
        let seed = placement_seed(act, game_seed);
        let lists: &[(&[Node], Validate)] = match act {
            0 => &[(&ACT1_LIST1, Validate::Act1List1), (&ACT1_LIST2, Validate::Act1List2)],
            1 => &[(&ACT2_LIST1, Validate::Overlap), (&ACT2_LIST2, Validate::Overlap)],
            3 => &[(&ACT4_LIST1, Validate::Overlap), (&ACT4_LIST2, Validate::Overlap)],
            4 => &[(&ACT5_LIST1, Validate::None), (&ACT5_LIST2, Validate::None), (&ACT5_LIST3, Validate::Overlap)],
            _ => &[], // Act III: Depend chains only
        };
        let d = usize::from(difficulty.min(2));
        let mut placed = HashMap::new();
        let mut directions = HashMap::new();
        // Each list copies the act seed afresh; none writes it back.
        for &(nodes, gate) in lists {
            let p = run_placement(levels, d, nodes, seed, gate);
            for (i, n) in nodes.iter().enumerate() {
                placed.insert(n.level, p.coords[i]);
                directions.insert(n.level, p.cur_dir[i]);
            }
        }
        Self { act, difficulty, placed, directions }
    }

    /// A level's rectangle in tiles: its placement, or its Depend chain.
    #[must_use]
    pub fn coords(&self, levels: &Levels, level: i32) -> Option<Coords> {
        if let Some(&c) = self.placed.get(&level) {
            return Some(c);
        }
        let def = levels.get(level)?;
        let base = if def.depend != 0 { self.coords(levels, def.depend)? } else { Coords::default() };
        let (w, h) = def.size[usize::from(self.difficulty.min(2))];
        Some(Coords { x: def.offset.0 + base.x, y: def.offset.1 + base.y, w, h })
    }

    /// The Rogue Encampment's map variant: its own placement direction, 0..=3 for
    /// `TownN1`/`E1`/`S1`/`W1` (`DRLGLEVEL_ParseLevelData` pass 2, `0x006772C0`).
    #[must_use]
    pub fn rogue_encampment_pick(&self) -> Option<i32> {
        (self.act == 0).then(|| self.directions.get(&1).copied()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_touching_at_an_edge_are_free() {
        let a = Coords { x: 0, y: 0, w: 10, h: 10 };
        assert!(free_room_ex(a, Coords { x: 10, y: 0, w: 5, h: 5 }), "edge to edge");
        assert!(!free_room_ex(a, Coords { x: 9, y: 9, w: 5, h: 5 }), "one tile of overlap");
    }

    /// Diff against libd2's engine recordings (`LIBD2_DIR` = a libd2 checkout) using the
    /// operator's `Levels.txt` (`BNETCC_D2_DATA_DIR`). The recordings stay in libd2; nothing
    /// from them is copied here.
    #[test]
    fn with_libd2_recordings_origins_match_the_engine() {
        let (Ok(libd2), Ok(data_dir)) = (std::env::var("LIBD2_DIR"), std::env::var("BNETCC_D2_DATA_DIR")) else {
            return;
        };
        let data = d2_data::GameData::load(&data_dir).expect("game data");
        let levels = data.levels();
        let golden = std::path::Path::new(&libd2).join("packages/drlg/src/golden");
        let files = [
            ("seed_1.jsonl", 1u32),
            ("seed_2.jsonl", 2),
            ("seed_1000.jsonl", 1000),
            ("seed_65535.jsonl", 65535),
            ("seed_305419896.jsonl", 305_419_896),
            ("seed_3133731337.jsonl", 3_133_731_337),
            ("deep_seed_305419896.jsonl", 305_419_896),
            ("deep_seed_1.jsonl", 1),
            ("deep_seed_2.jsonl", 2),
        ];
        let (mut checked, mut mismatched) = (0, Vec::new());
        for (file, seed) in files {
            let text = std::fs::read_to_string(golden.join(file)).expect("golden file");
            let mut acts: HashMap<u8, Act> = HashMap::new();
            for line in text.lines().filter(|l| l.contains("\"evt\":\"drlg_level\"")) {
                let (Some(id), Some(recorded)) = (json_int(line, "\"levelId\":"), recorded_coords(line)) else {
                    continue;
                };
                // Not produced by the act graph: mazes positioned at generation time, and
                // Act III's jungle placement (libd2's graphDerivable).
                if matches!(id, 28 | 107 | 76..=83) {
                    continue;
                }
                let Some(def) = levels.get(id as i32) else { continue };
                let act = acts.entry(def.act).or_insert_with(|| Act::build(levels, def.act, 0, seed));
                checked += 1;
                if act.coords(levels, id as i32) != Some(recorded) {
                    mismatched.push((file, id, act.coords(levels, id as i32), recorded));
                }
            }
        }
        eprintln!("{checked} recorded level origins compared");
        assert!(checked > 100, "only {checked} levels compared");
        assert!(mismatched.is_empty(), "{} of {checked} differ: {:?}", mismatched.len(), &mismatched[..mismatched.len().min(10)]);
    }

    /// The handshake test's seed: libd2's object dump puts this town's origin at tile (1136, 864).
    #[test]
    fn with_a_real_install_the_test_seed_town_is_where_the_object_dump_says() {
        let Ok(data_dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let data = d2_data::GameData::load(&data_dir).expect("game data");
        let act = Act::build(data.levels(), 0, 0, 0x1234_5678);
        let town = act.coords(data.levels(), 1).unwrap();
        eprintln!("seed 0x12345678: Rogue Encampment {town:?}, pick {:?}", act.rogue_encampment_pick());
        assert_eq!((town.x, town.y), (1136, 864));
    }

    fn json_int(line: &str, key: &str) -> Option<i64> {
        let at = line.find(key)? + key.len();
        let digits: String = line[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        digits.parse().ok()
    }

    fn recorded_coords(line: &str) -> Option<Coords> {
        let at = line.find("\"coords\":{")?;
        let obj = &line[at..at + line[at..].find('}')?];
        Some(Coords {
            x: json_int(obj, "\"x\":")? as i32,
            y: json_int(obj, "\"y\":")? as i32,
            w: json_int(obj, "\"w\":")? as i32,
            h: json_int(obj, "\"h\":")? as i32,
        })
    }
}
