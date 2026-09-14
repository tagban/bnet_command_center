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
//! (`0x005BE020`) decides whether it leads a unique pack instead. A group
//! (`SPAWN_SpawnMonsterWithMinions`, `0x0054DF80`) is `MinGrp..=MaxGrp` of the class — just one
//! for fallen and scarabs, whose minions make their group — at a random spot in the room, and
//! each member brings `PartyMin..=PartyMax` of its `minion1`.
//!
//! Not ported: the collision test for where a monster may stand (spots are random), unique pack
//! leaders' names and mods (they spawn as plain monsters of their class), champions, and
//! wandering monsters. Ported from libd2 `packages/drlg/src/drlg/monpop.zig` (MIT); the group and
//! minion counts follow the 1.14d `Game.exe` code at `0x0054DF80`.

use std::collections::HashMap;

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

    /// The groups a room of `level` spawns, in order. `area` is the room in world subtiles
    /// `(x, y, width, height)`, `level_rooms` how many rooms the level has, `game` the game seed
    /// the slots step and `room` the room's seed.
    pub fn spawn_room(&mut self, data: &GameData, level: i32, area: (i32, i32, i32, i32), level_rooms: usize, game: &mut Seed, room: &mut Seed) -> Vec<Group> {
        let nightmare_or_hell = self.nightmare_or_hell;
        let Some(region) = self.by_level.get_mut(&level) else { return Vec::new() };
        region.rooms_seen += 1;
        let density = region.density.min(10_000);
        if density <= 0 {
            return Vec::new();
        }
        let (x0, y0, width, height) = area;
        let mut groups = Vec::new();
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
            // SPAWN_FindRandomPositionForMonster (0x0054DC40), inside GetRoomCorners' inset.
            let x = x0 + 1 + room.pick((width - 1).max(1) as u32) as i32;
            let y = y0 + 1 + room.pick((height - 1).max(1) as u32) as i32;
            groups.push(Group { class, x, y, size: group, party: rules.party, minions: rules.minions, unique });
        }
        groups
    }
}

/// One spawn: a group of a class around a spot, with each member's minions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    /// The class.
    pub class: i32,
    /// Where the first member stands, world subtiles.
    pub x: i32,
    /// Where the first member stands, world subtiles.
    pub y: i32,
    /// `MinGrp`/`MaxGrp` (1 for a single spawn or a unique pack's leader).
    pub size: (i32, i32),
    /// `PartyMin`/`PartyMax` minions per member.
    pub party: (i32, i32),
    /// `minion1`/`minion2`.
    pub minions: [i32; 2],
    /// A unique pack's leader, spawned as a plain monster of its class.
    pub unique: bool,
}
