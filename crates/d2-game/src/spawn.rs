//! The monsters a level's rooms spawn when a player first comes near them.
//!
//! When a game starts, `AllocMonsterRegion` (`0x005479C0`) gives every level a roster: from one
//! seed stream `{game seed, 0x29A}` taken level by level in id order,
//! `MONREGION_PopulateMonsterTypes` (`0x005475E0`) draws up to `NumMon` distinct classes from
//! the level's `mon*` columns (`nmon*` past Normal), the first a ranged one on a `rangedspawn`
//! level, keeping those `isSpawn` allows with their `Rarity`. (`SEED_RollChampionPack` then rolls
//! champion looks on the same stream; that is not ported, so rosters after the first level with
//! monsters are not the engine's for the same seed — which only matters to a replay.)
//!
//! When a room is populated, `MONSTER_SpawnRoomMonsters` (`0x0054EC90`) walks one slot per 3×3
//! subtiles: each steps the game seed, and a slot whose low word mod 100000 is within the
//! level's `MonDen` spawns. The class is a rarity-weighted roster pick on the room's seed
//! (`0x005BDE80`), sometimes swapped for its `spawn` class; `MONSTERREGION_CheckSpawnDensity`
//! (`0x005BE020`) decides whether it leads a unique pack instead. Each spawn is placed before
//! the next slot rolls: a group (`SPAWN_SpawnMonsterWithMinions`, `0x0054DF80`) is
//! `MinGrp..=MaxGrp` of the class — just one for fallen and scarabs, whose minions make their
//! group — and each member brings `PartyMin..=PartyMax` minions, `minion1` and `minion2` in turn.
//!
//! Where they stand ([`find_spot`], [`probe`]) follows `0x0054DC40` and `0x005B2A00`: a spot rolled
//! on the room's seed and tested against the room's collision map with the class's size and
//! collision mask; the other members search rings of 3 subtiles around the first, minions rings
//! of 4 around their parent.
//!
//! Not ported: unique pack leaders' names and mods (they spawn as plain monsters of their class),
//! champions, wandering monsters, the flying classes' own placement (`spawnCol` 1, `0x005B2700`),
//! the few classes with extra checks at their spot (`0x005FD350`), where objects already stand,
//! and the arrival spots other than the waypoint that spawns keep away from. Ported from libd2
//! `packages/drlg/src/drlg/monpop.zig` (MIT) and the 1.14d `Game.exe`.

use std::collections::{HashMap, HashSet};

use d2_data::GameData;
use d2_drlg::rng::Seed;

/// Most classes a roster holds (`D2MonsterRegionFieldStrc[13]`).
const ROSTER_MAX: usize = 13;
/// The fallen and scarab base classes, which spawn alone (`0x0054EC40`).
const SINGLE_SPAWN_BASES: [i32; 2] = [19, 91];

/// One level's monster region.
#[derive(Debug, Clone, Default)]
pub struct Region {
    /// `MonDen` for the game's difficulty.
    density: i32,
    /// `MonUMin`/`MonUMax` for the game's difficulty.
    uniques: (i32, i32),
    /// The classes picked for the game, with their rarity.
    roster: Vec<(i32, i32)>,
    /// Their rarities summed.
    total_rarity: i32,
    /// `umon*`: unique pack leaders on Normal.
    unique_leaders: Vec<i32>,
    /// Rooms of the level populated so far.
    rooms_seen: i32,
    /// Unique packs placed so far.
    uniques_placed: i32,
}

impl Region {
    /// The classes picked for the game, with their rarity.
    #[must_use]
    pub fn roster(&self) -> &[(i32, i32)] {
        &self.roster
    }

    /// `MONREGION_RollRandomMonsterType` (`0x005BDE80`): a unique leader (on Normal) or a
    /// rarity-weighted roster pick, maybe swapped for its `spawn` class; -1 for none.
    fn roll_class(&self, data: &GameData, room: &mut Seed, swap_above: u32, unique: bool, nightmare_or_hell: bool) -> i32 {
        if unique && !nightmare_or_hell {
            if self.unique_leaders.is_empty() {
                return -1;
            }
            return self.unique_leaders[room.pick(self.unique_leaders.len() as u32) as usize];
        }
        if self.roster.is_empty() {
            return -1;
        }
        let mut bucket = room.pick(self.total_rarity as u32) as i32 + 1;
        let mut pick = self.roster.len() - 1;
        for (i, &(_, rarity)) in self.roster.iter().enumerate() {
            bucket -= rarity;
            if bucket < 1 {
                pick = i;
                break;
            }
        }
        let class = self.roster[pick].0;
        match data.monsters().get(class).map(|m| m.spawn) {
            Some(rules) if rules.spawn >= 0 && rules.place_spawn => {
                if swap_above < room.roll() % 100 {
                    rules.spawn
                } else {
                    class
                }
            }
            _ => class,
        }
    }

