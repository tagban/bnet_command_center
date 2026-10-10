//! The realm's end of the link to its Diablo II game server (`bnetcc_gslink`).
//!
//! The game server is a separate program. It dials `diablo2.game_server_link`, proves the shared
//! token, and from then on answers the realm's requests. One game server is linked at a time; a
//! newer connection that proves the token replaces the older one (a restarted game server does not
//! wait for its dead predecessor's socket to time out).
//!
//! A game server says hello with its link version; any from `bnetcc_gslink::MIN_VERSION` up is
//! accepted, and it is only ever sent what that version knows: a version-1 game server creates and
//! joins games as before, and the lobby's game list and details stay empty with it.
//!
//! Every request has a short deadline. No game server, a dropped link or a late answer all come back
//! as [`LinkError::Down`], which the realm turns into the client's own "Server Down".

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bnetcc_gslink::{
    read_message, token_matches, write_message, CreateError, FromGameServer, GameSettings, LobbyGame, PublicGame, ToGameServer,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// How long a request waits for the game server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a connecting game server has to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a request got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// No game server linked, the link dropped, or it did not answer in time.
    Down,
}

/// The linked game server, if any, and the requests waiting on it.
#[derive(Default)]
pub struct GameServerLink {
    token: String,
    /// The current connection: its generation, the link version it said hello with, and its
    /// outbound queue.
    current: Mutex<Option<Current>>,
    generation: AtomicU64,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<FromGameServer>>>,
    /// The game list it last pushed.
    games: Mutex<Vec<PublicGame>>,
}

/// The linked game server's connection.
struct Current {
    generation: u64,
    version: u32,
    tx: mpsc::Sender<ToGameServer>,
}

impl std::fmt::Debug for GameServerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameServerLink").field("connected", &self.connected()).finish_non_exhaustive()
    }
}

impl GameServerLink {
    /// A link that accepts game servers proving `token`.
    #[must_use]
    pub fn new(token: &str) -> Arc<Self> {
        Arc::new(Self { token: token.to_string(), ..Self::default() })
    }

