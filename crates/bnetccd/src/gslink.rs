//! The realm's end of the link to its Diablo II game server (`bnetcc_gslink`).
//!
//! The game server is a separate program. It dials `diablo2.game_server_link`, proves the shared
//! token, and from then on answers the realm's requests. One game server is linked at a time; a
//! newer connection that proves the token replaces the older one (a restarted game server does not
//! wait for its dead predecessor's socket to time out).
//!
//! Every request has a short deadline. No game server, a dropped link or a late answer all come back
//! as [`LinkError::Down`], which the realm turns into the client's own "Server Down".

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bnetcc_gslink::{read_message, token_matches, write_message, FromGameServer, PublicGame, ToGameServer};
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
    /// The current connection's outbound queue and its generation.
    current: Mutex<Option<(u64, mpsc::Sender<ToGameServer>)>>,
    generation: AtomicU64,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<FromGameServer>>>,
    /// The game list it last pushed.
    games: Mutex<Vec<PublicGame>>,
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
        match hello {
            Ok(Ok(Some(FromGameServer::Hello { token, version }))) => {
                if !token_matches(&self.token, &token) {
                    warn!(%peer, "game server link refused: wrong token");
                    let _ = write_message(&mut wr, &refuse("wrong token")).await;
                    return;
                }
                if version != bnetcc_gslink::VERSION {
                    warn!(%peer, version, "game server link refused: another link version");
                    let _ = write_message(&mut wr, &refuse(&format!("the realm speaks link version {}", bnetcc_gslink::VERSION))).await;
                    return;
                }
            }
            _ => {
                warn!(%peer, "game server link closed: no hello");
                return;
            }
        }
        if write_message(&mut wr, &ToGameServer::Welcome).await.is_err() {
            return;
        }

        let (tx, mut rx) = mpsc::channel::<ToGameServer>(64);
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let replaced = self.current.lock().expect("link lock").replace((generation, tx)).is_some();
        info!(%peer, replaced, "Diablo II game server linked");
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
                        FromGameServer::Created { id, .. } | FromGameServer::Joined { id, .. } | FromGameServer::Map { id, .. } => *id,
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
        if current.as_ref().is_some_and(|(g, _)| *g == generation) {
            *current = None;
            self.games.lock().expect("games lock").clear();
            info!(%peer, "Diablo II game server unlinked");
        }
    }

    /// Send a request built around its id and wait for the matching reply.
    async fn request(&self, make: impl FnOnce(u64) -> ToGameServer) -> Result<FromGameServer, LinkError> {
        let tx = self.current.lock().expect("link lock").as_ref().map(|(_, tx)| tx.clone()).ok_or(LinkError::Down)?;
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

    /// Create a game: its token, or why not.
    ///
    /// # Errors
    ///
    /// [`LinkError::Down`] with no answer from a game server.
    pub async fn create(&self, name: &str, password: &str, difficulty: u8) -> Result<Result<u16, bnetcc_gslink::CreateError>, LinkError> {
        let (name, password) = (name.to_string(), password.to_string());
        match self.request(|id| ToGameServer::Create { id, name, password, difficulty }).await? {
            FromGameServer::Created { result, .. } => Ok(result),
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

    /// A stand-in game server for tests: links with `token` and answers with `answer`.
    pub(crate) async fn fake_game_server(
        addr: SocketAddr,
        token: &str,
        answer: impl Fn(ToGameServer) -> Option<FromGameServer> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let stream = TcpStream::connect(addr).await.expect("dial the realm");
        let (mut rd, mut wr) = stream.into_split();
        write_message(&mut wr, &FromGameServer::Hello { token: token.to_string(), version: bnetcc_gslink::VERSION }).await.unwrap();
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

    #[tokio::test]
    async fn requests_are_answered_by_the_linked_game_server() {
        let (link, addr) = listening("s3cret").await;
        assert_eq!(link.create("baal", "", 0).await, Err(LinkError::Down), "no game server yet");

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
        assert_eq!(link.create("baal", "", 2).await, Ok(Ok(3)));
        assert_eq!(link.create("taken", "", 0).await, Ok(Err(bnetcc_gslink::CreateError::NameTaken)));
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
        assert_eq!(link.create("baal", "", 0).await, Err(LinkError::Down));
    }
}
