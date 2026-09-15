//! Diablo II game server — **test server** (`diablo2.game_server_probe`).
//!
//! A client that joins a game is taken through the join exactly as
//! the 1.14d engine runs it (`docs/D2GS-114D-WIRE.md` §4): `AF 01`, then on `GAMELOGON`
//! `01 00` and `02`, then on `ENTERGAME` `59 5E 28 29 0B`, the player's stats, `23 23 95 03 53 07`, the
//! rooms around the spawn with their objects and NPCs (`07`, `51`, `AC AA 6D`), `15 7E` and, a
//! server frame later, `04`.
//! After that the server follows the player's walking with rooms, answers NPCs, the stash and
//! waypoints, and runs the fight (`d2_game::battle`): swings at monsters, monsters chasing and
//! hitting back, deaths, experience and levels. Every packet the client sends is logged.
//!
//! Games live only in memory. The realm creates them (`MCP_CREATEGAME`), stages a join for
//! one character (`MCP_JOINGAME`), and this module matches the client's `GAMELOGON` against
//! that staging. The engine tables come from the operator's own `Game.exe`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bnetcc_proto::d2::status;
use bnetcc_proto::d2gs::{self, cs, item_action, join_failed, ClientPacketLen, EngineTables, GameLogon, Outbox};
use bnetcc_storage::Character;
use d2_data::engine::EngineData;
use d2_data::{stat, GameData};
use d2_data::item_bits::{self, Item, Location};
use d2_formats::d2s::Save;
use d2_drlg::act::Act;
use d2_drlg::collision::TileSources;
use d2_drlg::preset::PresetLevel;
use d2_drlg::world::{RoomId, World};
use d2_game::battle::{self, Battle, Event};
use d2_game::clock::ActClock;
use d2_game::inventory::{Held, Inventory, Place};
use d2_game::loot;
use d2_game::population::{unit_type, waypoint_spawn, MonsterRoom, Population, Spawned, SUBCLASS_WAYPOINT};
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::session::hex_preview;

mod items;
pub mod map;
mod trade;

/// Players per game, as the engine allows.
const MAX_PLAYERS: usize = 8;

/// A game nobody has connected to is dropped after this long.
const UNJOINED_GAME_TTL: Duration = Duration::from_secs(10 * 60);

/// Time to send `GAMELOGON` after connecting.
const LOGON_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle limit once in a game; the client pings every few seconds.
const IN_GAME_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One engine frame at 25 frames per second — the gap between `SrvJoinAct` and
/// `sUpdateClients` sending `04`.
const SERVER_FRAME: Duration = Duration::from_millis(40);

/// The Rogue Encampment.
const TOWN_AREA: u16 = 1;
/// Map seed for a game whose town cannot be built (no MPQs, or a build error). Each game
/// otherwise gets its own random seed, as the engine's `game+0x7C`, and with it one of the
/// four camps. `jaenster/libd2`'s engine dump for this seed puts its waypoint at (5799, 4414).
const FALLBACK_MAP_SEED: u32 = 0x1234_5678;
/// Spawn for [`FALLBACK_MAP_SEED`]: the waypoint's tile at subtile (3, 3)
/// (`d2_game::population::waypoint_spawn`).
const FALLBACK_SPAWN: (u16, u16) = (5798, 4413);
/// Top-left tile of the room holding [`FALLBACK_SPAWN`].
const FALLBACK_SPAWN_ROOM: (u16, u16) = (1152, 880);
/// Guid of the joining player's unit.
const PLAYER_GUID: u32 = 1;

/// The NPC classes that heal a player who talks to them (`0x00578E70`): Akara 148, Atma 176, Fara 178,
/// Ormus 255, Jamella 405 and Malah 513.
const HEALERS: [u16; 6] = [148, 176, 178, 255, 405, 513];

/// How long a swing at a monster out of reach waits for the player to get there.
const ATTACK_WAIT: Duration = Duration::from_secs(3);

/// Walking speed in subtiles a second: `CharStats.txt` `WalkVelocity` 6 as the engine's
/// `dwVelocity` (`6 << 8`), which moves `velocity / 16` of a subtile a frame (libd2
/// `world/src/motion.zig`, from `0x0064FE40`) at 25 frames a second.
const WALK_SPEED: f64 = 6.0 * 25.0 / 16.0;
/// Running speed, from `RunVelocity` 9 the same way.
const RUN_SPEED: f64 = 9.0 * 25.0 / 16.0;
/// A room leaves a client's view only once it is this many tiles from the player's room — one
/// room further than the engine's near gap (6). The server walks straight lines without
/// collision, so it can run ahead of the client's player; dropping a room the client still
/// stands in would free it under its player.
const KEEP_GAP: i32 = 6 + 8;

/// The test game server: its engine tables, the game rules, and the games the realm has created.
#[derive(Debug)]
pub struct GameServer {
    tables: EngineTables,
    /// `charstats.txt` and friends from the install; without them a player joins with no stats.
    rules: Option<GameData>,
    /// Where each game's Rogue Encampment comes from; without one a player stands in an empty
    /// town.
    towns: Towns,
    /// Multiplies walking and running speed — for tests.
    speed_scale: f64,
    /// The engine's day periods and clock speed, from `Game.exe`; without them games stay at
    /// the start of the day.
    day: Option<([d2_data::engine::DayPeriod; 6], i32)>,
    /// Tile libraries and maps read from the install, parsed once for every game.
    tile_sources: TileSources,
    /// Where characters are saved; without it nothing is kept past a game.
    storage: Option<crate::storage::StorageHandle>,
    games: Mutex<Games>,
}

#[derive(Debug)]
enum Towns {
    None,
    /// Built per game from its map seed, with the engine's preset object table.
    FromInstall(Box<EngineData>),
    /// The same town for every game, for tests.
    #[cfg(test)]
    Fixed(PresetLevel),
}

#[derive(Debug, Default)]
struct Games {
    next_id: u16,
    by_id: HashMap<u16, Game>,
}

#[derive(Debug)]
struct Game {
    name: String,
    password: String,
    hash: u32,
    difficulty: u8,
    created: Instant,
    /// Characters the realm has cleared to join, not yet connected.
    staged: Vec<Character>,
    /// Characters connected to this game.
    connected: Vec<String>,
    /// `game+0x7C`: the seed the client lays the act out from (`0x03`).
    map_seed: u32,
    /// The Rogue Encampment for [`Game::map_seed`].
    town: Option<PresetLevel>,
    /// The act's walkable levels and their rooms: what a player's near rooms are drawn from.
    world: Option<World>,
    /// Where joining players are placed.
    spawn: (u16, u16),
    /// The town's objects and NPCs, spawned room by room as players come near.
    population: Option<Population>,
    /// Act I's time of day, stepped once per server frame since the game was created.
    clock: Option<ActClock>,
    /// Frames the clock has been stepped.
    clock_frames: u64,
    /// How many times the clock asked for its clients to be told (`0x53`).
    clock_reports: u64,
    /// Where each connected player stands, world subtiles, for the admin panel's map.
    positions: HashMap<String, (i32, i32)>,
    /// The rooms each connected player's client holds.
    views: HashMap<String, Vec<RoomId>>,
    /// The fight: the hostile monsters of the rooms populated so far, and the players.
    battle: Battle,
    /// Packets the fight has for each player, sent on its connection's next frame.
    outgoing: HashMap<String, Vec<Vec<u8>>>,
    /// Guids of the warp tiles sent so far, by level and index into the level's warps.
    warp_guids: HashMap<(i32, usize), u32>,
    /// Gold and items lying on the ground, by item guid.
    ground: HashMap<u32, GroundItem>,
    /// What each connected player carries.
    carried: HashMap<String, Carried>,
    /// The version every item made in the game carries: 101 in an expansion game, 2 in a classic
    /// one (`0x00530930`).
    item_version: u16,
    /// A ladder game: ladder-only uniques drop.
    ladder: bool,
    /// The uniques made in the game so far, which do not drop again.
    made_uniques: HashSet<u16>,
    /// Where made items' seeds come from.
    item_seeds: d2_drlg::rng::Seed,
    /// Item guids handed out when the game has no population to count them.
    spare_item_guid: u32,
    /// Vendors' stocks, by the vendor's guid ([`trade`]).
    stores: HashMap<u32, trade::Store>,
}

/// Gold or an item on the ground.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroundItem {
    room: RoomId,
    x: u16,
    y: u16,
    what: Loot,
}

/// What lies on the ground.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Loot {
    /// A gold pile.
    Gold(u32),
    /// An item, by class.
    Item { class: i32, item: Box<Item> },
}

/// A player's items, and whether they came from its save whole: when its `.d2s` holds items the
/// server cannot read or place (the stash, the cube, sockets) nothing is picked up, used or
/// written back.
#[derive(Debug, Clone, Default)]
struct Carried {
    inventory: Inventory,
    complete: bool,
    /// The identify scroll made ready, waiting for the item it identifies.
    identify_with: Option<u32>,
}

/// Why the realm could not create a game.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    /// A game by that name exists.
    NameTaken,
    /// Every game id is in use.
    Full,
}

/// Why the realm could not stage a join.
#[derive(Debug, PartialEq, Eq)]
pub enum JoinError {
    /// No game by that name.
    NoSuchGame,
    /// Wrong password.
    BadPassword,
    /// Eight players already.
    Full,
}

impl GameServer {
    /// A game server with no games.
    #[must_use]
    pub fn new(tables: EngineTables, rules: Option<GameData>) -> Self {
        Self { tables, rules, towns: Towns::None, speed_scale: 1.0, day: None, tile_sources: TileSources::new(), storage: None, games: Mutex::new(Games::default()) }
    }

    /// Save characters to `storage`.
    #[must_use]
    pub fn with_storage(mut self, storage: crate::storage::StorageHandle) -> Self {
        self.storage = Some(storage);
        self
    }

    /// A player's level in its game's fight.
    fn player_level(&self, game_id: u16, name: &str) -> Option<u32> {
        self.lock().by_id.get(&game_id)?.battle.player_level(name)
    }

