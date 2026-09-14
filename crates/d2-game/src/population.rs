//! Filling a world's rooms with their units.
//!
//! The engine populates a room the first time a player comes near it (`0x0052D0F0`, bit 0 of
//! the room's `+0x34` flags): `0x005559A0` walks the room's preset units twice — everything that
//! is not a monster first, then the monsters — and spawns each through `0x005557D0`. Every unit
//! allocated takes the next guid for its type (`0x00552EE0`, counters at `game+0x90`, starting
//! at 1) and a seed stepped off the game's (`0x00552DF0`: `{game seed low, 0x29A}`).
//!
//! The game seed at `game+0xD0` is not the map seed: the engine starts it from the clock, so
//! which bow a town rogue carries differs from game to game on Blizzard's servers too.

use d2_data::presets::PresetMonster;
use d2_data::GameData;
use std::collections::HashMap;

use d2_drlg::preset::{PlacedUnit, PresetLevel, UnitClass};
use d2_drlg::rng::Seed;
use d2_drlg::world::RoomId;
use d2_drlg::Coords;

use crate::spawn::{collision_mask, find_spot, probe, rand_range, Ground, Group, Rect, Regions};

/// Engine unit types (`unit+0x00`).
pub mod unit_type {
    /// A player.
    pub const PLAYER: u8 = 0;
    /// A monster or NPC.
    pub const MONSTER: u8 = 1;
    /// An object.
    pub const OBJECT: u8 = 2;
    /// An item.
    pub const ITEM: u8 = 4;
    /// A warp tile.
    pub const WARP: u8 = 5;
}

/// The mode a map places its monsters in: neutral (`DRLGPRESET_LoadDrlgFile`, `0x00665950`).
pub const PRESET_MONSTER_MODE: u8 = 1;

/// A monster's life as `0xAC` and `0x6D` carry it, in 128ths; full is `0x80` (`0x005A5650`).
pub const FULL_LIFE: u8 = 0x80;

/// The Uber Tristram levels (`0x85..=0x88`) filter their preset objects; not ported.
const UBER_LEVELS: std::ops::RangeInclusive<i32> = 0x85..=0x88;

/// A unit spawned from the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spawned {
    /// An object (`0x51`).
    Object {
        /// Its guid.
        guid: u32,
        /// `objects.txt` class.
        class: u16,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
        /// Object mode after its `InitFn`.
        mode: u8,
        /// The object data's byte `+4` (portal target, shrine state…); 0 for map objects.
        interaction: u8,
    },
    /// A monster or NPC (`0xAC`).
    Monster {
        /// Its guid.
        guid: u32,
        /// `MonStats.txt` class.
        class: u16,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
        /// Unit mode.
        mode: u8,
        /// Life in 128ths.
        life: u8,
        /// The variant picked for each graphics component.
        components: [u8; 16],
        /// Variants each component has, for the wire.
        variants: [u8; 16],
    },
}

impl Spawned {
    /// The unit's guid.
    #[must_use]
    pub fn guid(&self) -> u32 {
        match self {
            Self::Object { guid, .. } | Self::Monster { guid, .. } => *guid,
        }
    }
}

/// The towns (`0x006426A0`): Rogue Encampment, Lut Gholein, Kurast Docks, Pandemonium Fortress,
/// Harrogath.
#[must_use]
pub fn is_town(level_id: i32) -> bool {
    matches!(level_id, 1 | 40 | 75 | 103 | 109)
}

/// The mode an object spawned from the map starts in: 0, unless its class's `InitFn` changes it
/// (`0x0054F5D0` calls table `0x00731BC0[InitFn]`). `None` for a routine not ported yet.
///
/// - 0: nothing to do.
/// - 8 (`0x005500C0`, torches): mode 2, lit.
/// - 17 (`0x00547210`, waypoints): mode 2 in a town (`0x0061AB00`). The routine first looks for
///   a pending activation of this waypoint, which a fresh game does not have.
/// - 16 (`0x00552B30`, wells): the mode stays 0; the interaction byte is twice `Parm2`
///   ([`object_interaction`]).
/// - 54 (`0x005940E0`, Cain's start in the Rogue Encampment): records the spot for his quest;
///   the mode stays 0.
///
/// `PreOperate` classes may roll into mode 2 afterwards; that roll is not ported.
#[must_use]
pub fn object_mode(init_fn: u8, pre_operate: bool, level_id: i32) -> Option<u8> {
    if pre_operate {
        return None;
    }
    match init_fn {
        0 | 16 | 54 => Some(0),
        8 => Some(2),
        17 => Some(if is_town(level_id) { 2 } else { 0 }),
        _ => None,
    }
}

/// The byte `0x51` carries from the object's data (`+4`) after an `InitFn` that sets none by a
/// roll: twice `Parm2` for a well (16); 0 otherwise.
#[must_use]
pub fn object_interaction(init_fn: u8, parm2: i32) -> u8 {
    match init_fn {
        16 => (parm2 as u8).wrapping_add(parm2 as u8),
        _ => 0,
    }
}

