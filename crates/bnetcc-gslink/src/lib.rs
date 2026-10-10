//! The link between the realm (`bnetccd`) and a Diablo II game server.
//!
//! The game server runs as its own process, so the realm and the chat that shares its process can
//! stay up while the game server restarts, and the other way round. The game server dials the
//! realm (`diablo2.game_server_link`), proves itself with the shared token, and then answers the
//! realm's requests: create a game, stage a join, list its open games and describe one for the
//! lobby's Join Game screen, describe its games for the admin map. Every few seconds it also pushes
//! the list of its games for the public status page.
//!
//! While no game server is connected the realm answers game creation with "Server Down", the
//! client's own message for it, and everything else keeps working.
//!
//! Characters are not carried over the link: both processes open the same database, the realm to
//! list and create characters, the game server to load the one joining and save it as it plays.
//!
//! Framing: a `u32` little-endian length, then that many bytes of JSON.
//!
//! # Versions
//!
//! The hello carries the link version the game server speaks. The realm accepts any from
//! [`MIN_VERSION`] to [`VERSION`] and never sends a game server a message newer than the version it
//! said hello with, so either side can be upgraded first:
//!
//! - **1**: `Create`, `Join`, the map requests and the pushed `Games`.
//! - **2**: adds [`ToGameServer::CreateGame`] (a create carrying what the lobby shows about the
//!   game), [`ToGameServer::ListGames`] and [`ToGameServer::GameInfo`]. A version-2 game server
//!   refused by a realm that only knows version 1 says hello again as version 1.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message either side accepts. A level's collision map for the admin page is the
/// biggest thing sent, well under this.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// The newest link version; what a game server offers in its hello.
pub const VERSION: u32 = 2;

/// The oldest link version the realm still accepts, and what a game server falls back to when a
/// realm refuses [`VERSION`].
pub const MIN_VERSION: u32 = 1;

/// The first version with the lobby's messages ([`ToGameServer::CreateGame`],
/// [`ToGameServer::ListGames`], [`ToGameServer::GameInfo`]).
pub const LOBBY_VERSION: u32 = 2;

/// What the realm sends a game server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToGameServer {
    /// The hello was accepted.
    Welcome,
    /// The hello was refused; the connection closes after this.
    Refused {
        /// Why, for the game server's log.
        reason: String,
    },
    /// Create a game.
    Create {
        /// Matches the reply to the request.
        id: u64,
        /// The game's name, as typed.
        name: String,
        /// Its password, empty for none.
        password: String,
        /// 0 Normal, 1 Nightmare, 2 Hell.
        difficulty: u8,
    },
    /// Create a game, with what the lobby shows about it (link version 2; [`ToGameServer::Create`]
    /// for a version-1 game server). Answered with [`FromGameServer::Created`].
    CreateGame {
        /// Matches the reply to the request.
        id: u64,
        /// The game's name, as typed.
        name: String,
        /// Its password, empty for none.
        password: String,
        /// 0 Normal, 1 Nightmare, 2 Hell.
        difficulty: u8,
        /// Its description, level restriction, player limit and the kind of character that made it.
        settings: GameSettings,
    },
    /// Stage a character's join: the next `GAMELOGON` naming it gets in.
    Join {
        /// Matches the reply to the request.
        id: u64,
        /// The game's name.
        name: String,
        /// The password the player typed.
        password: String,
        /// The owning account, checked against the character's.
        account: u64,
        /// The character, by name; the game server loads it from the database.
        character: String,
    },
    /// The open games, for the lobby's game list (link version 2). Answered with
    /// [`FromGameServer::GameList`]; the realm picks the ones a character may see.
    ListGames {
        /// Matches the reply to the request.
        id: u64,
    },
    /// One game, by name, for the lobby's game details (link version 2). Answered with
    /// [`FromGameServer::GameInfo`].
    GameInfo {
        /// Matches the reply to the request.
        id: u64,
        /// The game's name (any case).
        name: String,
    },
    /// Every game, for the admin map (`/d2/games.json`).
    MapGames {
        /// Matches the reply to the request.
        id: u64,
    },
    /// One level's map and marks (`/d2/level.json`).
    MapLevel {
        /// Matches the reply to the request.
        id: u64,
        /// The game.
        game: u16,
        /// The level.
        level: i32,
    },
    /// What stands in a level now (`/d2/live.json`).
    MapLive {
        /// Matches the reply to the request.
        id: u64,
        /// The game.
        game: u16,
        /// The level.
        level: i32,
    },
}

