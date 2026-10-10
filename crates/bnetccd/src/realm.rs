//! The Diablo II closed realm (MCP): character list, create, select, delete, upgrade, and the
//! game lobby.
//!
//! A closed-realm client logs in over BNCS as usual, asks for the realm list
//! (`SID_QUERYREALMS2`), and logs on to one (`SID_LOGONREALMEX`), which hands it an address
//! and sixteen opaque `u32`s. It then opens a **second** connection to that address and
//! presents those `u32`s in `MCP_STARTUP`. That address is this server's own BNCS port: the
//! realm connection opens with the same `0x01` protocol selector as a login, and the two are
//! told apart by the next byte (`0xFF` begins every BNCS frame; an MCP frame begins with its
//! length). So the realm needs no extra forwarded port. See `docs/DIABLO2.md`.
//!
//! Because one process is both login server and realm, the sixteen `u32`s carry a random
//! handle into [`Node`]'s ticket table rather than anything cryptographic — see
//! [`crate::node::RealmTicket`].
//!
//! **Games are played on a separate game server.** Create and join are passed over the link
//! (`crate::gslink`) to the Diablo II game server, its own program, and the Join Game screen's list
//! and details come from it; while none is linked the lobby answers "Server Down" / "game does not
//! exist" and an empty list, which the client shows as ordinary messages. Characters, selection and
//! chat all work without it.
//!
//! The game list holds only the games the playing character could join, as the realm did: of its
//! kind (expansion or classic, hardcore or softcore, ladder or not), a difficulty it has reached,
//! within the game's level restriction, and not full. Games with a password are listed too — the
//! Join Game screen has a password box. A join the list would not have offered is refused with the
//! client's own reason.
//!
//! Wire layouts are from BNETDocs, cross-checked against the MIT-licensed
//! `jaenster/d2-dedicated-server` realm (a retail 1.14d client renders its replies) — see
//! `docs/LEGAL.md` §1.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::d2::{self, Portrait};
use bnetcc_gslink::{GameSettings, LobbyGame};
use bnetcc_proto::mcp::{
    self, char_create_result, char_delete_result, char_logon_result, create_game_result,
    join_result, msg, startup_result, Frame,
};
use bnetcc_proto::product;
use bnetcc_storage::Character;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::node::{D2Realm, Node, RealmTicket};
use crate::session::SessionLimits;
use crate::storage::CreateCharacterError;

/// Longest string we read from a realm request — names, passwords, descriptions.
const STR_MAX: usize = 64;

/// How long the realm connection may sit idle once started. The client holds it open for as
/// long as the player is in the lobby, sending nothing while they chat.
const IDLE: Duration = Duration::from_secs(24 * 3600);

/// Result for a failed `MCP_CHARUPGRADE`.
const UPGRADE_FAILED: u32 = 0x7A;

/// The most games one `MCP_GAMELIST` reply lists: the 1.14d client keeps 1000 (`0x0077BDC8`, 59
/// bytes each).
const GAMELIST_MAX: usize = 1000;

/// A level difference of this or more means none: the client shows no level line for it, and
/// sends `0xFF` when the box is unticked.
const NO_LEVEL_DIFFERENCE: u8 = 100;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The IPv4 address to send a client to, for the realm or a game. A client on this network
/// gets the address it reached us on — always reachable from where it is. Anyone else gets
/// the configured `diablo2.address` (resolved now, so a dynamic-DNS name stays current), or
/// that same local address if none is configured.
pub async fn client_facing_ipv4(
    peer: SocketAddr,
    local: Option<SocketAddr>,
    realm: &D2Realm,
) -> Option<std::net::Ipv4Addr> {
    use std::net::IpAddr;
    fn v4(ip: IpAddr) -> Option<std::net::Ipv4Addr> {
        match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(v6) => v6.to_ipv4_mapped(),
        }
    }
    let local = local.and_then(|a| v4(a.ip()));
    let peer_is_local =
        v4(peer.ip()).is_some_and(|ip| ip.is_private() || ip.is_loopback() || ip.is_link_local());
    if peer_is_local || realm.address.is_none() {
        return local;
    }
    let configured = realm.address.as_deref().unwrap_or_default();
    if let Ok(ip) = configured.parse::<std::net::Ipv4Addr>() {
        return Some(ip);
    }
    match tokio::net::lookup_host((configured, 0)).await {
        Ok(addrs) => addrs.filter_map(|a| v4(a.ip())).next().or(local),
        Err(e) => {
            warn!(host = configured, error = %e, "could not resolve diablo2.address; using the local address");
            local
        }
    }
}

/// Whether the byte after the protocol selector begins an MCP frame rather than a BNCS one.
/// Waits briefly for it; a client that says nothing is treated as BNCS, so a login client
/// that waits to be spoken to first still gets its `SID_PING`, just a moment later.
pub async fn is_realm_connection(stream: &TcpStream) -> bool {
    let mut b = [0u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(2), stream.peek(&mut b)).await,
        Ok(Ok(1)) if b[0] != bnetcc_proto::bncs::MAGIC
    )
}