/// `objects.txt` `SubClass` bit of waypoints.
pub const SUBCLASS_WAYPOINT: u8 = 0x40;

/// Where a player entering a preset level without a warp is put, in world subtiles: on the
/// level's waypoint — the first room flagged as holding one, its first preset object whose
/// class is a waypoint (`0x0066AD80`), at that tile's subtile (3, 3) (`0x0061B060`). The engine
/// then takes the nearest spot free for the player (`0x0064E7B0`, collision mask `0x1C09`);
/// that search is not ported, and a waypoint has no collision of its own. `None` if the level
/// has no waypoint, where the engine falls back to other rooms (`0x0066B1F0`), not ported.
#[must_use]
pub fn waypoint_spawn(data: &GameData, level: &PresetLevel) -> Option<(u16, u16)> {
    level.units.iter().find_map(|u| match u.class {
        UnitClass::Object(class)
            if class <= 0x23C && data.objects().get(class).is_some_and(|o| o.sub_class & SUBCLASS_WAYPOINT != 0) =>
        {
            let at = |v: i32| u16::try_from(v.div_euclid(5) * 5 + 3).ok();
            Some((at(u.x)?, at(u.y)?))
        }
        _ => None,
    })
}

/// Pick each graphics component from the unit's seed (`0x005739D0`): uniform over the class's
/// variants, the seed untouched for a component with none.
#[must_use]
pub fn roll_components(seed: &mut Seed, variants: &[u8; 16]) -> [u8; 16] {
    variants.map(|count| seed.pick(u32::from(count)) as u8)
}

/// What became of a room's preset units the first time it was populated.
#[derive(Debug)]
pub struct Activated<'a> {
    /// The units spawned, in spawn order.
    pub units: &'a [Spawned],
    /// Units the map places that are not spawned because their code is not ported, described
    /// for logs. Critters are not listed: the engine never spawns them on the server.
    pub not_ported: Vec<String>,
}

/// A game's map units, room by room.
#[derive(Debug, Clone)]
pub struct Population {
    /// `game+0xD0`.
    seed: Seed,
    /// The object control's seed (`game+0x10F0`), which object init routines roll on.
    control: Seed,
    /// `game+0x90`, by unit type.
    last_guid: [u32; 6],
    /// The rooms populated so far.
    rooms: HashMap<RoomId, Vec<Spawned>>,
    /// Every level's monster roster, when rooms spawn monsters.
    regions: Option<Regions>,
}

/// The state a room's spawns share while it fills.
struct Placing<'g, 't> {
    game: &'g mut Seed,
    room: &'g mut Seed,
    ground: &'g mut Ground<'t>,
    rect: Rect,
    arrival: Option<((i32, i32), i32)>,
}

/// A room that spawns its level's monsters as it is populated.
#[derive(Clone, Copy)]
pub struct MonsterRoom<'a> {
    /// The room, in tiles.
    pub area: Coords,
    /// How many rooms its level has.
    pub level_rooms: usize,
    /// Collision flags by world subtile, `None` where no room has a map.
    pub collision: &'a dyn Fn(i32, i32) -> Option<u8>,
    /// Where players arrive in the level — its waypoint's tile corner, in world subtiles
    /// ([`arrival`]) — and `Levels.txt` `WarpDist`: spawns stay farther away than its square
    /// root.
    pub arrival: Option<((i32, i32), i32)>,
}

/// Where a level's players arrive, as monster spawns keep away from it (`0x0054DB50`): its first
/// waypoint's tile corner (`0x0066AD80`), in world subtiles.
#[must_use]
pub fn arrival(data: &GameData, units: &[PlacedUnit]) -> Option<(i32, i32)> {
    units.iter().find_map(|u| match u.class {
        UnitClass::Object(class) if class <= 0x23C && data.objects().get(class).is_some_and(|o| o.sub_class & SUBCLASS_WAYPOINT != 0) => {
            Some((u.x.div_euclid(5) * 5, u.y.div_euclid(5) * 5))
        }
        _ => None,
    })
}

impl Population {
    /// An empty population with the game's random seed.
    #[must_use]
    pub fn new(game_seed: u32) -> Self {
        let control = Seed::new(Seed::new(game_seed, 0x29A).roll(), 0x29A);
        Self { seed: Seed::new(game_seed, 0x29A), control, last_guid: [0; 6], rooms: HashMap::new(), regions: None }
    }

    /// Let rooms spawn their levels' monsters, with rosters picked for `difficulty` from the
    /// game seed ([`Regions::build`]).
    #[must_use]
    pub fn with_monsters(mut self, data: &GameData, game_seed: u32, difficulty: u8) -> Self {
        self.regions = Some(Regions::build(data, game_seed, difficulty));
        self
    }