    /// The `.d2s` for a player as it stands: its save (or a new character's) with the fight's
    /// stats, its level, the waypoints learned on this difficulty and the items it carries (when
    /// they came from its save whole); and its level.
    fn character_save(&self, p: &Player) -> Option<(Vec<u8>, u32)> {
        let (stats, items, skills) = {
            let g = self.lock();
            let game = g.by_id.get(&p.game_id)?;
            let items = game.carried.get(&p.character.name).filter(|c| c.complete).map(|c| items::save_list(&c.inventory));
            (game.battle.player_stats(&p.character.name)?, items, game.battle.player_skills(&p.character.name))
        };
        let now = u32::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).unwrap_or(0);
        let mut save = p.save.clone().unwrap_or_else(|| {
            let created = u32::try_from(p.character.created_at).unwrap_or(now);
            Save::new(&p.character.name, p.character.class, p.character.status, created, &[])
        });
        for &(id, value) in &stats {
            save.set_stat(u16::from(id), value);
        }
        let level = stats.iter().find(|&&(id, _)| id == stat::LEVEL).map_or(1, |&(_, v)| v);
        save.set_level(u8::try_from(level).unwrap_or(99), now);
        let d = usize::from(p.difficulty.min(2));
        save.waypoints[d][2..2 + d2gs::WAYPOINT_FLAG_BYTES].copy_from_slice(&p.waypoints);
        if let (Some((learned, hands)), Some(rules)) = (skills, &self.rules) {
            for (slot, id) in save.skills.iter_mut().zip(d2_game::skills::class_skills(rules, p.character.class)) {
                *slot = learned.get(&id).copied().unwrap_or(0);
            }
            for (at, hand) in [(0x78, hands[0]), (0x7C, hands[1])] {
                if let Some(bytes) = save.header.get_mut(at..at + 4) {
                    bytes.copy_from_slice(&hand.to_le_bytes());
                }
            }
        }
        if let (Some(items), Some(rules)) = (items, &self.rules) {
            // The player's own list is replaced; the corpse's and any mercenary's that follow it stay.
            if let Ok((_, end)) = item_bits::read_save_list(&save.items, rules.items(), rules.item_stats()) {
                let mut list = item_bits::write_save_list(&items, rules.items(), rules.item_stats());
                list.extend_from_slice(&save.items[end..]);
                save.items = list;
            }
        }
        Some((save.to_bytes(), level))
    }

    /// Write a player's character to storage: its `.d2s`, and its level for the realm's
    /// character list. The stored character is read back first, so only these two fields change,
    /// and its ladder bit wins over the save's: a season that ended while the player was in the
    /// game has already taken it off the ladder.
    async fn save_character(&self, p: &mut Player) {
        let (Some(storage), Some((bytes, level))) = (&self.storage, self.character_save(p)) else { return };
        let Some(mut character) = storage.character_by_name(&p.character.name).await else { return };
        let bytes = match Save::parse(&bytes) {
            Ok(mut save) => {
                save.set_status((save.status() & !status::LADDER) | (character.status & status::LADDER));
                save.to_bytes()
            }
            Err(_) => bytes,
        };
        character.save = Some(bytes.clone());
        character.level = u8::try_from(level).unwrap_or(99);
        match storage.update_character(character).await {
            Ok(_) => {
                info!(character = %p.character.name, level, bytes = bytes.len(), "character saved");
                p.save = Save::parse(&bytes).ok();
                p.saved_level = level;
            }
            Err(e) => warn!(character = %p.character.name, error = %e, "character not saved"),
        }
    }

    /// Build each new game's town from its own map seed with these engine tables (the rules
    /// must hold the install's maps).
    #[must_use]
    pub fn with_engine(mut self, engine: EngineData) -> Self {
        self.day = Some((engine.day_periods, engine.clock_speeds[0]));
        self.towns = Towns::FromInstall(Box::new(engine));
        self
    }

    /// Run games' clocks on these periods — for tests.
    #[cfg(test)]
    #[must_use]
    pub fn with_day(mut self, periods: [d2_data::engine::DayPeriod; 6], ticks_per_degree: i32) -> Self {
        self.day = Some((periods, ticks_per_degree));
        self
    }

    /// Give every new game this town (it must be built from these rules) — for tests.
    #[cfg(test)]
    #[must_use]
    pub fn with_town(mut self, town: PresetLevel) -> Self {
        self.towns = Towns::Fixed(town);
        self
    }

    /// Scale walking and running speed — for tests.
    #[cfg(test)]
    #[must_use]
    pub fn with_speed_scale(mut self, scale: f64) -> Self {
        self.speed_scale = scale;
        self
    }

    /// A new game's map seed, town and walkable world: a random seed, as the engine draws one
    /// per game, and the act it produces; [`FALLBACK_MAP_SEED`] if that town cannot be built.
    fn new_map(&self, difficulty: u8) -> (u32, Option<PresetLevel>, Option<World>) {
        let (Towns::FromInstall(engine), Some(data)) = (&self.towns, &self.rules) else {
            #[cfg(test)]
            if let Towns::Fixed(town) = &self.towns {
                let level = d2_drlg::world::WorldLevel {
                    id: town.level_id,
                    area: town.area,
                    rooms: town.rooms.clone(),
                    units: town.units.clone(),
                    pieces: Vec::new(),
                    collision: Vec::new(),
                    warps: Vec::new(),
                };
                return (FALLBACK_MAP_SEED, Some(town.clone()), Some(World::from_levels(vec![level])));
            }
            return (FALLBACK_MAP_SEED, None, None);
        };
        let build = |seed: u32| {
            let act = Act::build(data.levels(), 0, difficulty, seed);
            PresetLevel::build(data, engine, &act, i32::from(TOWN_AREA)).map(|town| {
                let world = World::build(data, engine, &act, Some(&town), &self.tile_sources);
                for (level, why) in world.unbuilt() {
                    warn!(map_seed = %format!("{seed:#010x}"), level, error = %why, "level not generated; players will not see it");
                }
                (town, world)
            })
        };
        let seed: u32 = rand::thread_rng().gen();
        match build(seed) {
            Ok((town, world)) => (seed, Some(town), Some(world)),
            Err(e) => {
                warn!(map_seed = %format!("{seed:#010x}"), error = %e, "town not built for this seed; using the fallback seed");
                match build(FALLBACK_MAP_SEED) {
                    Ok((town, world)) => (FALLBACK_MAP_SEED, Some(town), Some(world)),
                    Err(_) => (FALLBACK_MAP_SEED, None, None),
                }
            }
        }
    }

    /// Load `Game.exe` and the game rules from `data_dir`, bind the game port on `ip`, and start
    /// serving. `None` (with a warning) if `Game.exe` or the port fails — the realm then keeps
    /// answering "Server Down". Rules that fail to load only cost the player its stats.
    pub async fn start(data_dir: &str, ip: std::net::IpAddr, storage: crate::storage::StorageHandle) -> Option<Arc<Self>> {
        let path = Path::new(data_dir).join("Game.exe");
        let (tables, engine) = match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|file| {
            let engine = EngineData::from_game_exe(&file).map_err(|e| e.to_string())?;
            let tables =
                EngineTables::new(&engine.huffman_code_lengths, engine.client_packet_sizes, engine.server_packet_sizes)
                    .map_err(|e| e.to_string())?;
            Ok((tables, engine))
        }) {
            Ok(t) => t,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "game server test disabled: cannot use Game.exe");
                return None;
            }
        };
        let addr = SocketAddr::new(ip, d2gs::GAME_PORT);
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!(%addr, error = %e, "game server test disabled: cannot bind the game port");
                return None;
            }
        };
        let rules = match GameData::load(data_dir) {
            Ok(r) => Some(r),
            Err(e) => {
                warn!(dir = data_dir, error = %e, "game rules not loaded: players will join with no stats");
                None
            }
        };
        let server = Arc::new(Self::new(tables, rules).with_engine(engine).with_storage(storage));
        warn!(
            %addr,
            "Diablo II TEST game server is on: games can be created and joined; Act I's town and \
             wilderness, their monsters and the fight are simulated; nothing is saved \
             (diablo2.game_server_probe)"
        );
        tokio::spawn(serve(listener, Arc::clone(&server)));
        Some(server)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Games> {
        self.games.lock().expect("games lock")
    }

    /// Create a game; its id is the token the realm hands out.
    ///
    /// # Errors
    ///
    /// [`CreateError`].
    pub fn create(&self, name: &str, password: &str, difficulty: u8) -> Result<u16, CreateError> {
        let difficulty = difficulty.min(2);
        let (map_seed, town, world) = self.new_map(difficulty);
        let spawn = town
            .as_ref()
            .zip(self.rules.as_ref())
            .and_then(|(town, rules)| waypoint_spawn(rules, town))
            .unwrap_or(FALLBACK_SPAWN);
        let mut g = self.lock();
        g.by_id.retain(|_, game| !game.connected.is_empty() || game.created.elapsed() < UNJOINED_GAME_TTL);
        if g.by_id.values().any(|game| game.name.eq_ignore_ascii_case(name)) {
            return Err(CreateError::NameTaken);
        }
        let id = (0..u16::MAX)
            .map(|i| g.next_id.wrapping_add(i).max(1))
            .find(|id| !g.by_id.contains_key(id))
            .ok_or(CreateError::Full)?;
        g.next_id = id.wrapping_add(1);
        g.by_id.insert(
            id,
            Game {
                name: name.to_string(),
                password: password.to_string(),
                hash: rand::thread_rng().gen(),
                difficulty,
                created: Instant::now(),
                staged: Vec::new(),
                connected: Vec::new(),
                map_seed,
                population: world.is_some().then(|| {
                    let seed = rand::thread_rng().gen();
                    let population = Population::new(seed);
                    match &self.rules {
                        Some(rules) => population.with_monsters(rules, seed, difficulty),
                        None => population,
                    }
                }),
                spawn,
                town,
                world,
                clock: self.day.map(|(periods, speed)| ActClock::new(0, periods, speed)),
                clock_frames: 0,
                clock_reports: 0,
                positions: HashMap::new(),
                views: HashMap::new(),
                battle: Battle::new(difficulty, rand::thread_rng().gen()),
                outgoing: HashMap::new(),
                warp_guids: HashMap::new(),
                ground: HashMap::new(),
                carried: HashMap::new(),
                item_version: 101,
                ladder: false,
                made_uniques: HashSet::new(),
                item_seeds: d2_drlg::rng::Seed::new(rand::thread_rng().gen(), 0x29A),
                spare_item_guid: 0,
                stores: HashMap::new(),
            },
        );
        let game = &g.by_id[&id];
        let map = game.town.as_ref().map_or("none", |t| t.map.as_str());
        let levels: Vec<i32> = game.world.as_ref().map(|w| w.levels().iter().map(|l| l.id).collect()).unwrap_or_default();
        info!(game = %name, id, difficulty, map_seed = %format!("{map_seed:#010x}"), %map, ?spawn, ?levels, "test game created");
        // Where each scanned piece (cave mouths, tower and graveyard entrances) sits, in world
        // subtiles at its room's middle — for finding them in the client.
        if let (Some(world), Some(rules)) = (&game.world, &self.rules) {
            let entrances: Vec<(i32, i32, i32, i32)> = world
                .levels()
                .iter()
                .flat_map(|l| {
                    l.pieces.iter().zip(&l.rooms).filter_map(move |(&piece, room)| {
                        let scanned = piece != 0 && rules.lvl_prests().by_def(piece).is_some_and(|r| r.scan);
                        scanned.then_some((l.id, piece, room.x * 5 + room.w * 5 / 2, room.y * 5 + room.h * 5 / 2))
                    })
                })
                .collect();
            info!(game = %name, id, ?entrances, "entrance pieces (level, LvlPrest def, x, y)");
        }
        Ok(id)
    }

    /// Clear `character` to join the game named `name`: `(game id, game hash)` for
    /// `MCP_JOINGAME`.
    ///
    /// # Errors
    ///
    /// [`JoinError`].
    pub fn stage_join(&self, name: &str, password: &str, character: Character) -> Result<(u16, u32), JoinError> {
        let mut g = self.lock();
        let (&id, game) = g
            .by_id
            .iter_mut()
            .find(|(_, game)| game.name.eq_ignore_ascii_case(name))
            .ok_or(JoinError::NoSuchGame)?;
        if game.password != password {
            return Err(JoinError::BadPassword);
        }
        if game.connected.len() >= MAX_PLAYERS {
            return Err(JoinError::Full);
        }
        game.staged.retain(|c| !c.name.eq_ignore_ascii_case(&character.name));
        game.staged.push(character);
        Ok((id, game.hash))
    }

    /// Match a `GAMELOGON` to a staged join, moving the character into the game.
    fn claim(&self, logon: &GameLogon) -> Option<Player> {
        let mut g = self.lock();
        let game = g.by_id.get_mut(&logon.game_id).filter(|game| game.hash == logon.game_hash)?;
        let at = game.staged.iter().position(|c| c.name.eq_ignore_ascii_case(&logon.name))?;
        let character = game.staged.remove(at);
        game.battle.set_expansion(character.status & status::EXPANSION != 0);
        game.item_version = if character.status & status::EXPANSION != 0 { 101 } else { 2 };
        game.ladder = character.status & status::LADDER != 0;
        game.connected.push(character.name.clone());
        Some(Player::new(logon.game_id, character, game.difficulty, game.map_seed, game.spawn))
    }

    /// The room holding `(x, y)` and the rooms near it, if the game has a world and the spot is
    /// in one of its rooms.
    fn near(&self, game_id: u16, x: f64, y: f64) -> Option<(RoomId, Vec<RoomId>)> {
        let g = self.lock();
        let world = g.by_id.get(&game_id)?.world.as_ref()?;
        let room = world.room_at(x as i32, y as i32)?;
        Some((room, world.rooms_near(room)))
    }

    /// The view a client keeps after its player enters `room` with `near` around it: the near
    /// rooms, and those of `old` still within [`KEEP_GAP`] of `room`.
    fn kept_view(&self, game_id: u16, room: RoomId, near: &[RoomId], old: &[RoomId]) -> Vec<RoomId> {
        let g = self.lock();
        let Some(world) = g.by_id.get(&game_id).and_then(|game| game.world.as_ref()) else { return near.to_vec() };
        let mut view = near.to_vec();
        view.extend(old.iter().filter(|r| !near.contains(r) && world.rooms_within(room, **r, KEEP_GAP)));
        view
    }

    /// The waypoint a player travelled to lights up (`0x00547210` spawns it in mode 1 for a pending
    /// arrival): an unlit one spawned there turns to mode 1, its `0x0E`.
    fn light_waypoint_at(&self, game_id: u16, x: u16, y: u16) -> Option<Vec<u8>> {
        let rules = self.rules.as_ref()?;
        let mut g = self.lock();
        let game = g.by_id.get_mut(&game_id)?;
        let world = game.world.as_ref()?;
        let population = game.population.as_mut()?;
        let room = world.room_at(i32::from(x), i32::from(y))?;
        let guid = population.units(room)?.iter().find_map(|u| match *u {
            Spawned::Object { guid, class, x: ox, y: oy, mode: 0, .. }
                if rules.objects().get(i32::from(class)).is_some_and(|o| o.operate_fn == 23)
                    && (i32::from(ox) - i32::from(x)).abs() <= 5
                    && (i32::from(oy) - i32::from(y)).abs() <= 5 =>
            {
                Some(guid)
            }
            _ => None,
        })?;
        population.set_object_mode(guid, 1);
        Some(d2gs::object_state(guid, true, 1))
    }

    /// Where a player taking warp tile `guid` arrives: the level and the spot.
    fn warp_arrival(&self, game_id: u16, guid: u32) -> Option<(i32, u16, u16)> {
        let rules = self.rules.as_ref()?;
        let g = self.lock();
        let game = g.by_id.get(&game_id)?;
        let (&(level, index), _) = game.warp_guids.iter().find(|(_, &g)| g == guid)?;
        let world = game.world.as_ref()?;
        let warp = world.levels().iter().find(|l| l.id == level)?.warps.get(index)?;
        let (to, x, y) = world.warp_arrival(rules, level, warp.slot)?;
        Some((to, u16::try_from(x).ok()?, u16::try_from(y).ok()?))
    }

    /// Where a unit of the game's population stands.
    fn unit_position(&self, game_id: u16, kind: u32, guid: u32) -> Option<(u16, u16)> {
        let g = self.lock();
        if kind == u32::from(unit_type::WARP) {
            let game = g.by_id.get(&game_id)?;
            let (&(level, index), _) = game.warp_guids.iter().find(|(_, &g)| g == guid)?;
            let warp = game.world.as_ref()?.levels().iter().find(|l| l.id == level)?.warps.get(index)?;
            return Some((u16::try_from(warp.x).ok()?, u16::try_from(warp.y).ok()?));
        }
        if kind == u32::from(unit_type::ITEM) {
            let item = g.by_id.get(&game_id)?.ground.get(&guid)?;
            return Some((item.x, item.y));
        }
        let population = g.by_id.get(&game_id)?.population.as_ref()?;
        match *population.find(u8::try_from(kind).ok()?, guid)?.1 {
            Spawned::Object { x, y, .. } | Spawned::Monster { x, y, .. } => Some((x, y)),
        }
    }

    /// What the engine answers `0x13` with (`0x00548B00`), for the units the test knows:
    /// - an NPC `MonStats.txt` lets players talk to: its dialog (`0x00572C10`: `27 29 28`), with
    ///   no quest messages and a new character's clear flags;
    /// - the stash (`OperateFn` 32, class 267): `77 10` (`0x00564CD0`);
    /// - a waypoint (`OperateFn` 23, `0x00584E30`): the player learns its level's waypoint; an
    ///   inactive one turns active (`0E`, mode 1), an active one opens the menu (`63`).
    ///
    /// Range, busy and collision checks are not ported: the client walks up before it asks.
    fn interact(&self, game_id: u16, kind: u32, guid: u32, waypoints: &mut [u8; d2gs::WAYPOINT_FLAG_BYTES]) -> Vec<Vec<u8>> {
        let (Some(rules), Ok(kind)) = (&self.rules, u8::try_from(kind)) else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some(population) = game.population.as_mut() else { return Vec::new() };
        let Some((room, unit)) = population.find(kind, guid) else { return Vec::new() };
        match *unit {
            Spawned::Monster { class, mode, .. } => {
                let talks = rules.monsters().get(i32::from(class)).is_some_and(|m| m.interact);
                if !talks || matches!(mode, 0 | 12) {
                    return Vec::new();
                }
                vec![
                    d2gs::npc_no_quest_messages(unit_type::MONSTER, guid),
                    d2gs::game_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]),
                    d2gs::npc_dialog_quest_flags(guid, &[0; d2gs::QUEST_FLAG_BYTES]),
                ]
            }
            Spawned::Object { class, mode, .. } => match rules.objects().get(i32::from(class)).map(|o| o.operate_fn) {
                Some(32) if class == 267 => vec![d2gs::ui_action(d2gs::UI_OPEN_STASH)],
                Some(23) => {
                    if let Some(bit) = rules.levels().get(room.level).and_then(|l| l.waypoint) {
                        if let Some(byte) = waypoints.get_mut(usize::from(bit / 8)) {
                            *byte |= 1 << (bit % 8);
                        }
                    }
                    match mode {
                        0 => {
                            population.set_object_mode(guid, 1);
                            vec![d2gs::object_state(guid, true, 1)]
                        }
                        1 | 2 => vec![d2gs::waypoint_menu(guid, waypoints)],
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
        }
    }

    /// A player starts talking to an NPC (`0x2F`, handler `0x0054B930` → `0x00572E60` →
    /// `0x00578E70`): the healers — Akara, Atma, Fara, Ormus, Jamella and Malah — restore its
    /// life, mana and stamina, each sent as its stat when it was short (`0x00578D30`). Poison and
    /// cold, which it also cures, and a hireling's heal are not ported; the engine heals once per
    /// conversation, which a heal that only fills what is missing makes no difference to.
    fn npc_talk(&self, game_id: u16, name: &str, kind: u32, guid: u32) -> Vec<Vec<u8>> {
        let Ok(kind) = u8::try_from(kind) else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some((_, unit)) = game.population.as_ref().and_then(|p| p.find(kind, guid)) else { return Vec::new() };
        let Spawned::Monster { class, .. } = *unit else { return Vec::new() };
        if !HEALERS.contains(&class) {
            return Vec::new();
        }
        let healed = game.battle.heal(name);
        if !healed.is_empty() {
            info!(game_id, player = name, npc = class, "healed");
        }
        healed.iter().filter_map(|e| battle_packet(e, name)).collect()
    }

    /// Where waypoint travel from waypoint `guid` to `level` puts the player (`0x0054C5D0` →
    /// `0x00584F60`): the waypoint must be one (`OperateFn` 23), the level another one the player
    /// has learned, in Act I and in the world; the player lands on that level's waypoint tile at
    /// subtile (3, 3) (`0x0061B060`, as at the start). The engine's ten-second guard and the
    /// search for a free spot are not ported.
    fn waypoint_travel(&self, game_id: u16, guid: u32, level: i32, learned: &[u8; d2gs::WAYPOINT_FLAG_BYTES]) -> Option<(u16, u16)> {
        let rules = self.rules.as_ref()?;
        let g = self.lock();
        let game = g.by_id.get(&game_id)?;
        let (room, unit) = game.population.as_ref()?.find(unit_type::OBJECT, guid)?;
        let Spawned::Object { class, .. } = *unit else { return None };
        if rules.objects().get(i32::from(class))?.operate_fn != 23 || room.level == level || level == 0 {
            return None;
        }
        let def = rules.levels().get(level).filter(|d| d.act == 0)?;
        let bit = def.waypoint?;
        if learned.get(usize::from(bit / 8))? & (1 << (bit % 8)) == 0 {
            return None;
        }
        let world = game.world.as_ref()?;
        let target = world.levels().iter().find(|l| l.id == level)?;
        target.units.iter().find_map(|u| match u.class {
            d2_drlg::preset::UnitClass::Object(class)
                if class <= 0x23C && rules.objects().get(class).is_some_and(|o| o.sub_class & SUBCLASS_WAYPOINT != 0) =>
            {
                let at = |v: i32| u16::try_from(v.div_euclid(5) * 5 + 3).ok();
                Some((at(u.x)?, at(u.y)?))
            }
            _ => None,
        })
    }

    /// What a client is sent when its player's near rooms change from `from` to `to`, in the
    /// engine's order (`0x00537B50`): each room that came near as its `0x07` and the packets
    /// `SendUnitToClient` sends for its units (`0x0053A8E0`) — populating rooms nobody has been
    /// near yet — then each room left behind as a `0x0A` per unit and its `0x08`
    /// (`0x0053A9B0`).
    fn view_change(&self, game_id: u16, from: &[RoomId], to: &[RoomId]) -> Vec<Vec<u8>> {
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some(world) = &game.world else { return Vec::new() };
        let mut packets = Vec::new();
        for &id in to.iter().filter(|id| !from.contains(id)) {
            let Some(room) = world.room(id) else { continue };
            packets.push(d2gs::load_room(room.x as u16, room.y as u16, id.level as u8));
            let (Some(rules), Some(population)) = (&self.rules, &mut game.population) else {
                continue;
            };
            let collision = |x: i32, y: i32| world.collision_at(x, y);
            let monsters = world.levels().iter().find(|l| l.id == id.level).and_then(|level| {
                // A piece whose LvlPrest.txt row does not populate is flagged no-spawn
                // (DRLGROOMEX_AllocRoomExTypePreset); its rooms get no monsters.
                let piece = level.pieces.get(id.index).copied().unwrap_or(0);
                let populates = piece == 0 || rules.lvl_prests().by_def(piece).is_some_and(|r| r.populate);
                let def = rules.levels().get(id.level)?;
                let arrival = d2_game::population::arrival(rules, &level.units).map(|spot| (spot, def.warp_dist));
                (!d2_game::population::is_town(id.level) && populates)
                    .then_some(MonsterRoom { area: room, level_rooms: level.rooms.len(), collision: &collision, arrival })
            });
            let activated = population.activate(rules, id.level, id, world.units_in(id), monsters);
            for skipped in &activated.not_ported {
                debug!(game_id, ?room, unit = %skipped, "map unit not spawned: not ported");
            }
            for unit in activated.units {
                match *unit {
                    Spawned::Object { guid, class, x, y, mode, interaction } => {
                        packets.push(d2gs::assign_object(guid, class, x, y, mode, interaction));
                    }
                    Spawned::Monster { guid, class, x, y, .. } => {
                        if !d2_game::population::is_town(id.level) {
                            game.battle.add_monster(rules, guid, i32::from(class), id, x, y);
                        }
                        packets.extend(monster_packets(rules, unit));
                    }
                }
            }
            // Gold and items already lying there.
            for (&guid, item) in game.ground.iter().filter(|(_, p)| p.room == id) {
                packets.push(ground_packet(rules, guid, item, false, game.item_version));
            }
            // The room's warps that lead somewhere built, after its other units.
            let Some(level) = world.levels().iter().find(|l| l.id == id.level) else { continue };
            for (index, warp) in level.warps.iter().enumerate().filter(|(_, w)| w.room == id.index) {
                if world.warp_destination(rules, id.level, warp.slot).is_none() {
                    continue;
                }
                let Some(class) = rules.levels().get(id.level).and_then(|d| u8::try_from(d.warp[usize::from(warp.slot)]).ok()) else { continue };
                let guid = *game.warp_guids.entry((id.level, index)).or_insert_with(|| population.next_guid(unit_type::WARP));
                packets.push(d2gs::assign_warp(guid, class, warp.x as u16, warp.y as u16));
            }
        }
        for &id in from.iter().filter(|id| !to.contains(id)) {
            let Some(room) = world.room(id) else { continue };
            for unit in game.population.as_ref().and_then(|p| p.units(id)).unwrap_or(&[]) {
                let kind = match unit {
                    Spawned::Object { .. } => unit_type::OBJECT,
                    Spawned::Monster { .. } => unit_type::MONSTER,
                };
                packets.push(d2gs::remove_unit(kind, unit.guid()));
            }
            for (&guid, _) in game.ground.iter().filter(|(_, p)| p.room == id) {
                packets.push(d2gs::remove_unit(unit_type::ITEM, guid));
            }
            if let Some(level) = world.levels().iter().find(|l| l.id == id.level) {
                for (index, _) in level.warps.iter().enumerate().filter(|(_, w)| w.room == id.index) {
                    if let Some(&guid) = game.warp_guids.get(&(id.level, index)) {
                        packets.push(d2gs::remove_unit(unit_type::WARP, guid));
                    }
                }
            }
            packets.push(d2gs::unload_room(room.x as u16, room.y as u16, id.level as u8));
        }
        packets
    }

    /// The game's Act I clock brought up to date — stepped once per [`SERVER_FRAME`] since the
    /// game was created, as the engine steps it every frame (`0x0052D7B0`) — as `(reports so far,
    /// 0x53 packet)`. A client that has seen fewer reports is sent the packet.
    fn clock(&self, game_id: u16) -> Option<(u64, Vec<u8>)> {
        let mut g = self.lock();
        let game = g.by_id.get_mut(&game_id)?;
        let due = (game.created.elapsed().as_micros() / SERVER_FRAME.as_micros()) as u64;
        let clock = game.clock.as_mut()?;
        while game.clock_frames < due {
            if clock.step() {
                game.clock_reports += 1;
            }
            game.clock_frames += 1;
        }
        let (period, ticks, eclipse) = clock.state();
        Some((game.clock_reports, d2gs::act_environment(period, ticks, eclipse)))
    }

    /// Record where a player stands.
    fn set_position(&self, game_id: u16, name: &str, x: f64, y: f64) {
        let mut g = self.lock();
        if let Some(game) = g.by_id.get_mut(&game_id) {
            game.positions.insert(name.to_string(), (x as i32, y as i32));
        }
    }

    /// Record the rooms a player's client holds.
    fn set_view(&self, game_id: u16, name: &str, view: &[RoomId]) {
        let mut g = self.lock();
        if let Some(game) = g.by_id.get_mut(&game_id) {
            game.views.insert(name.to_string(), view.to_vec());
        }
    }

    /// A player joins its game's fight with a new character's stats.
    fn join_battle(&self, game_id: u16, name: &str, class: u8, stats: &[(u8, u32)]) {
        let Some(rules) = &self.rules else { return };
        let mut g = self.lock();
        if let Some(game) = g.by_id.get_mut(&game_id) {
            game.battle.add_player(rules, name, class, stats);
        }
    }

    /// Run the game's fight up to now — one step per [`SERVER_FRAME`] since the game was
    /// created, whichever connection gets here first — sharing out what it produces, and hand
    /// back what `name`'s client is to be sent.
    fn battle_step(&self, game_id: u16, name: &str, motion: battle::Motion) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        game.battle.set_motion(name, motion);
        let due = (game.created.elapsed().as_micros() / SERVER_FRAME.as_micros()) as u64;
        if due > game.battle.frame() {
            let Game { battle, world, positions, views, population, outgoing, ground, item_version, ladder, made_uniques, item_seeds, difficulty, .. } = game;
            for (player, &(x, y)) in positions.iter() {
                let level = world.as_ref().and_then(|w| w.room_at(x, y)).map(|r| r.level);
                battle.place_player(player, level.map(|l| (x, y, l)), views.get(player).map_or(&[], Vec::as_slice));
            }
            let Some(world) = world.as_ref() else { return Vec::new() };
            let events = {
                let populated = population.as_ref();
                let open = |x: i32, y: i32| {
                    let room = world.room_at(x, y).filter(|r| populated.is_some_and(|p| p.units(*r).is_some()))?;
                    world.collision_at(x, y).filter(|c| c & d2_game::path::WALL == 0).map(|_| room.level)
                };
                battle.advance(rules, due, &open)
            };
            for event in events {
                let (x, y, what) = match &event {
                    Event::GoldDrop { x, y, amount, .. } => (*x, *y, Loot::Gold(*amount)),
                    Event::ItemDrop { x, y, code, mods, level, .. } => {
                        let making = loot::Making { version: *item_version, difficulty: *difficulty, ladder: *ladder, magic_find: 0 };
                        match make_item(rules, code, *level, *mods, making, made_uniques, item_seeds.roll()) {
                            Some((class, item)) => {
                                debug!(game_id, item = %code, quality = item.quality.number(), "item dropped");
                                (*x, *y, Loot::Item { class, item: Box::new(item) })
                            }
                            None => {
                                info!(game_id, %code, "treasure dropped an item not made (gold piles by code, stacks of simple items)");
                                continue;
                            }
                        }
                    }
                    _ => {
                        share_out(event, rules, world, battle, views, outgoing, population.as_mut());
                        continue;
                    }
                };
                // On the ground, seen by whoever holds its room.
                let Some(population) = population.as_mut() else { continue };
                let taken = |x: i32, y: i32| ground.values().any(|p| (i32::from(p.x), i32::from(p.y)) == (x, y));
                let Some((room, x, y)) = drop_spot(world, &taken, (i32::from(x), i32::from(y))) else { continue };
                let item = GroundItem { room, x: x as u16, y: y as u16, what };
                let guid = population.next_guid(unit_type::ITEM);
                let packet = ground_packet(rules, guid, &item, true, *item_version);
                ground.insert(guid, item);
                for (player, view) in views.iter() {
                    if view.contains(&room) {
                        outgoing.entry(player.clone()).or_default().push(packet.clone());
                    }
                }
            }
        }
        game.outgoing.remove(name).unwrap_or_default()
    }

    /// Where a monster in the game's fight stands, while it lives.
    fn battle_monster(&self, game_id: u16, guid: u32) -> Option<(u16, u16)> {
        let g = self.lock();
        let (x, y, mode, _) = g.by_id.get(&game_id)?.battle.monster(guid)?;
        (mode != battle::DEAD_MODE).then_some((x, y))
    }

    /// A player picks up what lies on the ground (engine `0x16` → `0x548B00` type 4 → `0x563560`),
    /// or lifts it onto its cursor when `to_cursor` (`0x0055CF50`).
    ///
    /// Gold (`0x55C850`): it takes what its purse holds (level × 10,000, `0x622E70`), the pile goes
    /// for everyone near, and its client is told its gold as the engine tells it (`0x19` for a small
    /// gain). What does not fit is dropped as a new pile from the player (`0x55B030`, placed as
    /// [`drop_spot`] places drops).
    ///
    /// An item is worn when [`items::auto_equip`] finds it a free body location, else goes where
    /// [`Inventory::place_for`] puts it: its unit goes for everyone near (`0x0A`) and its client is
    /// sent it worn (`0x9D` 6), in the belt (`0x9C` `0x0E`) or in the inventory (`0x9C` 4). With
    /// no room it stays where it is.
    ///
    /// ⚠️ Range is not checked (the engine picks up within 5 subtiles and walks the player
    /// closer otherwise; the client walks up before it asks).
    fn pick_up(&self, game_id: u16, name: &str, guid: u32, to_cursor: bool) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some(item) = game.ground.remove(&guid) else { return Vec::new() };
        let taken = match &item.what {
            Loot::Gold(amount) => take_gold(rules, game, name, guid, *amount),
            Loot::Item { class, item: body } => take_item(rules, game, name, guid, *class, body, to_cursor),
        };
        let Some((replies, dropped)) = taken else {
            game.ground.insert(guid, item);
            return Vec::new();
        };
        refresh_gear(rules, game, name);
        // Everyone else near: the unit goes, then what was dropped again.
        let gone = d2gs::remove_unit(unit_type::ITEM, guid);
        let shown = [(item.room, gone.clone())].into_iter().chain(dropped);
        for (room, packet) in shown {
            for (player, view) in &game.views {
                if player != name && view.contains(&room) {
                    game.outgoing.entry(player.clone()).or_default().push(packet.clone());
                }
            }
        }
        let mut out = vec![gone];
        out.extend(replies);
        out
    }

    /// A player uses an item it holds — in the belt (`0x26`, `0x00562390`) or the inventory
    /// (`0x20`, `0x0055E170`). A healing, mana or rejuvenation potion is drunk
    /// ([`battle::Battle::drink`]) and goes: `0x9C` action `0x0F` from the belt, `0x9D` action 5
    /// from the inventory, both flagged used, then what the drink changed at once. Anything else,
    /// and anything used from the wrong place or by a dead player, does nothing.
    ///
    /// Not ported: other items' uses (scrolls, stamina, antidote and thawing potions), and belt
    /// columns sliding down, which one row of slots never needs.
    fn use_item(&self, game_id: u16, name: &str, guid: u32, from_belt: bool) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some(carried) = game.carried.get_mut(name).filter(|c| c.complete) else { return Vec::new() };
        let Some(held) = carried.inventory.get(guid).cloned() else { return Vec::new() };
        let in_place = match held.place {
            Place::Belt(_) => from_belt,
            Place::Grid { .. } => !from_belt,
            _ => false,
        };
        if !in_place {
            return Vec::new();
        }
        if &held.code() == b"isc " {
            // `pSpell` 1 (`0x005BE130`): the scroll waits for its target; the client turns its cursor.
            carried.identify_with = Some(guid);
            let skill = (0..rules.skills().len() as i32).find(|&id| rules.skills().get(id).is_some_and(|s| s.name == "Book of Identify")).unwrap_or(218);
            return vec![d2gs::item_spell_ready(0, guid, skill as u16)];
        }
        let Some(potion) = rules.items().get(held.class).filter(|d| d.useable).and_then(battle::Potion::of) else {
            debug!(game_id, player = name, item = %d2_data::items::code_str(&held.code()), "item use not ported");
            return Vec::new();
        };
        let Some(changed) = game.battle.drink(name, potion) else { return Vec::new() };
        carried.inventory.remove(guid);
        info!(game_id, player = name, guid, item = %d2_data::items::code_str(&held.code()), "potion drunk");
        let mut out: Vec<Vec<u8>> = items::held_packet(rules, &held, true).into_iter().collect();
        out.extend(changed.iter().filter_map(|e| battle_packet(e, name)));
        out
    }

    /// A player identifies an item it holds with the scroll it made ready (`0x27` → `0x00561ED0`):
    /// the scroll is used up (`0x9D` 5 from the inventory, `0x9C` `0x0F` from the belt, flagged
    /// used), then the item comes back whole (`0x9D` `0x15`, `0x0055E0D0`/`0x00562590`). Nothing
    /// for an item already identified or a scroll not made ready.
    ///
    /// Not ported: tomes, and the book skill's charge count (`0x22`).
    fn identify_item(&self, game_id: u16, name: &str, item: u32, scroll: u32) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let Some(carried) = game.carried.get_mut(name).filter(|c| c.complete) else { return Vec::new() };
        if carried.identify_with != Some(scroll) {
            return Vec::new();
        }
        let inv = &mut carried.inventory;
        let (Some(used), Some(target)) = (inv.get(scroll).cloned(), inv.get(item).cloned()) else { return Vec::new() };
        if target.item.identified() || !matches!(target.place, Place::Grid { .. } | Place::Body(_)) || !matches!(used.place, Place::Grid { .. } | Place::Belt(_)) {
            return Vec::new();
        }
        carried.identify_with = None;
        inv.remove(scroll);
        let Some(mut known) = inv.remove(item) else { return Vec::new() };
        known.item.flags |= item_bits::flags::IDENTIFIED;
        let mut shown = known.placed();
        shown.flags |= items::REBUILD;
        inv.insert(known);
        info!(game_id, player = name, item, "item identified");
        let mut out: Vec<Vec<u8>> = items::held_packet(rules, &used, true).into_iter().collect();
        out.push(items::owned(rules, item_action::UPDATE, item, &shown));
        refresh_gear(rules, game, name);
        out
    }

    /// A player moves an item it holds, as its client asks (§ [`items`]): the packets its client
    /// is sent, empty when the move is refused and nothing changes. The server keeps its own copy
    /// of where everything is and checks each move against it.
    fn move_item(&self, game_id: u16, name: &str, request: ItemMove) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let wearer = game.battle.player_requirements(name);
        let Some(carried) = game.carried.get_mut(name).filter(|c| c.complete) else { return Vec::new() };
        let inv = &mut carried.inventory;
        let cursor_is = |inv: &Inventory, guid: u32| inv.at(Place::Cursor).is_some_and(|h| h.guid == guid);
        let out = match request {
            ItemMove::Lift { guid } => {
                let Some(held) = inv.get(guid).filter(|h| matches!(h.place, Place::Grid { .. })).cloned() else { return Vec::new() };
                if !inv.move_to(guid, Place::Cursor) {
                    return Vec::new();
                }
                vec![items::owned(rules, item_action::REMOVE_FROM_CONTAINER, guid, &items::lifted(&held, held.place))]
            }
            ItemMove::Insert { guid, col, row, grid } => {
                if grid != 0 || !cursor_is(inv, guid) || col > 0xFF || row > 0xFF || !inv.move_to(guid, Place::Grid { col: col as u8, row: row as u8 }) {
                    return Vec::new();
                }
                inv.get(guid).and_then(|h| items::held_packet(rules, h, false)).into_iter().collect()
            }
            ItemMove::Equip { guid, body } => {
                let Some(held) = inv.at(Place::Cursor).filter(|h| h.guid == guid).cloned() else { return Vec::new() };
                if !items::wearable_at(rules, inv, held.class, body, wearer.map_or(0, |w| w.0)) || !inv.move_to(guid, Place::Body(body)) {
                    return Vec::new();
                }
                let mut item = held.item.clone();
                item.location = Location::Equipped { body };
                item.flags |= items::REBUILD;
                vec![items::owned(rules, item_action::EQUIP, guid, &item)]
            }
            ItemMove::Unequip { body } => {
                let Some(held) = inv.at(Place::Body(body)).cloned() else { return Vec::new() };
                if inv.at(Place::Cursor).is_some() || !inv.move_to(held.guid, Place::Cursor) {
                    return Vec::new();
                }
                vec![items::owned(rules, item_action::UNEQUIP, held.guid, &items::lifted(&held, held.place))]
            }
            ItemMove::SwapEquipped { guid, body } => {
                let (Some(new), Some(old)) = (inv.at(Place::Cursor).filter(|h| h.guid == guid).cloned(), inv.at(Place::Body(body)).cloned()) else {
                    return Vec::new();
                };
                let mut probe = inv.clone();
                probe.remove(old.guid);
                if !items::wearable_at(rules, &probe, new.class, body, wearer.map_or(0, |w| w.0)) {
                    return Vec::new();
                }
                inv.remove(old.guid);
                inv.move_to(guid, Place::Body(body));
                let mut lifted = old.clone();
                lifted.place = Place::Cursor;
                inv.insert(lifted);
                let mut out_item = items::lifted(&old, old.place);
                out_item.flags |= items::SWAPPED_OUT | items::REBUILD;
                let mut in_item = new.item.clone();
                in_item.location = Location::Equipped { body };
                in_item.flags |= items::SWAPPED_IN | items::REBUILD;
                vec![items::owned(rules, item_action::SWAP_BODY, old.guid, &out_item), items::owned(rules, item_action::SWAP_BODY, guid, &in_item)]
            }
            ItemMove::SwapGrid { guid, other, col, row } => {
                let (Some(new), Some(old)) = (inv.at(Place::Cursor).filter(|h| h.guid == guid).cloned(), inv.get(other).filter(|h| matches!(h.place, Place::Grid { .. })).cloned()) else {
                    return Vec::new();
                };
                let (Ok(col), Ok(row)) = (u8::try_from(col), u8::try_from(row)) else { return Vec::new() };
                let mut probe = inv.clone();
                probe.remove(old.guid);
                probe.remove(guid);
                let mut placed = new.clone();
                placed.place = Place::Grid { col, row };
                if !probe.fits(&placed) {
                    return Vec::new();
                }
                probe.insert(placed.clone());
                let mut lifted = old.clone();
                lifted.place = Place::Cursor;
                probe.insert(lifted);
                *inv = probe;
                vec![items::world(rules, item_action::SWAP_IN_CONTAINER, old.guid, &items::lifted(&old, old.place)), items::world(rules, item_action::SWAP_IN_CONTAINER, guid, &placed.placed())]
            }
            ItemMove::Belt { guid, slot } => {
                let Some(held) = inv.at(Place::Cursor).filter(|h| h.guid == guid).cloned() else { return Vec::new() };
                if held.size != (1, 1) || !rules.items().beltable(held.class) || slot > 0xFF || !inv.move_to(guid, Place::Belt(slot as u8)) {
                    return Vec::new();
                }
                inv.get(guid).and_then(|h| items::held_packet(rules, h, false)).into_iter().collect()
            }
            ItemMove::Unbelt { guid } => {
                let Some(held) = inv.get(guid).filter(|h| matches!(h.place, Place::Belt(_))).cloned() else { return Vec::new() };
                if !inv.move_to(guid, Place::Cursor) {
                    return Vec::new();
                }
                vec![items::world(rules, item_action::REMOVE_FROM_BELT, guid, &items::lifted(&held, held.place))]
            }
            ItemMove::SwapBelt { guid, other } => {
                let (Some(new), Some(old)) = (inv.at(Place::Cursor).filter(|h| h.guid == guid).cloned(), inv.get(other).filter(|h| matches!(h.place, Place::Belt(_))).cloned()) else {
                    return Vec::new();
                };
                if new.size != (1, 1) || !rules.items().beltable(new.class) {
                    return Vec::new();
                }
                inv.remove(old.guid);
                inv.move_to(guid, old.place);
                let mut lifted = old.clone();
                lifted.place = Place::Cursor;
                inv.insert(lifted);
                let mut placed = new.clone();
                placed.place = old.place;
                vec![items::world(rules, item_action::SWAP_IN_BELT, old.guid, &items::lifted(&old, old.place)), items::world(rules, item_action::SWAP_IN_BELT, guid, &placed.placed())]
            }
            ItemMove::Drop { guid } => return drop_cursor_item(rules, game, name, guid),
        };
        info!(game_id, player = name, ?request, "item moved");
        refresh_gear(rules, game, name);
        out
    }

    /// A joining player's items from its save, held in the game, and the packets that give them
    /// to its client: `0x9C` action 4 for the inventory, action `0x0E` for the belt, `0x9D` 6 worn.
    /// A save holding anything else (the stash, the cube, socketed items) is left as it is and the
    /// player carries nothing. A character never saved gets its class's starting items
    /// ([`starting_items`]).
    fn join_items(&self, p: &Player) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&p.game_id) else { return Vec::new() };
        let saved = match &p.save {
            None => Some(starting_items(rules, game, p.character.class)),
            Some(save) => match item_bits::read_save_list(&save.items, rules.items(), rules.item_stats()) {
                Ok((list, _)) => Some(list),
                Err(why) => {
                    warn!(character = %p.character.name, ?why, "its saved items do not read");
                    None
                }
            },
        };
        let mut carried = Carried { inventory: Inventory::default(), complete: saved.is_some(), identify_with: None };
        for item in saved.unwrap_or_default() {
            let (Some(place), Some(class)) = (items::saved_place(&item), rules.items().class_of(&item.code)) else {
                carried.complete = false;
                break;
            };
            if item.socketed > 0 {
                carried.complete = false;
                break;
            }
            let size = rules.items().get(class).map_or((1, 1), |d| d.inv_size);
            let guid = match game.population.as_mut() {
                Some(population) => population.next_guid(unit_type::ITEM),
                None => {
                    game.spare_item_guid += 1;
                    game.spare_item_guid
                }
            };
            let mut held = Held { guid, class, size, place, item };
            if p.save.is_none() && !carried.inventory.fits(&held) {
                // A starting item finds its spot as a pickup does.
                let belts = rules.items().get(class).is_some_and(|d| d.auto_belt) && rules.items().beltable(class);
                let Some(spot) = carried.inventory.place_for(size, belts) else { continue };
                held.place = spot;
            }
            if !carried.inventory.insert(held) {
                carried.complete = false;
                break;
            }
        }
        if !carried.complete {
            warn!(character = %p.character.name, "its saved items are not all ones the server places; it carries nothing this game");
            carried.inventory = Inventory::default();
        }
        let packets = carried.inventory.items().iter().filter_map(|held| items::held_packet(rules, held, false)).collect();
        game.carried.insert(p.character.name.clone(), carried);
        refresh_gear(rules, game, &p.character.name);
        packets
    }

    /// A joining player's skills from its save — its class's 30 levels (`if`) and the skills on its
    /// mouse buttons (header `0x78` left, `0x7C` right) — given to the fight, and the packets that
    /// tell its client: `0x94` with every skill it has, then `0x23` for the left and right buttons.
    fn join_skills(&self, p: &Player) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut learned = BTreeMap::new();
        let mut hands = [0, 0];
        if let Some(save) = &p.save {
            for (&id, &level) in d2_game::skills::class_skills(rules, p.character.class).iter().zip(&save.skills) {
                learned.insert(id, level);
            }
            let hand = |at: usize| save.header.get(at..at + 4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap_or([0; 4])));
            hands = [hand(0x78), hand(0x7C)];
        }
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&p.game_id) else { return Vec::new() };
        game.battle.set_player_skills(&p.character.name, &learned, hands);
        let Some((skills, hands)) = game.battle.player_skills(&p.character.name) else { return Vec::new() };
        let common = d2_game::skills::COMMON_SKILLS;
        let listed: Vec<(u16, u8)> = common
            .iter()
            .filter_map(|id| skills.get(id).map(|&l| (*id, l)))
            .chain(skills.iter().filter(|(id, &l)| l > 0 && !common.contains(id)).map(|(&id, &l)| (id, l)))
            .filter_map(|(id, l)| Some((u16::try_from(id).ok()?, l)))
            .collect();
        vec![
            d2gs::skill_list(PLAYER_GUID, &listed),
            d2gs::select_skill(unit_type::PLAYER, PLAYER_GUID, true, u16::try_from(hands[0]).unwrap_or(0), u32::MAX),
            d2gs::select_skill(unit_type::PLAYER, PLAYER_GUID, false, u16::try_from(hands[1]).unwrap_or(0), u32::MAX),
        ]
    }

    /// A player puts a point into a skill (`0x3B`): its new level (`0x21`) and unspent points.
    fn learn_skill(&self, game_id: u16, name: &str, skill: u16) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        let events = game.battle.learn_skill(rules, name, i32::from(skill));
        if !events.is_empty() {
            info!(game_id, player = name, skill, "skill point spent");
        }
        events.iter().filter_map(|e| battle_packet(e, name)).collect()
    }

    /// A player puts a skill on a mouse button (`0x3C`): `0x23` back when it has the skill.
    fn select_skill(&self, game_id: u16, name: &str, skill: u16, left: bool, item: u32) -> Vec<Vec<u8>> {
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        if !game.battle.select_skill(name, i32::from(skill), left) {
            return Vec::new();
        }
        vec![d2gs::select_skill(unit_type::PLAYER, PLAYER_GUID, left, skill, item)]
    }

    /// A player uses a mouse button's skill ([`Battle::player_skill`]).
    fn player_skill(&self, game_id: u16, name: &str, left: bool, aim: battle::Aim) -> battle::SkillUse {
        let Some(rules) = &self.rules else { return battle::SkillUse::Refused };
        let mut g = self.lock();
        g.by_id.get_mut(&game_id).map_or(battle::SkillUse::Refused, |game| game.battle.player_skill(rules, name, left, aim))
    }

    /// A player swings at a monster.
    fn player_attack(&self, game_id: u16, name: &str, guid: u32) -> bool {
        let mut g = self.lock();
        g.by_id.get_mut(&game_id).is_some_and(|game| game.battle.player_attack(name, guid))
    }

    /// Whether a dead player's death throes are over on its client.
    fn player_death_settled(&self, game_id: u16, name: &str) -> bool {
        self.lock().by_id.get(&game_id).is_some_and(|game| game.battle.player_death_settled(name))
    }

    /// Whether a player lies dead.
    fn player_dead(&self, game_id: u16, name: &str) -> bool {
        self.lock().by_id.get(&game_id).is_some_and(|game| game.battle.player_dead(name))
    }

    /// A player spends an attribute point (`0x3A`): the stats that change.
    fn spend_stat_point(&self, game_id: u16, name: &str, stat: u8) -> Vec<Vec<u8>> {
        let Some(rules) = &self.rules else { return Vec::new() };
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        game.battle.spend_stat_point(rules, name, stat).iter().filter_map(|e| battle_packet(e, name)).collect()
    }

    /// A dead player restarts at full life, as the release handler `0x0054C0E0` does it: life,
    /// mana and stamina set to their maximums and each sent as its stat (`0x00548520` →
    /// `0x0053BE40`, `0x1D`–`0x1F`), life first — a life stat for a player lying in mode 0x11 is
    /// what stands it up on the client (`0x0045D780` → `0x0045D4B0`).
    fn revive(&self, game_id: u16, name: &str) -> Vec<Vec<u8>> {
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return Vec::new() };
        game.battle.revive(name);
        let Some(stats) = game.battle.player_stats(name) else { return Vec::new() };
        [stat::HITPOINTS, stat::MANA, stat::STAMINA]
            .iter()
            .filter_map(|&id| stats.iter().find(|&&(s, _)| s == id).map(|&(s, v)| d2gs::set_stat(s, v)))
            .collect()
    }

    /// A connected character left; an emptied game goes with it.
    fn leave(&self, game_id: u16, name: &str) {
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return };
        game.connected.retain(|n| !n.eq_ignore_ascii_case(name));
        game.positions.remove(name);
        game.views.remove(name);
        game.outgoing.remove(name);
        game.carried.remove(name);
        for store in game.stores.values_mut() {
            store.open.remove(name);
        }
        game.battle.remove_player(name);
        if game.connected.is_empty() && game.staged.is_empty() {
            let game = g.by_id.remove(&game_id).expect("present");
            info!(game = %game.name, id = game_id, "test game closed: last player left");
        }
    }
}