/// Serve one realm connection (the protocol selector already consumed).
pub async fn mcp_session(
    mut stream: TcpStream,
    peer: SocketAddr,
    node: Arc<Node>,
    limits: SessionLimits,
) -> std::io::Result<()> {
    let Some(realm) = node.d2_realm.clone() else {
        return Ok(());
    };
    let local = stream.local_addr().ok();
    let mut s = Mcp { node, peer, local, realm, ticket: None, selected: None };
    let mut buf = RecvBuf::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let deadline = if s.ticket.is_some() { IDLE } else { limits.handshake_timeout };
        let n = match tokio::time::timeout(deadline, stream.read(&mut chunk)).await {
            Ok(r) => r?,
            Err(_) => {
                debug!(%peer, "realm connection timed out");
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);

        let mut out = Vec::new();
        let mut close = false;
        loop {
            let frame = match mcp::decode_frame(&mut buf, limits.max_frame.max(mcp::DEFAULT_MAX_FRAME)) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    debug!(%peer, error = %e, "malformed realm frame; closing");
                    close = true;
                    break;
                }
            };
            // Nothing but the handshake before the handshake: every other request is about
            // an account we do not know yet.
            if s.ticket.is_none() && frame.id != msg::STARTUP {
                debug!(%peer, id = %format!("{:#04x}", frame.id), "realm request before MCP_STARTUP; closing");
                close = true;
                break;
            }
            let (replies, keep_open) = s.handle(&frame).await;
            for reply in replies {
                if mcp::encode_frame(&reply, &mut out).is_err() {
                    warn!(%peer, id = reply.id, "realm reply too large to encode");
                }
            }
            if !keep_open {
                close = true;
                break;
            }
        }
        if !out.is_empty() {
            stream.write_all(&out).await?;
        }
        if close {
            return Ok(());
        }
    }
}

struct Mcp {
    node: Arc<Node>,
    peer: SocketAddr,
    /// The address this realm connection arrived on.
    local: Option<SocketAddr>,
    realm: D2Realm,
    ticket: Option<RealmTicket>,
    /// The character chosen with `MCP_CHARLOGON`.
    selected: Option<String>,
}

impl Mcp {
    /// Handle one request: the replies to send, and whether to keep the connection.
    async fn handle(&mut self, frame: &Frame) -> (Vec<Frame>, bool) {
        // Temporary: at INFO until a real client has been seen end to end (docs/DIABLO2.md).
        info!(
            peer = %self.peer,
            id = %format!("{:#04x}", frame.id),
            len = frame.body.len(),
            body = %crate::session::hex_preview(&frame.body),
            "MCP frame in"
        );
        let reply = match frame.id {
            msg::STARTUP => return self.startup(frame),
            msg::CHARLIST2 => self.char_list(frame, true).await,
            msg::CHARLIST => self.char_list(frame, false).await,
            msg::CHARCREATE => self.char_create(frame).await,
            msg::CHARLOGON => self.char_logon(frame).await,
            msg::CHARDELETE => self.char_delete(frame).await,
            msg::CHARUPGRADE => self.char_upgrade(frame).await,
            msg::MOTD => {
                let mut w = Writer::with_capacity(8 + self.node.motd.len());
                w.u8(0).cstr(self.node.motd.as_bytes());
                Some(Frame::new(msg::MOTD, w.finish()))
            }
            msg::GAMELIST => return (self.game_list(frame).await, true),
            msg::GAMEINFO => Some(self.game_info(frame).await),
            msg::CREATEGAME => Some(self.create_game(frame).await),
            msg::JOINGAME => Some(self.join_game(frame).await),
            msg::REQUESTLADDERDATA => Some(self.ladder_data(frame).await),
            // Neither has a reply: the client gave up on a create, or asked a character's rank
            // (a request whose reply the client has no handler for).
            msg::CANCELGAMECREATE | msg::CHARRANK => None,
            other => {
                info!(peer = %self.peer, id = %format!("{other:#04x}"), "unhandled realm request");
                None
            }
        };
        (reply.into_iter().collect(), true)
    }

    fn account_id(&self) -> bnetcc_core::AccountId {
        self.ticket.as_ref().map_or(0, |t| t.account.id)
    }

    /// Whether this connection's client can use Lord of Destruction characters.
    fn expansion_client(&self) -> bool {
        self.ticket.as_ref().is_some_and(|t| t.product == product::D2XP)
    }

    /// `MCP_STARTUP`: `u32 cookie, u32 status, u32[2] chunk1, u32[12] chunk2, cstr name`. The
    /// ticket id is `chunk1`, as `SID_LOGONREALMEX` wrote it.
    fn startup(&mut self, frame: &Frame) -> (Vec<Frame>, bool) {
        let mut r = frame.reader();
        let parsed = (|| {
            let cookie = r.u32()?;
            let _status = r.u32()?;
            let lo = r.u32()?;
            let hi = r.u32()?;
            Ok::<_, bnetcc_proto::ProtoError>((cookie, u64::from(lo) | (u64::from(hi) << 32)))
        })();
        let ticket = parsed.ok().and_then(|(cookie, id)| self.node.realm_ticket(id, cookie));
        let result = match ticket {
            Some(t) => {
                info!(peer = %self.peer, account = %t.account.name, product = %t.product, "realm connection started");
                self.ticket = Some(t);
                startup_result::OK
            }
            None => {
                info!(peer = %self.peer, "realm connection presented an unknown logon; refused");
                startup_result::UNAVAILABLE
            }
        };
        let mut w = Writer::with_capacity(4);
        w.u32(result);
        (vec![Frame::new(msg::STARTUP, w.finish())], result == startup_result::OK)
    }

    /// The characters this connection may see: all of them for Lord of Destruction; the
    /// classic ones for a classic client, which cannot play an expansion character.
    async fn visible_characters(&self) -> Vec<Character> {
        let expansion = self.expansion_client();
        match self.node.characters(self.account_id()).await {
            Ok(chars) => chars
                .into_iter()
                .filter(|c| expansion || c.status & d2::status::EXPANSION == 0)
                .collect(),
            Err(e) => {
                warn!(peer = %self.peer, error = %e, "could not load characters");
                Vec::new()
            }
        }
    }