    /// `0x005B2A00`: place a monster of `class` where [`probe`] finds room around (`x`, `y`) —
    /// `rings` below 0 for that spot — take its guid and seed from the game seed, roll its look,
    /// and unless `minions` is false bring its `PartyMin..=PartyMax` minions, `minion1` and
    /// `minion2` in turn, each in rings of 4 around it (`0x005B2830`). Returns where it stands and
    /// its seed.
    #[allow(clippy::too_many_arguments)]
    fn create(&mut self, data: &GameData, class: i32, at: (i32, i32), rings: i32, place: &mut Placing<'_, '_>, units: &mut Vec<Spawned>, minions: bool) -> Option<(i32, i32, Seed)> {
        let m = data.monsters().get(class).filter(|m| !m.critter)?;
        let (x, y) = probe(place.room, place.ground, place.rect, at.0, at.1, rings, m.size, collision_mask(m.spawn_collision))?;
        let (ux, uy) = (u16::try_from(x).ok()?, u16::try_from(y).ok()?);
        let seed = Seed::new(place.game.roll(), 0x29A);
        let guid = self.next_guid(unit_type::MONSTER);
        let mut seed = seed;
        let components = roll_components(&mut seed, &m.components);
        units.push(Spawned::Monster { guid, class: class as u16, x: ux, y: uy, mode: PRESET_MONSTER_MODE, life: FULL_LIFE, components, variants: m.components });
        place.ground.occupy(x, y, m.size);
        let rules = m.spawn;
        if minions && rules.minions[0] >= 0 {
            let count = rand_range(&mut seed, rules.party.0, rules.party.1);
            let alternates = usize::from(rules.minions[1] >= 0);
            let mut which = 0;
            for _ in 0..count.max(0) {
                self.create(data, rules.minions[which], (x, y), 4, place, units, false);
                which = if which + 1 > alternates { 0 } else { which + 1 };
            }
        }
        Some((x, y, seed))
    }

    /// `SPAWN_SpawnMonsterWithMinions` (`0x0054DF80`): a spot for the group ([`find_spot`]), its
    /// first member there, then `MinGrp - 1..=MaxGrp - 1` more (rolled on the first's seed) in
    /// rings of 3 around it.
    fn spawn_group(&mut self, data: &GameData, group: Group, place: &mut Placing<'_, '_>, units: &mut Vec<Spawned>) {
        let Some(m) = data.monsters().get(group.class) else { return };
        let arrival = place.arrival;
        let near = move |x: i32, y: i32| arrival.is_some_and(|((ax, ay), dist)| (x - ax) * (x - ax) + (y - ay) * (y - ay) < dist);
        let Some(spot) = find_spot(place.room, place.ground, place.rect, m.size, collision_mask(m.spawn_collision), &near) else { return };
        let Some((x, y, mut seed)) = self.create(data, group.class, spot, -1, place, units, true) else { return };
        let (min, max) = (group.size.0 as u8 - 1, group.size.1 as u8 - 1);
        let extra = seed.pick(u32::from(max - min) + 1) as i32 + i32::from(min);
        for _ in 0..extra {
            self.create(data, group.class, (x, y), 3, place, units, true);
        }
    }

    /// Take the next guid for a unit type (`0x00552EE0`, which skips 0 on wrapping).
    pub fn next_guid(&mut self, unit_type: u8) -> u32 {
        let last = &mut self.last_guid[usize::from(unit_type.min(5))];
        *last = last.wrapping_add(1).max(1);
        *last
    }

    /// Allocate a map unit: its guid and its own seed.
    fn allocate(&mut self, unit_type: u8) -> (u32, Seed) {
        let seed = Seed::new(self.seed.roll(), 0x29A);
        (self.next_guid(unit_type), seed)
    }

    /// The units of a room already populated.
    #[must_use]
    pub fn units(&self, room: RoomId) -> Option<&[Spawned]> {
        self.rooms.get(&room).map(Vec::as_slice)
    }

    /// `InitFn` 1 (`0x0054F9D0`): a shrine's type, rolled on the object control's seed. `Parm0`
    /// 0 picks among every type; 1 is a health shrine, 2 a mana shrine, anything else a boost nine
    /// times in ten and a magic shrine otherwise — each a pick from that effect class
    /// (`0x0054F770`). Either way up to eight tries for a type allowed this deep (`LevelMin`).
    /// Types 5, 4 and 16 become 3, 2 and 18.
    fn roll_shrine(&mut self, data: &GameData, parm0: i32, level_id: i32) -> u8 {
        let shrines = data.shrines();
        let too_deep = |shrine: usize| shrines.get(shrine).is_some_and(|s| (level_id as u32) < s.level_min as u32);
        let mut shrine;
        if parm0 == 0 {
            let mut tries = 8;
            loop {
                shrine = self.control.pick(shrines.len().saturating_sub(1) as u32) as usize + 1;
                tries -= 1;
                if !too_deep(shrine) || tries <= 0 {
                    break;
                }
            }
        } else {
            let class = match parm0 {
                1 => 2,
                2 => 3,
                _ => {
                    if self.control.roll() % 10 != 0 {
                        4
                    } else {
                        1
                    }
                }
            };
            let bucket = shrines.of_class(class);
            let mut tries = 8;
            loop {
                shrine = bucket.get(self.control.pick(bucket.len() as u32) as usize).copied().unwrap_or(0).max(1);
                tries -= 1;
                if !too_deep(shrine) || tries <= 0 {
                    break;
                }
            }
        }
        match shrine {
            5 => 3,
            4 => 2,
            16 => 18,
            other => u8::try_from(other).unwrap_or(0),
        }
    }

