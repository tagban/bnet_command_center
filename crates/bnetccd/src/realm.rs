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
    let mut s = Mcp { node, peer, realm, ticket: None, selected: None };
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
            msg::JOINGAME => Some(self.join_game(frame)),
            // An empty ladder: the all-zero form the client clears its list with.
            msg::REQUESTLADDERDATA => Some(Frame::new(msg::REQUESTLADDERDATA, vec![0; 14])),
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
    /// cstr description`. Reply: `u16 request id, u16 token, u16 unknown, u32 result`.
    fn create_game(&self, frame: &Frame) -> Frame {
        let mut r = frame.reader();
        let request_id = r.u16().unwrap_or(0);
        let name = (|| {
            r.u32()?;
            r.bytes(3)?;
            r.cstr(STR_MAX)
        })()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();
        let result = if name.is_empty() || name.len() > 15 {
            create_game_result::INVALID_NAME
        } else {
            info!(
                peer = %self.peer,
                character = ?self.selected,
                game = %name,
                "game creation requested; no Diablo II game server is configured"
            );
            create_game_result::SERVERS_DOWN
        };
        let mut w = Writer::with_capacity(10);
        w.u16(request_id).u16(0).u16(0).u32(result);
        Frame::new(msg::CREATEGAME, w.finish())
    }

    /// `MCP_JOINGAME`: `u16 request id, cstr name, cstr password`. Reply: `u16 request id,
    /// u16 token, u16 unknown, u32 game server IP, u32 game hash, u32 result`. Every field is
    /// present even on failure — the client reads them all, and only then the result.
    fn join_game(&self, frame: &Frame) -> Frame {
        let request_id = frame.reader().u16().unwrap_or(0);
        let mut w = Writer::with_capacity(18);
        w.u16(request_id).u16(0).u16(0).u32(0).u32(0).u32(join_result::NO_SUCH_GAME);
        Frame::new(msg::JOINGAME, w.finish())
    }
}

/// The portrait a stored character is drawn with.
#[must_use]
pub fn portrait(c: &Character) -> Portrait {
    Portrait { class: c.class, status: c.status, level: c.level, progression: c.progression }
}