    /// `MCP_CHARLIST2` (`u32 requested`) or the older `MCP_CHARLIST`. Reply: `u16 requested,
    /// u32 total, u16 returned`, then per character (`u32 expiry` in the second form only)
    /// `cstr name, cstr portrait`.
    async fn char_list(&self, frame: &Frame, with_expiry: bool) -> Option<Frame> {
        let requested = frame.reader().u32().unwrap_or(8);
        let chars = self.visible_characters().await;
        let total = chars.len() as u32;
        let returned = chars.len().min(requested as usize);
        let mut w = Writer::with_capacity(8 + returned * 64);
        w.u16(requested.min(u32::from(u16::MAX)) as u16).u32(total).u16(returned as u16);
        for c in chars.iter().take(returned) {
            if with_expiry {
                // Characters on this realm do not expire. Far future, never "expired".
                w.u32(0xFFFF_FFFF);
            }
            w.cstr(c.name.as_bytes()).cstr(&portrait(c).charlist_bytes(total));
        }
        debug!(peer = %self.peer, total, returned, "character list");
        Some(Frame::new(frame.id, w.finish()))
    }

    /// `MCP_CHARCREATE`: `u32 class, u16 status, cstr name`. Reply: `u32 result`. A character made
    /// is the one the connection plays: the retail client goes straight on to `MCP_MOTD`, chat and
    /// `MCP_CREATEGAME`/`MCP_JOINGAME` without an `MCP_CHARLOGON` (seen with a real client,
    /// 2026-09-15).
    async fn char_create(&mut self, frame: &Frame) -> Option<Frame> {
        let reply = |result: u32| {
            let mut w = Writer::with_capacity(4);
            w.u32(result);
            Some(Frame::new(msg::CHARCREATE, w.finish()))
        };
        let mut r = frame.reader();
        let parsed = (|| {
            let class = r.u32()?;
            let status = r.u16()?;
            let name = r.cstr(STR_MAX)?;
            Ok::<_, bnetcc_proto::ProtoError>((class, status, String::from_utf8_lossy(name).into_owned()))
        })();
        let Ok((class, status_word, name)) = parsed else {
            return reply(char_create_result::INVALID);
        };
        // The low byte is the .d2s status the creation screen's checkboxes set.
        let status = (status_word & 0xFF) as u8 & d2::status::CREATABLE;
        let expansion = status & d2::status::EXPANSION != 0;
        let class_ok = u8::try_from(class).is_ok_and(|c| {
            d2::class::is_valid(c) && (expansion || !d2::class::expansion_only(c))
        });
        if !d2::valid_character_name(&name) || !class_ok || (expansion && !self.expansion_client()) {
            info!(peer = %self.peer, %name, class, status, "character creation refused: invalid");
            return reply(char_create_result::INVALID);
        }
        let owned = self.node.characters(self.account_id()).await.map_or(0, |c| c.len());
        if owned >= self.realm.max_characters {
            info!(peer = %self.peer, %name, owned, "character creation refused: account is full");
            return reply(char_create_result::INVALID);
        }
        let now = now_secs();
        let character = Character {
            account: self.account_id(),
            name: name.clone(),
            class: class as u8,
            status,
            level: 1,
            progression: 0,
            created_at: now,
            last_played: now,
            save: None,
        };
        match self.node.create_character(character).await {
            Ok(()) => {
                info!(
                    peer = %self.peer,
                    account = %self.ticket.as_ref().map_or("", |t| t.account.name.as_str()),
                    %name,
                    class = d2::class::name(class as u8),
                    status = %format!("{status:#04x}"),
                    "character created"
                );
                self.selected = Some(name);
                reply(char_create_result::OK)
            }
            Err(CreateCharacterError::NameTaken) => reply(char_create_result::NAME_TAKEN),
            Err(CreateCharacterError::Backend(e)) => {
                warn!(peer = %self.peer, %name, error = %e, "character creation failed");
                reply(char_create_result::INVALID)
            }
        }
    }

    /// Whether the character this connection plays is a hardcore one that has died.
    async fn selected_is_dead(&self) -> bool {
        match &self.selected {
            Some(selected) => self.own_character(selected).await.is_some_and(|c| is_dead_hardcore(&c)),
            None => false,
        }
    }

    /// One of this account's characters that this client can use, by name.
    async fn own_character(&self, name: &str) -> Option<Character> {
        self.node.character_by_name(name).await.filter(|c| {
            c.account == self.account_id()
                && (self.expansion_client() || c.status & d2::status::EXPANSION == 0)
        })
    }

    /// `MCP_REQUESTLADDERDATA`: `u8 ladder type, u16 start` — sixteen entries of that ladder from
    /// the zero-based start ([`ladder_reply`]).
    async fn ladder_data(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let ladder = r.u8().unwrap_or(0);
        let start = r.u16().unwrap_or(0);
        let characters = self.node.all_characters().await;
        Frame::new(msg::REQUESTLADDERDATA, ladder_reply(&characters, ladder, start, self.account_id()))
    }

    /// `MCP_CHARLOGON`: `cstr name`. Reply: `u32 result`.
    async fn char_logon(&mut self, frame: &Frame) -> Option<Frame> {
        let name = String::from_utf8_lossy(frame.reader().cstr(STR_MAX).unwrap_or_default()).into_owned();
        let result = match self.own_character(&name).await {
            Some(mut c) => {
                c.last_played = now_secs();
                let _ = self.node.update_character(c.clone()).await;
                info!(peer = %self.peer, character = %c.name, "character selected");
                self.selected = Some(c.name);
                char_logon_result::OK
            }
            None => char_logon_result::NOT_FOUND,
        };
        let mut w = Writer::with_capacity(4);
        w.u32(result);
        Some(Frame::new(msg::CHARLOGON, w.finish()))
    }