/// What a game server sends the realm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FromGameServer {
    /// The first message on a connection.
    Hello {
        /// The shared secret from both configs.
        token: String,
        /// [`VERSION`].
        version: u32,
    },
    /// The answer to [`ToGameServer::Create`].
    Created {
        /// The request's id.
        id: u64,
        /// The game's token (its id on the game server), or why not.
        result: Result<u16, CreateError>,
    },
    /// The answer to [`ToGameServer::Join`].
    Joined {
        /// The request's id.
        id: u64,
        /// Where the client goes, or why not.
        result: Result<Joined, JoinError>,
    },
    /// The answer to a map request: the JSON body, `None` for no such game or level.
    Map {
        /// The request's id.
        id: u64,
        /// The body.
        json: Option<String>,
    },
    /// The game server's games, pushed every few seconds.
    Games {
        /// Every game.
        games: Vec<PublicGame>,
    },
    /// The answer to [`ToGameServer::ListGames`]: every open game, oldest first.
    GameList {
        /// The request's id.
        id: u64,
        /// The games.
        games: Vec<LobbyGame>,
    },
    /// The answer to [`ToGameServer::GameInfo`]: the game, `None` when there is no such game.
    GameInfo {
        /// The request's id.
        id: u64,
        /// The game.
        game: Option<LobbyGame>,
    },
}

/// What the lobby shows about a game, fixed when it is created: what `MCP_CREATEGAME` carries,
/// and the kind of character that made it (a game is played by characters of its creator's kind).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSettings {
    /// The description typed at creation (up to 31 bytes), empty for none.
    pub description: String,
    /// The most players it takes, 1 to 8.
    pub max_players: u8,
    /// How far from the creator's level a character joining may be, `None` for no restriction.
    pub level_difference: Option<u8>,
    /// The creator's character level when it made the game.
    pub creator_level: u8,
    /// Made by a Lord of Destruction character.
    pub expansion: bool,
    /// Made by a hardcore character.
    pub hardcore: bool,
    /// Made by a ladder character.
    pub ladder: bool,
}

/// A game as the lobby lists and describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyGame {
    /// The game's token (its id on the game server).
    pub token: u16,
    /// The game's name.
    pub name: String,
    /// It has a password.
    pub private: bool,
    /// 0 Normal, 1 Nightmare, 2 Hell.
    pub difficulty: u8,
    /// What it was created with; `None` for a game created over link version 1, which the lobby
    /// knows only by name, difficulty and players.
    pub settings: Option<GameSettings>,
    /// The characters playing in it.
    pub players: Vec<LobbyPlayer>,
    /// Seconds since it was created.
    pub age_secs: u64,
}

/// A character playing in a game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyPlayer {
    /// The character's name.
    pub name: String,
    /// Its class, 0 Amazon to 6 Assassin.
    pub class: u8,
    /// Its character level.
    pub level: u8,
}

/// Why a game could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreateError {
    /// A game by that name exists.
    NameTaken,
    /// The game server holds all the games it can.
    Full,
}

/// Why a join could not be staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinError {
    /// No game by that name.
    NoSuchGame,
    /// Wrong password.
    BadPassword,
    /// Eight players already.
    Full,
    /// The character is not in the database, or not the account's.
    NoSuchCharacter,
}

/// A staged join: what the realm tells the client in `MCP_JOINGAME`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    /// The game's token.
    pub token: u16,
    /// The game's hash, which the client presents in `GAMELOGON`.
    pub hash: u32,
}

/// A game, as the public status page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicGame {
    /// The game's name.
    pub name: String,
    /// It has a password (the page hides its name).
    pub private: bool,
    /// 0 Normal, 1 Nightmare, 2 Hell.
    pub difficulty: u8,
    /// Players in it.
    pub players: usize,
    /// Seconds since it was created.
    pub age_secs: u64,
}