    /// A spawned unit by engine unit type and guid, with the room it was spawned in.
    #[must_use]
    pub fn find(&self, kind: u8, guid: u32) -> Option<(RoomId, &Spawned)> {
        self.rooms.iter().find_map(|(room, units)| {
            units
                .iter()
                .find(|u| match u {
                    Spawned::Object { guid: g, .. } => kind == unit_type::OBJECT && *g == guid,
                    Spawned::Monster { guid: g, .. } => kind == unit_type::MONSTER && *g == guid,
                })
                .map(|u| (*room, u))
        })
    }

    /// Where a spawned monster now stands, its mode and its life in 128ths — so a room that comes
    /// back into view sends it as it is.
    pub fn update_monster(&mut self, monster: u32, to: (u16, u16), new_mode: u8, new_life: u8) {
        for unit in self.rooms.values_mut().flatten() {
            if let Spawned::Monster { guid, x, y, mode, life, .. } = unit {
                if *guid == monster {
                    (*x, *y, *mode, *life) = (to.0, to.1, new_mode, new_life);
                    return;
                }
            }
        }
    }

    /// Move a spawned monster into the room it now stands in, if that room has been populated
    /// (a room not yet in play must still spawn its own units); its room before, if it moved.
    pub fn move_monster(&mut self, monster: u32, to: RoomId) -> Option<RoomId> {
        if !self.rooms.contains_key(&to) {
            return None;
        }
        let (&from, index) = self.rooms.iter().find_map(|(room, units)| {
            units.iter().position(|u| matches!(u, Spawned::Monster { guid, .. } if *guid == monster)).map(|i| (room, i))
        })?;
        if from == to {
            return None;
        }
        let unit = self.rooms.get_mut(&from)?.remove(index);
        self.rooms.get_mut(&to)?.push(unit);
        Some(from)
    }

    /// Change a spawned object's mode (`0x00624690`); `true` if it changed.
    pub fn set_object_mode(&mut self, object: u32, to: u8) -> bool {
        for unit in self.rooms.values_mut().flatten() {
            if let Spawned::Object { guid, mode, .. } = unit {
                if *guid == object {
                    let changed = *mode != to;
                    *mode = to;
                    return changed;
                }
            }
        }
        false
    }