/// The packet `recipient` is sent for a fight event, if any: the player's own events use its
/// unit, [`PLAYER_GUID`]; a monster's swing is shown only to the player swung at, whose unit the
/// packet can name.
fn battle_packet(event: &Event, recipient: &str) -> Option<Vec<u8>> {
    Some(match event {
        Event::MonsterLife { guid, life, .. } => d2gs::unit_life(unit_type::MONSTER, *guid, *life),
        Event::MonsterReaction { guid, event, x, y, life, alive, .. } => d2gs::monster_reaction(*guid, *event, *x, *y, *life, *alive),
        Event::MonsterWalk { guid, x, y, .. } => d2gs::monster_walk(*guid, *x, *y, battle::WALK_VELOCITY_PERCENT),
        Event::MonsterStop { guid, x, y, life, .. } => d2gs::monster_standing(*guid, *x, *y, *life),
        Event::MonsterAttack { guid, target, x, y, .. } if target == recipient => d2gs::monster_attack(*guid, PLAYER_GUID, *x, *y),
        Event::MonsterAttack { .. } | Event::MonsterState { .. } | Event::GoldDrop { .. } | Event::ItemDrop { .. } => return None,
        Event::PlayerReaction { event, .. } => d2gs::player_reaction(unit_type::PLAYER, PLAYER_GUID, *event, 0, 0),
        Event::PlayerVitals { life, mana, stamina, .. } => d2gs::life_and_position(*life, *mana, *stamina, 0, 0, 0, 0),
        Event::Experience { old, new, .. } => d2gs::experience(*old, *new),
        Event::PlayerStat { stat, value, .. } => d2gs::set_stat(*stat, *value),
        Event::SkillLevel { skill, level, .. } => d2gs::skill_level(unit_type::PLAYER, PLAYER_GUID, *skill, *level, 0),
    })
}

/// A treasure class's item code as a simple item the server makes as it is: a simple
/// (`compactsave`) misc item that is not gold, a quest item or a stack — potions, scrolls, gems,
/// runes.
fn simple_item_class(rules: &GameData, code: &str) -> Option<i32> {
    if code.len() > 4 {
        return None;
    }
    let class = rules.items().class_of(&d2_data::items::code(code))?;
    let def = rules.items().get(class)?;
    let simple = def.compact && !def.quest && !def.stackable && def.file == d2_data::items::ItemFile::Misc && &def.code != b"gld ";
    simple.then_some(class)
}

/// The item a treasure class's code makes, by class: a simple item as it is, anything else
/// through [`loot::make`] (weapons, armour, rings and amulets, charms, stacks); `None` for gold
/// named by code and codes the tables lack.
fn make_item(rules: &GameData, code: &str, level: i32, mods: d2_data::treasure::QualityMods, making: loot::Making, made_uniques: &mut HashSet<u16>, seed: u32) -> Option<(i32, Item)> {
    if code.len() > 4 {
        return None;
    }
    let packed = d2_data::items::code(code);
    let class = rules.items().class_of(&packed)?;
    if simple_item_class(rules, code).is_some() {
        return Some((class, Item::new(packed, making.version, level.clamp(1, 99) as u8, Location::Ground { x: 0, y: 0 })));
    }
    if &packed == b"gld " {
        return None;
    }
    loot::make(rules, packed, level, mods, making, made_uniques, seed).map(|item| (class, item))
}

/// `0x9C` action 0 for something on the ground, `dropping` for the fall; gold as the game's
/// `version`.
fn ground_packet(rules: &GameData, guid: u32, item: &GroundItem, dropping: bool, version: u16) -> Vec<u8> {
    match &item.what {
        Loot::Gold(amount) => d2gs::ground_gold(guid, item.x, item.y, *amount, dropping, version),
        Loot::Item { item: body, .. } => {
            let mut shown = (**body).clone();
            shown.location = Location::Ground { x: item.x, y: item.y };
            if dropping {
                shown.flags |= item_bits::flags::DROPPED;
            }
            items::world(rules, item_action::ADD_TO_GROUND, guid, &shown)
        }
    }
}

/// A new character's starting items (`0x00534F10`), each at where it goes first: a beltable item
/// in the belt, an item with a body location worn there, anything else in the inventory. Places
/// already taken are found again as a pickup finds them.
fn starting_items(rules: &GameData, game: &mut Game, class: u8) -> Vec<Item> {
    let skill = rules.start_skill(class);
    let mut out = Vec::new();
    let mut belt = 0u8;
    for (n, start) in rules.start_items(class).iter().enumerate() {
        for _ in 0..start.count {
            let Some(mut item) = loot::starter(rules, start.code, game.item_version, skill.filter(|_| n == 0), game.item_seeds.roll()) else { continue };
            let Some(item_class) = rules.items().class_of(&item.code) else { continue };
            item.location = if rules.items().beltable(item_class) {
                belt += 1;
                Location::Belt { slot: belt - 1 }
            } else {
                match start.location.as_deref().and_then(items::body_location) {
                    Some(body) => Location::Equipped { body },
                    None => Location::Stored { col: 15, row: 15, page: 0 },
                }
            };
            out.push(item);
        }
    }
    out
}

/// Tell the fight what a player's worn items add ([`d2_game::gear`]).
fn refresh_gear(rules: &GameData, game: &mut Game, name: &str) {
    let Game { carried, battle, .. } = game;
    let Some(carried) = carried.get(name) else { return };
    let worn: Vec<(u8, &Item)> = carried
        .inventory
        .items()
        .iter()
        .filter_map(|h| match h.place {
            Place::Body(body) => Some((body, &h.item)),
            _ => None,
        })
        .collect();
    battle.set_player_gear(rules, name, d2_game::gear::gear(rules, &worn));
}

/// An item move a client asks for (`0x17`–`0x1F`, `0x23`–`0x25`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemMove {
    /// `0x19`: out of the inventory onto the cursor.
    Lift { guid: u32 },
    /// `0x18`: the cursor item into a grid at a cell.
    Insert { guid: u32, col: u32, row: u32, grid: u32 },
    /// `0x1A`: the cursor item worn.
    Equip { guid: u32, body: u8 },
    /// `0x1C`: a worn item onto the cursor.
    Unequip { body: u8 },
    /// `0x1D`: the cursor item worn in place of the one there, which goes to the cursor.
    SwapEquipped { guid: u32, body: u8 },
    /// `0x1F`: the cursor item into the inventory in place of `other`, which goes to the cursor.
    SwapGrid { guid: u32, other: u32, col: u32, row: u32 },
    /// `0x23`: the cursor item into a belt slot.
    Belt { guid: u32, slot: u32 },
    /// `0x24`: a belt item onto the cursor.
    Unbelt { guid: u32 },
    /// `0x25`: the cursor item into the belt in place of `other`, which goes to the cursor.
    SwapBelt { guid: u32, other: u32 },
    /// `0x17`: the cursor item dropped.
    Drop { guid: u32 },
}

/// The cursor item dropped at the player's feet (`0x00563C00`: the spot as [`drop_spot`] finds
/// one from the player, then `0x9C` action 2 — the client plays the fall — for every player
/// holding the room). Empty when nothing is on the cursor or there is no spot.
fn drop_cursor_item(rules: &GameData, game: &mut Game, name: &str, guid: u32) -> Vec<Vec<u8>> {
    let Some(from) = game.positions.get(name).copied() else { return Vec::new() };
    let Some(world) = game.world.as_ref() else { return Vec::new() };
    let ground = &game.ground;
    let taken = |x: i32, y: i32| ground.values().any(|p| (i32::from(p.x), i32::from(p.y)) == (x, y));
    let Some((room, x, y)) = drop_spot(world, &taken, from) else { return Vec::new() };
    let Some(carried) = game.carried.get_mut(name).filter(|c| c.complete) else { return Vec::new() };
    if !carried.inventory.at(Place::Cursor).is_some_and(|h| h.guid == guid) {
        return Vec::new();
    }
    let Some(held) = carried.inventory.remove(guid) else { return Vec::new() };
    let mut item = held.item;
    item.location = Location::Ground { x: x as u16, y: y as u16 };
    let packet = items::world(rules, item_action::DROP_TO_GROUND, guid, &item);
    info!(player = name, guid, item = %d2_data::items::code_str(&item.code), x, y, "item dropped");
    game.ground.insert(guid, GroundItem { room, x: x as u16, y: y as u16, what: Loot::Item { class: held.class, item: Box::new(item) } });
    for (player, view) in &game.views {
        if player != name && view.contains(&room) {
            game.outgoing.entry(player.clone()).or_default().push(packet.clone());
        }
    }
    vec![packet]
}

/// What a pickup sends its player, and what it put back on the ground for everyone else near.
type Taken = (Vec<Vec<u8>>, Option<(RoomId, Vec<u8>)>);

/// A gold pile of `amount` (already off the ground) into a player's purse: what its client is told,
/// and a new pile, falling, for what did not fit. `None` when the purse takes nothing.
fn take_gold(rules: &GameData, game: &mut Game, name: &str, guid: u32, amount: u32) -> Option<Taken> {
    let (taken, total) = game.battle.pick_up_gold(name, amount).filter(|&(taken, _)| taken > 0)?;
    info!(player = name, guid, taken, total, "gold picked up");
    let mut replies = vec![d2gs::gold_update(total - taken, total)];
    let dropped = drop_gold_at(rules, game, name, amount - taken);
    if let Some((_, packet)) = &dropped {
        replies.push(packet.clone());
    }
    Some((replies, dropped))
}

/// A falling gold pile of `amount` at the nearest free spot to a player: where it went and its
/// packet. `None` for no gold or no spot.
fn drop_gold_at(rules: &GameData, game: &mut Game, name: &str, amount: u32) -> Option<(RoomId, Vec<u8>)> {
    if amount == 0 {
        return None;
    }
    let ground = &game.ground;
    let occupied = |x: i32, y: i32| ground.values().any(|p| (i32::from(p.x), i32::from(p.y)) == (x, y));
    let from = game.positions.get(name).copied()?;
    let (room, x, y) = drop_spot(game.world.as_ref()?, &occupied, from)?;
    let population = game.population.as_mut()?;
    let left = GroundItem { room, x: x as u16, y: y as u16, what: Loot::Gold(amount) };
    let new_guid = population.next_guid(unit_type::ITEM);
    let packet = ground_packet(rules, new_guid, &left, true, game.item_version);
    game.ground.insert(new_guid, left);
    Some((room, packet))
}

/// An item (already off the ground) onto a player's cursor (`to_cursor`, `0x9C` action 1), or
/// worn, into its belt or into its inventory: the packet that gives it to its client. `None` when
/// there is no room or its items are not in hand.
fn take_item(rules: &GameData, game: &mut Game, name: &str, guid: u32, class: i32, item: &Item, to_cursor: bool) -> Option<Taken> {
    let def = rules.items().get(class)?;
    let wearer = game.battle.player_requirements(name).map(|(class, level, strength, dexterity)| items::Wearer { class, level, strength, dexterity });
    let carried = game.carried.get_mut(name).filter(|c| c.complete)?;
    let mut item = item.clone();
    item.flags &= !item_bits::flags::DROPPED;
    let place = if to_cursor {
        Some(Place::Cursor).filter(|_| carried.inventory.at(Place::Cursor).is_none())
    } else {
        let worn = wearer.and_then(|w| items::auto_equip(rules, &carried.inventory, class, &item, w)).map(Place::Body);
        worn.or_else(|| carried.inventory.place_for(def.inv_size, def.auto_belt && rules.items().beltable(class)))
    };
    let Some(place) = place else {
        info!(player = name, guid, item = %d2_data::items::code_str(&item.code), "no room to pick it up");
        return None;
    };
    let held = Held { guid, class, size: def.inv_size, place, item };
    let packet = match place {
        Place::Cursor => items::world(rules, item_action::GROUND_TO_CURSOR, guid, &held.placed()),
        _ => items::held_packet(rules, &held, false)?,
    };
    info!(player = name, guid, item = %d2_data::items::code_str(&held.code()), quality = held.item.quality.number(), ?place, "item picked up");
    carried.inventory.insert(held).then_some(())?;
    Some((vec![packet], None))
}

/// `SendUnitToClient` for a monster: `0xAC`, its alignment state, and `0x6D` unless it is a
/// corpse.
fn monster_packets(rules: &GameData, unit: &Spawned) -> Vec<Vec<u8>> {
    let Spawned::Monster { guid, class, x, y, mode, life, ref components, ref variants } = *unit else { return Vec::new() };
    let alignment = rules.monsters().get(i32::from(class)).map_or(0, d2_data::monsters::MonsterClass::alignment);
    let mut packets = vec![d2gs::assign_monster(guid, class, x, y, life, mode, components, variants), d2gs::alignment_state(unit_type::MONSTER, guid, alignment)];
    if mode != battle::DEAD_MODE {
        packets.push(d2gs::monster_standing(guid, x, y, life));
    }
    packets
}

/// Queue a fight event's packet for each player it concerns — its own player, or every player
/// whose client holds the monster's room — and keep the room's population in step. A monster
/// that walks into another room belongs to that room from then on, as the engine moves a unit
/// between rooms: a client holding only the room it left forgets it, and one holding only the
/// room it entered is sent it — never a unit in a room the client has not loaded, which halts it
/// (`0x00465420`, error 316).
fn share_out(
    event: Event,
    rules: &GameData,
    world: &World,
    battle: &mut Battle,
    views: &HashMap<String, Vec<RoomId>>,
    outgoing: &mut HashMap<String, Vec<Vec<u8>>>,
    population: Option<&mut Population>,
) {
    if let Event::MonsterState { guid, x, y, mode, life } = event {
        let Some(population) = population else { return };
        population.update_monster(guid, (x, y), mode, life);
        let Some(to) = world.room_at(i32::from(x), i32::from(y)) else { return };
        let Some(from) = population.move_monster(guid, to) else { return };
        battle.set_monster_room(guid, to);
        let Some((_, unit)) = population.find(unit_type::MONSTER, guid) else { return };
        for (player, view) in views {
            let (had, has) = (view.contains(&from), view.contains(&to));
            if had && !has {
                outgoing.entry(player.clone()).or_default().push(d2gs::remove_unit(unit_type::MONSTER, guid));
            } else if has && !had {
                outgoing.entry(player.clone()).or_default().extend(monster_packets(rules, unit));
            }
        }
        return;
    }
    if let Some(player) = event.player() {
        if let Some(packet) = battle_packet(&event, player) {
            outgoing.entry(player.to_string()).or_default().push(packet);
        }
        return;
    }
    let Some(room) = event.room() else { return };
    for (player, view) in views {
        if view.contains(&room) {
            if let Some(packet) = battle_packet(&event, player) {
                outgoing.entry(player.clone()).or_default().push(packet);
            }
        }
    }
}

/// Where the engine puts something dropped by a unit at `from` (`0x00555DA0`): it starts two
/// subtiles right and three down when there is a room there, else at `from`, and takes that spot
/// or the nearest free one ring by ring out to 50 subtiles (`0x0064E810` → `0x0064DEA0`, the
/// candidate with the least `|dx| + |dy|` in the first ring holding one). Free is in a room, clear
/// of the spawn mask `0x3E01` — here walls and items (`taken`); objects, doors and the no-path bit
/// are not tracked — and in a straight line from `from` past no wall (mask `0x801`, `0x0066A670`;
/// the engine's line walk is not ported). `None` when nothing is free: the engine then makes nothing.
fn drop_spot(world: &World, taken: &dyn Fn(i32, i32) -> bool, from: (i32, i32)) -> Option<(RoomId, i32, i32)> {
    // A room without a collision map (the town's, for now) blocks nothing.
    let open = |x: i32, y: i32| world.room_at(x, y).is_some() && world.collision_at(x, y).map_or(true, |c| c & d2_game::path::WALL == 0);
    let free = |x: i32, y: i32| open(x, y) && !taken(x, y) && d2_game::path::clear_line(from, (x, y), &open);
    let start = if world.room_at(from.0 + 2, from.1 + 3).is_some() { (from.0 + 2, from.1 + 3) } else { from };
    let spot = |x: i32, y: i32| Some((world.room_at(x, y)?, x, y));
    if free(start.0, start.1) {
        return spot(start.0, start.1);
    }
    let (sx, sy) = start;
    for r in 1..50 {
        let mut best: Option<(i32, i32, i32)> = None;
        let mut consider = |x: i32, y: i32| {
            let d = (x - sx).abs() + (y - sy).abs();
            if free(x, y) && best.map_or(true, |(_, _, bd)| d < bd) {
                best = Some((x, y, d));
            }
        };
        for y in sy - r..=sy + r {
            consider(sx - r, y);
            consider(sx + r, y);
        }
        for x in sx - r + 1..=sx + r - 1 {
            consider(x, sy - r);
            consider(x, sy + r);
        }
        if let Some((x, y, _)) = best {
            return spot(x, y);
        }
    }
    None
}