    /// Whether a game server is linked.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.current.lock().expect("link lock").is_some()
    }

    /// The link version the linked game server speaks, if one is linked.
    #[must_use]
    pub fn version(&self) -> Option<u32> {
        self.current.lock().expect("link lock").as_ref().map(|c| c.version)
    }

    /// The games the linked game server last reported; empty with none linked.
    #[must_use]
    pub fn public_games(&self) -> Vec<PublicGame> {
        if !self.connected() {
            return Vec::new();
        }
        self.games.lock().expect("games lock").clone()
    }

    /// Accept game servers on `listener` for as long as the process runs.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let link = Arc::clone(&self);
                    tokio::spawn(async move { link.connection(stream, peer).await });
                }
                Err(e) => {
                    warn!(error = %e, "game server link accept failed");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }

    async fn connection(self: Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        let _ = stream.set_nodelay(true);
        let (mut rd, mut wr) = stream.into_split();
        let hello = tokio::time::timeout(HELLO_TIMEOUT, read_message::<FromGameServer>(&mut rd)).await;
        let refuse = |reason: &str| ToGameServer::Refused { reason: reason.to_string() };
        let version = match hello {
            Ok(Ok(Some(FromGameServer::Hello { token, version }))) => {
                if !token_matches(&self.token, &token) {
                    warn!(%peer, "game server link refused: wrong token");
                    let _ = write_message(&mut wr, &refuse("wrong token")).await;
                    return;
                }
                if !(bnetcc_gslink::MIN_VERSION..=bnetcc_gslink::VERSION).contains(&version) {
                    warn!(%peer, version, "game server link refused: another link version");
                    let reason = format!("the realm speaks link versions {} to {}", bnetcc_gslink::MIN_VERSION, bnetcc_gslink::VERSION);
                    let _ = write_message(&mut wr, &refuse(&reason)).await;
                    return;
                }
                version
            }
            _ => {
                warn!(%peer, "game server link closed: no hello");
                return;
            }
        };
        if write_message(&mut wr, &ToGameServer::Welcome).await.is_err() {
            return;
        }

        let (tx, mut rx) = mpsc::channel::<ToGameServer>(64);
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let replaced = self.current.lock().expect("link lock").replace(Current { generation, version, tx }).is_some();
        info!(%peer, replaced, version, "Diablo II game server linked");
        let writer = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if write_message(&mut wr, &message).await.is_err() {
                    break;
                }
            }
        });

        loop {
            match read_message::<FromGameServer>(&mut rd).await {
                Ok(Some(FromGameServer::Games { games })) => *self.games.lock().expect("games lock") = games,
                Ok(Some(FromGameServer::Hello { .. })) => {}
                Ok(Some(reply)) => {
                    let id = match &reply {
                        FromGameServer::Created { id, .. }
                        | FromGameServer::Joined { id, .. }
                        | FromGameServer::Map { id, .. }
                        | FromGameServer::GameList { id, .. }
                        | FromGameServer::GameInfo { id, .. } => *id,
                        _ => continue,
                    };
                    if let Some(waiter) = self.pending.lock().expect("pending lock").remove(&id) {
                        let _ = waiter.send(reply);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(%peer, error = %e, "game server link read failed");
                    break;
                }
            }
        }
        writer.abort();
        let mut current = self.current.lock().expect("link lock");
        if current.as_ref().is_some_and(|c| c.generation == generation) {
            *current = None;
            self.games.lock().expect("games lock").clear();
            info!(%peer, "Diablo II game server unlinked");
        }
    }

    /// Send a request built around its id and wait for the matching reply.
    async fn request(&self, make: impl FnOnce(u64) -> ToGameServer) -> Result<FromGameServer, LinkError> {
        let tx = self.current.lock().expect("link lock").as_ref().map(|c| c.tx.clone()).ok_or(LinkError::Down)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (waiter, reply) = oneshot::channel();
        self.pending.lock().expect("pending lock").insert(id, waiter);
        let answered = async {
            tx.send(make(id)).await.map_err(|_| LinkError::Down)?;
            reply.await.map_err(|_| LinkError::Down)
        };
        let result = tokio::time::timeout(REQUEST_TIMEOUT, answered).await.unwrap_or(Err(LinkError::Down));
        self.pending.lock().expect("pending lock").remove(&id);
        result
    }

    /// Whether the linked game server answers the lobby's requests (link version 2 and up).
    fn has_lobby(&self) -> bool {
        self.version().is_some_and(|v| v >= bnetcc_gslink::LOBBY_VERSION)
    }

    /// Create a game: its token, or why not. A version-1 game server is not told `settings`.
    ///
    /// # Errors
    ///
    /// [`LinkError::Down`] with no answer from a game server.
    pub async fn create(&self, name: &str, password: &str, difficulty: u8, settings: &GameSettings) -> Result<Result<u16, CreateError>, LinkError> {
        let (name, password) = (name.to_string(), password.to_string());
        let reply = if self.has_lobby() {
            let settings = settings.clone();
            self.request(|id| ToGameServer::CreateGame { id, name, password, difficulty, settings }).await?
        } else {
            self.request(|id| ToGameServer::Create { id, name, password, difficulty }).await?
        };
        match reply {
            FromGameServer::Created { result, .. } => Ok(result),
            _ => Err(LinkError::Down),
        }
    }

    /// The game server's open games, oldest first; none from a version-1 game server.
    ///
    /// # Errors
    ///
    /// [`LinkError::Down`] with no answer from a game server.
    pub async fn list_games(&self) -> Result<Vec<LobbyGame>, LinkError> {
        if self.version().is_none() {
            return Err(LinkError::Down);
        }
        if !self.has_lobby() {
            return Ok(Vec::new());
        }
        match self.request(|id| ToGameServer::ListGames { id }).await? {
            FromGameServer::GameList { games, .. } => Ok(games),
            _ => Err(LinkError::Down),
        }
    }

    /// One game by name: `None` when there is no such game, or the game server is version 1.
    ///
    /// # Errors
    ///
    /// [`LinkError::Down`] with no answer from a game server.
    pub async fn game_info(&self, name: &str) -> Result<Option<LobbyGame>, LinkError> {
        if self.version().is_none() {
            return Err(LinkError::Down);
        }
        if !self.has_lobby() {
            return Ok(None);
        }
        let name = name.to_string();
        match self.request(|id| ToGameServer::GameInfo { id, name }).await? {
            FromGameServer::GameInfo { game, .. } => Ok(game),
            _ => Err(LinkError::Down),
        }
    }

    /// Stage a character's join: the game's token and hash, or why not.
    ///
    /// # Errors
    ///
    /// [`LinkError::Down`] with no answer from a game server.
    pub async fn join(
        &self,
        name: &str,
        password: &str,
        account: u64,
        character: &str,
    ) -> Result<Result<bnetcc_gslink::Joined, bnetcc_gslink::JoinError>, LinkError> {
        let (name, password, character) = (name.to_string(), password.to_string(), character.to_string());
        match self.request(|id| ToGameServer::Join { id, name, password, account, character }).await? {
            FromGameServer::Joined { result, .. } => Ok(result),
            _ => Err(LinkError::Down),
        }
    }

    /// A map request's JSON body: `None` for no such game or level, or no game server.
    pub async fn map(&self, make: impl FnOnce(u64) -> ToGameServer) -> Option<String> {
        match self.request(make).await {
            Ok(FromGameServer::Map { json, .. }) => json,
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in game server for tests that speaks link version 1 (create, join and the maps):
    /// links with `token` and answers with `answer`.
    pub(crate) async fn fake_game_server(
        addr: SocketAddr,
        token: &str,
        answer: impl Fn(ToGameServer) -> Option<FromGameServer> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        fake_game_server_speaking(addr, token, bnetcc_gslink::MIN_VERSION, answer).await
    }

    /// A stand-in game server saying hello with link `version`.
    pub(crate) async fn fake_game_server_speaking(
        addr: SocketAddr,
        token: &str,
        version: u32,
        answer: impl Fn(ToGameServer) -> Option<FromGameServer> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let stream = TcpStream::connect(addr).await.expect("dial the realm");
        let (mut rd, mut wr) = stream.into_split();
        write_message(&mut wr, &FromGameServer::Hello { token: token.to_string(), version }).await.unwrap();
        assert_eq!(read_message::<ToGameServer>(&mut rd).await.unwrap(), Some(ToGameServer::Welcome));
        tokio::spawn(async move {
            while let Ok(Some(request)) = read_message::<ToGameServer>(&mut rd).await {
                if let Some(reply) = answer(request) {
                    if write_message(&mut wr, &reply).await.is_err() {
                        break;
                    }
                }
            }
        })
    }

    /// A link listening on an ephemeral port.
    pub(crate) async fn listening(token: &str) -> (Arc<GameServerLink>, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let link = GameServerLink::new(token);
        tokio::spawn(Arc::clone(&link).serve(listener));
        (link, addr)
    }

    async fn until(mut check: impl FnMut() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition never held");
    }

    fn settings() -> GameSettings {
        GameSettings {
            description: String::new(),
            max_players: 8,
            level_difference: None,
            creator_level: 1,
            expansion: true,
            hardcore: false,
            ladder: false,
        }
    }

    #[tokio::test]
    async fn requests_are_answered_by_the_linked_game_server() {
        let (link, addr) = listening("s3cret").await;
        assert_eq!(link.create("baal", "", 0, &settings()).await, Err(LinkError::Down), "no game server yet");
        assert_eq!(link.list_games().await, Err(LinkError::Down));

        let _gs = fake_game_server(addr, "s3cret", |request| match request {
            ToGameServer::Create { id, name, .. } if name == "taken" => {
                Some(FromGameServer::Created { id, result: Err(bnetcc_gslink::CreateError::NameTaken) })
            }
            ToGameServer::Create { id, .. } => Some(FromGameServer::Created { id, result: Ok(3) }),
            ToGameServer::Join { id, account, .. } => Some(FromGameServer::Joined {
                id,
                result: if account == 9 { Ok(bnetcc_gslink::Joined { token: 3, hash: 77 }) } else { Err(bnetcc_gslink::JoinError::NoSuchCharacter) },
            }),
            _ => None,
        })
        .await;
        until(|| link.connected()).await;
        assert_eq!(link.version(), Some(1));
        assert_eq!(link.create("baal", "", 2, &settings()).await, Ok(Ok(3)), "a version-1 game server is sent the old create");
        assert_eq!(link.create("taken", "", 0, &settings()).await, Ok(Err(bnetcc_gslink::CreateError::NameTaken)));
        assert_eq!(link.list_games().await, Ok(Vec::new()), "and never asked for the lobby's lists");
        assert_eq!(link.game_info("baal").await, Ok(None));
        assert_eq!(link.join("baal", "", 9, "Tyrael").await, Ok(Ok(bnetcc_gslink::Joined { token: 3, hash: 77 })));
        assert_eq!(link.join("baal", "", 1, "Tyrael").await, Ok(Err(bnetcc_gslink::JoinError::NoSuchCharacter)));
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused_and_a_dropped_game_server_reads_as_down() {
        let (link, addr) = listening("s3cret").await;
        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut rd, mut wr) = stream.into_split();
        write_message(&mut wr, &FromGameServer::Hello { token: "guess".into(), version: bnetcc_gslink::VERSION }).await.unwrap();
        assert!(matches!(read_message::<ToGameServer>(&mut rd).await.unwrap(), Some(ToGameServer::Refused { .. })));
        assert!(!link.connected());

        let gs = fake_game_server(addr, "s3cret", |_| None).await;
        until(|| link.connected()).await;
        gs.abort();
        let _ = gs.await;
        until(|| !link.connected()).await;
        assert_eq!(link.create("baal", "", 0, &settings()).await, Err(LinkError::Down));
    }

    #[tokio::test]
    async fn a_version_two_game_server_gets_the_lobby_requests_and_a_newer_one_is_refused() {
        use bnetcc_gslink::{LobbyGame, VERSION};
        let (link, addr) = listening("s3cret").await;
        let game = LobbyGame { token: 4, name: "moo".into(), private: true, difficulty: 1, settings: Some(settings()), players: Vec::new(), age_secs: 9 };
        let listed = game.clone();
        let _gs = fake_game_server_speaking(addr, "s3cret", VERSION, move |request| match request {
            ToGameServer::CreateGame { id, settings, .. } => Some(FromGameServer::Created { id, result: Ok(u16::from(settings.creator_level)) }),
            ToGameServer::ListGames { id } => Some(FromGameServer::GameList { id, games: vec![listed.clone()] }),
            ToGameServer::GameInfo { id, name } => Some(FromGameServer::GameInfo { id, game: (name == "moo").then(|| listed.clone()) }),
            _ => None,
        })
        .await;
        until(|| link.connected()).await;
        assert_eq!(link.version(), Some(VERSION));
        let mut made = settings();
        made.creator_level = 42;
        assert_eq!(link.create("moo", "", 1, &made).await, Ok(Ok(42)), "the settings went with the create");
        assert_eq!(link.list_games().await, Ok(vec![game.clone()]));
        assert_eq!(link.game_info("moo").await, Ok(Some(game)));
        assert_eq!(link.game_info("gone").await, Ok(None));

        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut rd, mut wr) = stream.into_split();
        write_message(&mut wr, &FromGameServer::Hello { token: "s3cret".into(), version: VERSION + 1 }).await.unwrap();
        assert!(matches!(read_message::<ToGameServer>(&mut rd).await.unwrap(), Some(ToGameServer::Refused { .. })));
        assert_eq!(link.version(), Some(VERSION), "the linked one stays");
    }
}