    /// `MONSTERREGION_CheckSpawnDensity` (`0x005BE020`): whether this spawn leads a unique pack —
    /// likelier as the level fills while it is short of `MonUMin`, 6% below `MonUMax`.
    fn unique_pack(&self, room: &mut Seed, level_rooms: usize) -> bool {
        let placed = self.uniques_placed & 0xFF;
        if placed < self.uniques.0 && level_rooms != 0 && (room.roll() % 100) < (self.rooms_seen * 100 / level_rooms as i32) as u32 {
            return true;
        }
        if placed < self.uniques.1 && room.roll() % 100 < 6 {
            return true;
        }
        room.step();
        false
    }
}

/// Every level's monster region for one game.
#[derive(Debug, Clone, Default)]
pub struct Regions {
    by_level: HashMap<i32, Region>,
    nightmare_or_hell: bool,
}

impl Regions {
    /// Pick every level's roster for `game_seed` at `difficulty` (`0x005479C0`).
    #[must_use]
    pub fn build(data: &GameData, game_seed: u32, difficulty: u8) -> Self {
        let d = usize::from(difficulty.min(2));
        let monsters = data.monsters();
        let mut seed = Seed::new(game_seed, 0x29A);
        let mut by_level = HashMap::new();
        for id in 1..1024 {
            let Some(def) = data.levels().get(id) else { continue };
            let columns = &def.monsters;
            let names = if d == 0 { &columns.normal } else { &columns.nightmare };
            let mut pool: Vec<i32> = names.iter().filter_map(|n| monsters.class_named(n)).collect();
            let rules = |class: i32| monsters.get(class).map(|m| m.spawn).unwrap_or_default();
            let mut roster = Vec::new();
            let wanted = usize::try_from(columns.types).unwrap_or(0).min(ROSTER_MAX).min(pool.len());
            for picked in 0..wanted {
                if pool.is_empty() {
                    break;
                }
                let size = pool.len() as u32;
                let mut at = seed.pick(size) as usize;
                if picked == 0 && columns.ranged_first {
                    for _ in 0..20 {
                        if rules(pool[at]).ranged {
                            break;
                        }
                        at = seed.pick(size) as usize;
                    }
                }
                let class = pool.remove(at);
                let r = rules(class);
                if r.is_spawn && roster.len() < ROSTER_MAX {
                    roster.push((class, r.rarity));
                }
            }
            let total_rarity = roster.iter().map(|&(_, r)| r).sum();
            let unique_leaders = columns.unique.iter().filter_map(|n| monsters.class_named(n)).collect();
            let region = Region {
                density: columns.density[d],
                uniques: columns.uniques[d],
                roster,
                total_rarity,
                unique_leaders,
                ..Region::default()
            };
            by_level.insert(id, region);
        }
        Self { by_level, nightmare_or_hell: d != 0 }
    }

    /// A level's region.
    #[must_use]
    pub fn level(&self, id: i32) -> Option<&Region> {
        self.by_level.get(&id)
    }

    /// Spawn a room of `level`'s monsters: each slot that spawns is handed to `place` with the
    /// game and room seeds before the next slot rolls. `area` is the room in world subtiles
    /// `(x, y, width, height)`, `level_rooms` how many rooms the level has, `game` the game seed
    /// the slots step and `room` the room's seed.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_room(
        &mut self,
        data: &GameData,
        level: i32,
        area: (i32, i32, i32, i32),
        level_rooms: usize,
        game: &mut Seed,
        room: &mut Seed,
        place: &mut dyn FnMut(Group, &mut Seed, &mut Seed),
    ) {
        let nightmare_or_hell = self.nightmare_or_hell;
        let Some(region) = self.by_level.get_mut(&level) else { return };
        region.rooms_seen += 1;
        let density = region.density.min(10_000);
        if density <= 0 {
            return;
        }
        let (_, _, width, height) = area;
        for _ in 0..(height / 3) * (width / 3) {
            if game.roll() % 100_000 > density as u32 {
                continue;
            }
            let class = region.roll_class(data, room, 0x14, false, nightmare_or_hell);
            let Some(monster) = data.monsters().get(class) else { break };
            let (class, rules, unique) = if region.unique_pack(room, level_rooms) {
                let leader = region.roll_class(data, room, 0, true, nightmare_or_hell);
                let Some(m) = data.monsters().get(leader) else { continue };
                region.uniques_placed += 1;
                (leader, m.spawn, true)
            } else {
                (class, monster.spawn, false)
            };
            let group = if SINGLE_SPAWN_BASES.contains(&rules.base) || unique { (1, 1) } else { rules.group };
            if !unique && rules.sparse != 0 && rules.sparse < (game.roll() % 100) as i32 {
                continue;
            }
            if group.0 == 0 || group.1 == 0 || group.0 > group.1 {
                continue;
            }
            place(Group { class, size: group, unique }, game, room);
        }
    }
}