/// Accept game connections forever.
pub async fn serve(listener: TcpListener, server: Arc<GameServer>) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let server = Arc::clone(&server);
                tokio::spawn(async move {
                    if let Err(e) = session(stream, peer, &server).await {
                        debug!(%peer, error = %e, "game connection ended with an error");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, "game port accept failed");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    AwaitLogon,
    AwaitEnterGame,
    InGame,
}

struct Player {
    game_id: u16,
    character: Character,
    difficulty: u8,
    map_seed: u32,
    spawn: (u16, u16),
    /// The character's `.d2s`, if one has been saved and reads as this character's.
    save: Option<Save>,
    /// Waypoints learned on this difficulty.
    waypoints: [u8; d2gs::WAYPOINT_FLAG_BYTES],
    /// Level when last saved.
    saved_level: u32,
}

impl Player {
    fn new(game_id: u16, character: Character, difficulty: u8, map_seed: u32, spawn: (u16, u16)) -> Self {
        let save = character.save.as_deref().and_then(|b| Save::parse(b).ok()).filter(|s| s.name().eq_ignore_ascii_case(&character.name) && s.class() == character.class);
        let mut flags = [0u8; d2gs::WAYPOINT_FLAG_BYTES];
        if let Some(s) = &save {
            flags.copy_from_slice(&s.waypoints[usize::from(difficulty.min(2))][2..2 + d2gs::WAYPOINT_FLAG_BYTES]);
        }
        let saved_level = save.as_ref().map_or(1, |s| u32::from(s.level()));
        Self { game_id, character, difficulty, map_seed, spawn, save, waypoints: flags, saved_level }
    }

    /// The stats the player joins with: its save's, or a new character's.
    fn join_stats(&self, rules: &GameData) -> Option<Vec<(u8, u32)>> {
        match &self.save {
            Some(save) => Some(rules.saved_character_stats(self.character.class, &save.stats)),
            None => rules.new_character_stats(self.character.class),
        }
    }
}

/// Where the server has a client's player: moved toward the spot it last walked or ran to,
/// in a straight line (collision is not ported), and the rooms the client holds.
#[derive(Debug, Clone, PartialEq)]
struct Walker {
    x: f64,
    y: f64,
    target: Option<(f64, f64)>,
    /// Subtiles a second.
    speed: f64,
    room: Option<RoomId>,
    view: Vec<RoomId>,
}

impl Walker {
    fn moving(&self) -> bool {
        self.target.is_some()
    }

    /// Standing, walking or running, for stamina; `scale` is the server's speed scale.
    fn motion(&self, scale: f64) -> battle::Motion {
        match self.target {
            None => battle::Motion::Standing,
            Some(_) if self.speed >= RUN_SPEED * scale - 1e-9 => battle::Motion::Running,
            Some(_) => battle::Motion::Walking,
        }
    }

    /// Head for `(x, y)`.
    fn go(&mut self, x: f64, y: f64, speed: f64) {
        self.target = Some((x, y));
        self.speed = speed;
    }

    /// Advance by `dt`; stop on arrival.
    fn step(&mut self, dt: Duration) {
        let Some((tx, ty)) = self.target else { return };
        let (dx, dy) = (tx - self.x, ty - self.y);
        let left = dx.hypot(dy);
        let travel = self.speed * dt.as_secs_f64();
        if travel >= left {
            (self.x, self.y, self.target) = (tx, ty, None);
        } else {
            self.x += dx / left * travel;
            self.y += dy / left * travel;
        }
    }
}

/// Move a player to (`x`, `y`) — waypoint travel, a warp, a respawn — the way `0x00554EA0` does:
/// the room landed in, the room stream, then `0x15` flagged as a warp. `false` if the spot is in
/// no room.
fn teleport(server: &GameServer, p: &Player, w: &mut Walker, x: u16, y: u16, outbox: &mut Outbox) -> bool {
    let Some((room, near)) = server.near(p.game_id, f64::from(x), f64::from(y)) else { return false };
    let own = server.lock().by_id.get(&p.game_id).and_then(|g| g.world.as_ref()?.room(room));
    if let Some(r) = own {
        outbox.push(&d2gs::load_room(r.x as u16, r.y as u16, room.level as u8));
    }
    for packet in server.view_change(p.game_id, &w.view, &near) {
        outbox.push(&packet);
    }
    outbox.push(&d2gs::reassign_player(0, PLAYER_GUID, x, y, 1));
    (w.x, w.y, w.target, w.room, w.view) = (f64::from(x), f64::from(y), None, Some(room), near);
    server.set_position(p.game_id, &p.character.name, w.x, w.y);
    server.set_view(p.game_id, &p.character.name, &w.view);
    true
}

/// After "You have died" (`0x41`): stand the player up where it lies, then take it to the camp's
/// waypoint, as `0x0054C0E0` does — life, mana and stamina as stats, then the move.
///
/// Two client rules decide the order. The client will not move a player in a death mode
/// (`0x004654C0` returns for modes 0 and 0x11), so a `0x15` for a corpse leaves it in rooms the
/// move just unloaded and the client halts (`0x0045D160`, error 1336): the stand-up comes first.
/// And the client does not run `0x0D` as it arrives: it queues it on the unit (`0x0045F730`; so
/// too `0x0C`–`0x10`, `0x17`, `0x4C`, `0x4D`, `0x67`–`0x72`) and runs the queue at the unit's next
/// update (`0x00480810` → `0x0045FA40`), after everything else in the frame. The corpse's `0x0D`
/// event 9 went out when the death throes ended ([`battle::Battle`]); sending it again here would
/// run after the stand-up and kill the player again in town, the "You have died" screen stuck
/// (tagban's test, 2026-09-15). `false` if the player is not dead or the camp has no room.
fn respawn(server: &GameServer, p: &Player, w: &mut Walker, outbox: &mut Outbox) -> bool {
    if !server.player_dead(p.game_id, &p.character.name) || server.near(p.game_id, f64::from(p.spawn.0), f64::from(p.spawn.1)).is_none() {
        return false;
    }
    for packet in server.revive(p.game_id, &p.character.name) {
        outbox.push(&packet);
    }
    teleport(server, p, w, p.spawn.0, p.spawn.1, outbox)
}

/// Whether the server's idea of the player stands close enough to a monster at (`x`, `y`) to hit it.
fn within_reach(w: &Walker, x: u16, y: u16) -> bool {
    d2_game::path::distance((w.x as i32, w.y as i32), (i32::from(x), i32::from(y))) <= battle::PLAYER_REACH
}

/// One client on the game port.
async fn session(mut stream: TcpStream, peer: SocketAddr, server: &GameServer) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    info!(%peer, "game connection; sending AF 01");
    // Raw, and alone: the client splits whatever arrives in the same read as the greeting
    // as raw packets, so nothing else is sent until it logs on.
    stream.write_all(&d2gs::GREETING).await?;

    let mut player: Option<Player> = None;
    let result = run(&mut stream, peer, server, &mut player).await;
    if let Some(mut p) = player {
        info!(%peer, character = %p.character.name, "game connection closed");
        server.save_character(&mut p).await;
        server.leave(p.game_id, &p.character.name);
    }
    result
}

async fn run(
    stream: &mut TcpStream,
    peer: SocketAddr,
    server: &GameServer,
    player: &mut Option<Player>,
) -> std::io::Result<()> {
    let tables = &server.tables;
    let mut stage = Stage::AwaitLogon;
    let mut inbox: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let mut outbox = Outbox::default();
    let mut walker: Option<Walker> = None;
    let mut clock_seen: u64 = 0;
    let mut last_packet = Instant::now();
    // The player's waypoint flags: its save's, set when it enters the game.
    let mut waypoints = [0u8; d2gs::WAYPOINT_FLAG_BYTES];
    let mut frames_since_check: u32 = 0;
    let mut frame = tokio::time::interval(SERVER_FRAME);
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_step = Instant::now();
    // A swing at a monster the player is still walking up to: its guid and when it was asked.
    let mut pending_attack: Option<(u32, Instant)> = None;
    // A release (0x41) waiting for the death throes to end.
    let mut pending_respawn = false;
    // When the corpse event (0x0D event 9) was queued for the client: the stand-up must reach the
    // client in a later frame than it (see `respawn`).
    let mut corpse_told: Option<Instant> = None;
    let corpse_event = d2gs::player_reaction(unit_type::PLAYER, PLAYER_GUID, battle::reaction::DEAD, 0, 0);

    loop {
        let limit = if stage == Stage::AwaitLogon { LOGON_TIMEOUT } else { IN_GAME_TIMEOUT };
        let in_game = stage == Stage::InGame;
        let read = tokio::select! {
            r = tokio::time::timeout(limit, stream.read(&mut chunk)) => Some(r),
            _ = frame.tick(), if in_game => None,
        };
        let Some(read) = read else {
            // A server frame: the act's clock, then the player if it moves.
            if last_packet.elapsed() > IN_GAME_TIMEOUT {
                info!(%peer, ?stage, "game connection timed out");
                return Ok(());
            }
            let Some(p) = player.as_ref() else { continue };
            if let Some((reports, packet)) = server.clock(p.game_id) {
                if reports > clock_seen {
                    clock_seen = reports;
                    outbox.push(&packet);
                    flush(stream, peer, tables, &mut outbox).await?;
                }
            }
            let motion = walker.as_ref().map_or(battle::Motion::Standing, |w| w.motion(server.speed_scale));
            for packet in server.battle_step(p.game_id, &p.character.name, motion) {
                if packet == corpse_event {
                    corpse_told = Some(Instant::now());
                }
                outbox.push(&packet);
            }
            if pending_respawn && server.player_death_settled(p.game_id, &p.character.name) {
                match corpse_told {
                    Some(told) if told.elapsed() >= SERVER_FRAME * 2 => {
                        pending_respawn = false;
                        corpse_told = None;
                        if let Some(w) = walker.as_mut() {
                            info!(%peer, x = p.spawn.0, y = p.spawn.1, "respawn in town");
                            respawn(server, p, w, &mut outbox);
                        }
                    }
                    Some(_) => {}
                    None => {
                        // The corpse event never went out (the client still plays the death
                        // throes, mode 0, and a stand-up needs 0x11): send it, stand up later.
                        info!(%peer, "corpse event sent before the release");
                        outbox.push(&corpse_event);
                        corpse_told = Some(Instant::now());
                    }
                }
            }
            frames_since_check += 1;
            if frames_since_check >= 25 {
                frames_since_check = 0;
                let level = server.player_level(p.game_id, &p.character.name);
                if let Some(p) = player.as_mut().filter(|p| level.is_some_and(|l| l != p.saved_level)) {
                    server.save_character(p).await;
                }
            }
            let Some(p) = player.as_ref() else { continue };
            if let (Some((guid, asked)), Some(w)) = (pending_attack, walker.as_mut()) {
                match server.battle_monster(p.game_id, guid) {
                    Some((mx, my)) if asked.elapsed() < ATTACK_WAIT => {
                        if within_reach(w, mx, my) {
                            server.player_attack(p.game_id, &p.character.name, guid);
                            pending_attack = None;
                            w.target = None;
                        } else if !w.moving() {
                            last_step = Instant::now();
                            w.go(f64::from(mx), f64::from(my), RUN_SPEED * server.speed_scale);
                        }
                    }
                    _ => pending_attack = None,
                }
            }
            flush(stream, peer, tables, &mut outbox).await?;
            let Some(w) = walker.as_mut().filter(|w| w.moving()) else { continue };
            let now = Instant::now();
            w.step(now.duration_since(last_step).min(SERVER_FRAME * 5));
            last_step = now;
            server.set_position(p.game_id, &p.character.name, w.x, w.y);
            if let Some((room, near)) = server.near(p.game_id, w.x, w.y) {
                if w.room != Some(room) {
                    if w.room.map(|r| r.level) != Some(room.level) {
                        info!(%peer, level = room.level, x = w.x as i32, y = w.y as i32, "player entered a level");
                    }
                    let view = server.kept_view(p.game_id, room, &near, &w.view);
                    for packet in server.view_change(p.game_id, &w.view, &view) {
                        outbox.push(&packet);
                    }
                    (w.room, w.view) = (Some(room), view);
                    server.set_view(p.game_id, &p.character.name, &w.view);
                    flush(stream, peer, tables, &mut outbox).await?;
                }
            }
            continue;
        };
        let n = match read {
            Ok(r) => r?,
            Err(_) => {
                info!(%peer, ?stage, "game connection timed out");
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(());
        }
        last_packet = Instant::now();
        inbox.extend_from_slice(&chunk[..n]);

        loop {
            let len = match d2gs::client_packet_len(&tables.client_sizes, &inbox) {
                ClientPacketLen::Incomplete => break,
                ClientPacketLen::Invalid(op) => {
                    warn!(%peer, op = %format!("{op:#04x}"), pending = %hex_preview(&inbox), "unframeable client packet; closing");
                    return Ok(());
                }
                ClientPacketLen::Len(len) => len,
            };
            let packet: Vec<u8> = inbox.drain(..len).collect();
            info!(%peer, ?stage, op = %format!("{:#04x}", packet[0]), len, body = %hex_preview(&packet), "D2GS packet in");
            let u16_at = |at: usize| packet.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            let u32_at = |at: usize| packet.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));

            match (stage, packet[0]) {
                (Stage::AwaitLogon, cs::GAME_LOGON) => {
                    let Some(joined) = logon(stream, peer, server, &packet, &mut outbox).await? else {
                        return Ok(());
                    };
                    *player = Some(joined);
                    stage = Stage::AwaitEnterGame;
                }
                (Stage::AwaitLogon, _) => {
                    info!(%peer, "packet before GAMELOGON; closing");
                    return Ok(());
                }
                (Stage::AwaitEnterGame, cs::ENTER_GAME) => {
                    let p = player.as_ref().expect("logged on");
                    waypoints = p.waypoints;
                    let joined = enter_game(stream, peer, server, p, &mut outbox).await?;
                    server.set_position(p.game_id, &p.character.name, joined.x, joined.y);
                    server.set_view(p.game_id, &p.character.name, &joined.view);
                    walker = Some(joined);
                    clock_seen = server.clock(p.game_id).map_or(0, |(reports, _)| reports);
                    stage = Stage::InGame;
                }
                (Stage::InGame, op @ (cs::WALK_TO_LOCATION | cs::RUN_TO_LOCATION)) => {
                    if let Some(w) = walker.as_mut() {
                        let speed = if op == cs::RUN_TO_LOCATION { RUN_SPEED } else { WALK_SPEED };
                        if !w.moving() {
                            last_step = Instant::now();
                        }
                        w.go(f64::from(u16_at(1)), f64::from(u16_at(3)), speed * server.speed_scale);
                    }
                }
                (Stage::InGame, cs::INTERACT) if u32_at(1) == u32::from(unit_type::WARP) => {
                    // A cave mouth or stairs: to the other side (0x005550B0).
                    let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) else { continue };
                    let Some((level, x, y)) = server.warp_arrival(p.game_id, u32_at(5)) else {
                        debug!(%peer, guid = u32_at(5), "warp to a level not built");
                        continue;
                    };
                    info!(%peer, level, x, y, "warp");
                    pending_attack = None;
                    if teleport(server, p, w, x, y, &mut outbox) {
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                }
                (Stage::InGame, cs::INTERACT) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.interact(p.game_id, u32_at(1), u32_at(5), &mut waypoints);
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                    if waypoints != p.waypoints {
                        p.waypoints = waypoints;
                        server.save_character(p).await;
                    }
                }
                (Stage::InGame, cs::WAYPOINT_TRAVEL) => {
                    let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) else { continue };
                    let level = i32::from(u16_at(5));
                    let Some((x, y)) = server.waypoint_travel(p.game_id, u32_at(1), level, &waypoints) else {
                        debug!(%peer, level, "waypoint travel refused");
                        continue;
                    };
                    info!(%peer, level, x, y, "waypoint travel");
                    if teleport(server, p, w, x, y, &mut outbox) {
                        if let Some(packet) = server.light_waypoint_at(p.game_id, x, y) {
                            outbox.push(&packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                }
                (
                    Stage::InGame,
                    op @ (cs::LEFT_SKILL_ON_UNIT
                    | cs::LEFT_SKILL_ON_UNIT_HOLD
                    | cs::LEFT_SKILL_ON_UNIT_REPEAT
                    | cs::LEFT_SKILL_ON_UNIT_HOLD_REPEAT
                    | cs::RIGHT_SKILL_ON_UNIT
                    | cs::RIGHT_SKILL_ON_UNIT_HOLD
                    | cs::RIGHT_SKILL_ON_UNIT_REPEAT
                    | cs::RIGHT_SKILL_ON_UNIT_HOLD_REPEAT),
                ) => {
                    // The button's skill: a missile skill casts where it stands; Attack and melee
                    // skills swing at the monster once the player is close enough (the engine walks
                    // it there, as does the client).
                    let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) else { continue };
                    let guid = u32_at(5);
                    if u32_at(1) != u32::from(unit_type::MONSTER) {
                        continue;
                    }
                    let left = matches!(op, cs::LEFT_SKILL_ON_UNIT | cs::LEFT_SKILL_ON_UNIT_HOLD | cs::LEFT_SKILL_ON_UNIT_REPEAT | cs::LEFT_SKILL_ON_UNIT_HOLD_REPEAT);
                    if server.player_skill(p.game_id, &p.character.name, left, battle::Aim::Unit(guid)) != battle::SkillUse::Swing {
                        continue;
                    }
                    let Some((mx, my)) = server.battle_monster(p.game_id, guid) else { continue };
                    if within_reach(w, mx, my) {
                        server.player_attack(p.game_id, &p.character.name, guid);
                        pending_attack = None;
                    } else {
                        if !w.moving() {
                            last_step = Instant::now();
                        }
                        w.go(f64::from(mx), f64::from(my), RUN_SPEED * server.speed_scale);
                        pending_attack = Some((guid, Instant::now()));
                    }
                }
                (Stage::InGame, op @ (cs::LEFT_SKILL_ON_LOCATION | cs::LEFT_SKILL_ON_LOCATION_HOLD | cs::RIGHT_SKILL_ON_LOCATION | cs::RIGHT_SKILL_ON_LOCATION_HOLD)) => {
                    let Some(p) = player.as_ref() else { continue };
                    let left = matches!(op, cs::LEFT_SKILL_ON_LOCATION | cs::LEFT_SKILL_ON_LOCATION_HOLD);
                    server.player_skill(p.game_id, &p.character.name, left, battle::Aim::At(u16_at(1), u16_at(3)));
                }
                (Stage::InGame, cs::ADD_SKILL_POINT) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.learn_skill(p.game_id, &p.character.name, u16_at(1));
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                        server.save_character(p).await;
                    }
                }
                (Stage::InGame, cs::SELECT_SKILL) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.select_skill(p.game_id, &p.character.name, u16_at(1), u16_at(3) & 0x8000 != 0, u32_at(5));
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                }
                (Stage::InGame, cs::ADD_STAT_POINT) => {
                    let Some(p) = player.as_ref() else { continue };
                    let Ok(stat) = u8::try_from(u16_at(1)) else { continue };
                    for packet in server.spend_stat_point(p.game_id, &p.character.name, stat) {
                        outbox.push(&packet);
                    }
                    flush(stream, peer, tables, &mut outbox).await?;
                }
                (Stage::InGame, cs::PICK_UP_ITEM) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.pick_up(p.game_id, &p.character.name, u32_at(5), u32_at(9) != 0);
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                        server.save_character(p).await;
                    }
                }
                (Stage::InGame, op @ (cs::USE_ITEM | cs::USE_BELT_ITEM)) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.use_item(p.game_id, &p.character.name, u32_at(1), op == cs::USE_BELT_ITEM);
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                        server.save_character(p).await;
                    }
                }
                (Stage::InGame, cs::IDENTIFY_ITEM) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = server.identify_item(p.game_id, &p.character.name, u32_at(1), u32_at(5));
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                        server.save_character(p).await;
                    }
                }
                (
                    Stage::InGame,
                    op @ (cs::DROP_ITEM
                    | cs::INSERT_ITEM
                    | cs::LIFT_ITEM
                    | cs::EQUIP_ITEM
                    | cs::UNEQUIP_ITEM
                    | cs::SWAP_EQUIPPED
                    | cs::SWAP_GRID_ITEM
                    | cs::BELT_ITEM
                    | cs::UNBELT_ITEM
                    | cs::SWAP_BELT_ITEM),
                ) => {
                    let Some(p) = player.as_mut() else { continue };
                    let body = |at: usize| u8::try_from(u32_at(at)).unwrap_or(0);
                    let request = match op {
                        cs::DROP_ITEM => ItemMove::Drop { guid: u32_at(1) },
                        cs::INSERT_ITEM => ItemMove::Insert { guid: u32_at(1), col: u32_at(5), row: u32_at(9), grid: u32_at(13) },
                        cs::LIFT_ITEM => ItemMove::Lift { guid: u32_at(1) },
                        cs::EQUIP_ITEM => ItemMove::Equip { guid: u32_at(1), body: body(5) },
                        cs::UNEQUIP_ITEM => ItemMove::Unequip { body: u8::try_from(u16_at(1)).unwrap_or(0) },
                        cs::SWAP_EQUIPPED => ItemMove::SwapEquipped { guid: u32_at(1), body: body(5) },
                        cs::SWAP_GRID_ITEM => ItemMove::SwapGrid { guid: u32_at(1), other: u32_at(5), col: u32_at(9), row: u32_at(13) },
                        cs::BELT_ITEM => ItemMove::Belt { guid: u32_at(1), slot: u32_at(5) },
                        cs::UNBELT_ITEM => ItemMove::Unbelt { guid: u32_at(1) },
                        _ => ItemMove::SwapBelt { guid: u32_at(1), other: u32_at(5) },
                    };
                    let replies = server.move_item(p.game_id, &p.character.name, request);
                    if replies.is_empty() {
                        debug!(game_id = p.game_id, player = %p.character.name, ?request, "item move refused");
                        continue;
                    }
                    for packet in &replies {
                        outbox.push(packet);
                    }
                    flush(stream, peer, tables, &mut outbox).await?;
                    server.save_character(p).await;
                }
                (Stage::InGame, cs::NPC_TALK) => {
                    let Some(p) = player.as_ref() else { continue };
                    let replies = server.npc_talk(p.game_id, &p.character.name, u32_at(1), u32_at(5));
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                }
                (Stage::InGame, cs::NPC_ACTION) => {
                    let Some(p) = player.as_ref() else { continue };
                    for packet in server.npc_action(p.game_id, &p.character.name, u32_at(1), u32_at(5)) {
                        outbox.push(&packet);
                    }
                    flush(stream, peer, tables, &mut outbox).await?;
                }
                (Stage::InGame, cs::NPC_CANCEL) => {
                    let Some(p) = player.as_ref() else { continue };
                    server.npc_cancel(p.game_id, &p.character.name, u32_at(5));
                }
                (Stage::InGame, op @ (cs::NPC_BUY | cs::NPC_SELL)) => {
                    let Some(p) = player.as_mut() else { continue };
                    let replies = if op == cs::NPC_BUY {
                        server.buy(p.game_id, &p.character.name, u32_at(1), u32_at(5), u32_at(9))
                    } else {
                        server.sell(p.game_id, &p.character.name, u32_at(1), u32_at(5), u16_at(9))
                    };
                    for packet in &replies {
                        outbox.push(packet);
                    }
                    flush(stream, peer, tables, &mut outbox).await?;
                    server.save_character(p).await;
                }
                (Stage::InGame, cs::RESPAWN) => {
                    // Done on the next frame the corpse has settled (see `respawn`).
                    pending_respawn = true;
                    pending_attack = None;
                }
                (Stage::InGame, cs::UPDATE_POSITION) => {
                    // The client's own idea of where its player is (engine `0x0054CD50` re-syncs to
                    // it): take it, and let the next frame follow it with rooms.
                    if let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) {
                        (w.x, w.y) = (f64::from(u16_at(1)), f64::from(u16_at(3)));
                        server.set_position(p.game_id, &p.character.name, w.x, w.y);
                        if !w.moving() {
                            w.target = Some((w.x, w.y));
                            last_step = Instant::now();
                        }
                    }
                }
                (Stage::InGame, op @ (cs::WALK_TO_UNIT | cs::RUN_TO_UNIT)) => {
                    let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) else { continue };
                    if let Some((x, y)) = server.unit_position(p.game_id, u32_at(1), u32_at(5)) {
                        let speed = if op == cs::RUN_TO_UNIT { RUN_SPEED } else { WALK_SPEED };
                        if !w.moving() {
                            last_step = Instant::now();
                        }
                        w.go(f64::from(x), f64::from(y), speed * server.speed_scale);
                    }
                }
                (_, cs::PING) => {
                    outbox.push(&d2gs::pong());
                    flush(stream, peer, tables, &mut outbox).await?;
                }
                (_, cs::LEAVE_GAME) => {
                    outbox.push(&[d2gs::sc::UNLOAD_COMPLETE]);
                    outbox.push(&[d2gs::sc::GAME_EXIT]);
                    flush(stream, peer, tables, &mut outbox).await?;
                    outbox.push(&[d2gs::sc::TERMINATED]);
                    flush(stream, peer, tables, &mut outbox).await?;
                    return Ok(());
                }
                // Logged above; the test answers nothing else.
                _ => {}
            }
        }
    }
}

