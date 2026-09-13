//! Diablo II game server — **handshake test only** (`diablo2.game_server_probe`).
//!
//! No world is simulated. A client that joins a game is taken through the join exactly as
//! the 1.14d engine runs it (`docs/D2GS-114D-WIRE.md` §4): `AF 01`, then on `GAMELOGON`
//! `01 00` and `02`, then on `ENTERGAME` `59 0B 23 23 03 53 07 15 7E` and, a server frame
//! later, `04`.
//! After that nothing is sent but ping replies; every packet the client sends is logged.
//! The point is to learn, against a real client, whether our compression and join sequence
//! are accepted and what the client asks for next — not to play.
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

/// Every test game uses this map seed. `jaenster/libd2`'s engine dumps for it place the
/// Rogue Encampment's objects (Normal) at world subtiles around (5800, 4450): stash
/// (5806, 4444), campfire (5799, 4457), waypoint (5799, 4414).
const MAP_SEED: u32 = 0x1234_5678;
/// The Rogue Encampment.
const TOWN_AREA: u16 = 1;
/// Spawn point: beside the campfire and stash for [`MAP_SEED`]. Walkability unverified —
/// the real engine picks this from the generated town, which we do not have yet.
const SPAWN: (u16, u16) = (5810, 4450);
/// Top-left tile of the 8×8-tile room holding [`SPAWN`]: the same dump puts the stash, 4
/// subtiles away, in the room at tile (1160, 888).
const SPAWN_ROOM: (u16, u16) = (1160, 888);
/// Guid of the joining player's unit.
const PLAYER_GUID: u32 = 1;

/// The test game server: its engine tables and the games the realm has created.
#[derive(Debug)]
pub struct GameServer {
    tables: EngineTables,
    games: Mutex<Games>,
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
    pub fn new(tables: EngineTables) -> Self {
        Self { tables, games: Mutex::new(Games::default()) }
    }