    /// `MCP_CHARDELETE`: `u16 request id, cstr name`. Reply: `u16 request id, u32 result`.
    async fn char_delete(&mut self, frame: &Frame) -> Option<Frame> {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let name = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let deleted = matches!(self.node.delete_character(self.account_id(), &name).await, Ok(true));
        if deleted {
            info!(peer = %self.peer, character = %name, "character deleted");
            if self.selected.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&name)) {
                self.selected = None;
            }
        }
        let mut w = Writer::with_capacity(6);
        w.u16(request_id)
            .u32(if deleted { char_delete_result::OK } else { char_delete_result::NOT_FOUND });
        Some(Frame::new(msg::CHARDELETE, w.finish()))
    }

    /// `MCP_CHARUPGRADE`: `cstr name` — convert a classic character to Lord of Destruction.
    /// Reply: `u32 result`. Already being expansion is success: the player has what they asked.
    async fn char_upgrade(&self, frame: &Frame) -> Option<Frame> {
        let name = String::from_utf8_lossy(frame.reader().cstr(STR_MAX).unwrap_or_default()).into_owned();
        let result = match self.node.character_by_name(&name).await {
            Some(c) if c.account == self.account_id() && self.expansion_client() => {
                if c.status & d2::status::EXPANSION != 0 {
                    0
                } else {
                    let mut upgraded = c;
                    upgraded.status |= d2::status::EXPANSION;
                    match self.node.update_character(upgraded).await {
                        Ok(true) => {
                            info!(peer = %self.peer, character = %name, "character upgraded to expansion");
                            0
                        }
                        _ => UPGRADE_FAILED,
                    }
                }
            }
            Some(_) => UPGRADE_FAILED,
            None => char_logon_result::NOT_FOUND,
        };
        let mut w = Writer::with_capacity(4);
        w.u32(result);
        Some(Frame::new(msg::CHARUPGRADE, w.finish()))
    }

    /// The character this connection plays, if it has chosen or made one.
    async fn playing(&self) -> Option<Character> {
        match &self.selected {
            Some(selected) => self.own_character(selected).await,
            None => None,
        }
    }

    /// `MCP_GAMELIST`: `u16 request id, u32 flags (0x800 for a hardcore character), cstr filter`.
    /// The reply is one `MCP_GAMELIST` packet per game the playing character may join whose name
    /// holds the filter ([`listable`]), then the end marker ([`mcp::gamelist_end`]). The 1.14d
    /// client asks again every 15 seconds while the screen is open.
    async fn game_list(&self, frame: &Frame) -> Vec<Frame> {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let _flags = r.u32().unwrap_or(0);
        let filter = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let games = match (&self.realm.game_server, self.playing().await) {
            (Some(game_server), Some(character)) => {
                game_server.list_games().await.unwrap_or_default().into_iter().filter(|g| listable(&character, g, &filter)).collect()
            }
            _ => Vec::new(),
        };
        debug!(peer = %self.peer, games = games.len(), %filter, "game list");
        let mut frames: Vec<Frame> = games
            .iter()
            .take(GAMELIST_MAX)
            .map(|g| {
                let description = g.settings.as_ref().map_or("", |s| s.description.as_str());
                let players = u8::try_from(g.players.len()).unwrap_or(u8::MAX);
                let body = mcp::gamelist_entry(request_id, u32::from(g.token), players, status_of(g), g.name.as_bytes(), description.as_bytes());
                Frame::new(msg::GAMELIST, body)
            })
            .collect();
        frames.push(Frame::new(msg::GAMELIST, mcp::gamelist_end(request_id)));
        frames
    }

    /// `MCP_GAMEINFO`: `u16 request id, cstr name` — the Join Game screen asks about the game
    /// selected in the list. The reply is [`mcp::gameinfo_reply`], or [`mcp::gameinfo_none`] for a
    /// game that is gone.
    async fn game_info(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let name = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let game = match &self.realm.game_server {
            Some(game_server) => game_server.game_info(&name).await.ok().flatten(),
            None => None,
        };
        Frame::new(msg::GAMEINFO, game.map_or_else(|| mcp::gameinfo_none(request_id), |g| gameinfo_body(request_id, &g)))
    }

    /// `MCP_CREATEGAME`: `u16 request id, u32 flags, u8 1, u8 level difference (0xFF for none),
    /// u8 max players, cstr name, cstr password, cstr description`, difficulty in
    /// `(flags >> 12) & 3` (1.14d `0x0044A400`). Reply: `u16 request id, u16 token, u16 unknown,
    /// u32 result`. The game is of the creating character's kind, and the lobby shows its level.
    async fn create_game(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let (flags, level_difference, max_players, name) = (|| {
            let flags = r.u32()?;
            let _ = r.u8()?;
            let level_difference = r.u8()?;
            let max_players = r.u8()?;
            Ok::<_, bnetcc_proto::ProtoError>((flags, level_difference, max_players, r.cstr(STR_MAX)?))
        })()
        .map_or((0, 0xFF, 8, String::new()), |(f, l, m, n)| (f, l, m, String::from_utf8_lossy(n).into_owned()));
        let password = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let description = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let creator = self.playing().await;
        let settings = GameSettings {
            description: clip(&description, mcp::GAME_DESCRIPTION_MAX),
            max_players: if (1..=8).contains(&max_players) { max_players } else { 8 },
            level_difference: (level_difference < NO_LEVEL_DIFFERENCE).then_some(level_difference),
            creator_level: creator.as_ref().map_or(1, |c| c.level.max(1)),
            expansion: creator.as_ref().map_or(self.expansion_client(), |c| c.status & d2::status::EXPANSION != 0),
            hardcore: creator.as_ref().is_some_and(|c| c.status & d2::status::HARDCORE != 0),
            ladder: creator.as_ref().is_some_and(|c| c.status & d2::status::LADDER != 0),
        };
        let (token, result) = if name.is_empty() || name.len() > 15 {
            (0, create_game_result::INVALID_NAME)
        } else if self.selected_is_dead().await {
            info!(peer = %self.peer, character = ?self.selected, game = %name, "game creation refused: a dead hardcore character");
            (0, create_game_result::DEAD_HARDCORE)
        } else if let Some(game_server) = &self.realm.game_server {
            match game_server.create(&name, &password, ((flags >> 12) & 3) as u8, &settings).await {
                Ok(Ok(id)) => {
                    info!(peer = %self.peer, character = ?self.selected, game = %name, "game created");
                    (id, create_game_result::OK)
                }
                Ok(Err(bnetcc_gslink::CreateError::NameTaken)) => (0, create_game_result::NAME_TAKEN),
                Ok(Err(bnetcc_gslink::CreateError::Full)) => (0, create_game_result::SERVERS_DOWN),
                Err(crate::gslink::LinkError::Down) => {
                    info!(peer = %self.peer, game = %name, "game creation requested; the Diablo II game server is not linked");
                    (0, create_game_result::SERVERS_DOWN)
                }
            }
        } else {
            info!(
                peer = %self.peer,
                character = ?self.selected,
                game = %name,
                "game creation requested; no Diablo II game server is configured"
            );
            (0, create_game_result::SERVERS_DOWN)
        };
        let mut w = Writer::with_capacity(10);
        w.u16(request_id).u16(token).u16(0).u32(result);
        Frame::new(msg::CREATEGAME, w.finish())
    }

    /// `MCP_JOINGAME`: `u16 request id, cstr name, cstr password`. Reply: `u16 request id,
    /// u16 token, u16 unknown, u32 game server IP, u32 game hash, u32 result`. Every field is
    /// present even on failure — the client reads them all, and only then the result. The
    /// token is the game's id on the game server and the IP is in network order: the client
    /// dials that address on port 4000 and presents the token and hash in `GAMELOGON`.
    async fn join_game(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let name = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let password = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();

        let ip = client_facing_ipv4(self.peer, self.local, &self.realm).await;
        let character = self.playing().await;
        // What the game list would not have offered this character, refused for the client's own
        // reason; a game the game server cannot describe (version 1) is left to it.
        let refusal = match (&self.realm.game_server, &character) {
            (Some(game_server), Some(c)) if !is_dead_hardcore(c) => match game_server.game_info(&name).await {
                Ok(Some(game)) => join_refusal(c, &game),
                _ => None,
            },
            _ => None,
        };
        let staged = match (&self.realm.game_server, character, ip) {
            // A dead hardcore character stays on the list but plays no more (status 0x0C).
            (_, Some(character), _) if is_dead_hardcore(&character) => {
                info!(peer = %self.peer, character = %character.name, game = %name, "join refused: a dead hardcore character");
                Err(join_result::DEAD_HARDCORE)
            }
            (_, Some(character), _) if refusal.is_some() => {
                info!(peer = %self.peer, character = %character.name, game = %name, result = ?refusal, "join refused: not a game for this character");
                Err(refusal.unwrap_or(join_result::NO_SUCH_GAME))
            }
            (Some(game_server), Some(character), Some(ip)) => match game_server.join(&name, &password, character.account, &character.name).await {
                Ok(Ok(joined)) => Ok((joined.token, joined.hash, ip)),
                Ok(Err(e)) => Err(match e {
                    bnetcc_gslink::JoinError::NoSuchGame | bnetcc_gslink::JoinError::NoSuchCharacter => join_result::NO_SUCH_GAME,
                    bnetcc_gslink::JoinError::BadPassword => join_result::BAD_PASSWORD,
                    bnetcc_gslink::JoinError::Full => join_result::FULL,
                }),
                Err(crate::gslink::LinkError::Down) => Err(join_result::NO_SUCH_GAME),
            },
            _ => Err(join_result::NO_SUCH_GAME),
        };
        let mut w = Writer::with_capacity(18);
        match staged {
            Ok((id, hash, ip)) => {
                info!(peer = %self.peer, character = ?self.selected, game = %name, id, %ip, "sending client to the game server");
                w.u16(request_id).u16(id).u16(0).bytes(&ip.octets()).u32(hash).u32(join_result::OK);
            }
            Err(result) => {
                w.u16(request_id).u16(0).u16(0).u32(0).u32(0).u32(result);
            }
        }
        Frame::new(msg::JOINGAME, w.finish())
    }
}