/// `GAMELOGON`: check it, then `01 00` and `02`. `None` if refused (a reason was sent).
async fn logon(
    stream: &mut TcpStream,
    peer: SocketAddr,
    server: &GameServer,
    packet: &[u8],
    outbox: &mut Outbox,
) -> std::io::Result<Option<Player>> {
    let tables = &server.tables;
    let refuse = |reason: u32| d2gs::join_failed_packet(reason);
    let logon = match GameLogon::parse(packet) {
        Ok(l) => l,
        Err(e) => {
            info!(%peer, error = %e, "malformed GAMELOGON; closing");
            return Ok(None);
        }
    };
    info!(
        %peer,
        game_id = logon.game_id,
        hash = %format!("{:#010x}", logon.game_hash),
        class = logon.class,
        version = %format!("{:#04x}", logon.version),
        byte_20 = %format!("{:#04x}", logon.unknown_20),
        name = %logon.name,
        "GAMELOGON"
    );
    if logon.version != d2gs::VERSION_114D {
        outbox.push(&refuse(join_failed::WRONG_VERSION));
        flush(stream, peer, tables, outbox).await?;
        return Ok(None);
    }
    let Some(joined) = server.claim(&logon) else {
        info!(%peer, name = %logon.name, "GAMELOGON matches no staged join; refusing");
        outbox.push(&refuse(join_failed::GENERIC));
        flush(stream, peer, tables, outbox).await?;
        return Ok(None);
    };
    let (character, difficulty) = (&joined.character, joined.difficulty);
    if logon.class != character.class {
        warn!(%peer, sent = logon.class, stored = character.class, "GAMELOGON class differs from the character's");
    }

    let expansion = character.status & status::EXPANSION != 0;
    let ladder = character.status & status::LADDER != 0;
    let hardcore = character.status & status::HARDCORE != 0;
    let flags = d2gs::game_flags(difficulty, hardcore, expansion, ladder);
    outbox.push(&d2gs::game_flags_packet(difficulty, flags, expansion, ladder));
    outbox.push(&[d2gs::sc::LOADING]);
    flush(stream, peer, tables, outbox).await?;
    // The engine sends 02 once the character is in hand — for a realm game, when the
    // database answers. Ours is already loaded.
    outbox.push(&[d2gs::sc::LOAD_SUCCESS]);
    flush(stream, peer, tables, outbox).await?;
    Ok(Some(joined))
}

/// `ENTERGAME`, in the engine's order (`HandleSrvJoinAct`): `ClientAddPlayerToGame` creates
/// the player — its `0x59`, before it has a position, then the quest setup a new player gets
/// (`0x00546270`: `5E 28 29`) — names it the client's own with `0x0B`,
/// sends its stats (`0x1D`–`0x1F`), both selected skills and its life/mana (`0x95`); then the act
/// (`03 53`); then `PlacePlayerInAct` loads the spawn room (`07`), enters the player into it —
/// every near room's `07` and units — and places the player (`15 7E`). `04` follows a frame
/// later. Returns where the player stands and the rooms its client holds.
async fn enter_game(
    stream: &mut TcpStream,
    peer: SocketAddr,
    server: &GameServer,
    p: &Player,
    outbox: &mut Outbox,
) -> std::io::Result<Walker> {
    let tables = &server.tables;
    let (x, y) = p.spawn;
    // Unplaced: at (0, 0) the client creates the unit without looking for a room.
    outbox.push(&d2gs::assign_player(PLAYER_GUID, p.character.class, &p.character.name, 0, 0));
    // SendUnitToClient's states for a player (0x00570E30): its alignment, good, which the engine
    // sets as it creates the player (0x005348C0). Without it the client counts the player as evil
    // like the monsters and will not let either side attack.
    outbox.push(&d2gs::alignment_state(0, PLAYER_GUID, 2));
    // A fresh game's quests (every quest object starts available) and a new character's flags,
    // all clear: the client halts on entering a new area without them.
    outbox.push(&d2gs::quest_states(&[1; d2gs::QUESTS]));
    outbox.push(&d2gs::player_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]));
    outbox.push(&d2gs::game_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]));
    outbox.push(&d2gs::own_unit(0, PLAYER_GUID));
    // Its stats, one packet each (ClientAddPlayerToGame walks the stat list through 0x548520),
    // as a new character: no .d2s is loaded yet.
    let stats = server.rules.as_ref().and_then(|r| p.join_stats(r)).unwrap_or_default();
    for &(id, value) in &stats {
        outbox.push(&d2gs::set_stat(id, value));
    }
    server.join_battle(p.game_id, &p.character.name, p.character.class, &stats);
    // Its items, once the client has its own unit to put them on (the action 4 and 0x0E handlers
    // place them in the client's own player, `0x7A6A70`, which 0x0B sets).
    for packet in server.join_items(p) {
        outbox.push(&packet);
    }
    for packet in server.join_skills(p) {
        outbox.push(&packet);
    }
    // Then life, mana and stamina in whole points, before the player has a position (0x548760).
    if !stats.is_empty() {
        let whole = |id: u8| stats.iter().find(|&&(s, _)| s == id).map_or(0, |&(_, v)| (v >> 8) as u16);
        outbox.push(&d2gs::life_and_position(whole(stat::HITPOINTS), whole(stat::MANA), whole(stat::STAMINA), 0, 0, 0, 0));
    }
    outbox.push(&d2gs::load_act(0, p.map_seed, TOWN_AREA, 0));
    // The act's time of day as it stands (a new act starts in period 2 at angle 0: day). The
    // client's 0x53 handler reads its own player unit, which is why 0x0B has to be in first.
    let environment = server.clock(p.game_id).map_or_else(|| d2gs::act_environment(2, 0, false), |(_, packet)| packet);
    outbox.push(&environment);
    let mut walker = Walker { x: f64::from(x), y: f64::from(y), target: None, speed: 0.0, room: None, view: Vec::new() };
    let rooms = match server.near(p.game_id, walker.x, walker.y) {
        Some((room, near)) => {
            let own = server.lock().by_id.get(&p.game_id).and_then(|g| g.world.as_ref()?.room(room));
            let mut packets: Vec<Vec<u8>> =
                own.map(|r| d2gs::load_room(r.x as u16, r.y as u16, room.level as u8)).into_iter().collect();
            packets.extend(server.view_change(p.game_id, &[], &near));
            (walker.room, walker.view) = (Some(room), near);
            packets
        }
        None => vec![d2gs::load_room(FALLBACK_SPAWN_ROOM.0, FALLBACK_SPAWN_ROOM.1, TOWN_AREA as u8)],
    };
    for packet in &rooms {
        outbox.push(packet);
    }
    outbox.push(&d2gs::reassign_player(0, PLAYER_GUID, x, y, 1));
    outbox.push(&d2gs::player_placed());
    flush(stream, peer, tables, outbox).await?;
    tokio::time::sleep(SERVER_FRAME).await;
    outbox.push(&[d2gs::sc::LOAD_COMPLETE]);
    flush(stream, peer, tables, outbox).await?;
    info!(
        %peer,
        character = %p.character.name,
        difficulty = p.difficulty,
        room_packets = rooms.len(),
        "join sequence sent; from here the server follows the player's walking and logs the rest"
    );
    Ok(walker)
}

