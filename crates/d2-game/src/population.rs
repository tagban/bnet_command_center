//! Filling a preset level's rooms with their units.
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
use d2_drlg::preset::{PresetLevel, UnitClass};
use d2_drlg::rng::Seed;

/// Engine unit types (`unit+0x00`).
pub mod unit_type {
    /// A player.
    pub const PLAYER: u8 = 0;
    /// A monster or NPC.
    pub const MONSTER: u8 = 1;
    /// An object.
    pub const OBJECT: u8 = 2;
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
        0 | 54 => Some(0),
        8 => Some(2),
        17 => Some(if is_town(level_id) { 2 } else { 0 }),
        _ => None,
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
    /// `game+0x90`, by unit type.
    last_guid: [u32; 6],
    rooms: Vec<Option<Vec<Spawned>>>,
}

impl Population {
    /// An empty population for a level of `rooms` rooms, with the game's random seed.
    #[must_use]
    pub fn new(game_seed: u32, rooms: usize) -> Self {
        Self { seed: Seed::new(game_seed, 0x29A), last_guid: [0; 6], rooms: vec![None; rooms] }
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
    pub fn units(&self, room: usize) -> Option<&[Spawned]> {
        self.rooms.get(room)?.as_deref()
    }

    /// Populate `room` of `level` unless it already is (`0x005559A0`), and return its units.
    pub fn activate(&mut self, data: &GameData, level: &PresetLevel, room: usize) -> Activated<'_> {
        let mut not_ported = Vec::new();
        if matches!(self.rooms.get(room), Some(None)) {
            let mut units = Vec::new();
            let position = |x: i32, y: i32| (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0));
            // Pass 1: everything but monsters.
            for unit in level.units_in(room) {
                let (x, y) = position(unit.x, unit.y);
                match &unit.class {
                    UnitClass::Monster(_) => {}
                    _ if UBER_LEVELS.contains(&level.level_id) => not_ported.push(format!("{:?} in an Uber level", unit.class)),
                    UnitClass::Object(0x23D) => {}
                    &UnitClass::Object(class) if class > 0x23D => {
                        not_ported.push(format!("object {class} at ({x}, {y}): special spawn 0x0054F490"));
                    }
                    &UnitClass::Object(class) => {
                        let def = data.objects().get(class);
                        match def.and_then(|d| object_mode(d.init_fn, d.pre_operate, level.level_id)) {
                            Some(mode) => {
                                let (guid, _) = self.allocate(unit_type::OBJECT);
                                units.push(Spawned::Object { guid, class: class as u16, x, y, mode, interaction: 0 });
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
            for unit in level.units_in(room) {
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
            self.rooms[room] = Some(units);
        }
        Activated { units: self.rooms.get(room).and_then(Option::as_deref).unwrap_or(&[]), not_ported }
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
        let mut pop = Population::new(0x1234, level.rooms.len());
        assert!(pop.units(0).is_none());
        let first = pop.activate(&data, &level, 0);
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

        let again = pop.activate(&data, &level, 0);
        assert_eq!((again.units.to_vec(), again.not_ported.len()), (units, 0), "populated once");
        let second = pop.activate(&data, &level, 1);
        assert!(matches!(second.units, [Spawned::Object { guid: 3, class: 3, mode: 0, .. }]), "guids carry on across rooms");
        assert!(pop.activate(&data, &level, 7).units.is_empty());
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
        let mut pop = Population::new(1, level.rooms.len());
        let mut objects = Vec::new();
        let mut monsters = Vec::new();
        for near in level.rooms_near(room) {
            let a = pop.activate(&data, &level, near);
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

    #[test]
    fn object_modes_follow_the_init_routines() {
        assert_eq!(object_mode(0, false, 2), Some(0));
        assert_eq!(object_mode(8, false, 2), Some(2));
        assert_eq!(object_mode(17, false, 1), Some(2));
        assert_eq!(object_mode(17, false, 3), Some(0), "a waypoint in the wild starts inactive");
        assert_eq!(object_mode(0, true, 1), None);
        assert_eq!(object_mode(23, false, 1), None);
        let mut pop = Population::new(0, 0);
        pop.last_guid[2] = u32::MAX;
        assert_eq!(pop.next_guid(unit_type::OBJECT), 1, "0 is skipped on wrapping");
    }
}