/// Read one message; `Ok(None)` when the other side closed the connection cleanly.
///
/// # Errors
///
/// I/O failure, a message over [`MAX_MESSAGE`], or one that is not the expected JSON.
pub async fn read_message<T: DeserializeOwned>(r: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("link message of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write one message.
///
/// # Errors
///
/// I/O failure, or a message over [`MAX_MESSAGE`].
pub async fn write_message<T: Serialize>(w: &mut (impl AsyncWrite + Unpin), message: &T) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if body.len() > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "link message too large"));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame).await?;
    w.flush().await
}

/// Whether `offered` is the configured token, compared in time independent of where they differ.
#[must_use]
pub fn token_matches(expected: &str, offered: &str) -> bool {
    let (a, b) = (expected.as_bytes(), offered.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip_over_a_stream() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let sent = ToGameServer::Join { id: 7, name: "baal run".into(), password: String::new(), account: 3, character: "Tyrael".into() };
        write_message(&mut a, &sent).await.unwrap();
        let reply = FromGameServer::Joined { id: 7, result: Ok(Joined { token: 2, hash: 0xDEAD_BEEF }) };
        write_message(&mut a, &reply).await.unwrap();
        drop(a);
        assert_eq!(read_message::<ToGameServer>(&mut b).await.unwrap(), Some(sent));
        assert_eq!(read_message::<FromGameServer>(&mut b).await.unwrap(), Some(reply));
        assert_eq!(read_message::<FromGameServer>(&mut b).await.unwrap(), None, "a clean close");
    }

    #[tokio::test]
    async fn the_lobby_messages_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let settings = GameSettings {
            description: "cows only".into(),
            max_players: 4,
            level_difference: Some(5),
            creator_level: 30,
            expansion: true,
            hardcore: false,
            ladder: true,
        };
        let create = ToGameServer::CreateGame { id: 1, name: "moo".into(), password: String::new(), difficulty: 2, settings: settings.clone() };
        let game = LobbyGame {
            token: 9,
            name: "moo".into(),
            private: false,
            difficulty: 2,
            settings: Some(settings),
            players: vec![LobbyPlayer { name: "Tyrael".into(), class: 3, level: 31 }],
            age_secs: 75,
        };
        let sent: Vec<FromGameServer> =
            vec![FromGameServer::GameList { id: 2, games: vec![game.clone()] }, FromGameServer::GameInfo { id: 3, game: None }];
        write_message(&mut a, &create).await.unwrap();
        for m in &sent {
            write_message(&mut a, m).await.unwrap();
        }
        drop(a);
        assert_eq!(read_message::<ToGameServer>(&mut b).await.unwrap(), Some(create));
        for m in sent {
            assert_eq!(read_message::<FromGameServer>(&mut b).await.unwrap(), Some(m));
        }
    }

    #[test]
    fn version_one_messages_keep_their_wire_form() {
        // A version-1 peer must still read and write these exactly as before.
        let create = ToGameServer::Create { id: 4, name: "baal".into(), password: String::new(), difficulty: 1 };
        assert_eq!(serde_json::to_string(&create).unwrap(), r#"{"type":"Create","id":4,"name":"baal","password":"","difficulty":1}"#);
        let hello = FromGameServer::Hello { token: "t".into(), version: MIN_VERSION };
        assert_eq!(serde_json::to_string(&hello).unwrap(), r#"{"type":"Hello","token":"t","version":1}"#);
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_before_reading_it() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(u32::MAX).to_le_bytes()).await.unwrap();
        assert!(read_message::<FromGameServer>(&mut b).await.is_err());
    }

    #[test]
    fn tokens_must_match_exactly_and_not_be_empty() {
        assert!(token_matches("s3cret", "s3cret"));
        assert!(!token_matches("s3cret", "s3cret "));
        assert!(!token_matches("s3cret", "S3cret"));
        assert!(!token_matches("", ""), "an unset token lets nobody in");
    }
}