/// The hardest difficulty a character has reached: one per difficulty completed, from its act
/// progress (`.d2s` `0x25`, four acts a difficulty in the classic game and five in the expansion).
#[must_use]
pub fn highest_difficulty(c: &Character) -> u8 {
    let acts = if c.status & d2::status::EXPANSION != 0 { 5 } else { 4 };
    (c.progression / acts).min(2)
}

/// Why `c` may not join `game`, as `MCP_JOINGAME` says it, or `None` when it may (as far as the
/// lobby knows: the password and the room left are the game server's to check). A game created over
/// link version 1 is known only by difficulty.
#[must_use]
pub fn join_refusal(c: &Character, game: &LobbyGame) -> Option<u32> {
    let has = |bit: u8| c.status & bit != 0;
    if let Some(s) = &game.settings {
        // The client has a message for one side of each pair; the other side never sees the game.
        match (s.hardcore, has(d2::status::HARDCORE)) {
            (true, false) => return Some(join_result::NOT_HARDCORE),
            (false, true) => return Some(join_result::NO_SUCH_GAME),
            _ => {}
        }
        match (s.expansion, has(d2::status::EXPANSION)) {
            (true, false) => return Some(join_result::NOT_EXPANSION),
            (false, true) => return Some(join_result::NOT_CLASSIC),
            _ => {}
        }
        match (s.ladder, has(d2::status::LADDER)) {
            (true, false) => return Some(join_result::NOT_LADDER),
            (false, true) => return Some(join_result::NO_SUCH_GAME),
            _ => {}
        }
    }
    if game.difficulty > highest_difficulty(c) {
        return Some(if game.difficulty >= 2 { join_result::NO_HELL } else { join_result::NO_NIGHTMARE });
    }
    if let Some(s) = &game.settings {
        if let Some(difference) = s.level_difference {
            if c.level.abs_diff(s.creator_level) > difference {
                return Some(join_result::LEVEL_REQUIREMENT);
            }
        }
        if game.players.len() >= usize::from(s.max_players.clamp(1, 8)) {
            return Some(join_result::FULL);
        }
    }
    None
}