/// Compress and send everything queued, logging the plaintext.
async fn flush(stream: &mut TcpStream, peer: SocketAddr, tables: &EngineTables, outbox: &mut Outbox) -> std::io::Result<()> {
    if outbox.is_empty() {
        return Ok(());
    }
    let plain: Vec<String> = outbox.pending().map(hex_preview).collect();
    let mut wire = Vec::new();
    if let Err(e) = outbox.flush(&tables.huffman, &mut wire) {
        warn!(%peer, error = %e, "could not frame queued packets");
    }
    info!(%peer, packets = %plain.join(" "), wire_bytes = wire.len(), "D2GS packets out");
    stream.write_all(&wire).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bnetcc_proto::d2gs::{item_flags, Huffman, ItemSpot, CLIENT_OPCODES, SERVER_OPCODES};

    /// A simple item as treasure makes it.
    fn simple_item(code: &[u8; 4], version: u16) -> Box<Item> {
        Box::new(Item::new(*code, version, 1, Location::Ground { x: 0, y: 0 }))
    }

    /// Tables shaped like the engine's for the packets the join uses, with a made-up
    /// complete Huffman code (no Blizzard data in tests).
    /// Game rules from made-up tables in the real shape: every class has vitality 20, hpadd 30,
    /// energy 15 and stamina 80.
    pub(crate) fn test_rules() -> GameData {
        use d2_formats::excel::Table;
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\r\n");
        for name in d2_data::CLASSES {
            cs.push_str(&format!("{name}\t25\t20\t15\t20\t0\t80\t30\r\n"));
        }
        let exp = "Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\tExpRatio\r\n\
                   MaxLvl\t99\t99\t99\t99\t99\t99\t99\t10\r\n0\t0\t0\t0\t0\t0\t0\t0\t1024\r\n\
                   1\t500\t500\t500\t500\t500\t500\t500\t1024\r\n";
        GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(exp.as_bytes())).unwrap()
    }

    pub(crate) fn test_tables() -> EngineTables {
        let mut lengths = [9u8; 256];
        lengths[0] = 1;
        lengths[0xAF] = 8;
        let mut client = [0i32; CLIENT_OPCODES];
        client[usize::from(cs::GAME_LOGON)] = 37;
        client[usize::from(cs::ENTER_GAME)] = 1;
        client[usize::from(cs::PING)] = 13;
        client[usize::from(cs::LEAVE_GAME)] = 1;
        client[usize::from(cs::WALK_TO_LOCATION)] = 5;
        client[usize::from(cs::RUN_TO_LOCATION)] = 5;
        client[usize::from(cs::WALK_TO_UNIT)] = 9;
        client[usize::from(cs::RUN_TO_UNIT)] = 9;
        client[usize::from(cs::INTERACT)] = 9;
        client[usize::from(cs::WAYPOINT_TRAVEL)] = 9;
        client[usize::from(cs::UPDATE_POSITION)] = 5;
        client[usize::from(cs::PICK_UP_ITEM)] = 13;
        EngineTables::new(&lengths, client, [0i32; SERVER_OPCODES]).unwrap()
    }

    pub(crate) fn character(name: &str, class: u8, status: u8) -> Character {
        Character {
            account: 1,
            name: name.into(),
            class,
            status,
            level: 1,
            progression: 0,
            created_at: 0,
            last_played: 0,
            save: None,
        }
    }

    pub(crate) fn logon_packet(game_id: u16, hash: u32, class: u8, version: u32, name: &str) -> Vec<u8> {
        let mut p = vec![0u8; GameLogon::LEN];
        p[0] = cs::GAME_LOGON;
        p[1..5].copy_from_slice(&hash.to_le_bytes());
        p[5..7].copy_from_slice(&game_id.to_le_bytes());
        p[7] = class;
        p[8..12].copy_from_slice(&version.to_le_bytes());
        p[21..21 + name.len()].copy_from_slice(name.as_bytes());
        p
    }

    /// Read one compressed frame and return its plaintext.
    pub(crate) async fn read_frame(s: &mut TcpStream, huffman: &Huffman) -> Vec<u8> {
        let mut first = [0u8; 1];
        s.read_exact(&mut first).await.expect("frame header");
        let (header, total) = if first[0] < 0xF0 {
            (1, usize::from(first[0]))
        } else {
            let mut lo = [0u8; 1];
            s.read_exact(&mut lo).await.expect("frame header");
            (2, (usize::from(first[0] & 0x0F) << 8) | usize::from(lo[0]))
        };
        let mut body = vec![0u8; total - header];
        s.read_exact(&mut body).await.expect("frame body");
        let mut plain = Vec::new();
        huffman.decompress(&body, &mut plain);
        plain
    }

    pub(crate) async fn spawn(server: Arc<GameServer>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(serve(listener, server));
        addr
    }

    /// With the operator's `Game.exe` (`BNETCC_D2_GAME_EXE`), the wire tables it yields reproduce
    /// a frame captured off a live 1.14d server: plaintext GameFlags + 0x00 in, `05 7a 09 a5 f0`
    /// on the wire. The capture is from jaenster/libd2 (MIT).
    #[test]
    fn with_a_real_game_exe_the_codec_matches_a_live_server() {
        let Ok(path) = std::env::var("BNETCC_D2_GAME_EXE") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(path).unwrap()).expect("1.14d tables");
        let tables = EngineTables::new(&engine.huffman_code_lengths, engine.client_packet_sizes, engine.server_packet_sizes).unwrap();
        let mut out = Vec::new();
        d2gs::encode_frame(&tables.huffman, &[0x01, 0x00, 0x04, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00], &mut out).unwrap();
        assert_eq!(out, vec![0x05, 0x7A, 0x09, 0xA5, 0xF0]);
        let size = |op: u8| engine.client_packet_sizes[usize::from(op)];
        assert_eq!((size(cs::PICK_UP_ITEM), size(cs::USE_ITEM), size(cs::USE_BELT_ITEM)), (13, 13, 13), "the item packets' lengths");
        let moves = [cs::DROP_ITEM, cs::INSERT_ITEM, cs::LIFT_ITEM, cs::EQUIP_ITEM, cs::UNEQUIP_ITEM, cs::SWAP_EQUIPPED, cs::SWAP_GRID_ITEM, cs::BELT_ITEM, cs::UNBELT_ITEM, cs::SWAP_BELT_ITEM];
        assert_eq!(moves.map(size), [5, 17, 5, 9, 3, 9, 17, 9, 5, 9], "the item move packets' lengths");
        assert_eq!((size(cs::IDENTIFY_ITEM), engine.server_packet_sizes[usize::from(d2gs::sc::ITEM_SPELL_READY)]), (9, 8), "identify and its cursor");
    }

    #[test]
    fn games_are_created_joined_and_claimed_by_name() {
        let gs = GameServer::new(test_tables(), Some(test_rules()));
        let id = gs.create("Baal Run", "pw", 2).unwrap();
        assert_eq!(gs.create("baal run", "", 0), Err(CreateError::NameTaken));
        assert_eq!(gs.stage_join("nope", "", character("A", 0, 0)), Err(JoinError::NoSuchGame));
        assert_eq!(gs.stage_join("BAAL RUN", "wrong", character("A", 0, 0)), Err(JoinError::BadPassword));
        let (joined, hash) = gs.stage_join("baal run", "pw", character("Tyrael", 1, 0x20)).unwrap();
        assert_eq!(joined, id);

        let mut logon = GameLogon::parse(&logon_packet(id, hash ^ 1, 1, 0x0E, "Tyrael")).unwrap();
        assert!(gs.claim(&logon).is_none(), "the hash must match");
        logon.game_hash = hash;
        let joined = gs.claim(&logon).expect("staged");
        assert_eq!((joined.character.name.as_str(), joined.difficulty), ("Tyrael", 2));
        assert_eq!((joined.map_seed, joined.spawn), (FALLBACK_MAP_SEED, FALLBACK_SPAWN), "no install: the fallback camp");
        assert!(gs.claim(&logon).is_none(), "a staged join is used once");

        gs.leave(id, "tyrael");
        assert_eq!(gs.stage_join("baal run", "pw", character("A", 0, 0)), Err(JoinError::NoSuchGame), "empty games close");
    }

    #[tokio::test]
    async fn a_client_is_taken_through_the_join_sequence() {
        let gs = Arc::new(GameServer::new(test_tables(), Some(test_rules())));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0x60)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let huffman = &gs.tables.huffman;

        let mut c = TcpStream::connect(addr).await.unwrap();
        let mut greeting = [0u8; 2];
        c.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [0xAF, 0x01], "raw greeting, compression on");

        c.write_all(&logon_packet(id, hash, 4, 0x0E, "TestBan")).await.unwrap();
        let flags = read_frame(&mut c, huffman).await;
        assert_eq!(flags, vec![0x01, 0x00, 0x04, 0x00, 0x30, 0x00, 0x01, 0x01, 0x00], "01 (expansion, ladder) + 00");
        assert_eq!(read_frame(&mut c, huffman).await, vec![0x02]);

        c.write_all(&[cs::ENTER_GAME]).await.unwrap();
        let entered = split_packets(&read_frame(&mut c, huffman).await);
        let packets: Vec<&[u8]> = entered.iter().map(Vec::as_slice).collect();
        let ops: Vec<u8> = packets.iter().map(|p| p[0]).filter(|op| !(0x1D..=0x1F).contains(op)).collect();
        assert_eq!(
            ops,
            vec![0x59, 0xAA, 0x5E, 0x28, 0x29, 0x0B, 0x94, 0x23, 0x23, 0x95, 0x03, 0x53, 0x07, 0x15, 0x7E],
            "the engine's order, one frame"
        );
        assert_eq!(packets[1], d2gs::alignment_state(0, 1, 2), "the player's alignment: good");
        assert_eq!(packets[2], d2gs::quest_states(&[1; d2gs::QUESTS]), "every quest available");
        let stats: Vec<&[u8]> = packets[6..].iter().copied().take_while(|p| (0x1D..=0x1F).contains(&p[0])).collect();
        assert_eq!(stats.len(), 15, "every stat a new character starts with, right after 0x0B");
        assert!(stats.contains(&&[0x1E, stat::MAXHP, 0x00, 50][..]), "max life (20 vit + 30) << 8");
        assert!(stats.contains(&&[0x1D, stat::LEVEL, 1][..]));
        assert_eq!(&packets[0][6..13], b"TestBan", "the character's name in 0x59");
        assert_eq!(&packets[0][22..26], &[0, 0, 0, 0], "0x59 before placement: no position");
        assert_eq!(packets[5], &[0x0B, 0, 1, 0, 0, 0], "then: that unit is yours");
        assert_eq!(packets[6 + 15], d2gs::skill_list(1, &d2_game::skills::COMMON_SKILLS.map(|s| (s as u16, 1))), "the common skills");
        assert_eq!(&packets[6 + 15 + 4][2..6], &FALLBACK_MAP_SEED.to_le_bytes(), "0x03 carries the seed");
        assert_eq!(packets[6 + 15 + 6], d2gs::load_room(1152, 880, 1), "no town: the fallback seed's spawn room");
        assert_eq!(&packets[6 + 15 + 7][6..10], &[0xA6, 0x16, 0x3D, 0x11], "placed on its waypoint (5798, 4413)");
        assert_eq!(read_frame(&mut c, huffman).await, vec![0x04]);

        let mut ping = vec![0u8; 13];
        ping[0] = cs::PING;
        c.write_all(&ping).await.unwrap();
        let pong = read_frame(&mut c, huffman).await;
        assert_eq!((pong[0], pong.len()), (0x8F, 33));

        c.write_all(&[cs::LEAVE_GAME]).await.unwrap();
        assert_eq!(read_frame(&mut c, huffman).await, vec![0x05, 0x06]);
        assert_eq!(read_frame(&mut c, huffman).await, vec![0xB0]);
    }

    /// A made-up town in a row of four rooms: the second holds a torch (object 1, InitFn 8), the
    /// waypoint players start on (object 3), the stash (267) and an NPC who talks (monster 10,
    /// two helmets to pick from); a crate stands in the fourth room, two rooms away.
    fn test_town() -> (GameData, PresetLevel) {
        town_with_npc(10, "guard")
    }

    /// [`test_town`] with its talking NPC as monster `class`, `MonStats.txt` `Id` `id`.
    fn town_with_npc(class: u16, id: &str) -> (GameData, PresetLevel) {
        use d2_data::levels::Levels;
        use d2_data::monsters::{Monsters, COMPONENT_COLUMNS};
        use d2_data::presets::{MonPresets, Objects, PresetMonster};
        use d2_drlg::preset::{PlacedUnit, UnitClass};
        use d2_drlg::Coords;
        use d2_formats::excel::Table;
        let mut rules = test_rules();
        // Rows 0..=267: a torch (1), a crate (2), the waypoint (3) and, at the engine's class
        // id, the stash (267).
        let mut objects = String::from("Name\tInitFn\tPreOperate\tSubClass\tOperateFn\r\n");
        for class in 0..=267 {
            objects += match class {
                1 => "torch\t8\t0\t0\t13\r\n",
                2 => "crate\t0\t0\t0\t0\r\n",
                3 => "waypoint\t17\t0\t64\t23\r\n",
                267 => "bank\t0\t0\t0\t32\r\n",
                _ => "none\t0\t0\t0\t0\r\n",
            };
        }
        let objects = Table::parse(objects.as_bytes());
        let levels = "Id\tAct\tSizeX\tSizeY\tSizeX(N)\tSizeY(N)\tSizeX(H)\tSizeY(H)\tOffsetX\tOffsetY\tDepend\tDrlgType\tLevelType\tWaypoint\r\n\
                      1\t0\t32\t8\t32\t8\t32\t8\t0\t0\t0\t2\t1\t0\r\n\
                      3\t0\t8\t8\t8\t8\t8\t8\t0\t0\t0\t3\t2\t1\r\n";
        rules.set_levels(Levels::from_table(&Table::parse(levels.as_bytes())).unwrap());
        let monstats = Table::parse(format!("Id\thcIdx\tMonStatsEx\tnpc\tinteract\r\n{id}\t{class}\t{id}\t1\t1\r\n").as_bytes());
        let mut ms2 = String::from("Id\tcritter");
        for c in COMPONENT_COLUMNS {
            ms2 += &format!("\t{c}");
        }
        ms2 += &format!("\r\n{id}\t0\tcap,helm\r\n");
        let empty = |col: &str| Table::parse(format!("{col}\r\n").as_bytes());
        let presets = MonPresets::from_tables(&Table::parse(format!("Act\tPlace\r\n1\t{id}\r\n").as_bytes()), &monstats, &empty("Superunique"), &empty("code")).unwrap();
        rules.set_map_tables(presets, Monsters::from_tables(&monstats, &Table::parse(ms2.as_bytes())).unwrap(), Objects::from_table(&objects));
        let room = |x, y| Coords { x, y, w: 8, h: 8 };
        let at = |class, x, y| PlacedUnit { class, x, y, path: Vec::new() };
        let town = PresetLevel {
            level_id: 1,
            area: Coords { x: 1152, y: 888, w: 32, h: 8 },
            map: String::new(),
            rooms: vec![room(1152, 888), room(1160, 888), room(1168, 888), room(1176, 888)],
            units: vec![
                at(UnitClass::Monster(PresetMonster::Class { class: i32::from(class), name: id.into() }), 5815, 4455),
                at(UnitClass::Object(1), 5812, 4444),
                at(UnitClass::Object(2), 5890, 4444),
                at(UnitClass::Object(3), 5805, 4447),
                at(UnitClass::Object(267), 5825, 4460),
            ],
        };
        (rules, town)
    }

    /// Split a run of server packets by the sizes the join and the room stream use.
    fn split_packets(mut rest: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(&op) = rest.first() {
            let size = match op {
                0x59 => 26,
                0x0B | 0x07 | 0x08 | 0x0A | 0x1F => 6,
                0x04 => 1,
                0x1D => 3,
                0x1E => 4,
                0x23 | 0x95 | 0x0D => 13,
                0x03 => 12,
                0x53 | 0x6D => 10,
                0x15 => 11,
                0x7E => 5,
                0x51 => 14,
                0xAA => usize::from(rest[6]),
                0xAC => usize::from(rest[12]),
                0x5E => 38,
                0x28 => 103,
                0x29 => 97,
                0x8F => 33,
                0x27 => 40,
                0x63 => 21,
                0x77 => 2,
                0x94 => 6 + 3 * usize::from(rest[1]),
                other => panic!("unexpected opcode {other:#04x}"),
            };
            out.push(rest[..size].to_vec());
            rest = &rest[size..];
        }
        out
    }

    #[test]
    fn a_walker_heads_straight_for_its_spot_and_stops_there() {
        let mut w = Walker { x: 0.0, y: 0.0, target: None, speed: 0.0, room: None, view: Vec::new() };
        w.go(30.0, 40.0, 10.0);
        w.step(Duration::from_secs(1));
        assert!((w.x - 6.0).abs() < 1e-9 && (w.y - 8.0).abs() < 1e-9, "10 subtiles along the line: {w:?}");
        assert!(w.moving());
        w.step(Duration::from_secs(10));
        assert_eq!((w.x, w.y, w.moving()), (30.0, 40.0, false), "arrives, not overshoots");
        assert!((RUN_SPEED - 14.0625).abs() < 1e-9 && (WALK_SPEED - 9.375).abs() < 1e-9);
    }

    /// Running east through the made-up town: entering the third room loads the fourth (with
    /// its crate); entering the fourth drops the first, now two rooms away — the second, one
    /// room away and full of units, stays.
    #[tokio::test]
    async fn running_across_rooms_loads_what_comes_near_and_drops_what_is_left_behind() {
        let (rules, town) = test_town();
        let gs = Arc::new(GameServer::new(test_tables(), Some(rules)).with_town(town).with_speed_scale(20.0));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let huffman = &gs.tables.huffman;
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.read_exact(&mut [0u8; 2]).await.unwrap();
        c.write_all(&logon_packet(id, hash, 4, 0x0E, "TestBan")).await.unwrap();
        read_frame(&mut c, huffman).await;
        read_frame(&mut c, huffman).await;
        c.write_all(&[cs::ENTER_GAME]).await.unwrap();
        let mut joined = Vec::new();
        while joined.last() != Some(&0x04) {
            joined.extend(read_frame(&mut c, huffman).await);
        }

        let mut run = vec![cs::RUN_TO_LOCATION];
        run.extend_from_slice(&5900u16.to_le_bytes());
        run.extend_from_slice(&4450u16.to_le_bytes());
        c.write_all(&run).await.unwrap();
        let mut seen = Vec::new();
        while !seen.contains(&d2gs::unload_room(1152, 888, 1)) {
            let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut c, huffman)).await.expect("rooms follow the run");
            seen.extend(split_packets(&frame));
        }
        assert_eq!(
            seen,
            vec![d2gs::load_room(1176, 888, 1), d2gs::assign_object(4, 2, 5890, 4444, 0, 0), d2gs::unload_room(1152, 888, 1)]
        );

        // Back to the first room: it and its neighbours come back; the fourth, two rooms off,
        // goes with its crate.
        let mut back = vec![cs::RUN_TO_LOCATION];
        back.extend_from_slice(&5760u16.to_le_bytes());
        back.extend_from_slice(&4450u16.to_le_bytes());
        c.write_all(&back).await.unwrap();
        let mut seen = Vec::new();
        while !seen.contains(&d2gs::unload_room(1176, 888, 1)) {
            let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut c, huffman)).await.expect("rooms follow the run back");
            seen.extend(split_packets(&frame));
        }
        assert_eq!(seen, vec![d2gs::load_room(1152, 888, 1), d2gs::remove_unit(2, 4), d2gs::unload_room(1176, 888, 1)]);
    }

    /// Talking to the NPC opens its dialog, operating the stash opens it, and the waypoint opens
    /// its menu with the camp's waypoint learnt; a crate nobody can use gets no answer.
    #[tokio::test]
    async fn clicking_an_npc_the_stash_or_the_waypoint_gets_the_engines_answer() {
        let (rules, town) = test_town();
        let gs = Arc::new(GameServer::new(test_tables(), Some(rules)).with_town(town));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let huffman = &gs.tables.huffman;
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.read_exact(&mut [0u8; 2]).await.unwrap();
        c.write_all(&logon_packet(id, hash, 4, 0x0E, "TestBan")).await.unwrap();
        read_frame(&mut c, huffman).await;
        read_frame(&mut c, huffman).await;
        c.write_all(&[cs::ENTER_GAME]).await.unwrap();
        let mut joined = Vec::new();
        while joined.last() != Some(&0x04) {
            joined.extend(read_frame(&mut c, huffman).await);
        }
        let interact = |kind: u32, guid: u32| {
            let mut p = vec![cs::INTERACT];
            p.extend_from_slice(&kind.to_le_bytes());
            p.extend_from_slice(&guid.to_le_bytes());
            p
        };
        // The guard is monster 1; objects: torch 1, waypoint 2, stash 3.
        c.write_all(&interact(1, 1)).await.unwrap();
        assert_eq!(
            split_packets(&read_frame(&mut c, huffman).await),
            vec![
                d2gs::npc_no_quest_messages(1, 1),
                d2gs::game_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]),
                d2gs::npc_dialog_quest_flags(1, &[0; d2gs::QUEST_FLAG_BYTES]),
            ]
        );
        c.write_all(&interact(2, 3)).await.unwrap();
        assert_eq!(read_frame(&mut c, huffman).await, d2gs::ui_action(0x10));
        c.write_all(&interact(2, 1)).await.unwrap(); // the torch: no answer
        c.write_all(&interact(2, 2)).await.unwrap();
        let mut camp = [0u8; d2gs::WAYPOINT_FLAG_BYTES];
        camp[0] = 1;
        assert_eq!(read_frame(&mut c, huffman).await, d2gs::waypoint_menu(2, &camp));
    }

    /// A waypoint out in the wilderness starts inactive: the first click learns it and turns it
    /// on (`0E`, mode 1), the next opens the menu with its level ticked, from which the camp's
    /// waypoint can be travelled to.
    #[test]
    fn a_wild_waypoint_turns_on_before_it_opens_its_menu() {
        use d2_drlg::preset::{PlacedUnit, UnitClass};
        use d2_drlg::world::WorldLevel;
        use d2_drlg::Coords;
        let (rules, town) = test_town();
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("probe", "", 0).unwrap();
        let plains = Coords { x: 1152, y: 896, w: 8, h: 8 };
        {
            let mut g = gs.lock();
            let game = g.by_id.get_mut(&id).unwrap();
            let mut levels = game.world.take().unwrap().levels().to_vec();
            let waypoint = PlacedUnit { class: UnitClass::Object(3), x: 5779, y: 4499, path: Vec::new() };
            levels.push(WorldLevel { id: 3, area: plains, rooms: vec![plains], units: vec![waypoint], pieces: vec![0], collision: Vec::new(), warps: Vec::new() });
            game.world = Some(World::from_levels(levels));
        }
        let room = RoomId { level: 3, index: 0 };
        let packets = gs.view_change(id, &[], &[room]);
        assert_eq!(packets, vec![d2gs::load_room(1152, 896, 3), d2gs::assign_object(1, 3, 5779, 4499, 0, 0)], "inactive");
        let mut learned = [0u8; d2gs::WAYPOINT_FLAG_BYTES];
        learned[0] = 1;
        assert_eq!(gs.interact(id, 2, 1, &mut learned), vec![d2gs::object_state(1, true, 1)]);
        assert_eq!(learned[0], 0b11, "the camp's and Cold Plains' bits");
        assert_eq!(gs.interact(id, 2, 1, &mut learned), vec![d2gs::waypoint_menu(1, &learned)]);
        assert_eq!(gs.waypoint_travel(id, 1, 1, &learned), Some((5808, 4448)), "to the camp's waypoint tile, at (3, 3)");
        assert_eq!(gs.waypoint_travel(id, 1, 3, &learned), None, "already there");
        assert_eq!(gs.waypoint_travel(id, 1, 1, &[0; d2gs::WAYPOINT_FLAG_BYTES]), None, "not learned");
        assert_eq!(gs.waypoint_travel(id, 9, 1, &learned), None, "no such waypoint");
        assert_eq!(gs.view_change(id, &[room], &[]), vec![d2gs::remove_unit(2, 1), d2gs::unload_room(1152, 896, 3)]);
    }

    /// The act's clock runs from the game's creation: a joined client is told its time of day
    /// again once it has moved more than 16 degrees (here at one tick a degree, 17 frames).
    #[tokio::test]
    async fn a_joined_client_is_told_as_the_day_moves_on() {
        use d2_data::engine::DayPeriod;
        let p = |angle, phase| DayPeriod { angle, phase };
        let periods = [p(320, 3), p(340, 3), p(0, 0), p(160, 1), p(180, 1), p(200, 2)];
        let gs = Arc::new(GameServer::new(test_tables(), Some(test_rules())).with_day(periods, 1));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let huffman = &gs.tables.huffman;
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.read_exact(&mut [0u8; 2]).await.unwrap();
        c.write_all(&logon_packet(id, hash, 4, 0x0E, "TestBan")).await.unwrap();
        read_frame(&mut c, huffman).await;
        read_frame(&mut c, huffman).await;
        c.write_all(&[cs::ENTER_GAME]).await.unwrap();
        let joined = split_packets(&read_frame(&mut c, huffman).await);
        let at_join = joined.iter().find(|p| p[0] == 0x53).unwrap().clone();
        assert_eq!(read_frame(&mut c, huffman).await, vec![0x04]);
        let update = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut c, huffman)).await.expect("a clock report");
        assert_eq!((update[0], &update[1..5]), (0x53, &2u32.to_le_bytes()[..]), "still day");
        let ticks = |p: &[u8]| u32::from_le_bytes([p[5], p[6], p[7], p[8]]);
        assert!(ticks(&update) >= 17 && ticks(&update) > ticks(&at_join), "{at_join:02x?} then {update:02x?}");
    }

    #[tokio::test]
    async fn the_rooms_around_the_spawn_come_with_their_units_before_the_player_is_placed() {
        let (rules, town) = test_town();
        let gs = Arc::new(GameServer::new(test_tables(), Some(rules)).with_town(town));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let huffman = &gs.tables.huffman;
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.read_exact(&mut [0u8; 2]).await.unwrap();
        c.write_all(&logon_packet(id, hash, 4, 0x0E, "TestBan")).await.unwrap();
        read_frame(&mut c, huffman).await;
        read_frame(&mut c, huffman).await;
        c.write_all(&[cs::ENTER_GAME]).await.unwrap();

        let mut plain = Vec::new();
        while plain.last() != Some(&0x04) || plain.len() < 2 {
            plain.extend(read_frame(&mut c, huffman).await);
        }
        let ops: Vec<(u8, Vec<u8>)> = split_packets(&plain).into_iter().map(|p| (p[0], p)).collect();
        let tail: Vec<u8> = ops.iter().map(|(op, _)| *op).skip_while(|&op| op != 0x53).collect();
        assert_eq!(
            tail,
            vec![0x53, 0x07, 0x07, 0x07, 0x51, 0x51, 0x51, 0xAC, 0xAA, 0x6D, 0x07, 0x15, 0x7E, 0x04],
            "spawn room, then its three near rooms — units after their own room — then placement"
        );
        let body = |op: u8| ops.iter().find(|(o, _)| *o == op).map(|(_, b)| b.clone()).unwrap();
        assert_eq!(body(0x15), d2gs::reassign_player(0, 1, 5808, 4448, 1), "placed on the waypoint's tile");
        assert_eq!(body(0x51), d2gs::assign_object(1, 1, 5812, 4444, 2, 0), "the torch, lit");
        let npc = body(0xAC);
        assert_eq!((&npc[1..5], &npc[5..7], npc[11]), (&1u32.to_le_bytes()[..], &10u16.to_le_bytes()[..], 0x80));
        assert_eq!(body(0x6D), d2gs::monster_standing(1, 5815, 4455, 0x80));
        let rooms: Vec<Vec<u8>> = ops.iter().filter(|(op, _)| *op == 0x07).map(|(_, b)| b.clone()).collect();
        assert_eq!(rooms[0], d2gs::load_room(1160, 888, 1), "PlacePlayerInAct's own 07 first");
        assert!(!rooms.contains(&d2gs::load_room(1176, 888, 1)), "two rooms away is not near");
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`, holding `Game.exe` and the MPQs): each
    /// game draws its own map seed, the camps vary, and players start on each camp's waypoint.
    #[test]
    fn with_a_real_install_each_game_gets_its_own_camp() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let gs = GameServer::new(test_tables(), Some(GameData::load(&dir).unwrap())).with_engine(engine);
        let mut maps = std::collections::BTreeSet::new();
        for i in 0..32 {
            let id = gs.create(&format!("camp {i}"), "", 0).unwrap();
            let g = gs.lock();
            let game = &g.by_id[&id];
            let town = game.town.as_ref().expect("built");
            assert_ne!(game.map_seed, FALLBACK_MAP_SEED);
            assert!(town.room_index_at(game.spawn.0.into(), game.spawn.1.into()).is_some(), "{:?}", game.spawn);
            maps.insert(town.map.clone());
        }
        assert!(maps.len() > 1, "32 games, one camp: {maps:?}");
    }

    /// With the operator's install: in every game the Cold Plains waypoint room sends its waypoint,
    /// inactive, when it loads.
    #[test]
    fn with_a_real_install_the_cold_plains_waypoint_is_sent_with_its_room() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let gs = GameServer::new(test_tables(), Some(GameData::load(&dir).unwrap())).with_engine(engine);
        for i in 0..8 {
            let id = gs.create(&format!("plains {i}"), "", 0).unwrap();
            let (room, unit) = {
                let g = gs.lock();
                let world = g.by_id[&id].world.as_ref().unwrap();
                assert!(world.unbuilt().is_empty(), "{:?}", world.unbuilt());
                let plains = world.levels().iter().find(|l| l.id == 3).unwrap();
                let unit = plains.units.iter().find(|u| matches!(u.class, d2_drlg::preset::UnitClass::Object(119))).cloned().unwrap();
                (world.room_at(unit.x, unit.y).unwrap(), unit)
            };
            let packets = gs.view_change(id, &[], &[room]);
            assert!(
                packets.iter().any(|p| p[0] == 0x51 && p[6..8] == 119u16.to_le_bytes() && p[8..10] == (unit.x as u16).to_le_bytes() && p[12] == 0),
                "game {i}: {packets:02x?}"
            );
        }
    }

    /// With the operator's install: the admin map describes a game — its levels, Blood Moor's
    /// collision map with walls, open ground and the Den of Evil's mouth, and the monsters of
    /// the rooms a player has walked into. With `BNETCC_D2_MAP_DUMP` set to a directory, the
    /// JSON the page reads is written there for a browser preview.
    #[test]
    fn with_a_real_install_the_admin_map_describes_a_game() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let gs = GameServer::new(test_tables(), Some(GameData::load(&dir).unwrap())).with_engine(engine);
        let id = gs.create("map", "", 0).unwrap();
        let moor = gs.lock().by_id[&id].world.as_ref().unwrap().levels().iter().find(|l| l.id == 2).unwrap().area;
        let (x, y) = (f64::from((moor.x + moor.w / 2) * 5), f64::from((moor.y + moor.h / 2) * 5));
        gs.set_position(id, "Walker", x, y);
        let (_, near) = gs.near(id, x, y).unwrap();
        gs.view_change(id, &[], &near);

        let games = gs.map_games();
        assert_eq!(games.len(), 1);
        assert_eq!((games[0].players[0].name.as_str(), games[0].players[0].level), ("Walker", Some(2)));
        assert!(games[0].levels.iter().any(|l| l.id == 2 && l.name == "Blood Moor"), "{:?}", games[0].levels);

        let level = gs.map_level(id, 2).unwrap();
        let raw: Vec<u8> = {
            // Undo the base64 to check the grid.
            let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = Vec::new();
            for chunk in level.grid.as_bytes().chunks(4) {
                let n = chunk.iter().filter(|&&c| c != b'=').fold((0u32, 0u32), |(n, k), &c| (n | (alphabet.iter().position(|&a| a == c).unwrap() as u32) << (18 - 6 * k), k + 1));
                for i in 0..n.1.saturating_sub(1) {
                    out.push((n.0 >> (16 - 8 * i)) as u8);
                }
            }
            out
        };
        assert_eq!(raw.len(), (moor.w * 5 * moor.h * 5) as usize);
        let walls = raw.iter().filter(|&&c| c != map::NO_MAP && c & 1 != 0).count();
        let open = raw.iter().filter(|&&c| c != map::NO_MAP && c & 1 == 0).count();
        assert!(walls > 1000 && open > 10_000, "walls {walls}, open {open}");
        assert!(level.entrances.iter().any(|e| e.name == "DOE Entrance"), "{:?}", level.entrances);

        let live = gs.map_live(id, 2).unwrap();
        assert!(live.populated_rooms > 0 && live.populated_rooms <= live.rooms);
        if let Ok(out) = std::env::var("BNETCC_D2_MAP_DUMP") {
            let out = Path::new(&out);
            std::fs::create_dir_all(out.join("d2")).unwrap();
            std::fs::write(out.join("d2/games.json"), serde_json::to_string(&games).unwrap()).unwrap();
            std::fs::write(out.join("d2/level.json"), serde_json::to_string(&level).unwrap()).unwrap();
            std::fs::write(out.join("d2/live.json"), serde_json::to_string(&live).unwrap()).unwrap();
        }
    }

    /// Run a game's fight forward by `frames` server frames at once, as if that much time had
    /// passed, and return what `name`'s client is sent.
    fn fight_for(gs: &GameServer, id: u16, name: &str, frames: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for _ in 0..frames {
            {
                let mut g = gs.lock();
                let game = g.by_id.get_mut(&id).unwrap();
                game.created = game.created.checked_sub(SERVER_FRAME).unwrap();
            }
            out.extend(gs.battle_step(id, name, battle::Motion::Standing));
        }
        out
    }

    /// A hostile monster out in a made-up Blood Moor room joins the fight when its room loads;
    /// swinging at it until it dies sends the player its death frames — dying, then the corpse a
    /// death animation later — the experience and its gold; its room loaded again shows the
    /// corpse and the pile, which the player picks up once.
    #[test]
    fn a_monster_hit_until_it_dies_pays_out_and_stays_dead() {
        use d2_data::monsters::Monsters;
        use d2_data::presets::MonPresets;
        use d2_drlg::preset::{PlacedUnit, UnitClass};
        use d2_drlg::world::WorldLevel;
        use d2_drlg::Coords;
        use d2_formats::excel::Table;
        let (mut rules, town) = test_town();
        let monstats = Table::parse(
            b"Id\thcIdx\tMonStatsEx\tnpc\tinteract\tCode\tLevel\tminHP\tmaxHP\tExp\tA1MinD\tA1MaxD\tA1TH\taidel\tTreasureClass1\r\n\
              guard\t10\tguard\t1\t1\tGU\t1\t1\t1\t0\t0\t0\t0\t5\t\r\n\
              brute\t11\tguard\t0\t0\tXX\t1\t1\t1\t50\t1\t1\t1\t5\tPurse\r\n",
        );
        let treasure = Table::parse(b"Treasure Class\tgroup\tlevel\tPicks\tNoDrop\tItem1\tProb1\r\nPurse\t\t\t-1\t\tgld\t1\r\n");
        rules.set_treasure(d2_data::treasure::TreasureClasses::from_table(&treasure).unwrap());
        let objects = rules.objects().clone();
        rules.set_map_tables(MonPresets::default(), Monsters::from_tables(&monstats, &Table::parse(b"Id\r\nguard\r\n")).unwrap(), objects);
        let monlvl = Table::parse(b"Level\tAC\tTH\tHP\tDM\tXP\r\n0\t100\t100\t100\t100\t100\r\n1\t100\t100\t100\t100\t100\r\n");
        rules.set_combat_tables(d2_data::monlvl::MonLvls::from_table(&monlvl).unwrap(), Default::default());
        let stats = rules.new_character_stats(4).unwrap();
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("probe", "", 0).unwrap();
        let moor = Coords { x: 1152, y: 896, w: 8, h: 8 };
        {
            let mut g = gs.lock();
            let game = g.by_id.get_mut(&id).unwrap();
            let mut levels = game.world.take().unwrap().levels().to_vec();
            let brute = PlacedUnit { class: UnitClass::Monster(d2_data::presets::PresetMonster::Class { class: 11, name: "brute".into() }), x: 5780, y: 4500, path: Vec::new() };
            levels.push(WorldLevel { id: 2, area: moor, rooms: vec![moor], units: vec![brute], pieces: vec![0], collision: Vec::new(), warps: Vec::new() });
            game.world = Some(World::from_levels(levels));
        }
        let room = RoomId { level: 2, index: 0 };
        gs.join_battle(id, "Hero", 4, &stats);
        gs.set_position(id, "Hero", 5782.0, 4500.0);
        gs.set_view(id, "Hero", &[room]);
        let loaded = gs.view_change(id, &[], &[room]);
        let guid = loaded.iter().find(|p| p[0] == 0xAC).map(|p| u32::from_le_bytes([p[1], p[2], p[3], p[4]])).expect("the brute is sent");
        assert!(gs.battle_monster(id, guid).is_some(), "and fights");
        let mut seen = Vec::new();
        for _ in 0..50 {
            if gs.battle_monster(id, guid).is_none() {
                break;
            }
            gs.player_attack(id, "Hero", guid);
            seen.extend(fight_for(&gs, id, "Hero", 20));
        }
        seen.extend(fight_for(&gs, id, "Hero", 40));
        let dying = seen.iter().position(|p| *p == d2gs::monster_reaction(guid, battle::reaction::DYING, 5780, 4500, 0, true)).expect("dying");
        let dead = seen.iter().position(|p| *p == d2gs::monster_reaction(guid, battle::reaction::DEAD, 5780, 4500, 0, false)).expect("dead");
        assert!(dying < dead);
        assert!(seen.contains(&d2gs::experience(0, 50)), "{seen:02x?}");
        let drop = seen.iter().find(|p| p[0] == 0x9C).expect("its gold falls");
        let pile = u32::from_le_bytes([drop[4], drop[5], drop[6], drop[7]]);
        assert_eq!((drop[1], drop[9] & 0x20), (0, 0x20), "added to the ground, falling");
        let left = gs.view_change(id, &[room], &[]);
        assert!(left.contains(&d2gs::remove_unit(unit_type::ITEM, pile)));
        let again = gs.view_change(id, &[], &[room]);
        let corpse = again.iter().find(|p| p[0] == 0xAC).unwrap();
        assert_eq!(corpse[11], 0, "no life");
        assert!(!again.iter().any(|p| p[0] == 0x6D), "a corpse is not stood up");
        assert!(again.iter().any(|p| p[0] == 0x9C && p[4..8] == pile.to_le_bytes() && p[9] & 0x20 == 0), "the pile lies there");
        assert_eq!(gs.unit_position(id, u32::from(unit_type::ITEM), pile), Some((5782, 4503)), "two right and three down");
        let got = gs.pick_up(id, "Hero", pile, false);
        assert_eq!(got[0], d2gs::remove_unit(unit_type::ITEM, pile));
        let gold = gs.lock().by_id[&id].battle.player_stats("Hero").unwrap().iter().find(|s| s.0 == stat::GOLD).unwrap().1;
        assert!((1..=5).contains(&gold), "level 1: 1 + rand(5): {gold}");
        assert_eq!(got[1], [0x19, gold as u8], "a small gain");
        assert!(gs.pick_up(id, "Hero", pile, false).is_empty(), "once");
        gs.view_change(id, &[room], &[]);
        assert!(!gs.view_change(id, &[], &[room]).iter().any(|p| p[0] == 0x9C), "gone for good");
    }

    /// A drop takes the spot two right and three down from its unit, or the nearest free one ring
    /// by ring (least `|dx| + |dy|`): not a wall, not another pile, not behind a wall.
    #[test]
    fn drops_find_the_nearest_free_spot() {
        use d2_drlg::world::WorldLevel;
        use d2_drlg::Coords;
        let area = Coords { x: 0, y: 0, w: 4, h: 4 };
        // 20 × 20 subtiles, open but for a wall along x = 12 from y = 0 to 19.
        let world = World::from_levels(vec![WorldLevel { id: 2, area, rooms: vec![area], units: Vec::new(), pieces: vec![0], collision: Vec::new(), warps: Vec::new() }]);
        let wall = |x: i32, _y: i32| x == 12;
        let with_wall = |taken: &dyn Fn(i32, i32) -> bool, from| {
            let open_world = |x: i32, y: i32| (0..20).contains(&x) && (0..20).contains(&y);
            let taken = |x: i32, y: i32| taken(x, y) || wall(x, y) || !open_world(x, y);
            drop_spot(&world, &taken, from).map(|(_, x, y)| (x, y))
        };
        assert_eq!(with_wall(&|_, _| false, (5, 5)), Some((7, 8)));
        // The start taken: ring 1's side edges come first, row by row, and the least |dx| + |dy|
        // wins; a tie keeps the first found.
        assert_eq!(with_wall(&|x, y| (x, y) == (7, 8), (5, 5)), Some((6, 8)));
        let busy = |x: i32, y: i32| (6..=8).contains(&x) && (7..=9).contains(&y);
        assert_eq!(with_wall(&busy, (5, 5)), Some((5, 8)), "ring 2: the left edge's middle ties straight up, and comes first");
        // From beside the wall, the start (12, 8) is the wall: the nearest free spot is on this side.
        assert_eq!(with_wall(&|_, _| false, (10, 5)), Some((11, 8)));
    }

    /// A purse holds 10,000 gold a level: picking up more takes what fits and drops the rest as a
    /// new pile from the player, falling, for everyone near.
    #[test]
    fn gold_beyond_the_purse_is_dropped_again_at_the_player() {
        let (rules, town) = test_town();
        let mut stats = rules.new_character_stats(4).unwrap();
        stats.push((stat::GOLD, 9_990));
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("probe", "", 0).unwrap();
        let (x, y) = gs.lock().by_id[&id].spawn;
        let room = gs.lock().by_id[&id].world.as_ref().unwrap().room_at(i32::from(x), i32::from(y)).unwrap();
        for name in ["Hero", "Friend"] {
            gs.join_battle(id, name, 4, &stats);
            gs.set_position(id, name, f64::from(x), f64::from(y));
            gs.set_view(id, name, &[room]);
        }
        gs.view_change(id, &[], &[room]);
        gs.lock().by_id.get_mut(&id).unwrap().ground.insert(77, GroundItem { room, x: x + 3, y, what: Loot::Gold(25) });
        let got = gs.pick_up(id, "Hero", 77, false);
        assert_eq!(got[..2], [d2gs::remove_unit(unit_type::ITEM, 77), vec![0x19, 10]], "10 fit");
        let (left, pile) = gs.lock().by_id[&id].ground.iter().map(|(&g, p)| (g, p.clone())).next().unwrap();
        assert_eq!((pile.x, pile.y, pile.what), (x + 2, y + 3, Loot::Gold(15)), "placed as drops are");
        assert_eq!(got[2], d2gs::ground_gold(left, x + 2, y + 3, 15, true, 101));
        let friend = gs.lock().by_id.get_mut(&id).unwrap().outgoing.remove("Friend").unwrap();
        assert_eq!(friend, [got[0].clone(), got[2].clone()], "the one near sees both");
        assert!(gs.pick_up(id, "Hero", left, false).is_empty(), "a full purse takes nothing");
    }

    /// Made-up item tables in the real shapes: Minor Healing and Mana Potions (belted), a
    /// Rejuvenation Potion, a Chipped Ruby, a Skeleton Key (a stack) and a Hand Axe.
    fn item_rules(mut rules: GameData) -> GameData {
        use d2_data::items::Items;
        use d2_formats::excel::Table;
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tBeltable\tBodyLoc1\tBodyLoc2\r\nPotion\tpoti\t\t1\t\t\r\nHealing Potion\thpot\tpoti\t1\t\t\r\nGem\tgem\t\t0\t\t\r\nKey\tkey\t\t0\t\t\r\nWeapon\tweap\t\t0\t\t\r\nAxe\taxe\tweap\t0\trarm\tlarm\r\n",
        );
        let weapons = Table::parse(b"name\tcode\ttype\tcompactsave\tcomponent\tinvwidth\tinvheight\tdurability\tmindam\tmaxdam\r\nHand Axe\thax\taxe\t0\t5\t1\t3\t28\t3\t6\r\n");
        let armor = Table::parse(b"name\tcode\ttype\r\n");
        let misc = Table::parse(
            b"name\tcode\ttype\tcomponent\tinvwidth\tinvheight\tcompactsave\tautobelt\tstackable\tuseable\tpSpell\tlen\tstat1\tcalc1\tstat2\tcalc2\r\n\
              Minor Healing Potion\thp1\thpot\t16\t1\t1\t1\t1\t0\t1\t3\t192\thpregen\t30\t\t\r\n\
              Rejuv Potion\trvs\tpoti\t16\t1\t1\t1\t1\t0\t1\t5\t\thitpoints\t35\tmana\t35\r\n\
              Chipped Ruby\tgcr\tgem\t16\t1\t1\t1\t0\t0\t0\t\t\t\t\t\t\r\n\
              Skeleton Key\tkey\tkey\t16\t1\t1\t0\t0\t1\t0\t\t\t\t\t\t\r\n",
        );
        rules.set_items(Items::from_tables(&itemtypes, &weapons, &armor, &misc).unwrap(), Vec::new());
        let stats = d2_data::item_stats::ItemStats::from_table(&Table::parse(
            b"Stat\tID\tSave Bits\tSave Add\tSave Param Bits\tValShift\r\narmorclass\t31\t11\t10\t\t\r\ndurability\t72\t8\t0\t\t\r\nmaxdurability\t73\t8\t0\t\t\r\ntohit\t19\t10\t0\t\t\r\nitem_numsockets\t194\t4\t0\t\t\r\n",
        ))
        .unwrap();
        rules.set_item_rules(stats, d2_data::item_stats::ItemRatios::default());
        rules
    }

    /// An identify scroll used turns the cursor (`0x3F`); pointed at an unidentified axe it is
    /// used up and the axe comes back identified, its stats and all.
    #[test]
    fn a_scroll_identifies_an_item() {
        let (rules, town) = test_town();
        let mut rules = item_rules(rules);
        let misc = d2_formats::excel::Table::parse(b"name\tcode\ttype\tcompactsave\tcomponent\tuseable\r\nScroll of Identify\tisc\tgem\t1\t16\t1\r\n");
        let itemtypes = d2_formats::excel::Table::parse(
            b"ItemType\tCode\tEquiv1\tBeltable\tBodyLoc1\tBodyLoc2\r\nGem\tgem\t\t0\t\t\r\nWeapon\tweap\t\t0\t\t\r\nAxe\taxe\tweap\t0\trarm\tlarm\r\n",
        );
        let weapons = d2_formats::excel::Table::parse(b"name\tcode\ttype\tcompactsave\tcomponent\tinvwidth\tinvheight\tdurability\r\nHand Axe\thax\taxe\t0\t5\t1\t3\t28\r\n");
        let empty = d2_formats::excel::Table::parse(b"name\tcode\ttype\r\n");
        rules.set_items(d2_data::items::Items::from_tables(&itemtypes, &weapons, &empty, &misc).unwrap(), Vec::new());
        let (hax, isc) = (rules.items().class_of(b"hax ").unwrap(), rules.items().class_of(b"isc ").unwrap());
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("sage", "", 0).unwrap();
        let p = Player::new(id, character("Hero", 0, 0x20), 0, 0, (0, 0));
        let stats = p.join_stats(gs.rules.as_ref().unwrap()).unwrap();
        gs.join_battle(id, "Hero", 0, &stats);
        gs.join_items(&p);
        {
            let mut g = gs.lock();
            let inv = &mut g.by_id.get_mut(&id).unwrap().carried.get_mut("Hero").unwrap().inventory;
            let mut axe = Item::new(*b"hax ", 101, 9, Location::Stored { col: 0, row: 0, page: 0 });
            axe.quality = item_bits::Quality::Magic { prefix: 0, suffix: 1 };
            axe.flags &= !item_bits::flags::IDENTIFIED;
            axe.stats = vec![item_bits::ItemStat { id: 19, param: 0, value: 12 }];
            assert!(inv.insert(Held { guid: 50, class: hax, size: (1, 3), place: Place::Grid { col: 0, row: 0 }, item: axe }));
            assert!(inv.insert(Held { guid: 51, class: isc, size: (1, 1), place: Place::Grid { col: 5, row: 0 }, item: *simple_item(b"isc ", 101) }));
        }
        assert!(gs.identify_item(id, "Hero", 50, 51).is_empty(), "not made ready");
        assert_eq!(gs.use_item(id, "Hero", 51, false), [d2gs::item_spell_ready(0, 51, 218)]);
        let done = gs.identify_item(id, "Hero", 50, 51);
        assert_eq!((done.len(), done[0][1], done[1][0], done[1][1]), (2, item_action::REMOVE_FROM_CONTAINER, 0x9D, item_action::UPDATE));
        let rules = gs.rules.as_ref().unwrap();
        let (axe, _) = item_bits::read(&done[1][13..], rules.items(), rules.item_stats(), item_bits::Target::Network).unwrap();
        assert!(axe.identified());
        assert_eq!(axe.stats, [item_bits::ItemStat { id: 19, param: 0, value: 12 }], "the stats go out once identified");
        assert!(gs.identify_item(id, "Hero", 50, 51).is_empty(), "once");
    }

    /// Akara's trade window opens with her stock on its pages; a staff bought costs her price,
    /// leaves her stock and lands in the inventory, and cannot be bought twice; potions, always in
    /// stock, fill the belt and one more goes to the inventory; the staff sold back pays half its
    /// cost and joins her stock again; nobody trades with a closed window.
    #[test]
    fn a_vendor_sells_and_buys_back() {
        use d2_formats::excel::Table;
        let (mut rules, town) = town_with_npc(148, "akara");
        let itemtypes = Table::parse(
            b"ItemType\tCode\tEquiv1\tBeltable\tStorePage\r\nWeapon\tweap\t\t0\tweap\r\nStaff\tstaf\tweap\t0\tweap\r\nPotion\tpoti\t\t1\tmisc\r\n",
        );
        let weapons = Table::parse(
            b"name\tcode\ttype\tlevel\tspawnable\tcost\tcomponent\tinvwidth\tinvheight\tdurability\tAkaraMin\tAkaraMax\r\nShort Staff\tsst\tstaf\t1\t1\t168\t5\t1\t3\t20\t1\t1\r\n",
        );
        let misc = Table::parse(
            b"name\tcode\ttype\tlevel\tspawnable\tcost\tcomponent\tinvwidth\tinvheight\tcompactsave\tautobelt\tPermStoreItem\tAkaraMax\r\nMinor Healing Potion\thp1\tpoti\t1\t1\t30\t16\t1\t1\t1\t1\t1\t1\r\n",
        );
        rules.set_items(d2_data::items::Items::from_tables(&itemtypes, &weapons, &Table::parse(b"name\tcode\ttype\r\n"), &misc).unwrap(), Vec::new());
        let stats = Table::parse(b"Stat\tID\tSave Bits\tSave Add\tSave Param Bits\tValShift\r\ndurability\t72\t8\t0\t\t\r\nmaxdurability\t73\t8\t0\t\t\r\n");
        rules.set_item_rules(d2_data::item_stats::ItemStats::from_table(&stats).unwrap(), d2_data::item_stats::ItemRatios::default());
        let npc = Table::parse(b"npc\tbuy mult\tsell mult\trep mult\tmax buy\tmax buy (N)\tmax buy (H)\r\nakara\t512\t1024\t128\t5000\t30000\t35000\r\n");
        let trades = d2_data::trade::NpcTrades::from_table(&npc, rules.monsters());
        rules.set_trade_tables(trades, Vec::new());
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("market", "", 0).unwrap();
        let p = Player::new(id, character("Hero", 0, 0x20), 0, 0, (0, 0));
        let stats = p.join_stats(gs.rules.as_ref().unwrap()).unwrap();
        gs.join_battle(id, "Hero", 0, &stats);
        gs.join_items(&p);
        gs.lock().by_id.get_mut(&id).unwrap().battle.pick_up_gold("Hero", 1000);
        gs.view_change(id, &[], &[RoomId { level: 1, index: 1 }]);
        let rules = gs.rules.clone().unwrap();
        let read = |packet: &[u8]| item_bits::read(&packet[8..], rules.items(), rules.item_stats(), item_bits::Target::Network).unwrap().0;
        let guid_of = |packet: &[u8]| u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let npc = 1;
        assert_eq!(gs.buy(id, "Hero", npc, 1, 0), [d2gs::npc_transaction(0, d2gs::transaction::CANNOT, u32::MAX, 1000)], "the window is not open");

        let window = gs.npc_action(id, "Hero", 1, npc);
        assert_eq!(window.len(), 2, "one staff, one potion");
        assert!(window.iter().all(|w| (w[0], w[1]) == (0x9C, item_action::ADD_TO_STORE)));
        let staff = window.iter().find(|w| &read(w).code == b"sst ").unwrap();
        assert!(matches!(read(staff).location, Location::Stored { page: 1, .. }), "the weapons page");
        let potion = window.iter().find(|w| &read(w).code == b"hp1 ").unwrap();
        assert!(matches!(read(potion).location, Location::Stored { page: 3, .. }), "the misc page");
        let (staff, potion) = (guid_of(staff), guid_of(potion));
        assert_eq!(gs.npc_action(id, "Hero", 1, npc).len(), 2, "the same stock again");

        let bought = gs.buy(id, "Hero", npc, staff, 0);
        assert_eq!(bought.len(), 4);
        let copy = u32::from_le_bytes(bought[0][7..11].try_into().unwrap());
        assert_eq!(bought[0], d2gs::npc_transaction(d2gs::transaction::BOUGHT, 0, copy, 1000 - 168), "Akara's price: the cost");
        assert_eq!(bought[1], d2gs::set_stat(14, 832));
        assert_eq!((bought[2][1], guid_of(&bought[2])), (item_action::REMOVE_FROM_STORE, staff));
        assert_eq!((bought[3][1], guid_of(&bought[3]), read(&bought[3]).location), (item_action::PUT_IN_CONTAINER, copy, Location::Stored { col: 0, row: 0, page: 0 }));
        assert_eq!(gs.buy(id, "Hero", npc, staff, 0), [d2gs::npc_transaction(0, d2gs::transaction::NO_ITEM, staff, 832)], "sold out");

        let potions = gs.buy(id, "Hero", npc, potion, 0x8000_0000);
        let deals: Vec<&Vec<u8>> = potions.iter().filter(|q| q[0] == 0x2A).collect();
        assert_eq!(deals.len(), 5, "four into the belt, a fifth into the inventory");
        let placed: Vec<u8> = potions.iter().filter(|q| q[0] == 0x9C).map(|q| q[1]).collect();
        assert_eq!(placed, [item_action::PUT_IN_BELT; 4].into_iter().chain([item_action::PUT_IN_CONTAINER]).collect::<Vec<_>>(), "never out of stock");
        assert_eq!(gs.lock().by_id[&id].battle.player_gold("Hero"), Some(832 - 5 * 30));

        let sold = gs.sell(id, "Hero", npc, copy, 0);
        assert_eq!((sold[0][0], sold[0][1], read_owned(&rules, &sold[0]).flags & item_bits::flags::USED), (0x9D, item_action::REMOVE_FROM_CONTAINER, item_bits::flags::USED));
        assert_eq!(sold[1], d2gs::npc_transaction(d2gs::transaction::SOLD, d2gs::transaction::SOLD_OK, copy, 682 + 84), "half the cost");
        assert_eq!(sold[2], vec![0x19, 84]);
        let back = sold.last().unwrap();
        assert_eq!((back[1], &read(back).code), (item_action::ADD_TO_STORE, b"sst "));
        assert_eq!(gs.lock().by_id[&id].stores[&npc].items.len(), 2);
        assert!(gs.lock().by_id[&id].carried["Hero"].inventory.get(copy).is_none());

        gs.npc_cancel(id, "Hero", npc);
        assert_eq!(gs.sell(id, "Hero", npc, copy, 0), [d2gs::npc_transaction(0, d2gs::transaction::NOT_OPEN, u32::MAX, 766)]);
    }

    fn read_owned(rules: &GameData, packet: &[u8]) -> Item {
        item_bits::read(&packet[13..], rules.items(), rules.item_stats(), item_bits::Target::Network).unwrap().0
    }

    /// A hand axe picked up is worn in the empty right hand; a second, which an Amazon cannot
    /// hold beside it, goes to the inventory. Lifted, swapped with the worn one, put back, lifted
    /// and dropped, each move answered with the packet the engine sends; what is worn is saved and
    /// comes back worn.
    #[tokio::test]
    async fn weapons_are_worn_moved_dropped_saved_and_brought_back() {
        use bnetcc_storage::memory::MemoryStorage;
        use bnetcc_storage::model::Credential;
        let storage = crate::storage::spawn(Box::new(MemoryStorage::new()));
        let owner = storage.create_account("Owner", Credential::Xsha1 { digest: [1; 20] }).await.unwrap();
        let mut hero = character("Hero", 0, 0x20);
        hero.account = owner.id;
        storage.create_character(hero.clone()).await.unwrap();
        let (rules, town) = test_town();
        let rules = item_rules(rules);
        let hax = rules.items().class_of(b"hax ").unwrap();
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town).with_storage(storage.clone());
        let id = gs.create("arms", "", 0).unwrap();
        let (x, y) = gs.lock().by_id[&id].spawn;
        let room = gs.lock().by_id[&id].world.as_ref().unwrap().room_at(i32::from(x), i32::from(y)).unwrap();
        let mut p = Player::new(id, hero.clone(), 0, 0, (x, y));
        let stats = p.join_stats(gs.rules.as_ref().unwrap()).unwrap();
        gs.join_battle(id, "Hero", 0, &stats);
        gs.join_items(&p);
        gs.set_position(id, "Hero", f64::from(x), f64::from(y));
        gs.set_view(id, "Hero", &[room]);
        let rules = gs.rules.clone().unwrap();
        let read_bits = |packet: &[u8]| {
            let at = if packet[0] == 0x9D { 13 } else { 8 };
            item_bits::read(&packet[at..], rules.items(), rules.item_stats(), item_bits::Target::Network).unwrap().0
        };
        for guid in [200, 201] {
            let mut axe = Item::new(*b"hax ", 101, 5, Location::Ground { x: 0, y: 0 });
            (axe.max_durability, axe.durability) = (28, 20);
            gs.lock().by_id.get_mut(&id).unwrap().ground.insert(guid, GroundItem { room, x: x + 1, y, what: Loot::Item { class: hax, item: Box::new(axe) } });
        }
        let worn = gs.pick_up(id, "Hero", 200, false);
        assert_eq!((worn[1][0], worn[1][1], worn[1][3]), (0x9D, item_action::EQUIP, 5), "worn, category 5");
        let item = read_bits(&worn[1]);
        assert_eq!((item.location, item.durability, item.max_durability), (Location::Equipped { body: 4 }, 20, 28));
        let carried = gs.pick_up(id, "Hero", 201, false);
        assert_eq!((carried[1][1], read_bits(&carried[1]).location), (item_action::PUT_IN_CONTAINER, Location::Stored { col: 0, row: 0, page: 0 }));

        let mv = |request| gs.move_item(id, "Hero", request);
        assert!(mv(ItemMove::Equip { guid: 201, body: 5 }).is_empty(), "not on the cursor");
        let lifted = mv(ItemMove::Lift { guid: 201 });
        assert_eq!((lifted[0][1], read_bits(&lifted[0]).location), (item_action::REMOVE_FROM_CONTAINER, Location::Cursor { body: 0, col: 0, row: 0, page: Some(0) }));
        assert!(mv(ItemMove::Equip { guid: 201, body: 5 }).is_empty(), "no second axe beside the first");
        assert!(mv(ItemMove::Equip { guid: 201, body: 1 }).is_empty(), "not on the head");
        let swapped = mv(ItemMove::SwapEquipped { guid: 201, body: 4 });
        let (out, into) = (read_bits(&swapped[0]), read_bits(&swapped[1]));
        assert_eq!((swapped[0][1], swapped[1][1]), (item_action::SWAP_BODY, item_action::SWAP_BODY));
        assert_eq!((out.location, out.flags & 0xC1, into.location, into.flags & 0xC1), (Location::Cursor { body: 4, col: 0, row: 0, page: None }, 0x81, Location::Equipped { body: 4 }, 0x41));
        let put = mv(ItemMove::Insert { guid: 200, col: 2, row: 0, grid: 0 });
        assert_eq!((put[0][1], read_bits(&put[0]).location), (item_action::PUT_IN_CONTAINER, Location::Stored { col: 2, row: 0, page: 0 }));
        assert!(mv(ItemMove::Insert { guid: 200, col: 9, row: 3, grid: 0 }).is_empty(), "no longer on the cursor");
        mv(ItemMove::Lift { guid: 200 });
        let dropped = mv(ItemMove::Drop { guid: 200 });
        assert_eq!((dropped[0][0], dropped[0][1]), (0x9C, item_action::DROP_TO_GROUND));
        assert!(matches!(read_bits(&dropped[0]).location, Location::Ground { .. }));
        assert!(gs.lock().by_id[&id].ground.contains_key(&200), "on the ground for anyone");

        gs.save_character(&mut p).await;
        let stored = storage.character_by_name("Hero").await.unwrap();
        gs.leave(id, "Hero");
        let again = gs.create("again", "", 0).unwrap();
        let back = gs.join_items(&Player::new(again, stored, 0, 0, (x, y)));
        assert_eq!(back.len(), 1);
        assert_eq!((back[0][1], read_bits(&back[0]).location), (item_action::EQUIP, Location::Equipped { body: 4 }));
    }

    #[test]
    fn only_simple_misc_items_are_made_from_treasure() {
        let rules = item_rules(test_rules());
        let class = |code: &str| simple_item_class(&rules, code);
        assert!(class("hp1").is_some() && class("rvs").is_some() && class("gcr").is_some());
        assert_eq!((class("key"), class("hax"), class("gld"), class("weap3"), class("hp1x")), (None, None, None, None, None), "stacks, equipment, gold and type picks are not");
        let hp1 = class("hp1").unwrap();
        let on_ground = GroundItem { room: RoomId { level: 2, index: 0 }, x: 10, y: 20, what: Loot::Item { class: hp1, item: simple_item(b"hp1 ", 2) } };
        assert_eq!(
            ground_packet(&rules, 5, &on_ground, true, 2),
            d2gs::item_world(item_action::ADD_TO_GROUND, 16, 5, 0x00A0_2010, 2, ItemSpot::Ground { x: 10, y: 20 }, *b"hp1 ")
        );
    }

    /// Potions picked up fill the four belt slots, then the inventory from its right-hand column;
    /// one from the belt is drunk and taken out; a gem, not beltable, goes straight to the
    /// inventory; a full inventory leaves things on the ground. What is carried is saved and comes
    /// back into the belt and inventory in the next game.
    #[tokio::test]
    async fn potions_are_belted_drunk_saved_and_brought_back() {
        use bnetcc_storage::memory::MemoryStorage;
        use bnetcc_storage::model::Credential;
        let storage = crate::storage::spawn(Box::new(MemoryStorage::new()));
        let owner = storage.create_account("Owner", Credential::Xsha1 { digest: [1; 20] }).await.unwrap();
        let mut hero = character("Hero", 4, 0x20);
        hero.account = owner.id;
        storage.create_character(hero.clone()).await.unwrap();
        let (rules, town) = test_town();
        let rules = item_rules(rules);
        let (hp1, rvs, gcr) = (simple_item_class(&rules, "hp1").unwrap(), simple_item_class(&rules, "rvs").unwrap(), simple_item_class(&rules, "gcr").unwrap());
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town).with_storage(storage.clone());
        let id = gs.create("probe", "", 0).unwrap();
        let (x, y) = gs.lock().by_id[&id].spawn;
        let room = gs.lock().by_id[&id].world.as_ref().unwrap().room_at(i32::from(x), i32::from(y)).unwrap();
        let mut p = Player::new(id, hero.clone(), 0, 0, (x, y));
        let stats = p.join_stats(gs.rules.as_ref().unwrap()).unwrap();
        gs.join_battle(id, "Hero", 4, &stats);
        assert!(gs.join_items(&p).is_empty(), "a new character carries nothing");
        gs.set_position(id, "Hero", f64::from(x), f64::from(y));
        gs.set_view(id, "Hero", &[room]);
        let put = |guid: u32, class: i32, code: &[u8; 4], dx: u16| {
            gs.lock().by_id.get_mut(&id).unwrap().ground.insert(guid, GroundItem { room, x: x + dx, y, what: Loot::Item { class, item: simple_item(code, 101) } });
        };
        for guid in 100..105 {
            put(guid, hp1, b"hp1 ", (guid - 99) as u16);
        }
        put(105, gcr, b"gcr ", 7);
        let simple = item_flags::SIMPLE;
        for (guid, slot) in (100..104).zip(0u8..) {
            let got = gs.pick_up(id, "Hero", guid, false);
            assert_eq!(got, [d2gs::remove_unit(unit_type::ITEM, guid), d2gs::item_world(0x0E, 16, guid, simple, 101, ItemSpot::Belt { slot }, *b"hp1 ")]);
        }
        let fifth = gs.pick_up(id, "Hero", 104, false);
        assert_eq!(fifth[1], d2gs::item_world(4, 16, 104, simple, 101, ItemSpot::Stored { col: 9, row: 3, page: 0 }, *b"hp1 "), "the belt is full");
        let gem = gs.pick_up(id, "Hero", 105, false);
        assert_eq!(gem[1], d2gs::item_world(4, 16, 105, simple, 101, ItemSpot::Stored { col: 9, row: 2, page: 0 }, *b"gcr "));
        assert!(gs.pick_up(id, "Hero", 105, false).is_empty(), "once");

        // Drinking: from the belt, and from the inventory by its own packet only.
        assert!(gs.use_item(id, "Hero", 101, false).is_empty(), "a belt potion is not used as an inventory one");
        assert_eq!(gs.use_item(id, "Hero", 101, true), [d2gs::item_world(0x0F, 16, 101, simple | item_flags::USED, 101, ItemSpot::Belt { slot: 1 }, *b"hp1 ")]);
        assert!(gs.use_item(id, "Hero", 101, true).is_empty(), "gone");
        assert!(gs.use_item(id, "Hero", 105, false).is_empty(), "a gem is not drunk");
        let from_pack = gs.use_item(id, "Hero", 104, false);
        assert_eq!(from_pack, [d2gs::item_owned(5, 16, 104, PLAYER_GUID, simple | item_flags::USED, 101, ItemSpot::Stored { col: 9, row: 3, page: 0 }, *b"hp1 ")]);
        // A rejuvenation potion picked up takes the free belt slot and tells the client its
        // life and mana when drunk.
        put(106, rvs, b"rvs ", 8);
        assert_eq!(gs.pick_up(id, "Hero", 106, false)[1], d2gs::item_world(0x0E, 16, 106, simple, 101, ItemSpot::Belt { slot: 1 }, *b"rvs "));
        let drunk = gs.use_item(id, "Hero", 106, true);
        assert_eq!((drunk.len(), drunk[1][0]), (2, 0x95), "{drunk:02x?}");

        // Saved and brought back.
        gs.save_character(&mut p).await;
        let stored = storage.character_by_name("Hero").await.unwrap();
        let save = Save::parse(stored.save.as_deref().unwrap()).unwrap();
        let rules = gs.rules.as_ref().unwrap();
        let (saved, _) = item_bits::read_save_list(&save.items, rules.items(), rules.item_stats()).unwrap();
        let held: Vec<(Location, [u8; 4])> = saved.iter().map(|i| (i.location, i.code)).collect();
        let (belt, grid) = (|slot| Location::Belt { slot }, Location::Stored { col: 9, row: 2, page: 0 });
        assert_eq!(held, [(belt(0), *b"hp1 "), (belt(2), *b"hp1 "), (belt(3), *b"hp1 "), (grid, *b"gcr ")]);
        assert!(save.items.ends_with(b"JM\0\0jfkf\0"), "the corpse's list and the expansion blocks follow");
        gs.leave(id, "Hero");
        let id = gs.create("again", "", 0).unwrap();
        let q = Player::new(id, stored, 0, 0, (x, y));
        let back = gs.join_items(&q);
        let places: Vec<(u8, u8)> = back.iter().map(|p| (p[1], p[8 + 7] >> 6 & 1)).collect();
        assert_eq!(back.len(), 4);
        assert_eq!(places.iter().filter(|(action, _)| *action == 0x0E).count(), 3, "three in the belt");
        assert_eq!(back[3], d2gs::item_world(4, 16, u32::from_le_bytes([back[3][4], back[3][5], back[3][6], back[3][7]]), simple, 101, ItemSpot::Stored { col: 9, row: 2, page: 0 }, *b"gcr "));

        // A full inventory leaves an item on the ground.
        {
            let mut g = gs.lock();
            let carried = g.by_id.get_mut(&id).unwrap().carried.get_mut("Hero").unwrap();
            let mut guid = 1000;
            while let Some(place) = carried.inventory.place_for((1, 1), true) {
                guid += 1;
                carried.inventory.insert(Held { guid, class: gcr, size: (1, 1), place, item: *simple_item(b"gcr ", 101) });
            }
        }
        let (x2, y2) = gs.lock().by_id[&id].spawn;
        let room2 = gs.lock().by_id[&id].world.as_ref().unwrap().room_at(i32::from(x2), i32::from(y2)).unwrap();
        gs.lock().by_id.get_mut(&id).unwrap().ground.insert(7, GroundItem { room: room2, x: x2, y: y2, what: Loot::Item { class: hp1, item: simple_item(b"hp1 ", 101) } });
        assert!(gs.pick_up(id, "Hero", 7, false).is_empty());
        assert!(gs.lock().by_id[&id].ground.contains_key(&7), "still there");
    }

    /// A character leaving a game is saved as a `.d2s` holding its stats and its waypoints; the
    /// next game it joins loads them back, and a save that is not its own is ignored.
    #[tokio::test]
    async fn a_character_is_saved_with_its_waypoints_and_joins_with_them() {
        use bnetcc_storage::memory::MemoryStorage;
        use bnetcc_storage::model::Credential;
        let storage = crate::storage::spawn(Box::new(MemoryStorage::new()));
        let owner = storage.create_account("Owner", Credential::Xsha1 { digest: [1; 20] }).await.unwrap();
        let mut hero = character("Hero", 4, 0x20);
        hero.account = owner.id;
        storage.create_character(hero.clone()).await.unwrap();
        let gs = GameServer::new(test_tables(), Some(test_rules())).with_storage(storage.clone());
        let id = gs.create("probe", "", 0).unwrap();

        let mut p = Player::new(id, hero.clone(), 0, 0, (0, 0));
        assert!(p.save.is_none());
        let rules = gs.rules.as_ref().unwrap();
        gs.join_battle(id, "Hero", 4, &p.join_stats(rules).unwrap());
        p.waypoints[0] = 0b1001;
        gs.save_character(&mut p).await;
        let stored = storage.character_by_name("Hero").await.unwrap();
        let save = Save::parse(stored.save.as_deref().expect("saved")).unwrap();
        assert_eq!((save.name(), save.class(), save.level()), ("Hero".to_string(), 4, 1));
        assert_eq!(save.waypoints[0][2], 0b1001);
        assert_eq!(save.stat(u16::from(stat::MAXHP)), 50 << 8);

        let mut played = save.clone();
        played.set_stat(u16::from(stat::LEVEL), 2);
        played.set_stat(u16::from(stat::EXPERIENCE), 600);
        played.set_level(2, 0);
        let mut next = stored.clone();
        next.save = Some(played.to_bytes());
        let q = Player::new(id, next.clone(), 0, 0, (0, 0));
        assert_eq!(q.waypoints[0], 0b1001, "the camp and the other waypoint are known");
        let joined = q.join_stats(rules).unwrap();
        assert!(joined.contains(&(stat::LEVEL, 2)) && joined.contains(&(stat::EXPERIENCE, 600)), "{joined:?}");
        assert!(joined.windows(2).all(|w| w[0].0 < w[1].0), "ascending stat order");
        next.name = "Other".into();
        assert!(Player::new(id, next, 0, 0, (0, 0)).save.is_none(), "another character's save is not loaded");
    }

    /// A new sorceress joins with the common skills (`0x94`) and Attack on both buttons; she spends
    /// her point on Fire Bolt (`0x21`, points to 0), puts it on her right button (`0x23`), is
    /// refused a skill she lacks, and her save keeps both — the next game gives them back.
    #[tokio::test]
    async fn skills_are_learned_selected_saved_and_brought_back() {
        use bnetcc_storage::memory::MemoryStorage;
        use bnetcc_storage::model::Credential;
        use d2_formats::excel::Table;
        let storage = crate::storage::spawn(Box::new(MemoryStorage::new()));
        let owner = storage.create_account("Owner", Credential::Xsha1 { digest: [1; 20] }).await.unwrap();
        let mut sorc = character("Sorc", 1, 0x20);
        sorc.account = owner.id;
        storage.create_character(sorc.clone()).await.unwrap();
        let mut rules = test_rules();
        let mut table = String::from("skill\tcharclass\treqlevel\tmaxlvl\tInGame\r\n");
        for id in 0..40 {
            let class = if (36..40).contains(&id) { "sor" } else { "" };
            table += &format!("skill {id}\t{class}\t1\t20\t1\r\n");
        }
        rules.set_skills(d2_data::skills::Skills::from_table(&Table::parse(table.as_bytes())));
        let gs = GameServer::new(test_tables(), Some(rules)).with_storage(storage.clone());
        let id = gs.create("tower", "", 0).unwrap();
        let mut p = Player::new(id, sorc.clone(), 0, 0, (0, 0));
        let mut stats = p.join_stats(gs.rules.as_ref().unwrap()).unwrap();
        stats.push((stat::NEWSKILLS, 1));
        gs.join_battle(id, "Sorc", 1, &stats);
        let joined = gs.join_skills(&p);
        let common: Vec<(u16, u8)> = d2_game::skills::COMMON_SKILLS.iter().map(|&s| (s as u16, 1)).collect();
        assert_eq!(joined, [d2gs::skill_list(PLAYER_GUID, &common), d2gs::select_skill(0, PLAYER_GUID, true, 0, u32::MAX), d2gs::select_skill(0, PLAYER_GUID, false, 0, u32::MAX)]);

        assert!(gs.select_skill(id, "Sorc", 36, false, u32::MAX).is_empty(), "not learned yet");
        assert!(gs.learn_skill(id, "Sorc", 70).is_empty(), "not a sorceress skill");
        assert_eq!(gs.learn_skill(id, "Sorc", 36), [d2gs::skill_level(0, PLAYER_GUID, 36, 1, 0), d2gs::set_stat(stat::NEWSKILLS, 0)]);
        assert!(gs.learn_skill(id, "Sorc", 37).is_empty(), "no points left");
        assert_eq!(gs.select_skill(id, "Sorc", 36, false, u32::MAX), [d2gs::select_skill(0, PLAYER_GUID, false, 36, u32::MAX)]);

        gs.save_character(&mut p).await;
        let stored = storage.character_by_name("Sorc").await.unwrap();
        let save = Save::parse(stored.save.as_deref().unwrap()).unwrap();
        assert_eq!((save.skills[0], save.skills[1], &save.header[0x7C..0x80]), (1, 0, &36u32.to_le_bytes()[..]), "Fire Bolt learned, on the right button");
        gs.leave(id, "Sorc");
        let again = gs.create("again", "", 0).unwrap();
        let q = Player::new(again, stored, 0, 0, (0, 0));
        gs.join_battle(again, "Sorc", 1, &q.join_stats(gs.rules.as_ref().unwrap()).unwrap());
        let back = gs.join_skills(&q);
        let mut listed = common.clone();
        listed.push((36, 1));
        assert_eq!(back, [d2gs::skill_list(PLAYER_GUID, &listed), d2gs::select_skill(0, PLAYER_GUID, true, 0, u32::MAX), d2gs::select_skill(0, PLAYER_GUID, false, 36, u32::MAX)]);
    }

    /// A player killed out in a made-up Blood Moor room and releasing (`0x41`) is stood up where it
    /// lies — life, mana and stamina as stats — before the camp's rooms load and `0x15` moves it:
    /// the client does not move a corpse, and halts when its room goes. No `0x0D` goes with it: the
    /// client runs that after the frame and would lay the player down again.
    #[test]
    fn a_dead_player_is_revived_before_it_is_moved_to_town() {
        use d2_data::monsters::Monsters;
        use d2_data::presets::MonPresets;
        use d2_drlg::preset::{PlacedUnit, UnitClass};
        use d2_drlg::world::WorldLevel;
        use d2_drlg::Coords;
        use d2_formats::excel::Table;
        let (mut rules, town) = test_town();
        let monstats = Table::parse(
            b"Id\thcIdx\tMonStatsEx\tnpc\tinteract\tCode\tLevel\tminHP\tmaxHP\tA1MinD\tA1MaxD\tA1TH\taidel\tVelocity\r\n\
              guard\t10\tguard\t1\t1\tGU\t1\t1\t1\t0\t0\t0\t5\t1\r\n\
              killer\t11\tguard\t0\t0\tXX\t50\t1000\t1000\t500\t500\t5000\t1\t8\r\n",
        );
        let objects = rules.objects().clone();
        rules.set_map_tables(MonPresets::default(), Monsters::from_tables(&monstats, &Table::parse(b"Id\r\nguard\r\n")).unwrap(), objects);
        let monlvl = Table::parse(b"Level\tAC\tTH\tHP\tDM\tXP\r\n0\t100\t100\t100\t100\t100\r\n50\t100\t100\t100\t100\t100\r\n");
        rules.set_combat_tables(d2_data::monlvl::MonLvls::from_table(&monlvl).unwrap(), Default::default());
        let stats = rules.new_character_stats(4).unwrap();
        let gs = GameServer::new(test_tables(), Some(rules)).with_town(town);
        let id = gs.create("probe", "", 0).unwrap();
        let moor = Coords { x: 1152, y: 896, w: 8, h: 8 };
        {
            let mut g = gs.lock();
            let game = g.by_id.get_mut(&id).unwrap();
            let mut levels = game.world.take().unwrap().levels().to_vec();
            let killer = PlacedUnit { class: UnitClass::Monster(d2_data::presets::PresetMonster::Class { class: 11, name: "killer".into() }), x: 5780, y: 4500, path: Vec::new() };
            levels.push(WorldLevel { id: 2, area: moor, rooms: vec![moor], units: vec![killer], pieces: vec![0], collision: Vec::new(), warps: Vec::new() });
            game.world = Some(World::from_levels(levels));
        }
        let room = RoomId { level: 2, index: 0 };
        gs.join_battle(id, "Hero", 4, &stats);
        gs.set_position(id, "Hero", 5781.0, 4500.0);
        gs.set_view(id, "Hero", &[room]);
        gs.view_change(id, &[], &[room]);
        for _ in 0..40 {
            if gs.player_dead(id, "Hero") {
                break;
            }
            fight_for(&gs, id, "Hero", 25);
        }
        assert!(gs.player_dead(id, "Hero"), "the killer kills");

        let player = Player::new(id, character("Hero", 4, 0), 0, FALLBACK_MAP_SEED, (5808, 4448));
        let mut walker = Walker { x: 5781.0, y: 4500.0, target: None, speed: 0.0, room: Some(room), view: vec![room] };
        let mut outbox = Outbox::default();
        assert!(respawn(&gs, &player, &mut walker, &mut outbox));
        let sent = split_packets(&outbox.pending().flatten().copied().collect::<Vec<u8>>());
        let full = gs.lock().by_id[&id].battle.player_stats("Hero").unwrap();
        let of = |id: u8| full.iter().find(|s| s.0 == id).unwrap().1;
        assert!(of(stat::HITPOINTS) > 0 && of(stat::HITPOINTS) == of(stat::MAXHP), "full life");
        assert_eq!(
            sent[..3],
            [d2gs::set_stat(stat::HITPOINTS, of(stat::HITPOINTS)), d2gs::set_stat(stat::MANA, of(stat::MANA)), d2gs::set_stat(stat::STAMINA, of(stat::STAMINA))],
            "life, mana and stamina as stats first: {sent:02x?}"
        );
        assert_eq!(sent[3][0], 0x07, "the camp's rooms after");
        assert_eq!(sent.last().unwrap()[0], 0x15, "then the move");
        assert!(!sent.iter().any(|p| p[0] == 0x0D), "no corpse event: the client would run it after the stand-up");
        assert!(!gs.player_dead(id, "Hero"));
        assert!(!respawn(&gs, &player, &mut walker, &mut Outbox::default()), "only the dead respawn");
    }

    /// With the operator's install: a player running across Blood Moor with monsters after it is
    /// never sent a monster standing outside the rooms its client holds (the client halts), and
    /// no monster follows it out of the level.
    #[test]
    fn with_a_real_install_chasing_monsters_stay_in_loaded_rooms() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let rules = GameData::load(&dir).unwrap();
        let stats = rules.new_character_stats(4).unwrap();
        let gs = GameServer::new(test_tables(), Some(rules)).with_engine(engine);
        let mut checked = 0;
        for i in 0..3 {
            let id = gs.create(&format!("chase {i}"), "", 0).unwrap();
            let (spawn, moor) = {
                let g = gs.lock();
                let game = &g.by_id[&id];
                (game.spawn, game.world.as_ref().unwrap().levels().iter().find(|l| l.id == 2).unwrap().area)
            };
            gs.join_battle(id, "Runner", 4, &stats);
            let mut w = Walker { x: f64::from(spawn.0), y: f64::from(spawn.1), target: None, speed: 0.0, room: None, view: Vec::new() };
            let (room, near) = gs.near(id, w.x, w.y).unwrap();
            gs.view_change(id, &[], &near);
            (w.room, w.view) = (Some(room), near);
            let far = (f64::from((moor.x + moor.w / 2) * 5), f64::from((moor.y + moor.h / 2) * 5));
            // Out to the middle of the moor, and back to the camp, slowly enough to be chased.
            for (tx, ty) in [far, (f64::from(spawn.0), f64::from(spawn.1))] {
                w.go(tx, ty, WALK_SPEED * 0.6);
                while w.moving() {
                    w.step(SERVER_FRAME);
                    gs.set_position(id, "Runner", w.x, w.y);
                    let mut sent = Vec::new();
                    if let Some((room, near)) = gs.near(id, w.x, w.y) {
                        if w.room != Some(room) {
                            let view = gs.kept_view(id, room, &near, &w.view);
                            sent.extend(gs.view_change(id, &w.view, &view));
                            (w.room, w.view) = (Some(room), view);
                        }
                    }
                    gs.set_view(id, "Runner", &w.view);
                    if gs.player_dead(id, "Runner") {
                        gs.revive(id, "Runner");
                    }
                    sent.extend(fight_for(&gs, id, "Runner", 1));
                    let g = gs.lock();
                    let world = g.by_id[&id].world.as_ref().unwrap();
                    for p in sent.iter().filter(|p| p[0] == 0xAC) {
                        let (x, y) = (i32::from(u16::from_le_bytes([p[7], p[8]])), i32::from(u16::from_le_bytes([p[9], p[10]])));
                        let at = world.room_at(x, y).expect("a monster in no room");
                        assert!(w.view.contains(&at), "game {i}: monster at ({x}, {y}) in {at:?}, not held by the client");
                        let guid = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
                        assert!(at.level != 1 || g.by_id[&id].battle.monster(guid).is_none(), "game {i}: a fighting monster in the camp");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 20, "only {checked} monsters sent");
    }

    /// With the operator's install: Blood Moor's cave mouth comes with its room as a warp unit
    /// (`0x09`); taking it lands in the Den of Evil, whose rooms load with their monsters, and the
    /// Den's own warp leads back out.
    #[test]
    fn with_a_real_install_the_den_of_evil_can_be_entered_and_left() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let gs = GameServer::new(test_tables(), Some(GameData::load(&dir).unwrap())).with_engine(engine);
        let mut monsters = 0;
        for i in 0..4 {
            let id = gs.create(&format!("den {i}"), "", 0).unwrap();
            let mouth_room = {
                let g = gs.lock();
                let moor = g.by_id[&id].world.as_ref().unwrap().levels().iter().find(|l| l.id == 2).unwrap();
                RoomId { level: 2, index: moor.warps[0].room }
            };
            let packets = gs.view_change(id, &[], &[mouth_room]);
            let warp = packets.iter().find(|p| p[0] == 0x09).unwrap_or_else(|| panic!("game {i}: no warp unit in {packets:02x?}"));
            assert_eq!((warp.len(), warp[1]), (11, 5));
            let guid = u32::from_le_bytes([warp[2], warp[3], warp[4], warp[5]]);
            let (level, x, y) = gs.warp_arrival(id, guid).expect("the cave mouth leads somewhere");
            assert_eq!(level, 8);
            let (room, near) = gs.near(id, f64::from(x), f64::from(y)).expect("a Den room");
            assert_eq!(room.level, 8);
            let inside = gs.view_change(id, &[mouth_room], &near);
            assert!(inside.contains(&d2gs::remove_unit(5, guid)), "leaving the mouth's room forgets its warp");
            assert!(inside.iter().any(|p| p[0] == 0x07 && p[5] == 8), "Den rooms load");
            let way_out = inside.iter().find(|p| p[0] == 0x09).expect("the Den's way out is in view");
            let out = u32::from_le_bytes([way_out[2], way_out[3], way_out[4], way_out[5]]);
            assert_eq!(gs.warp_arrival(id, out).map(|a| a.0), Some(2));
            monsters += inside.iter().filter(|p| p[0] == 0xAC).count();
        }
        assert!(monsters > 0, "the Den's first rooms hold no monsters in four games");
    }

    /// With the operator's install: a new character standing among Blood Moor's monsters is
    /// noticed and hit, and hitting one back until it dies gets its death frames and experience.
    #[test]
    fn with_a_real_install_a_blood_moor_fight_runs() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let rules = GameData::load(&dir).unwrap();
        let stats = rules.new_character_stats(4).unwrap();
        let gs = GameServer::new(test_tables(), Some(rules)).with_engine(engine);
        let (mut fought, mut hit_back, mut piles) = (0, 0, 0);
        for i in 0..6 {
            let id = gs.create(&format!("fight {i}"), "", 0).unwrap();
            let moor: Vec<RoomId> = {
                let g = gs.lock();
                let level = g.by_id[&id].world.as_ref().unwrap().levels().iter().find(|l| l.id == 2).unwrap();
                (0..level.rooms.len()).map(|index| RoomId { level: 2, index }).collect()
            };
            gs.view_change(id, &[], &moor);
            let target = {
                let g = gs.lock();
                let game = &g.by_id[&id];
                moor.iter()
                    .flat_map(|&room| game.population.as_ref().unwrap().units(room).unwrap_or(&[]).iter())
                    .find_map(|u| match *u {
                        Spawned::Monster { guid, x, y, .. } if game.battle.monster(guid).is_some() => Some((guid, x, y)),
                        _ => None,
                    })
            };
            let Some((guid, x, y)) = target else { continue };
            gs.join_battle(id, "Hero", 4, &stats);
            gs.set_position(id, "Hero", f64::from(x) + 2.0, f64::from(y));
            gs.set_view(id, "Hero", &moor);
            let mut seen = fight_for(&gs, id, "Hero", 1);
            for _ in 0..400 {
                if gs.battle_monster(id, guid).is_none() || gs.player_dead(id, "Hero") {
                    break;
                }
                let (mx, my) = gs.battle_monster(id, guid).unwrap();
                gs.set_position(id, "Hero", f64::from(mx) + 2.0, f64::from(my));
                gs.player_attack(id, "Hero", guid);
                seen.extend(fight_for(&gs, id, "Hero", 5));
            }
            // Then stand there while the rest of the pack comes.
            seen.extend(fight_for(&gs, id, "Hero", 500));
            let ops: std::collections::BTreeSet<u8> = seen.iter().map(|p| p[0]).collect();
            if gs.battle_monster(id, guid).is_none() {
                let death: Vec<&Vec<u8>> = seen.iter().filter(|p| p[0] == 0x69 && p[1..5] == guid.to_le_bytes()).collect();
                assert!(death.iter().any(|p| p[5] == 0x08) && death.iter().any(|p| p[5] == 0x09 && p[11] == 0), "{death:02x?}");
                assert!(ops.contains(&0x1A), "experience for the kill: {ops:02x?}");
                fought += 1;
                // Any gold lands on open ground.
                for p in seen.iter().filter(|p| p[0] == 0x9C) {
                    let bits = |from: usize, len: usize| (0..len).map(|i| u32::from((p[8 + (from + i) / 8] >> ((from + i) % 8)) & 1) << i).sum::<u32>();
                    let (x, y) = (bits(45, 16) as i32, bits(61, 16) as i32);
                    let g = gs.lock();
                    let world = g.by_id[&id].world.as_ref().unwrap();
                    assert!(world.collision_at(x, y).is_some_and(|c| c & d2_game::path::WALL == 0), "gold at ({x}, {y}) is in a wall");
                    piles += 1;
                }
            }
            hit_back += usize::from(ops.contains(&0x6C) && ops.contains(&0x95) && ops.contains(&0x0D));
        }
        assert!(fought > 0, "no game had a monster to fight");
        assert!(hit_back > 0, "no pack fought back");
        eprintln!("{fought} kills, {piles} piles on the ground");
    }

    /// With the operator's install: the potions, scrolls, gems and runes Act I treasure names are
    /// simple items the server makes, belted as the game belts them; stacks, jewellery and
    /// equipment are not made yet.
    #[test]
    fn with_a_real_install_treasure_potions_are_simple_items() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let rules = GameData::load(&dir).unwrap();
        let items = rules.items();
        for code in ["hp1", "hp5", "mp1", "mp5", "rvs", "rvl", "yps", "vps", "wms", "isc", "tsc", "gcv", "gfr", "skc", "r01"] {
            let class = simple_item_class(&rules, code).unwrap_or_else(|| panic!("{code} is simple"));
            let def = items.get(class).unwrap();
            assert_eq!((def.inv_size, items::category(&rules, &def.code)), ((1, 1), 16), "{code}");
        }
        for code in ["key", "aqv", "cqv", "opl", "rin", "amu", "jew", "cm1", "weap3", "armo6", "gld"] {
            assert_eq!(simple_item_class(&rules, code), None, "{code} is not made");
        }
        let def = |code: &str| items.get(items.class_of(&d2_data::items::code(code)).unwrap()).unwrap();
        let hp1 = items.class_of(&d2_data::items::code("hp1")).unwrap();
        assert!(def("hp1").auto_belt && items.beltable(hp1));
        let isc = items.class_of(&d2_data::items::code("isc")).unwrap();
        assert!(items.beltable(isc) && !def("isc").auto_belt, "scrolls fit the belt but are not put there");
        assert_eq!(battle::Potion::of(def("hp1")), Some(battle::Potion::Healing { points: 30, frames: 192 }));
        assert_eq!(battle::Potion::of(def("mp5")), Some(battle::Potion::Mana { points: 250, frames: 128 }));
        assert_eq!(battle::Potion::of(def("rvl")), Some(battle::Potion::Rejuvenation { life: 100, mana: 100 }));
        assert_eq!(battle::Potion::of(def("vps")), None);
    }

    /// A new Amazon starts with javelins in her right hand and a buckler in her left, four minor
    /// healing potions in the belt and the two scrolls in the inventory; a new Sorceress's staff
    /// carries a point of Fire Bolt.
    #[test]
    fn with_a_real_install_new_characters_start_with_their_class_items() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let rules = GameData::load(&dir).unwrap();
        let gs = GameServer::new(test_tables(), Some(rules.clone()));
        let id = gs.create("start", "", 0).unwrap();
        let places = |class: u8| {
            let p = Player::new(id, character("Fresh", class, 0), 0, 0, (0, 0));
            let packets = gs.join_items(&p);
            let g = gs.lock();
            let carried = &g.by_id[&id].carried["Fresh"];
            let held: Vec<(String, Place, u16, i32, bool)> = carried
                .inventory
                .items()
                .iter()
                .map(|h| (d2_data::items::code_str(&h.code()), h.place, h.item.quantity, h.item.durability, h.item.stats.iter().any(|s| s.id == 107)))
                .collect();
            (packets.len(), held)
        };
        let (sent, amazon) = places(0);
        assert_eq!(sent, 8);
        let summary: Vec<(&str, Place)> = amazon.iter().map(|(c, p, ..)| (c.as_str(), *p)).collect();
        assert_eq!(
            summary,
            [
                ("jav", Place::Body(4)),
                ("buc", Place::Body(5)),
                ("hp1", Place::Belt(0)),
                ("hp1", Place::Belt(1)),
                ("hp1", Place::Belt(2)),
                ("hp1", Place::Belt(3)),
                ("tsc", Place::Grid { col: 9, row: 3 }),
                ("isc", Place::Grid { col: 9, row: 2 }),
            ]
        );
        assert!(amazon[0].2 > 1 && amazon[1].3 > 0, "a full stack of javelins, a whole buckler: {amazon:?}");
        gs.lock().by_id.get_mut(&id).unwrap().carried.clear();
        let (_, sorceress) = places(1);
        assert_eq!((sorceress[0].0.as_str(), sorceress[0].1, sorceress[0].4), ("sst", Place::Body(4), true), "Fire Bolt on the staff");
    }

    #[test]
    fn with_a_real_install_walking_out_of_camp_loads_blood_moor() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let engine = EngineData::from_game_exe(&std::fs::read(Path::new(&dir).join("Game.exe")).unwrap()).unwrap();
        let gs = GameServer::new(test_tables(), Some(GameData::load(&dir).unwrap())).with_engine(engine);
        for i in 0..8 {
            let id = gs.create(&format!("walk {i}"), "", 0).unwrap();
            let (spawn, moor) = {
                let g = gs.lock();
                let game = &g.by_id[&id];
                let moor = game.world.as_ref().unwrap().levels().iter().find(|l| l.id == 2).unwrap().area;
                (game.spawn, moor)
            };
            let (tx, ty) = (f64::from((moor.x + moor.w / 2) * 5), f64::from((moor.y + moor.h / 2) * 5));
            let mut w = Walker { x: f64::from(spawn.0), y: f64::from(spawn.1), target: None, speed: 0.0, room: None, view: Vec::new() };
            let (room, near) = gs.near(id, w.x, w.y).unwrap();
            let mut loaded: Vec<Vec<u8>> = gs.view_change(id, &[], &near).into_iter().filter(|p| p[0] == 0x07).collect();
            (w.room, w.view) = (Some(room), near);
            w.go(tx, ty, RUN_SPEED);
            let mut moor_rooms = 0;
            while w.moving() {
                w.step(SERVER_FRAME);
                let Some((room, near)) = gs.near(id, w.x, w.y) else { continue };
                if w.room == Some(room) {
                    continue;
                }
                let view = gs.kept_view(id, room, &near, &w.view);
                for p in gs.view_change(id, &w.view, &view) {
                    match p[0] {
                        0x07 => {
                            moor_rooms += usize::from(p[5] == 2);
                            loaded.push(p);
                        }
                        0x08 => {
                            let mut as_load = p.clone();
                            as_load[0] = 0x07;
                            assert!(loaded.contains(&as_load), "dropped a room never loaded: {p:02x?}");
                            loaded.retain(|l| *l != as_load);
                        }
                        _ => {}
                    }
                }
                (w.room, w.view) = (Some(room), view);
            }
            assert!(moor_rooms > 10, "game {i}: only {moor_rooms} Blood Moor rooms loaded on the way");
        }
    }

    #[tokio::test]
    async fn a_wrong_version_is_refused_with_the_engines_reason() {
        let gs = Arc::new(GameServer::new(test_tables(), Some(test_rules())));
        let id = gs.create("probe", "", 0).unwrap();
        let (_, hash) = gs.stage_join("probe", "", character("TestBan", 4, 0)).unwrap();
        let addr = spawn(Arc::clone(&gs)).await;
        let mut c = TcpStream::connect(addr).await.unwrap();
        let mut greeting = [0u8; 2];
        c.read_exact(&mut greeting).await.unwrap();
        c.write_all(&logon_packet(id, hash, 4, 0x0D, "TestBan")).await.unwrap();
        assert_eq!(read_frame(&mut c, &gs.tables.huffman).await, vec![0xB4, 0x10, 0, 0, 0]);
    }
}
