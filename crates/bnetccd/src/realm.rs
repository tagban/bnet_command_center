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
//! **Games are not here yet.** Creating or joining one needs a Diablo II game server, which
//! this does not include; the lobby answers "server down" / "game does not exist", which the
//! client shows as ordinary messages. Characters, selection and chat all work without it.
//! With `diablo2.game_server_probe` on, games can be created and joined against the
//! handshake test in [`crate::d2gs`] — the client reaches the Rogue Encampment and no further.
//!
//! Wire layouts are from BNETDocs, cross-checked against the MIT-licensed
//! `jaenster/d2-dedicated-server` realm (a retail 1.14d client renders its replies) — see
//! `docs/LEGAL.md` §1.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::d2::{self, Portrait};
use bnetcc_proto::mcp::{
    self, char_create_result, char_delete_result, char_logon_result, create_game_result,
    join_result, msg, startup_result, Frame,
};
use bnetcc_proto::product;
use bnetcc_storage::Character;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::d2gs::{CreateError, JoinError};
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
            msg::GAMELIST => Some(self.game_list(frame)),
            msg::GAMEINFO => {
                let mut r = frame.reader();
                let request_id = r.u16().unwrap_or(0);
                let mut w = Writer::with_capacity(16);
                // Token -1: "no information", which the client reads before anything else.
                w.u16(request_id).u32(0xFFFF_FFFF).bytes(&[0; 8]);
                Some(Frame::new(msg::GAMEINFO, w.finish()))
            }
            msg::CREATEGAME => Some(self.create_game(frame)),
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

    /// `MCP_CHARCREATE`: `u32 class, u16 status, cstr name`. Reply: `u32 result`.
    async fn char_create(&self, frame: &Frame) -> Option<Frame> {
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
                reply(char_create_result::OK)
            }
            Err(CreateCharacterError::NameTaken) => reply(char_create_result::NAME_TAKEN),
            Err(CreateCharacterError::Backend(e)) => {
                warn!(peer = %self.peer, %name, error = %e, "character creation failed");
                reply(char_create_result::INVALID)
            }
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

    /// `MCP_GAMELIST`: `u16 request id, u32 unknown, cstr filter`. Each game would be its own
    /// packet; with no game server there are none, so the reply is only the end-of-list
    /// marker: `u16 request id, u32 0, u8 0, u32 end token`.
    fn game_list(&self, frame: &Frame) -> Frame {
        let request_id = frame.reader().u16().unwrap_or(0);
        let mut w = Writer::with_capacity(11);
        w.u16(request_id).u32(0).u8(0).u32(mcp::GAMELIST_END_TOKEN);
        Frame::new(msg::GAMELIST, w.finish())
    }

    /// `MCP_CREATEGAME`: `u16 request id, u32 flags, u8, u8, u8, cstr name, cstr password,
    /// cstr description`, difficulty in `(flags >> 12) & 3`. Reply: `u16 request id, u16 token,
    /// u16 unknown, u32 result`.
    fn create_game(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let (flags, name) = (|| {
            let flags = r.u32()?;
            r.bytes(3)?;
            Ok::<_, bnetcc_proto::ProtoError>((flags, r.cstr(STR_MAX)?))
        })()
        .map_or((0, String::new()), |(f, n)| (f, String::from_utf8_lossy(n).into_owned()));
        let password = String::from_utf8_lossy(r.cstr(STR_MAX).unwrap_or_default()).into_owned();
        let (token, result) = if name.is_empty() || name.len() > 15 {
            (0, create_game_result::INVALID_NAME)
        } else if let Some(game_server) = &self.realm.game_server {
            match game_server.create(&name, &password, ((flags >> 12) & 3) as u8) {
                Ok(id) => {
                    info!(peer = %self.peer, character = ?self.selected, game = %name, "game created");
                    (id, create_game_result::OK)
                }
                Err(CreateError::NameTaken) => (0, create_game_result::NAME_TAKEN),
                Err(CreateError::Full) => (0, create_game_result::SERVERS_DOWN),
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
        let character = match &self.selected {
            Some(selected) => self.own_character(selected).await,
            None => None,
        };
        let staged = match (&self.realm.game_server, character, ip) {
            (Some(game_server), Some(character), Some(ip)) => game_server
                .stage_join(&name, &password, character)
                .map(|(id, hash)| (id, hash, ip))
                .map_err(|e| match e {
                    JoinError::NoSuchGame => join_result::NO_SUCH_GAME,
                    JoinError::BadPassword => join_result::BAD_PASSWORD,
                    JoinError::Full => join_result::FULL,
                }),
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
fn experience_of(character: &Character) -> u32 {
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