/// Whether the game list shows `game` to `c`: a game it could join ([`join_refusal`]) whose name
/// holds `filter` (any case; an empty filter, which the 1.14d client always sends, holds every name).
#[must_use]
pub fn listable(c: &Character, game: &LobbyGame, filter: &str) -> bool {
    join_refusal(c, game).is_none() && (filter.is_empty() || game.name.to_lowercase().contains(&filter.to_lowercase()))
}

/// A game's status in the lobby's replies: its flags ([`mcp::game_flags`]).
fn status_of(game: &LobbyGame) -> u32 {
    let s = game.settings.as_ref();
    mcp::game_flags(game.difficulty, s.is_some_and(|s| s.hardcore), s.is_some_and(|s| s.expansion), s.is_some_and(|s| s.ladder))
}

/// The `MCP_GAMEINFO` reply describing `game` ([`mcp::gameinfo_reply`]).
#[must_use]
pub fn gameinfo_body(request_id: u16, game: &LobbyGame) -> Vec<u8> {
    let characters: Vec<mcp::GameInfoCharacter<'_>> =
        game.players.iter().map(|p| mcp::GameInfoCharacter { class: p.class, level: p.level, name: p.name.as_bytes() }).collect();
    let s = game.settings.as_ref();
    mcp::gameinfo_reply(
        request_id,
        &mcp::GameInfo {
            status: status_of(game),
            uptime_secs: u32::try_from(game.age_secs).unwrap_or(u32::MAX),
            creator_level: s.map_or(0, |s| s.creator_level),
            level_difference: s.and_then(|s| s.level_difference).unwrap_or(0xFF),
            max_players: s.map_or(8, |s| s.max_players),
            description: s.map_or(&b""[..], |s| s.description.as_bytes()),
            characters: &characters,
        },
    )
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn clip(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Whether a stored character is a hardcore one that has died: the Diablo II game server marks it
/// so (status `0x04` and `0x08`) when it dies, and it can create or join no game after.
#[must_use]
pub fn is_dead_hardcore(c: &Character) -> bool {
    c.status & d2::status::HARDCORE != 0 && c.status & d2::status::DEAD != 0
}

/// The portrait a stored character is drawn with.
#[must_use]
pub fn portrait(c: &Character) -> Portrait {
    Portrait { class: c.class, status: c.status, level: c.level, progression: c.progression }
}

/// Which characters a Diablo II ladder type lists (`MCP_REQUESTLADDERDATA`, BNETDocs): hardcore
/// or softcore, classic or expansion, and one class or all. Classic ladders are `0x00`–`0x05`
/// (hardcore) and `0x09`–`0x0E` (softcore), expansion ones `0x13`–`0x1A` and `0x1B`–`0x22`; the
/// first of each run is the overall ladder, the rest one class each in class order.
#[must_use]
pub fn ladder_filter(ladder: u8) -> Option<(bool, bool, Option<u8>)> {
    let (hardcore, expansion, base, classes) = match ladder {
        0x00..=0x05 => (true, false, 0x00, 5),
        0x09..=0x0E => (false, false, 0x09, 5),
        0x13..=0x1A => (true, true, 0x13, 7),
        0x1B..=0x22 => (false, true, 0x1B, 7),
        _ => return None,
    };
    let offset = ladder - base;
    (offset <= classes).then(|| (hardcore, expansion, offset.checked_sub(1)))
}

/// A character's experience, from its `.d2s` (0 without one).
pub(crate) fn experience_of(character: &Character) -> u32 {
    character.save.as_deref().and_then(|b| d2_formats::d2s::Save::parse(b).ok()).map_or(0, |s| s.stat(13))
}

/// The `MCP_REQUESTLADDERDATA` reply for `ladder` from `start`: the ladder characters of that
/// kind, most experienced first, down to rank 500 (`bnetcc_core::ladder::MAX_RANK`), sixteen at a
/// time (BNETDocs layout). Header `u8` ladder type,
/// `u16` total payload size, `u16` this chunk's size, `u16` its offset — one chunk here — then
/// `u32` first entry's rank, `u32` entries, `u32` 16, and per entry `u32` experience low and high
/// words, `u8` flags (class, `0x08` the asker's own, `0x10` dead, `0x20` hardcore, `0x40`
/// expansion), `u8` completed acts, `u16` level, `char[16]` name.
#[must_use]
pub fn ladder_reply(characters: &[Character], ladder: u8, start: u16, asker: bnetcc_core::AccountId) -> Vec<u8> {
    let mut listed: Vec<(&Character, u32)> = match ladder_filter(ladder) {
        Some((hardcore, expansion, class)) => characters
            .iter()
            .filter(|c| c.status & d2::status::LADDER != 0)
            .filter(|c| (c.status & d2::status::HARDCORE != 0) == hardcore && (c.status & d2::status::EXPANSION != 0) == expansion)
            .filter(|c| class.map_or(true, |k| c.class == k))
            .map(|c| (c, experience_of(c)))
            .collect(),
        None => Vec::new(),
    };
    listed.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.level.cmp(&a.0.level)).then_with(|| a.0.name.cmp(&b.0.name)));
    listed.truncate(bnetcc_core::ladder::MAX_RANK as usize);
    let page: Vec<&(&Character, u32)> = listed.iter().skip(usize::from(start)).take(16).collect();
    let mut payload = Writer::with_capacity(12 + page.len() * 28);
    payload.u32(u32::from(start)).u32(page.len() as u32).u32(16);
    for (c, experience) in page {
        let mut flags = c.class & 7;
        if c.account == asker {
            flags |= 0x08;
        }
        if c.status & d2::status::HARDCORE != 0 {
            flags |= 0x20;
            if c.status & 0x08 != 0 {
                flags |= 0x10;
            }
        }
        if c.status & d2::status::EXPANSION != 0 {
            flags |= 0x40;
        }
        let mut name = [0u8; 16];
        let n = c.name.len().min(15);
        name[..n].copy_from_slice(&c.name.as_bytes()[..n]);
        payload.u32(*experience).u32(0).u8(flags).u8(c.progression).u16(u16::from(c.level)).bytes(&name);
    }
    let payload = payload.finish();
    let mut w = Writer::with_capacity(7 + payload.len());
    w.u8(ladder).u16(payload.len() as u16).u16(payload.len() as u16).u16(0).bytes(&payload);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hero(name: &str, account: u64, class: u8, status: u8, level: u8, experience: u32) -> Character {
        let save = d2_formats::d2s::Save::new(name, class, status, 0, &[(12, u32::from(level)), (13, experience)]);
        Character { account, name: name.into(), class, status, level, progression: 1, created_at: 0, last_played: 0, save: Some(save.to_bytes()) }
    }

    fn lobby_game(name: &str, difficulty: u8, settings: Option<GameSettings>, players: usize) -> LobbyGame {
        LobbyGame {
            token: 1,
            name: name.into(),
            private: false,
            difficulty,
            settings,
            players: (0..players).map(|i| bnetcc_gslink::LobbyPlayer { name: format!("p{i}"), class: 0, level: 1 }).collect(),
            age_secs: 0,
        }
    }

    fn made_by(expansion: bool, hardcore: bool, ladder: bool) -> GameSettings {
        GameSettings { description: String::new(), max_players: 8, level_difference: None, creator_level: 20, expansion, hardcore, ladder }
    }

    #[test]
    fn the_list_offers_only_games_the_character_could_join() {
        use d2::status::{EXPANSION, HARDCORE, LADDER};
        let soft = hero("Soft", 1, 4, EXPANSION, 20, 0);
        let lod = Some(made_by(true, false, false));
        assert!(listable(&soft, &lobby_game("cows", 0, lod.clone(), 3), ""));
        assert!(listable(&soft, &lobby_game("old", 0, None, 0), ""), "a version-1 game is known by difficulty alone");

        assert_eq!(join_refusal(&soft, &lobby_game("hc", 0, Some(made_by(true, true, false)), 0)), Some(join_result::NOT_HARDCORE));
        assert_eq!(join_refusal(&soft, &lobby_game("classic", 0, Some(made_by(false, false, false)), 0)), Some(join_result::NOT_CLASSIC));
        assert_eq!(join_refusal(&soft, &lobby_game("ladder", 0, Some(made_by(true, false, true)), 0)), Some(join_result::NOT_LADDER));
        let classic = hero("Classic", 1, 4, 0, 20, 0);
        assert_eq!(join_refusal(&classic, &lobby_game("lod", 0, lod.clone(), 0)), Some(join_result::NOT_EXPANSION));
        let hardcore_ladder = hero("Hcl", 1, 4, EXPANSION | HARDCORE | LADDER, 20, 0);
        assert_eq!(join_refusal(&hardcore_ladder, &lobby_game("lod", 0, lod.clone(), 0)), Some(join_result::NO_SUCH_GAME));
        assert_eq!(join_refusal(&hardcore_ladder, &lobby_game("same", 0, Some(made_by(true, true, true)), 0)), None);

        // Difficulty: five acts a difficulty in the expansion, four in the classic game.
        let mut nightmare = soft.clone();
        nightmare.progression = 5;
        assert_eq!((highest_difficulty(&soft), highest_difficulty(&nightmare)), (0, 1));
        let mut classic_nightmare = classic.clone();
        classic_nightmare.progression = 4;
        assert_eq!(highest_difficulty(&classic_nightmare), 1);
        assert_eq!(join_refusal(&soft, &lobby_game("nm", 1, lod.clone(), 0)), Some(join_result::NO_NIGHTMARE));
        assert_eq!(join_refusal(&nightmare, &lobby_game("hell", 2, lod.clone(), 0)), Some(join_result::NO_HELL));
        assert_eq!(join_refusal(&nightmare, &lobby_game("nm", 1, lod.clone(), 0)), None);
        assert_eq!(join_refusal(&nightmare, &lobby_game("normal", 0, lod.clone(), 0)), None, "an easier difficulty is still open");

        // The level restriction is around the creator's level (20 here), and a full game is not offered.
        let restricted = |d| Some(GameSettings { level_difference: Some(d), ..made_by(true, false, false) });
        assert_eq!(join_refusal(&soft, &lobby_game("r0", 0, restricted(0), 0)), None, "level 20 in a level-20 game");
        let low = hero("Low", 1, 4, EXPANSION, 14, 0);
        assert_eq!(join_refusal(&low, &lobby_game("r5", 0, restricted(5), 0)), Some(join_result::LEVEL_REQUIREMENT));
        assert_eq!(join_refusal(&low, &lobby_game("r6", 0, restricted(6), 0)), None);
        let four = Some(GameSettings { max_players: 4, ..made_by(true, false, false) });
        assert_eq!(join_refusal(&soft, &lobby_game("four", 0, four.clone(), 4)), Some(join_result::FULL));
        assert!(listable(&soft, &lobby_game("four", 0, four, 3), ""));

        // The filter is any part of the name, in any case.
        assert!(listable(&soft, &lobby_game("Baal Run", 0, lod.clone(), 0), "baal"));
        assert!(listable(&soft, &lobby_game("Baal Run", 0, lod.clone(), 0), "RUN"));
        assert!(!listable(&soft, &lobby_game("Baal Run", 0, lod, 0), "cows"));
    }

    #[test]
    fn game_info_describes_the_game_and_its_characters() {
        let settings = GameSettings { description: "chaos".into(), max_players: 6, level_difference: Some(10), ..made_by(true, false, true) };
        let mut game = lobby_game("cs", 2, Some(settings), 0);
        game.players = vec![bnetcc_gslink::LobbyPlayer { name: "Tyrael".into(), class: 3, level: 85 }];
        game.age_secs = 61;
        let body = gameinfo_body(4, &game);
        assert_eq!(u32::from_le_bytes(body[2..6].try_into().unwrap()), mcp::game_flags(2, false, true, true));
        assert_eq!(u32::from_le_bytes(body[6..10].try_into().unwrap()), 61);
        assert_eq!(&body[10..14], &[20, 10, 6, 1], "creator level, difference, max players, characters");
        assert_eq!((body[14], body[30]), (3, 85), "class and level");
        assert_eq!(&body[46..], b"chaos\0Tyrael\0");

        // A game made over link version 1: no level line, no player limit.
        let body = gameinfo_body(4, &lobby_game("old", 0, None, 0));
        assert_eq!(&body[10..14], &[0, 0xFF, 8, 0]);
        assert_eq!(clip("ééé", 3), "é", "cut on a character boundary");
    }

    #[test]
    fn a_ladder_lists_its_kind_of_ladder_character_by_experience() {
        use d2::status::{EXPANSION, HARDCORE, LADDER};
        let chars = vec![
            hero("Low", 1, 4, LADDER | EXPANSION, 5, 2_000),
            hero("High", 2, 1, LADDER | EXPANSION, 20, 900_000),
            hero("Assassin", 3, 6, LADDER | EXPANSION, 12, 50_000),
            hero("NotLadder", 4, 4, EXPANSION, 90, 9_999_999),
            hero("Hardcore", 5, 4, LADDER | EXPANSION | HARDCORE | 0x08, 30, 1_000_000),
            hero("Classic", 6, 4, LADDER, 10, 30_000),
        ];
        assert_eq!(ladder_filter(0x1B), Some((false, true, None)));
        assert_eq!(ladder_filter(0x20), Some((false, true, Some(4))));
        assert_eq!(ladder_filter(0x0E), Some((false, false, Some(4))));
        assert_eq!(ladder_filter(0x07), None);

        let reply = ladder_reply(&chars, 0x1B, 0, 1);
        let total = u16::from_le_bytes([reply[1], reply[2]]) as usize;
        assert_eq!(reply.len(), 7 + total);
        assert_eq!(u32::from_le_bytes(reply[11..15].try_into().unwrap()), 3, "softcore expansion ladder characters");
        let name_at = |i: usize| String::from_utf8_lossy(&reply[19 + i * 28 + 12..19 + i * 28 + 28]).trim_end_matches('\0').to_string();
        assert_eq!((name_at(0), name_at(1), name_at(2)), ("High".into(), "Assassin".into(), "Low".into()));
        assert_eq!(u32::from_le_bytes(reply[19..23].try_into().unwrap()), 900_000);
        assert_eq!(reply[19 + 2 * 28 + 8], 4 | 0x08 | 0x40, "the asker's own Barbarian, highlighted");

        let hardcore = ladder_reply(&chars, 0x13, 0, 1);
        assert_eq!(hardcore[19 + 8], 4 | 0x10 | 0x20 | 0x40, "a dead hardcore character");
        let barbarians = ladder_reply(&chars, 0x20, 0, 1);
        assert_eq!(u32::from_le_bytes(barbarians[11..15].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(ladder_reply(&chars, 0x1B, 16, 1)[11..15].try_into().unwrap()), 0, "past the end");

        let crowd: Vec<Character> = (0..510).map(|i| hero(&format!("c{i}"), 7, 4, LADDER | EXPANSION, 10, 100_000 - i)).collect();
        assert_eq!(u32::from_le_bytes(ladder_reply(&crowd, 0x1B, 496, 1)[11..15].try_into().unwrap()), 4, "ranks 497 to 500, and no further");
    }
}