    /// Populate `room` of level `level_id` from the units standing in it (`0x005559A0`) and, for
    /// a [`MonsterRoom`], its level's monsters ([`Regions::spawn_room`]), unless it already is;
    /// return its units.
    pub fn activate<'u>(
        &mut self,
        data: &GameData,
        level_id: i32,
        room: RoomId,
        placed: impl IntoIterator<Item = &'u PlacedUnit>,
        monsters: Option<MonsterRoom>,
    ) -> Activated<'_> {
        let mut not_ported = Vec::new();
        if !self.rooms.contains_key(&room) {
            let placed: Vec<&PlacedUnit> = placed.into_iter().collect();
            let mut units = Vec::new();
            let position = |x: i32, y: i32| (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0));
            // Pass 1: everything but monsters.
            for &unit in &placed {
                let (x, y) = position(unit.x, unit.y);
                match &unit.class {
                    UnitClass::Monster(_) => {}
                    _ if UBER_LEVELS.contains(&level_id) => not_ported.push(format!("{:?} in an Uber level", unit.class)),
                    UnitClass::Object(0x23D) => {}
                    &UnitClass::Object(class) if class > 0x23D => {
                        not_ported.push(format!("object {class} at ({x}, {y}): special spawn 0x0054F490"));
                    }
                    &UnitClass::Object(class) if data.objects().get(class).is_some_and(|d| d.init_fn == 1 && !d.pre_operate) => {
                        let (guid, _) = self.allocate(unit_type::OBJECT);
                        let parm0 = data.objects().get(class).map_or(0, |d| d.parm0);
                        let shrine = self.roll_shrine(data, parm0, level_id);
                        units.push(Spawned::Object { guid, class: class as u16, x, y, mode: 0, interaction: shrine });
                    }
                    &UnitClass::Object(class) => {
                        let def = data.objects().get(class);
                        match def.and_then(|d| object_mode(d.init_fn, d.pre_operate, level_id)) {
                            Some(mode) => {
                                let (guid, _) = self.allocate(unit_type::OBJECT);
                                let interaction = def.map_or(0, |d| object_interaction(d.init_fn, d.parm2));
                                units.push(Spawned::Object { guid, class: class as u16, x, y, mode, interaction });
                            }
                            None => not_ported.push(format!(
                                "object {class} ({}) at ({x}, {y}): InitFn {} / PreOperate",
                                def.map_or("?", |d| d.name.as_str()),
                                def.map_or(0, |d| d.init_fn)
                            )),
                        }
                    }
                    UnitClass::Other { kind, id } => not_ported.push(format!("map unit type {kind} id {id} at ({x}, {y})")),
                }
            }
            // Pass 2: monsters (0x0054E600 → 0x0054E490).
            for &unit in &placed {
                let UnitClass::Monster(preset) = &unit.class else { continue };
                let (x, y) = position(unit.x, unit.y);
                match preset {
                    PresetMonster::Class { class, name } => match data.monsters().get(*class) {
                        Some(m) if m.critter => {}
                        Some(m) => {
                            let (guid, mut seed) = self.allocate(unit_type::MONSTER);
                            units.push(Spawned::Monster {
                                guid,
                                class: *class as u16,
                                x,
                                y,
                                mode: PRESET_MONSTER_MODE,
                                life: FULL_LIFE,
                                components: roll_components(&mut seed, &m.components),
                                variants: m.components,
                            });
                        }
                        None => not_ported.push(format!("monster {class} ({name}) at ({x}, {y}): no MonStats2 row")),
                    },
                    other => not_ported.push(format!("{other:?} at ({x}, {y})")),
                }
            }
            // Take the rosters only for a room that spawns: a town room must not drop them.
            if let Some((spot, mut regions)) = monsters.and_then(|spot| Some((spot, self.regions.take()?))) {
                let s = 5;
                let area = (spot.area.x * s, spot.area.y * s, spot.area.w * s, spot.area.h * s);
                let rect = Rect { left: area.0, top: area.1, right: area.0 + area.2, bottom: area.1 + area.3 };
                let mut room_seed = Seed::new(self.control.roll(), 0x29A);
                let mut ground = Ground::new(spot.collision);
                let mut game = self.seed;
                regions.spawn_room(data, level_id, area, spot.level_rooms, &mut game, &mut room_seed, &mut |group, game, room| {
                    if group.unique {
                        not_ported.push(format!("unique pack of monster {} spawned as a plain group", group.class));
                    }
                    let mut place = Placing { game, room, ground: &mut ground, rect, arrival: spot.arrival };
                    self.spawn_group(data, group, &mut place, &mut units);
                });
                self.seed = game;
                self.regions = Some(regions);
            }
            self.rooms.insert(room, units);
        }
        Activated { units: self.rooms.get(&room).map_or(&[], Vec::as_slice), not_ported }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::monsters::{Monsters, COMPONENT_COLUMNS};
    use d2_data::presets::{MonPresets, Objects};
    use d2_drlg::preset::PlacedUnit;
    use d2_drlg::Coords;
    use d2_formats::excel::Table;

    /// Made-up rules in the tables' real shape — no Blizzard data in tests. Object 1 is a torch
    /// (InitFn 8), 2 a waypoint (17), 3 a crate (0), 4 something unported (99). Monster 10 is an
    /// NPC whose LH has two variants, 11 a critter.
    pub(crate) fn rules() -> GameData {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\r\n");
        for name in d2_data::CLASSES {
            cs.push_str(&format!("{name}\t1\t1\t1\t1\t0\t1\t1\r\n"));
        }
        let exp = "Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\r\n0\t0\t0\t0\t0\t0\t0\t0\r\n1\t1\t1\t1\t1\t1\t1\t1\r\n";
        let mut data = GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(exp.as_bytes())).unwrap();
        let objects = Table::parse(b"Name\tInitFn\tPreOperate\r\nnone\t0\t0\r\ntorch\t8\t0\r\nwaypoint\t17\t0\r\ncrate\t0\t0\r\nodd\t99\t0\r\n");
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\r\nguard\t10\tguard\r\nhen\t11\then\r\n");
        let mut ms2 = String::from("Id\tcritter");
        for c in COMPONENT_COLUMNS {
            ms2 += &format!("\t{c}");
        }
        ms2 += "\r\nguard\t0\t\tlit\t\t\t\t\tsbw,lbw\r\nhen\t1\t\tlit\r\n";
        let monpreset = Table::parse(b"Act\tPlace\r\n1\tguard\r\n1\then\r\n");
        let empty = |col: &str| Table::parse(format!("{col}\r\n").as_bytes());
        let presets = MonPresets::from_tables(&monpreset, &monstats, &empty("Superunique"), &empty("code")).unwrap();
        let monsters = Monsters::from_tables(&monstats, &Table::parse(ms2.as_bytes())).unwrap();
        data.set_map_tables(presets, monsters, Objects::from_table(&objects));
        data
    }

    fn monster(class: i32, name: &str) -> UnitClass {
        UnitClass::Monster(PresetMonster::Class { class, name: name.into() })
    }

    /// Two rooms side by side in a town; the first holds (in map order) a guard, a torch, a hen,
    /// a waypoint, an unported object and another guard.
    pub(crate) fn town() -> PresetLevel {
        let at = |class, x, y| PlacedUnit { class, x, y, path: Vec::new() };
        PresetLevel {
            level_id: 1,
            area: Coords { x: 100, y: 100, w: 16, h: 8 },
            map: String::new(),
            rooms: vec![Coords { x: 100, y: 100, w: 8, h: 8 }, Coords { x: 108, y: 100, w: 8, h: 8 }],
            units: vec![
                at(monster(10, "guard"), 505, 505),
                at(UnitClass::Object(1), 510, 510),
                at(monster(11, "hen"), 511, 511),
                at(UnitClass::Object(2), 512, 512),
                at(UnitClass::Object(4), 513, 513),
                at(monster(10, "guard"), 514, 514),
                at(UnitClass::Object(3), 545, 505),
            ],
        }
    }

    #[test]
    fn objects_spawn_before_monsters_with_their_init_modes_and_critters_stay_with_the_client() {
        let (data, level) = (rules(), town());
        let room = |index| RoomId { level: 1, index };
        let mut pop = Population::new(0x1234);
        assert!(pop.units(room(0)).is_none());
        let first = pop.activate(&data, 1, room(0), level.units_in(0), None);
        assert_eq!(first.not_ported.len(), 1, "{:?}", first.not_ported);
        let units = first.units.to_vec();
        assert_eq!(units.len(), 4, "torch, waypoint, two guards; no hen");
        assert!(matches!(units[0], Spawned::Object { guid: 1, class: 1, mode: 2, x: 510, .. }), "a lit torch");
        assert!(matches!(units[1], Spawned::Object { guid: 2, class: 2, mode: 2, .. }), "a town waypoint is active");
        assert!(matches!(units[2], Spawned::Monster { guid: 1, class: 10, mode: 1, life: 0x80, x: 505, .. }), "monster guids count apart");
        assert!(matches!(units[3], Spawned::Monster { guid: 2, x: 514, .. }));
        let Spawned::Monster { components, variants, .. } = &units[2] else { unreachable!() };
        assert_eq!((variants[1], variants[6]), (1, 2));
        assert!(components.iter().zip(variants).all(|(&c, &v)| c < v.max(1)), "each pick within its variants");

        assert!(matches!(pop.find(unit_type::MONSTER, 2), Some((r, Spawned::Monster { x: 514, .. })) if r == room(0)));
        assert!(pop.find(unit_type::OBJECT, 9).is_none() && pop.find(unit_type::PLAYER, 1).is_none());
        let again = pop.activate(&data, 1, room(0), level.units_in(0), None);
        assert_eq!((again.units.to_vec(), again.not_ported.len()), (units, 0), "populated once");
        let second = pop.activate(&data, 1, room(1), level.units_in(1), None);
        assert!(matches!(second.units, [Spawned::Object { guid: 3, class: 3, mode: 0, .. }]), "guids carry on across rooms");
        assert!(pop.set_object_mode(3, 1) && !pop.set_object_mode(3, 1) && !pop.set_object_mode(99, 1));
        assert!(matches!(pop.units(room(1)), Some([Spawned::Object { mode: 1, .. }])));
        assert!(pop.activate(&data, 1, room(7), level.units_in(7), None).units.is_empty());
    }

    /// A shrine is spawned with its type in the interaction byte: a health one from the health
    /// class, and any from its class allowed at the level's depth.
    #[test]
    fn shrines_roll_their_type_from_their_class() {
        use d2_data::presets::Shrines;
        let mut data = rules();
        let objects = Table::parse(b"Name\tInitFn\tPreOperate\tParm0\r\nnone\t0\t0\t0\r\nwell\t1\t0\t1\r\nshrine\t1\t0\t3\r\n");
        let monstats = Table::parse(b"Id\thcIdx\r\n");
        let empty = |col: &str| Table::parse(format!("{col}\r\n").as_bytes());
        let presets = MonPresets::from_tables(&empty("Act\tPlace"), &monstats, &empty("Superunique"), &empty("code")).unwrap();
        data.set_map_tables(presets, Monsters::default(), Objects::from_table(&objects));
        // 0 none; 1-2 boosts; 3 health; 4 magic, only from level 50; 5 mana.
        data.set_shrines(Shrines::from_table(&Table::parse(
            b"effectclass\tLevelMin\r\n0\t0\r\n4\t1\r\n4\t1\r\n2\t1\r\n1\t50\r\n3\t1\r\n",
        )));
        let at = |class, x| PlacedUnit { class: UnitClass::Object(class), x, y: 505, path: Vec::new() };
        let units = [at(1, 505), at(2, 506), at(2, 507), at(2, 508)];
        for seed in 0..50 {
            let mut pop = Population::new(seed);
            let got = pop.activate(&data, 2, RoomId { level: 2, index: 0 }, units.iter(), None);
            assert!(got.not_ported.is_empty());
            let types: Vec<u8> = got.units.iter().map(|u| match u { Spawned::Object { interaction, mode: 0, .. } => *interaction, _ => 99 }).collect();
            assert_eq!(types[0], 3, "the well is the health shrine");
            // A shrine is a boost (1, 2) nine times in ten; a magic pick finds only type 4, too
            // deep for level 2, and gives up on it after eight tries; 4 is sent as 2.
            assert!(types[1..].iter().all(|t| matches!(t, 1 | 2)), "seed {seed}: {types:?}");
        }
    }

    #[test]
    fn components_are_seed_picks_and_a_missing_component_leaves_the_seed_alone() {
        let mut variants = [0u8; 16];
        variants[1] = 1;
        variants[6] = 2;
        variants[8] = 5;
        let mut seed = Seed::new(77, 0x29A);
        let got = roll_components(&mut seed, &variants);
        let mut expect_seed = Seed::new(77, 0x29A);
        let (tr, lh, s1) = (expect_seed.pick(1), expect_seed.pick(2), expect_seed.pick(5));
        assert_eq!((got[1], got[6], got[8]), (tr as u8, lh as u8, s1 as u8));
        assert_eq!(seed, expect_seed, "exactly three steps");
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`) and `Game.exe` (`BNETCC_D2_GAME_EXE`):
    /// around the campfire of the Rogue Encampment for seed 0x12345678 stand the stash, a lit
    /// torch and the camp's NPCs, and nothing the map places there is left unported.
    #[test]
    fn with_a_real_install_the_campfire_rooms_fill_with_the_camp() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = d2_data::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let act = d2_drlg::act::Act::build(data.levels(), 0, 0, 0x1234_5678);
        let level = PresetLevel::build(&data, &engine, &act, 1).unwrap();
        let room = level.room_index_at(5810, 4450).unwrap();
        let mut pop = Population::new(1);
        let mut objects = Vec::new();
        let mut monsters = Vec::new();
        for near in level.rooms_near(room) {
            let a = pop.activate(&data, 1, RoomId { level: 1, index: near }, level.units_in(near), None);
            assert!(a.not_ported.is_empty(), "{:?}", a.not_ported);
            for u in a.units {
                match *u {
                    Spawned::Object { class, mode, .. } => objects.push((class, mode)),
                    Spawned::Monster { class, .. } => monsters.push(class),
                }
            }
        }
        assert!(objects.contains(&(267, 0)), "the stash: {objects:?}");
        assert!(objects.contains(&(37, 2)), "a lit torch: {objects:?}");
        for npc in [147, 148, 150, 152, 154, 155] {
            assert!(monsters.contains(&npc), "{npc} in {monsters:?}");
        }
        assert!(!monsters.contains(&149), "chickens are the client's");
        assert_eq!(waypoint_spawn(&data, &level), Some((5798, 4413)), "on the waypoint at (5799, 4414)");

        // Every game seed gives one of the four camps, each with a waypoint to start on.
        let mut maps = std::collections::BTreeSet::new();
        for seed in (0..400u32).map(|i| i.wrapping_mul(0x9E37_79B9) ^ 0x5A5A) {
            let act = d2_drlg::act::Act::build(data.levels(), 0, 0, seed);
            let town = PresetLevel::build(&data, &engine, &act, 1).unwrap_or_else(|e| panic!("seed {seed:#x}: {e}"));
            let (x, y) = waypoint_spawn(&data, &town).unwrap_or_else(|| panic!("seed {seed:#x}: no waypoint"));
            assert!(town.room_index_at(x.into(), y.into()).is_some(), "seed {seed:#x}: spawn outside the town");
            maps.insert(town.map);
        }
        assert_eq!(maps.len(), 4, "{maps:?}");
    }

    /// With the operator's install and `Game.exe`: every object the Act I wilderness rooms place
    /// spawns — waypoints, their torches, shrines and wells — for a few seeds.
    #[test]
    fn with_a_real_install_the_wilderness_objects_spawn() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = d2_data::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        for seed in [1u32, 0x1234_5678, 0xBEEF] {
            let act = d2_drlg::act::Act::build(data.levels(), 0, 0, seed);
            let town = PresetLevel::build(&data, &engine, &act, 1).unwrap();
            let world = d2_drlg::world::World::build(&data, &engine, &act, Some(&town), &d2_drlg::collision::TileSources::new());
            let mut pop = Population::new(seed);
            let (mut shrines, mut waypoints) = (0, 0);
            for level in world.levels().iter().filter(|l| (2..=7).contains(&l.id)) {
                for index in 0..level.rooms.len() {
                    let room = RoomId { level: level.id, index };
                    let got = pop.activate(&data, level.id, room, world.units_in(room), None);
                    assert!(got.not_ported.is_empty(), "seed {seed:#x} {room:?}: {:?}", got.not_ported);
                    for unit in got.units {
                        if let Spawned::Object { class, interaction, .. } = *unit {
                            let def = data.objects().get(i32::from(class)).unwrap();
                            if def.init_fn == 1 {
                                assert!(interaction > 0, "{} got no shrine type", def.name);
                                shrines += 1;
                            }
                            waypoints += usize::from(def.sub_class & SUBCLASS_WAYPOINT != 0);
                        }
                    }
                }
            }
            assert_eq!(waypoints, 4, "seed {seed:#x}");
            assert!(shrines >= 20, "seed {seed:#x}: {shrines} shrines");
        }
    }

    /// With the operator's install and `Game.exe`: walking every open room of Act I's wilderness,
    /// each level fills with its own roster's monsters and their minions, in numbers like the
    /// game's.
    #[test]
    fn with_a_real_install_the_wilderness_fills_with_monsters() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(&dir).unwrap();
        let engine = d2_data::engine::EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        for seed in [1u32, 0x1234_5678, 0xBEEF] {
            let act = d2_drlg::act::Act::build(data.levels(), 0, 0, seed);
            let town = PresetLevel::build(&data, &engine, &act, 1).unwrap();
            let world = d2_drlg::world::World::build(&data, &engine, &act, Some(&town), &d2_drlg::collision::TileSources::new());
            let mut pop = Population::new(seed).with_monsters(&data, seed, 0);
            // A player starts in town: its rooms spawn no monsters and must leave the rosters.
            let camp = RoomId { level: 1, index: 0 };
            pop.activate(&data, 1, camp, world.units_in(camp), None);
            let collision = |x: i32, y: i32| world.collision_at(x, y);
            for level in world.levels().iter().filter(|l| (2..=7).contains(&l.id)) {
                let roster: Vec<i32> = pop.regions.as_ref().unwrap().level(level.id).unwrap().roster().iter().map(|r| r.0).collect();
                let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
                let arrival = arrival(&data, &level.units).map(|spot| (spot, data.levels().get(level.id).unwrap().warp_dist));
                assert_eq!(arrival.is_some(), (3..=6).contains(&level.id), "level {}: waypoint", level.id);
                for index in 0..level.rooms.len() {
                    let piece = level.pieces[index];
                    if piece != 0 && !data.lvl_prests().by_def(piece).unwrap().populate {
                        continue;
                    }
                    let room = RoomId { level: level.id, index };
                    let spot = MonsterRoom { area: level.rooms[index], level_rooms: level.rooms.len(), collision: &collision, arrival };
                    let got = pop.activate(&data, level.id, room, world.units_in(room), Some(spot));
                    for unit in got.units {
                        if let Spawned::Monster { class, x, y, .. } = *unit {
                            let m = data.monsters().get(i32::from(class)).unwrap();
                            let def = &data.levels().get(level.id).unwrap().monsters;
                            let leaders: Vec<i32> = roster.iter().copied().chain(def.unique.iter().filter_map(|n| data.monsters().class_named(n))).collect();
                            let allowed = leaders.iter().any(|&l| {
                                let r = data.monsters().get(l).unwrap().spawn;
                                l == i32::from(class) || r.minions.contains(&i32::from(class)) || r.spawn == i32::from(class)
                            });
                            assert!(allowed, "level {}: {} is no roster class, minion or replacement", level.id, m.id);
                            let r = level.rooms[index];
                            let (x, y) = (i32::from(x), i32::from(y));
                            assert!(x >= r.x * 5 && x < (r.x + r.w) * 5 && y >= r.y * 5 && y < (r.y + r.h) * 5, "inside its room");
                            let walk = world.collision_at(x, y).unwrap();
                            assert_eq!(walk & d2_drlg::collision::WALL, 0, "level {}: {} stands on a wall at ({x}, {y})", level.id, m.id);
                            *counts.entry(m.id.clone()).or_default() += 1;
                        }
                    }
                }
                let total: usize = counts.values().sum();
                eprintln!("seed {seed:#x} level {}: {total} monsters {counts:?}", level.id);
                assert!((10..400).contains(&total), "seed {seed:#x} level {}: {total}", level.id);
            }
        }
    }

    #[test]
    fn object_modes_follow_the_init_routines() {
        assert_eq!(object_mode(0, false, 2), Some(0));
        assert_eq!(object_mode(8, false, 2), Some(2));
        assert_eq!(object_mode(17, false, 1), Some(2));
        assert_eq!(object_mode(17, false, 3), Some(0), "a waypoint in the wild starts inactive");
        assert_eq!(object_mode(0, true, 1), None);
        assert_eq!(object_mode(23, false, 1), None);
        assert_eq!((object_mode(16, false, 3), object_interaction(16, 5), object_interaction(8, 5)), (Some(0), 10, 0), "a well");
        let mut pop = Population::new(0);
        pop.last_guid[2] = u32::MAX;
        assert_eq!(pop.next_guid(unit_type::OBJECT), 1, "0 is skipped on wrapping");
    }
}