    /// Load `Game.exe` from `data_dir`, bind the game port on `ip`, and start serving.
    /// `None` (with a warning) if either fails — the realm then keeps answering "Server Down".
    pub async fn start(data_dir: &str, ip: std::net::IpAddr) -> Option<Arc<Self>> {
        let path = Path::new(data_dir).join("Game.exe");
        let tables = match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|file| {
            EngineTables::from_game_exe(&file).map_err(|e| e.to_string())
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
        let server = Arc::new(Self::new(tables));
        warn!(
            %addr,
            "Diablo II game server HANDSHAKE TEST is on: games can be created and joined, but \
             clients only stand in town: no stats, NPCs or actions (diablo2.game_server_probe)"
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
                difficulty: difficulty.min(2),
                created: Instant::now(),
                staged: Vec::new(),
                connected: Vec::new(),
            },
        );
        info!(game = %name, id, difficulty, "test game created");
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
    fn claim(&self, logon: &GameLogon) -> Option<(Character, u8)> {
        let mut g = self.lock();
        let game = g.by_id.get_mut(&logon.game_id).filter(|game| game.hash == logon.game_hash)?;
        let at = game.staged.iter().position(|c| c.name.eq_ignore_ascii_case(&logon.name))?;
        let character = game.staged.remove(at);
        game.connected.push(character.name.clone());
        Some((character, game.difficulty))
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

    loop {
        let limit = if stage == Stage::AwaitLogon { LOGON_TIMEOUT } else { IN_GAME_TIMEOUT };
        let n = match tokio::time::timeout(limit, stream.read(&mut chunk)).await {
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
                    enter_game(stream, peer, server, p, &mut outbox).await?;
                    stage = Stage::InGame;
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
    let Some((character, difficulty)) = server.claim(&logon) else {
        info!(%peer, name = %logon.name, "GAMELOGON matches no staged join; refusing");
        outbox.push(&refuse(join_failed::GENERIC));
        flush(stream, peer, tables, outbox).await?;
        return Ok(None);
    };
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
    Ok(Some(Player { game_id: logon.game_id, character, difficulty }))
}

/// `ENTERGAME`, in the engine's order (`HandleSrvJoinAct`): `ClientAddPlayerToGame` creates
/// the player — its `0x59`, before it has a position — and names it the client's own with
/// `0x0B` plus both selected skills; then the act (`03 53`); then `PlacePlayerInAct` loads
/// the spawn room and places the player (`07 15 7E`). `04` follows a frame later.
async fn enter_game(
    stream: &mut TcpStream,
    peer: SocketAddr,
    server: &GameServer,
    p: &Player,
    outbox: &mut Outbox,
) -> std::io::Result<()> {
    let tables = &server.tables;
    let (x, y) = SPAWN;
    // Unplaced: at (0, 0) the client creates the unit without looking for a room.
    outbox.push(&d2gs::assign_player(PLAYER_GUID, p.character.class, &p.character.name, 0, 0));
    outbox.push(&d2gs::own_unit(0, PLAYER_GUID));
    outbox.push(&d2gs::select_skill(0, PLAYER_GUID, true, 0, u32::MAX));
    outbox.push(&d2gs::select_skill(0, PLAYER_GUID, false, 0, u32::MAX));
    outbox.push(&d2gs::load_act(0, MAP_SEED, TOWN_AREA, 0));
    // Period 2 starts at angle 0: the start of the day. The client's 0x53 handler reads its
    // own player unit, which is why 0x0B has to be in first.
    outbox.push(&d2gs::act_environment(2, 0, false));
    outbox.push(&d2gs::load_room(SPAWN_ROOM.0, SPAWN_ROOM.1, TOWN_AREA as u8));
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
        "join sequence sent; from here the test only logs what the client does"
    );
    Ok(())
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
    pub(crate) fn test_tables() -> EngineTables {
        let mut lengths = [9u8; 256];
        lengths[0] = 1;
        lengths[0xAF] = 8;
        let mut client = [0i32; CLIENT_OPCODES];
        client[usize::from(cs::GAME_LOGON)] = 37;
        client[usize::from(cs::ENTER_GAME)] = 1;
        client[usize::from(cs::PING)] = 13;
        client[usize::from(cs::LEAVE_GAME)] = 1;
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

    #[test]
    fn games_are_created_joined_and_claimed_by_name() {
        let gs = GameServer::new(test_tables());
        let id = gs.create("Baal Run", "pw", 2).unwrap();
        assert_eq!(gs.create("baal run", "", 0), Err(CreateError::NameTaken));
        assert_eq!(gs.stage_join("nope", "", character("A", 0, 0)), Err(JoinError::NoSuchGame));
        assert_eq!(gs.stage_join("BAAL RUN", "wrong", character("A", 0, 0)), Err(JoinError::BadPassword));
        let (joined, hash) = gs.stage_join("baal run", "pw", character("Tyrael", 1, 0x20)).unwrap();
        assert_eq!(joined, id);

        let mut logon = GameLogon::parse(&logon_packet(id, hash ^ 1, 1, 0x0E, "Tyrael")).unwrap();
        assert!(gs.claim(&logon).is_none(), "the hash must match");
        logon.game_hash = hash;
        let (c, difficulty) = gs.claim(&logon).expect("staged");
        assert_eq!((c.name.as_str(), difficulty), ("Tyrael", 2));
        assert!(gs.claim(&logon).is_none(), "a staged join is used once");

        gs.leave(id, "tyrael");
        assert_eq!(gs.stage_join("baal run", "pw", character("A", 0, 0)), Err(JoinError::NoSuchGame), "empty games close");
    }

    #[tokio::test]
    async fn a_client_is_taken_through_the_join_sequence() {
        let gs = Arc::new(GameServer::new(test_tables()));
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
        let entered = read_frame(&mut c, huffman).await;
        let mut packets = Vec::new();
        let mut rest = entered.as_slice();
        while let Some(&op) = rest.first() {
            let size = match op {
                0x59 => 26,
                0x0B | 0x07 => 6,
                0x23 => 13,
                0x03 => 12,
                0x53 => 10,
                0x15 => 11,
                0x7E => 5,
                other => panic!("unexpected opcode {other:#04x}"),
            };
            packets.push(&rest[..size]);
            rest = &rest[size..];
        }
        let ops: Vec<u8> = packets.iter().map(|p| p[0]).collect();
        assert_eq!(ops, vec![0x59, 0x0B, 0x23, 0x23, 0x03, 0x53, 0x07, 0x15, 0x7E], "the engine's order, one frame");
        assert_eq!(&packets[0][6..13], b"TestBan", "the character's name in 0x59");
        assert_eq!(&packets[0][22..26], &[0, 0, 0, 0], "0x59 before placement: no position");
        assert_eq!(packets[1], &[0x0B, 0, 1, 0, 0, 0], "then: that unit is yours");
        assert_eq!(&packets[4][2..6], &MAP_SEED.to_le_bytes());
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

    #[tokio::test]
    async fn a_wrong_version_is_refused_with_the_engines_reason() {
        let gs = Arc::new(GameServer::new(test_tables()));
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