/// The collision bits a class may not stand on, by `MonStats2.txt` `spawnCol` (`0x005B2A00`).
#[must_use]
pub fn collision_mask(spawn_collision: u8) -> u16 {
    match spawn_collision {
        1 => 0x01C0,
        2 => 0x3F11,
        3 => 0,
        _ => 0x3C01,
    }
}

/// A rectangle of world subtiles, right and bottom exclusive (`PtInRect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// One past the right edge.
    pub right: i32,
    /// One past the bottom edge.
    pub bottom: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// What a room's spawns stand on: the map's collision and the monsters placed so far.
pub struct Ground<'a> {
    terrain: &'a dyn Fn(i32, i32) -> Option<u8>,
    monsters: HashSet<(i32, i32)>,
}

/// `COLBIT_MONSTER`.
const MONSTER_BIT: u16 = 0x100;

impl<'a> Ground<'a> {
    /// Ground over a collision lookup by world subtile (`None` where no room has a map).
    #[must_use]
    pub fn new(terrain: &'a dyn Fn(i32, i32) -> Option<u8>) -> Self {
        Self { terrain, monsters: HashSet::new() }
    }

    fn cell(&self, x: i32, y: i32) -> Option<u16> {
        let terrain = u16::from((self.terrain)(x, y)?);
        Some(if self.monsters.contains(&(x, y)) { terrain | MONSTER_BIT } else { terrain })
    }

    fn shape(size: u8) -> &'static [(i32, i32)] {
        match size {
            0 | 1 => &[(0, 0)],
            2 => &[(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)],
            _ => &[(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0), (-1, 1), (0, 1), (1, 1)],
        }
    }

    /// `0x0064D9B0`: whether any subtile of the shape has a bit of `mask` — or has no map.
    #[must_use]
    pub fn blocked(&self, x: i32, y: i32, size: u8, mask: u16) -> bool {
        if size > 3 {
            return true;
        }
        let mut bits = 0;
        for &(dx, dy) in Self::shape(size) {
            match self.cell(x + dx, y + dy) {
                Some(c) => bits |= c,
                None => return true,
            }
        }
        bits & mask != 0
    }

    /// Mark a monster standing at a spot, over the shape it is tested with.
    pub fn occupy(&mut self, x: i32, y: i32, size: u8) {
        for &(dx, dy) in Self::shape(size.min(3)) {
            self.monsters.insert((x + dx, y + dy));
        }
    }
}

/// `0x005B2A00`'s search for where a monster may stand around (`x`, `y`): with `rings` below 0,
/// that very spot; otherwise square rings 3, 6, … `rings × 3` subtiles out, each entered at a
/// point rolled on the room's seed and walked around. A spot must be inside `rect` and clear of
/// `mask` for the class's `size`.
#[allow(clippy::too_many_arguments)]
pub fn probe(room: &mut Seed, ground: &Ground<'_>, rect: Rect, x: i32, y: i32, rings: i32, size: u8, mask: u16) -> Option<(i32, i32)> {
    let (mut c, last) = if rings < 0 { (0, 0) } else { (3, rings * 3) };
    if last < c {
        return None;
    }
    while c <= last {
        room.step();
        let (mut dx, mut dy, mut dir) = if room.low & 1 == 0 {
            (room.pick(c as u32) as i32, c, (1, 0))
        } else {
            (c, room.pick(c as u32) as i32, (0, 1))
        };
        room.step();
        if room.low & 1 != 0 {
            dx = -dx;
        }
        room.step();
        if room.low & 1 != 0 {
            dy = -dy;
        }
        let (mut px, mut py) = (x + dx, y + dy);
        let (left, right, top, bottom) = (x - c, x + c, y - c, y + c);
        let mut steps = if c == 0 { 1 } else { c * 8 };
        while steps > 0 {
            if px == left && py == top {
                dir = (1, 0);
            }
            if px == right {
                if py == top {
                    dir = (0, 1);
                }
                if py == bottom {
                    dir = (-1, 0);
                }
            }
            if px == left {
                if py == bottom {
                    dir = (0, -1);
                }
                if py == top && px == right && py == bottom {
                    dir = (0, 0);
                }
            }
            px += dir.0;
            py += dir.1;
            if rect.contains(px, py) && !ground.blocked(px, py, size, mask) {
                return Some((px, py));
            }
            steps -= 1;
        }
        c += 3;
    }
    None
}

