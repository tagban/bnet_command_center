//! Diablo II game server — **handshake test only** (`diablo2.game_server_probe`).
//!
//! Nothing is simulated. A client that joins a game is taken through the join exactly as
//! the 1.14d engine runs it (`docs/D2GS-114D-WIRE.md` §4): `AF 01`, then on `GAMELOGON`
//! `01 00` and `02`, then on `ENTERGAME` `59 5E 28 29 0B`, the player's stats, `23 23 95 03 53 07`, the
//! rooms around the spawn with their objects and NPCs (`07`, `51`, `AC AA 6D`), `15 7E` and, a
//! server frame later, `04`.
//! After that nothing is sent but ping replies; every packet the client sends is logged.
//! The point is to learn, against a real client, whether our packets are accepted and what the
//! client asks for next — not to play.
//!
//! Games live only in memory. The realm creates them (`MCP_CREATEGAME`), stages a join for
//! one character (`MCP_JOINGAME`), and this module matches the client's `GAMELOGON` against
//! that staging. The engine tables come from the operator's own `Game.exe`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bnetcc_proto::d2::status;
use bnetcc_proto::d2gs::{self, cs, join_failed, ClientPacketLen, EngineTables, GameLogon, Outbox};
use bnetcc_storage::Character;
use d2_data::engine::EngineData;
use d2_data::{stat, GameData};
use d2_drlg::act::Act;
use d2_drlg::preset::PresetLevel;
use d2_drlg::world::{RoomId, World};
use d2_game::population::{unit_type, waypoint_spawn, Population, Spawned};
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::session::hex_preview;

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
        Self { tables, rules, towns: Towns::None, speed_scale: 1.0, games: Mutex::new(Games::default()) }
    }

    /// Build each new game's town from its own map seed with these engine tables (the rules
    /// must hold the install's maps).
    #[must_use]
    pub fn with_engine(mut self, engine: EngineData) -> Self {
        self.towns = Towns::FromInstall(Box::new(engine));
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
                let level = d2_drlg::world::WorldLevel { id: town.level_id, area: town.area, rooms: town.rooms.clone() };
                return (FALLBACK_MAP_SEED, Some(town.clone()), Some(World::from_levels(vec![level])));
            }
            return (FALLBACK_MAP_SEED, None, None);
        };
        let build = |seed: u32| {
            let act = Act::build(data.levels(), 0, difficulty, seed);
            PresetLevel::build(data, engine, &act, i32::from(TOWN_AREA)).map(|town| {
                let world = World::build(data.levels(), &act, Some(&town));
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
    pub async fn start(data_dir: &str, ip: std::net::IpAddr) -> Option<Arc<Self>> {
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
        let server = Arc::new(Self::new(tables, rules).with_engine(engine));
        warn!(
            %addr,
            "Diablo II game server HANDSHAKE TEST is on: games can be created and joined; clients \
             stand in town with the objects and NPCs near the spawn, but cannot act \
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
                population: town.as_ref().map(|t| Population::new(rand::thread_rng().gen(), t.rooms.len())),
                spawn,
                town,
                world,
            },
        );
        let game = &g.by_id[&id];
        let map = game.town.as_ref().map_or("none", |t| t.map.as_str());
        let levels: Vec<i32> = game.world.as_ref().map(|w| w.levels().iter().map(|l| l.id).collect()).unwrap_or_default();
        info!(game = %name, id, difficulty, map_seed = %format!("{map_seed:#010x}"), %map, ?spawn, ?levels, "test game created");
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
        game.connected.push(character.name.clone());
        Some(Player {
            game_id: logon.game_id,
            character,
            difficulty: game.difficulty,
            map_seed: game.map_seed,
            spawn: game.spawn,
        })
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

    /// Where a unit of the game's population stands.
    fn unit_position(&self, game_id: u16, kind: u32, guid: u32) -> Option<(u16, u16)> {
        let g = self.lock();
        let population = g.by_id.get(&game_id)?.population.as_ref()?;
        match *population.find(u8::try_from(kind).ok()?, guid)? {
            Spawned::Object { x, y, .. } | Spawned::Monster { x, y, .. } => Some((x, y)),
        }
    }

    /// What the engine answers `0x13` with (`0x00548B00`), for the units the test knows:
    /// - an NPC `MonStats.txt` lets players talk to: its dialog (`0x00572C10`: `27 29 28`), with
    ///   no quest messages and a new character's clear flags;
    /// - the stash (`OperateFn` 32, class 267): `77 10` (`0x00564CD0`);
    /// - an active waypoint (`OperateFn` 23): the player learns its level's waypoint, then the
    ///   menu `63` (`0x00584E30`).
    ///
    /// Range, busy and collision checks are not ported: the client walks up before it asks.
    fn interact(&self, game_id: u16, kind: u32, guid: u32, waypoints: &mut [u8; d2gs::WAYPOINT_FLAG_BYTES]) -> Vec<Vec<u8>> {
        let (Some(rules), Ok(kind)) = (&self.rules, u8::try_from(kind)) else { return Vec::new() };
        let g = self.lock();
        let Some(game) = g.by_id.get(&game_id) else { return Vec::new() };
        let Some(unit) = game.population.as_ref().and_then(|p| p.find(kind, guid)) else { return Vec::new() };
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
                Some(23) if matches!(mode, 1 | 2) => {
                    let level = game.town.as_ref().map_or(i32::from(TOWN_AREA), |t| t.level_id);
                    if let Some(bit) = rules.levels().get(level).and_then(|l| l.waypoint) {
                        if let Some(byte) = waypoints.get_mut(usize::from(bit / 8)) {
                            *byte |= 1 << (bit % 8);
                        }
                    }
                    vec![d2gs::waypoint_menu(guid, waypoints)]
                }
                _ => Vec::new(),
            },
        }
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
        let town_level = game.town.as_ref().map(|t| t.level_id);
        for &id in to.iter().filter(|id| !from.contains(id)) {
            let Some(room) = world.room(id) else { continue };
            packets.push(d2gs::load_room(room.x as u16, room.y as u16, id.level as u8));
            let (Some(rules), Some(town), Some(population)) = (&self.rules, &game.town, &mut game.population) else {
                continue;
            };
            if Some(id.level) != town_level {
                continue;
            }
            let activated = population.activate(rules, town, id.index);
            for skipped in &activated.not_ported {
                debug!(game_id, ?room, unit = %skipped, "map unit not spawned: not ported");
            }
            for unit in activated.units {
                match *unit {
                    Spawned::Object { guid, class, x, y, mode, interaction } => {
                        packets.push(d2gs::assign_object(guid, class, x, y, mode, interaction));
                    }
                    Spawned::Monster { guid, class, x, y, mode, life, ref components, ref variants } => {
                        packets.push(d2gs::assign_monster(guid, class, x, y, life, mode, components, variants));
                        packets.push(d2gs::no_unit_states(unit_type::MONSTER, guid));
                        packets.push(d2gs::monster_standing(guid, x, y, life));
                    }
                }
            }
        }
        for &id in from.iter().filter(|id| !to.contains(id)) {
            let Some(room) = world.room(id) else { continue };
            if Some(id.level) == town_level {
                for unit in game.population.as_ref().and_then(|p| p.units(id.index)).unwrap_or(&[]) {
                    let kind = match unit {
                        Spawned::Object { .. } => unit_type::OBJECT,
                        Spawned::Monster { .. } => unit_type::MONSTER,
                    };
                    packets.push(d2gs::remove_unit(kind, unit.guid()));
                }
            }
            packets.push(d2gs::unload_room(room.x as u16, room.y as u16, id.level as u8));
        }
        packets
    }

    /// A connected character left; an emptied game goes with it.
    fn leave(&self, game_id: u16, name: &str) {
        let mut g = self.lock();
        let Some(game) = g.by_id.get_mut(&game_id) else { return };
        game.connected.retain(|n| !n.eq_ignore_ascii_case(name));
        if game.connected.is_empty() && game.staged.is_empty() {
            let game = g.by_id.remove(&game_id).expect("present");
            info!(game = %game.name, id = game_id, "test game closed: last player left");
        }
    }
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

/// One client on the game port.
async fn session(mut stream: TcpStream, peer: SocketAddr, server: &GameServer) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    info!(%peer, "game connection; sending AF 01");
    // Raw, and alone: the client splits whatever arrives in the same read as the greeting
    // as raw packets, so nothing else is sent until it logs on.
    stream.write_all(&d2gs::GREETING).await?;

    let mut player: Option<Player> = None;
    let result = run(&mut stream, peer, server, &mut player).await;
    if let Some(p) = player {
        info!(%peer, character = %p.character.name, "game connection closed");
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
    // The player's waypoint flags; a new character's are clear until it touches one.
    let mut waypoints = [0u8; d2gs::WAYPOINT_FLAG_BYTES];
    let mut frame = tokio::time::interval(SERVER_FRAME);
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_step = Instant::now();

    loop {
        let limit = if stage == Stage::AwaitLogon { LOGON_TIMEOUT } else { IN_GAME_TIMEOUT };
        let moving = walker.as_ref().is_some_and(Walker::moving);
        let read = tokio::select! {
            r = tokio::time::timeout(limit, stream.read(&mut chunk)) => Some(r),
            _ = frame.tick(), if moving => None,
        };
        let Some(read) = read else {
            // A server frame while the player moves: advance it and follow it with rooms.
            let (Some(w), Some(p)) = (walker.as_mut(), player.as_ref()) else { continue };
            let now = Instant::now();
            w.step(now.duration_since(last_step).min(SERVER_FRAME * 5));
            last_step = now;
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
                    walker = Some(enter_game(stream, peer, server, p, &mut outbox).await?);
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
                (Stage::InGame, cs::INTERACT) => {
                    let Some(p) = player.as_ref() else { continue };
                    let replies = server.interact(p.game_id, u32_at(1), u32_at(5), &mut waypoints);
                    if !replies.is_empty() {
                        for packet in &replies {
                            outbox.push(packet);
                        }
                        flush(stream, peer, tables, &mut outbox).await?;
                    }
                }
                (Stage::InGame, cs::UPDATE_POSITION) => {
                    // The client's own idea of where its player is (engine `0x0054CD50` re-syncs to
                    // it): take it, and let the next frame follow it with rooms.
                    if let Some(w) = walker.as_mut() {
                        (w.x, w.y) = (f64::from(u16_at(1)), f64::from(u16_at(3)));
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
    // A fresh game's quests (every quest object starts available) and a new character's flags,
    // all clear: the client halts on entering a new area without them.
    outbox.push(&d2gs::quest_states(&[1; d2gs::QUESTS]));
    outbox.push(&d2gs::player_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]));
    outbox.push(&d2gs::game_quest_flags(&[0; d2gs::QUEST_FLAG_BYTES]));
    outbox.push(&d2gs::own_unit(0, PLAYER_GUID));
    // Its stats, one packet each (ClientAddPlayerToGame walks the stat list through 0x548520),
    // as a new character: no .d2s is loaded yet.
    let stats = server.rules.as_ref().and_then(|r| r.new_character_stats(p.character.class)).unwrap_or_default();
    for &(id, value) in &stats {
        outbox.push(&d2gs::set_stat(id, value));
    }
    outbox.push(&d2gs::select_skill(0, PLAYER_GUID, true, 0, u32::MAX));
    outbox.push(&d2gs::select_skill(0, PLAYER_GUID, false, 0, u32::MAX));
    // Then life, mana and stamina in whole points, before the player has a position (0x548760).
    if !stats.is_empty() {
        let whole = |id: u8| stats.iter().find(|&&(s, _)| s == id).map_or(0, |&(_, v)| (v >> 8) as u16);
        outbox.push(&d2gs::life_and_position(whole(stat::HITPOINTS), whole(stat::MANA), whole(stat::STAMINA), 0, 0, 0, 0));
    }
    outbox.push(&d2gs::load_act(0, p.map_seed, TOWN_AREA, 0));
    // Period 2 starts at angle 0: the start of the day. The client's 0x53 handler reads its
    // own player unit, which is why 0x0B has to be in first.
    outbox.push(&d2gs::act_environment(2, 0, false));
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
    use bnetcc_proto::d2gs::{Huffman, CLIENT_OPCODES, SERVER_OPCODES};

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
        client[usize::from(cs::UPDATE_POSITION)] = 5;
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
            vec![0x59, 0x5E, 0x28, 0x29, 0x0B, 0x23, 0x23, 0x95, 0x03, 0x53, 0x07, 0x15, 0x7E],
            "the engine's order, one frame"
        );
        assert_eq!(packets[1], d2gs::quest_states(&[1; d2gs::QUESTS]), "every quest available");
        let stats: Vec<&[u8]> = packets[5..].iter().copied().take_while(|p| (0x1D..=0x1F).contains(&p[0])).collect();
        assert_eq!(stats.len(), 15, "every stat a new character starts with, right after 0x0B");
        assert!(stats.contains(&&[0x1E, stat::MAXHP, 0x00, 50][..]), "max life (20 vit + 30) << 8");
        assert!(stats.contains(&&[0x1D, stat::LEVEL, 1][..]));
        assert_eq!(&packets[0][6..13], b"TestBan", "the character's name in 0x59");
        assert_eq!(&packets[0][22..26], &[0, 0, 0, 0], "0x59 before placement: no position");
        assert_eq!(packets[4], &[0x0B, 0, 1, 0, 0, 0], "then: that unit is yours");
        assert_eq!(&packets[5 + 15 + 3][2..6], &FALLBACK_MAP_SEED.to_le_bytes(), "0x03 carries the seed");
        assert_eq!(packets[5 + 15 + 5], d2gs::load_room(1152, 880, 1), "no town: the fallback seed's spawn room");
        assert_eq!(&packets[5 + 15 + 6][6..10], &[0xA6, 0x16, 0x3D, 0x11], "placed on its waypoint (5798, 4413)");
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
                      1\t0\t32\t8\t32\t8\t32\t8\t0\t0\t0\t2\t1\t0\r\n";
        rules.set_levels(Levels::from_table(&Table::parse(levels.as_bytes())).unwrap());
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\tnpc\tinteract\r\nguard\t10\tguard\t1\t1\r\n");
        let mut ms2 = String::from("Id\tcritter");
        for c in COMPONENT_COLUMNS {
            ms2 += &format!("\t{c}");
        }
        ms2 += "\r\nguard\t0\tcap,helm\r\n";
        let empty = |col: &str| Table::parse(format!("{col}\r\n").as_bytes());
        let presets = MonPresets::from_tables(&Table::parse(b"Act\tPlace\r\n1\tguard\r\n"), &monstats, &empty("Superunique"), &empty("code")).unwrap();
        rules.set_map_tables(presets, Monsters::from_tables(&monstats, &Table::parse(ms2.as_bytes())).unwrap(), Objects::from_table(&objects));
        let room = |x, y| Coords { x, y, w: 8, h: 8 };
        let at = |class, x, y| PlacedUnit { class, x, y, path: Vec::new() };
        let town = PresetLevel {
            level_id: 1,
            area: Coords { x: 1152, y: 888, w: 32, h: 8 },
            map: String::new(),
            rooms: vec![room(1152, 888), room(1160, 888), room(1168, 888), room(1176, 888)],
            units: vec![
                at(UnitClass::Monster(PresetMonster::Class { class: 10, name: "guard".into() }), 5815, 4455),
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
                0x23 | 0x95 => 13,
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

    /// With the operator's install: walking from the waypoint to the middle of Blood Moor, the
    /// server loads Blood Moor's rooms along the way, and every room it drops it had loaded.
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