/// `SPAWN_FindRandomPositionForMonster` (`0x0054DC40`): up to 20 spots rolled inside the room's
/// inset (`0x0054DAC0`), skipping those `near` an arrival spot, the first a dry-run placement
/// ([`probe`] at the spot itself) accepts.
pub fn find_spot(room: &mut Seed, ground: &Ground<'_>, rect: Rect, size: u8, mask: u16, near: &dyn Fn(i32, i32) -> bool) -> Option<(i32, i32)> {
    let (left, top) = (rect.left + 1, rect.top + 1);
    let (width, height) = (rect.right - left, rect.bottom - top);
    for _ in 0..20 {
        let x = room.pick(width.max(0) as u32) as i32 + left;
        let y = room.pick(height.max(0) as u32) as i32 + top;
        if near(x, y) {
            continue;
        }
        if probe(room, ground, rect, x, y, -1, size, mask).is_some() {
            return Some((x, y));
        }
    }
    None
}

/// `0x004CC790`: `min` if `max` is not above it, else a pick in `min..=max`.
pub fn rand_range(seed: &mut Seed, min: i32, max: i32) -> i32 {
    if min >= max {
        return min;
    }
    seed.pick((max - min + 1) as u32) as i32 + min
}

/// One spawn: how many of a class to place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    /// The class.
    pub class: i32,
    /// `MinGrp`/`MaxGrp` (1 for a single spawn or a unique pack's leader).
    pub size: (i32, i32),
    /// A unique pack's leader, spawned as a plain monster of its class.
    pub unique: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_search_finds_the_one_open_subtile() {
        let open = |x: i32, y: i32| Some(if (x, y) == (106, 97) { 0 } else { 1 });
        let ground = Ground::new(&open);
        let rect = Rect { left: 80, top: 80, right: 120, bottom: 120 };
        let mut room = Seed::new(12345, 0x29A);
        assert_eq!(probe(&mut room.clone(), &ground, rect, 100, 100, -1, 1, 0x3C01), None, "the spot itself is walled");
        assert_eq!(probe(&mut room, &ground, rect, 100, 100, 3, 1, 0x3C01), Some((106, 97)), "on the second ring");
        assert!(ground.blocked(106, 97, 2, 0x3C01), "a cross reaches walled neighbours");
        assert!(!ground.blocked(106, 96, 1, 0), "no mask, nothing blocks");
        assert!(ground.blocked(106, 96, 1, 0x3C01));
    }

    #[test]
    fn a_spot_probe_costs_three_rolls_and_monsters_block_their_masks() {
        let open = |_: i32, _: i32| Some(0u8);
        let mut ground = Ground::new(&open);
        let rect = Rect { left: 0, top: 0, right: 40, bottom: 40 };
        let mut room = Seed::new(7, 0x29A);
        let spot = find_spot(&mut room, &ground, rect, 2, 0x3C01, &|_, _| false).unwrap();
        let mut expect = Seed::new(7, 0x29A);
        let x = expect.pick(39) as i32 + 1;
        let y = expect.pick(39) as i32 + 1;
        for _ in 0..3 {
            expect.step();
        }
        assert_eq!((spot, room), ((x, y), expect));
        ground.occupy(x, y, 2);
        assert!(!ground.blocked(x, y, 1, 0x3C01), "the default mask ignores monsters");
        assert!(ground.blocked(x, y, 1, 0x3F11));
        let mut far = Seed::new(7, 0x29A);
        assert_eq!(find_spot(&mut far, &ground, rect, 1, 0, &|_, _| true), None, "every roll near an arrival");
        assert_eq!(rand_range(&mut far, 3, 3), 3);
    }
}
