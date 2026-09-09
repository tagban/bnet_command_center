//! Per-connection session drivers.
//!
//! One tokio task per connection, owning its socket, its receive buffer and its session
//! state. A slow or hostile client can only stall itself — there is no shared reactor
//! thread whose progress every other user depends on. That is the single structural
//! difference from PvPGN, and the reason 2,000 connections is unremarkable here and
//! load-bearing there.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bnetcc_core::limits::{ClientClass, KeyId};
use bnetcc_core::session::SessionState;
use bnetcc_crypto::{logon_proof, proofs_match, xsha1_bytes};
use bnetcc_proto::bncs::{
    advertise_status, auth_check_status, decode_frame, encode_frame, logon_status, sid, Frame,
    ProtocolSelector, DEFAULT_MAX_FRAME,
};
use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::chat::{
    chat_event, normalize_channel_name, sanitize_chat_text, user_flags, EventId,
    CHANNEL_NAME_MAX, CHAT_TEXT_MAX, USERNAME_MAX,
};
use bnetcc_proto::error::FourCc;
use bnetcc_proto::line::{decode_line, encode_line, gateway_message};
use bnetcc_proto::product;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::node::{Account, KeyClaim, Node, Outbound, Wire};

/// Current time in milliseconds since the Unix epoch, for [`bnetcc_core::limits::KeyRegistry`]'s
/// cooldown window. Best-effort like [`next_server_token`]: if the clock is broken, keys
/// simply never cool down, which is safe (fails toward stricter, not laxer).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Derive a stable per-key fingerprint from the session-independent parts of a CD key
/// (its product and public values), never from the wire's `Hash` field — see
/// [`KeyId`]'s doc comment for why that field cannot be used for uniqueness tracking.
fn key_fingerprint(product_value: u32, public_value: u32) -> KeyId {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&product_value.to_le_bytes());
    buf[4..].copy_from_slice(&public_value.to_le_bytes());
    KeyId(xsha1_bytes(&buf))
}

/// Per-connection tunables, resolved from config once at startup.
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
    /// Largest accepted BNCS frame.
    pub max_frame: usize,
    /// Largest accepted gateway line.
    pub max_line: usize,
    /// Outbound queue depth in frames.
    pub outbound_queue: usize,
    /// Accept-to-authenticated deadline.
    pub handshake_timeout: Duration,
    /// Post-authentication idle deadline.
    pub idle_timeout: Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_frame: DEFAULT_MAX_FRAME,
            max_line: bnetcc_proto::line::DEFAULT_MAX_LINE,
            outbound_queue: 64,
            handshake_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(1200),
        }
    }
}

/// How much we ask the kernel for on each read.
///
/// Large enough that a burst of chat frames arrives in one syscall, small enough that
/// 2,000 idle connections do not pin 2,000 large buffers. The buffer only grows to this
/// when there is actually data.
const READ_CHUNK: usize = 4096;

/// Token source for `SID_AUTH_INFO` server tokens.
///
/// **Not cryptographically strong.** The server token is sent in the clear and its only
/// job is to make a captured `SID_LOGONRESPONSE2` proof useless on a later connection,
/// so it must be unpredictable across sessions. This mixes a process-start timestamp
/// with a counter, which is adequate against replay but not against a determined
/// attacker who can observe tokens.
///
/// TODO: replace with `getrandom` once the dependency is available. Tracked in
/// `docs/ROADMAP.md` phase 1.
fn next_server_token() -> u32 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut x = now ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 29;
    (x & 0xFFFF_FFFF) as u32
}

/// Entry point for one accepted connection.
pub async fn handle(stream: TcpStream, peer: SocketAddr, node: Arc<Node>, limits: SessionLimits) {
    let _ = stream.set_nodelay(true);
    let ip = peer.ip();

    // The protocol selector is the first byte and must arrive promptly. A peer that
    // connects and says nothing is a slowloris; it gets the handshake deadline and
    // nothing else.
    let mut sel = [0u8; 1];
    let read = tokio::time::timeout(limits.handshake_timeout, {
        let mut s = stream;
        async move {
            let n = s.read(&mut sel).await?;
            Ok::<_, std::io::Error>((s, n, sel[0]))
        }
    })
    .await;

    let (stream, n, selector) = match read {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            debug!(%peer, error = %e, "read failed before protocol selector");
            return;
        }
        Err(_) => {
            debug!(%peer, "timed out before sending a protocol selector");
            return;
        }
    };
    if n == 0 {
        return;
    }

    match ProtocolSelector::from_byte(selector) {
        Some(ProtocolSelector::Game) => {
            // The selector says "a game client"; it does not say *which* game. The
            // product only arrives in SID_AUTH_INFO, so we admit under the pending
            // class and promote once we know. See ClientClass.
            if let Err(reason) = node.admit(ClientClass::GamePending, ip) {
                debug!(%peer, ?reason, "game connection refused by admission control");
                return;
            }
            let (result, final_class) =
                bncs_session(stream, peer, Arc::clone(&node), limits).await;
            node.release(final_class, ip);
            if let Err(e) = result {
                debug!(%peer, error = %e, "bncs session ended");
            }
        }
        Some(ProtocolSelector::Chat) => {
            if let Err(reason) = node.admit(ClientClass::Gateway, ip) {
                debug!(%peer, ?reason, "gateway connection refused by admission control");
                return;
            }
            let result = gateway_session(stream, peer, Arc::clone(&node), limits).await;
            node.release(ClientClass::Gateway, ip);
            if let Err(e) = result {
                debug!(%peer, error = %e, "gateway session ended");
            }
        }
        Some(ProtocolSelector::Bnftp) => {
            // Clients fetch icons.bni, tos.txt and patch MPQs over this. Until it is
            // implemented we close cleanly rather than hang the client.
            debug!(%peer, "BNFTP requested but not implemented");
        }
        None => {
            // Unknown selector: close without responding, exactly as real Battle.net does.
            debug!(%peer, selector, "unknown protocol selector");
        }
    }
}

/// Read a username field, truncating rather than disconnecting if it is over-length.
///
/// Real Battle.net truncates usernames past 15 characters instead of rejecting them
/// (docs/PROTOCOL-NOTES.md). Reading with headroom and truncating after the fact matches
/// that; reading straight into `USERNAME_MAX` would instead fail the whole frame — and
/// the caller previously treated that as a reason to close the connection — the moment a
/// nonstandard client sent one byte too many.
fn read_username(r: &mut bnetcc_proto::buf::Reader<'_>) -> Result<Vec<u8>, bnetcc_proto::ProtoError> {
    const READ_LIMIT: usize = 64;
    let raw = r.cstr(READ_LIMIT)?;
    Ok(raw[..raw.len().min(USERNAME_MAX)].to_vec())
}

/// Spawn the writer half and return a handle plus its join handle.
fn spawn_writer(
    mut wr: tokio::net::tcp::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Wire>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(buf) = rx.recv().await {
            if wr.write_all(&buf).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    })
}

// ---------------------------------------------------------------------------
// BNCS
// ---------------------------------------------------------------------------

/// What the session loop should do after handling a frame.
enum Step {
    Continue,
    Close,
}

struct Bncs {
    node: Arc<Node>,
    out: Outbound,
    peer: SocketAddr,
    /// Starts as `GamePending`; promoted to `Game(product)` by `SID_AUTH_INFO`. The
    /// caller releases whichever class we end in.
    class: ClientClass,
    state: SessionState,
    server_token: u32,
    product: Option<FourCc>,
    /// The version byte from `SID_AUTH_INFO`, for logging. Not currently checked against
    /// anything — see `auth_check`.
    version_byte: Option<u32>,
    /// CD keys claimed by `auth_check`, released on disconnect regardless of whether
    /// this session ever logged in.
    claimed_keys: Vec<KeyId>,
    account: Option<Account>,
    channel: Option<Vec<u8>>,
    flags: u32,
}

async fn bncs_session(
    stream: TcpStream,
    peer: SocketAddr,
    node: Arc<Node>,
    limits: SessionLimits,
) -> (std::io::Result<()>, ClientClass) {
    let (mut rd, wr) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Wire>(limits.outbound_queue);
    let writer = spawn_writer(wr, rx);

    let mut s = Bncs {
        node,
        out: Outbound::new(tx.clone()),
        peer,
        class: ClientClass::GamePending,
        state: SessionState::Connected,
        server_token: next_server_token(),
        product: None,
        version_byte: None,
        claimed_keys: Vec::new(),
        account: None,
        channel: None,
        flags: 0,
    };

    let mut buf = RecvBuf::with_capacity(READ_CHUNK);
    loop {
        let deadline = if s.state.authenticated() {
            limits.idle_timeout
        } else {
            limits.handshake_timeout
        };

        let tail = buf.writable_tail(READ_CHUNK);
        let n = match tokio::time::timeout(deadline, rd.read(tail)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                buf.commit(0, READ_CHUNK);
                let class = s.class;
                s.cleanup();
                drop(tx);
                drop(s);
                let _ = writer.await;
                return (Err(e), class);
            }
            Err(_) => {
                buf.commit(0, READ_CHUNK);
                debug!(peer = %s.peer, state = s.state.name(), "session timed out");
                break;
            }
        };
        buf.commit(n, READ_CHUNK);
        if n == 0 {
            break; // clean EOF
        }

        // Drain every complete frame. Handling one packet per readiness event is
        // PvPGN's `sd_tcpinput()` mistake — it has no loop.
        loop {
            let frame = match decode_frame(&mut buf, limits.max_frame) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    debug!(peer = %s.peer, error = %e, "malformed frame; closing");
                    s.state = SessionState::Closing;
                    break;
                }
            };
            if !s.state.accepts(frame.id) {
                debug!(
                    peer = %s.peer,
                    id = frame.id,
                    state = s.state.name(),
                    "packet not accepted in this state; closing"
                );
                s.state = SessionState::Closing;
                break;
            }
            match s.handle(&frame).await {
                Step::Continue => {}
                Step::Close => {
                    s.state = SessionState::Closing;
                    break;
                }
            }
        }
        if matches!(s.state, SessionState::Closing) {
            break;
        }
    }

    let class = s.class;
    s.cleanup();
    drop(tx);
    drop(s);
    let _ = writer.await;
    (Ok(()), class)
}

impl Bncs {
    fn cleanup(&mut self) {
        let now = now_ms();
        for key in self.claimed_keys.drain(..) {
            self.node.release_key(key, now);
        }
        if let (Some(key), Some(account)) = (self.channel.take(), self.account.as_ref()) {
            let name = account.name.clone();
            let new_op = self.node.leave_channel(&key, account.id);
            let leave = chat_event(EventId::Leave, self.flags, 0, name.as_bytes(), b"");
            if let Some(wire) = encode(&leave) {
                let _ = self.node.broadcast(&key, &wire, Some(account.id));
            }
            if let Some(op) = new_op {
                debug!(channel = ?String::from_utf8_lossy(&key), new_operator = op, "operator inherited");
            }
        }
    }

    async fn handle(&mut self, frame: &Frame) -> Step {
        let step = match frame.id {
            sid::AUTH_INFO => self.auth_info(frame),
            sid::AUTH_CHECK => self.auth_check(frame),
            sid::LOGONRESPONSE2 => self.logon(frame).await,
            sid::CREATEACCOUNT2 => self.create_account(frame).await,
            sid::ENTERCHAT => self.enter_chat(frame),
            sid::JOINCHANNEL => self.join_channel(frame),
            sid::CHATCOMMAND => self.chat_command(frame),
            sid::GETCHANNELLIST => self.channel_list(),
            sid::GETADVLISTEX => self.game_list(),
            sid::CHECKAD => self.check_ad(frame),
            sid::STARTADVEX3 => self.advertise(),
            // Keepalive, the "sent on logoff even when not in a game" quirk, and the
            // advertisement telemetry we accept but do not act on.
            sid::PING
            | sid::STOPADV
            | sid::LEAVEGAME
            | sid::NOTIFYJOIN
            | sid::CLICKAD
            | sid::DISPLAYAD => Step::Continue,
            other => {
                debug!(peer = %self.peer, id = other, "unhandled packet");
                Step::Continue
            }
        };
        if matches!(step, Step::Continue) {
            if let Some(next) = self.state.next_on_success(frame.id) {
                // Only advance when the handler actually succeeded; handlers that fail
                // return Step::Close, and login failures leave the state alone by
                // returning Continue without having set `account`.
                if next != SessionState::LoggedIn || self.account.is_some() {
                    self.state = next;
                }
            }
        }
        step
    }

    fn send(&self, frame: &Frame) -> Step {
        if self.out.send_frame(frame) {
            Step::Continue
        } else {
            // Queue full: the peer is not reading. Never buffer without bound.
            debug!(peer = %self.peer, "outbound queue full; closing");
            Step::Close
        }
    }

    fn auth_info(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let _protocol = r.u32()?;
            let _platform = r.fourcc()?;
            let product = r.fourcc()?;
            let version_byte = r.u32()?;
            Ok::<_, bnetcc_proto::ProtoError>((product, version_byte))
        })();
        let Ok((product, version_byte)) = parsed else {
            return Step::Close;
        };
        self.product = Some(product);
        self.version_byte = Some(version_byte);
        // Not checked against anything yet — see `auth_check`'s doc comment. Logged so an
        // operator can see what real client builds are actually connecting.
        info!(
            peer = %self.peer,
            product = %product,
            version_byte = %format!("{version_byte:#010x}"),
            "SID_AUTH_INFO"
        );

        // Now that the product is known, move this connection onto that product's
        // limits. On refusal we close: there is no BNCS status meaning "too many
        // connections", and real Battle.net simply drops in this situation too.
        let target = ClientClass::Game(product);
        if let Err(reason) = self.node.reclassify(self.peer.ip(), self.class, target) {
            debug!(
                peer = %self.peer,
                product = %product,
                ?reason,
                "refused by the per-product connection limit"
            );
            return Step::Close;
        }
        self.class = target;

        if product::always_no_udp(product) {
            // Real Battle.net never answers UDP for these, so their clients always show
            // the No-UDP flag. Matching that avoids a difference users notice at once.
            self.flags |= user_flags::NO_UDP;
        }

        let srp = matches!(
            product::auth_family(product),
            Some(product::AuthFamily::Srp)
        );
        let logon_type: u32 = if srp { 0x02 } else { 0x00 };

        let mut w = Writer::with_capacity(64);
        w.u32(logon_type)
            .u32(self.server_token)
            .u32(0) // UDP value
            .u64(0) // CheckRevision MPQ filetime
            .cstr(b"ver-IX86-1.mpq")
            .cstr(b"A=1 B=1 C=1 4 A=A^S B=B^C C=C^A A=A^B");
        if srp {
            // WarCraft III expects a 128-byte RSA signature here and verifies it against
            // Blizzard's public key. We cannot produce one, so WC3 requires a patched
            // client. See docs/LEGAL.md §3 — this is a designed-in block, not a gap.
            w.bytes(&[0u8; 128]);
        }
        self.send(&Frame::new(sid::AUTH_INFO, w.finish()))
    }

    /// Version is still unchecked by design — see `auth_info`'s comment and
    /// `docs/PROTOCOL-NOTES.md`. CD-key uniqueness *is* enforced here: one live session
    /// per key, which is the actual gate on a bot fleet (addresses are cheap, keys are
    /// not — see `docs/WARNET.md`).
    ///
    /// The wire layout parsed below (client token, exe version, exe hash, key count,
    /// spawn flag, exe info, then per-key length/product/public/hash, then owner name)
    /// is this project's best-confidence read of `SID_AUTH_CHECK`'s request side and has
    /// **not** been confirmed against a real client capture. Refusing every logon over a
    /// wire format we are not sure of would be worse than not enforcing key uniqueness
    /// at all, so a parse failure here fails *open* — log it and accept — rather than
    /// closing the connection the way a confirmed frame layout's parse failure would.
    fn auth_check(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let _client_token = r.u32()?;
            let _exe_version = r.u32()?;
            let _exe_hash = r.u32()?;
            let key_count = r.u32()?;
            let _spawn = r.u32()?;
            let _exe_info = r.cstr(256)?;
            let mut keys = Vec::with_capacity(key_count.min(2) as usize);
            for _ in 0..key_count.min(2) {
                let _key_length = r.u32()?;
                let product_value = r.u32()?;
                let public_value = r.u32()?;
                let _reserved = r.u32()?;
                let _hash: [u8; 20] = r.array()?;
                keys.push(key_fingerprint(product_value, public_value));
            }
            let _owner = r.cstr(64)?;
            Ok::<_, bnetcc_proto::ProtoError>(keys)
        })();

        let keys = match parsed {
            Ok(keys) => keys,
            Err(e) => {
                warn!(
                    peer = %self.peer,
                    error = %e,
                    "could not parse SID_AUTH_CHECK against our unverified guess at its \
                     layout; accepting without a CD-key uniqueness check"
                );
                return self.auth_check_ok();
            }
        };

        let now = now_ms();
        for (i, &key) in keys.iter().enumerate() {
            let second = i == 1; // Warcraft III sends a base key and an expansion key.
            match self.node.claim_key(key, now) {
                KeyClaim::Ok => {}
                KeyClaim::InUse { holder } => {
                    for &done in &keys[..i] {
                        self.node.release_key(done, now);
                    }
                    let mut status = auth_check_status::KEY_IN_USE;
                    if second {
                        status |= auth_check_status::SECOND_KEY;
                    }
                    let mut w = Writer::with_capacity(32);
                    w.u32(status).cstr(holder.as_bytes());
                    return self.send(&Frame::new(sid::AUTH_CHECK, w.finish()));
                }
                KeyClaim::Banned => {
                    for &done in &keys[..i] {
                        self.node.release_key(done, now);
                    }
                    let mut status = auth_check_status::KEY_BANNED;
                    if second {
                        status |= auth_check_status::SECOND_KEY;
                    }
                    let mut w = Writer::with_capacity(8);
                    w.u32(status).cstr(b"");
                    return self.send(&Frame::new(sid::AUTH_CHECK, w.finish()));
                }
            }
        }
        self.claimed_keys = keys;
        self.auth_check_ok()
    }

    fn auth_check_ok(&mut self) -> Step {
        let mut w = Writer::with_capacity(8);
        w.u32(auth_check_status::PASSED).cstr(b"");
        self.send(&Frame::new(sid::AUTH_CHECK, w.finish()))
    }

    async fn logon(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let client_token = r.u32()?;
            let _server_token = r.u32()?;
            let proof: [u8; 20] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((client_token, proof, username))
        })();
        let Ok((client_token, proof, username)) = parsed else {
            return Step::Close;
        };
        let name = String::from_utf8_lossy(&username).to_string();

        let status = match self.node.account(&name).await {
            None => logon_status::NO_SUCH_ACCOUNT,
            Some(account) => {
                // Always use *our* server token, never the one the client echoed back.
                let expected = logon_proof(client_token, self.server_token, &account.password_hash);
                if proofs_match(&proof, &expected) {
                    info!(peer = %self.peer, account = %account.name, "logon accepted");
                    for &key in &self.claimed_keys {
                        self.node.record_key_holder_name(key, account.name.clone());
                    }
                    self.account = Some(account);
                    logon_status::SUCCESS
                } else {
                    logon_status::WRONG_PASSWORD
                }
            }
        };

        let mut w = Writer::with_capacity(8);
        w.u32(status);
        if status == logon_status::ACCOUNT_CLOSED {
            w.cstr(b"");
        }
        self.send(&Frame::new(sid::LOGONRESPONSE2, w.finish()))
    }

    async fn create_account(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let hash: [u8; 20] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((hash, username))
        })();
        let Ok((hash, username)) = parsed else {
            return Step::Close;
        };
        let name = String::from_utf8_lossy(&username).to_string();

        // Note: `SID_CREATEACCOUNT2` carries the password hashed **once**, unlike the
        // logon proof which is a double hash. Storing what arrives is therefore correct.
        //
        // Status codes beyond 0x00/0x04 are unverified against a real client capture —
        // BNETDocs documents 0x02 "name contains invalid characters" but this project has
        // not confirmed it. See docs/PROTOCOL-NOTES.md's unverified list.
        let status: u32 = match self.node.create_account(&name, hash).await {
            Ok(account) => {
                // Creation does not log you in; the client sends a logon next.
                info!(peer = %self.peer, account = %account.name, "account created");
                0x00
            }
            Err(crate::storage::CreateAccountError::NameTaken) => 0x04,
            Err(crate::storage::CreateAccountError::Invalid(why)) => {
                debug!(peer = %self.peer, name = %name, reason = %why, "account creation refused");
                0x02
            }
            Err(crate::storage::CreateAccountError::Backend(why)) => {
                warn!(peer = %self.peer, reason = %why, "account creation failed in storage");
                return Step::Close;
            }
        };

        let mut w = Writer::with_capacity(8);
        w.u32(status).cstr(b"");
        self.send(&Frame::new(sid::CREATEACCOUNT2, w.finish()))
    }

    fn enter_chat(&mut self, _frame: &Frame) -> Step {
        let Some(account) = self.account.clone() else {
            return Step::Close;
        };
        let mut w = Writer::with_capacity(64);
        w.cstr(account.name.as_bytes())
            .cstr(b"")
            .cstr(account.name.as_bytes());
        self.send(&Frame::new(sid::ENTERCHAT, w.finish()))
    }

    fn join_channel(&mut self, frame: &Frame) -> Step {
        let Some(account) = self.account.clone() else {
            return Step::Close;
        };
        let mut r = frame.reader();
        let parsed = (|| {
            let flags = r.u32()?;
            let name = r.cstr(CHANNEL_NAME_MAX)?.to_vec();
            Ok::<_, bnetcc_proto::ProtoError>((flags, name))
        })();
        let Ok((_join_flags, requested)) = parsed else {
            return Step::Close;
        };

        // Leave the current channel first; joining your current channel returns you to
        // the previous one on real Battle.net, but leaving unconditionally is the
        // behaviour clients cope with and is far easier to reason about.
        if let Some(prev) = self.channel.take() {
            if prev == normalize_channel_name(&requested) {
                // Re-joining the same channel: nothing to do.
                self.channel = Some(prev);
                return Step::Continue;
            }
            self.leave_current(&prev, &account);
        }

        match self.node.join_channel(
            &requested,
            account.id,
            &account.name,
            self.flags,
            self.out.clone(),
        ) {
            Err(denial) => {
                let event = match denial {
                    bnetcc_core::JoinDenial::Full => EventId::ChannelFull,
                    bnetcc_core::JoinDenial::Banned => EventId::ChannelRestricted,
                    bnetcc_core::JoinDenial::AlreadyPresent => return Step::Continue,
                };
                self.send(&chat_event(event, 0, 0, b"", &requested))
            }
            Ok((joined, existing)) => {
                self.flags = joined.outcome.flags;
                self.channel = Some(joined.key.clone());

                // 1. "You are in channel X".
                if matches!(
                    self.send(&chat_event(
                        EventId::Channel,
                        joined.flags,
                        0,
                        joined.display.as_bytes(),
                        b"",
                    )),
                    Step::Close
                ) {
                    return Step::Close;
                }
                // 2. One EID_SHOWUSER per occupant already present.
                for (name, flags) in existing {
                    if matches!(
                        self.send(&chat_event(
                            EventId::ShowUser,
                            flags,
                            0,
                            name.as_bytes(),
                            b"",
                        )),
                        Step::Close
                    ) {
                        return Step::Close;
                    }
                }
                // 3. Tell everyone else we arrived.
                let ev = chat_event(EventId::Join, self.flags, 0, account.name.as_bytes(), b"");
                if let Some(wire) = encode(&ev) {
                    let _ = self.node.broadcast(&joined.key, &wire, Some(account.id));
                }
                // 4. MOTD, as real Battle.net does on a first join.
                self.send(&chat_event(
                    EventId::Info,
                    0,
                    0,
                    b"",
                    self.node.motd.as_bytes(),
                ))
            }
        }
    }

    fn leave_current(&mut self, key: &[u8], account: &Account) {
        self.node.leave_channel(key, account.id);
        let ev = chat_event(EventId::Leave, self.flags, 0, account.name.as_bytes(), b"");
        if let Some(wire) = encode(&ev) {
            let _ = self.node.broadcast(key, &wire, Some(account.id));
        }
    }

    fn chat_command(&mut self, frame: &Frame) -> Step {
        let Some(account) = self.account.clone() else {
            return Step::Close;
        };
        let Some(key) = self.channel.clone() else {
            return Step::Continue;
        };
        let mut r = frame.reader();
        let Ok(raw) = r.cstr(CHAT_TEXT_MAX) else {
            return Step::Close;
        };
        // Strips control bytes: a relayed CR or LF makes the *receiving* client
        // disconnect and IP-ban us for five minutes.
        let text = sanitize_chat_text(raw);
        if text.is_empty() {
            return Step::Continue;
        }
        if text.first() == Some(&b'/') {
            return self.slash_command(&text, &account, &key);
        }

        let ev = chat_event(EventId::Talk, self.flags, 0, account.name.as_bytes(), &text);
        if let Some(wire) = encode(&ev) {
            // Real Battle.net does not echo your own channel talk back to you.
            let stalled = self.node.broadcast(&key, &wire, Some(account.id));
            if !stalled.is_empty() {
                debug!(count = stalled.len(), "dropped stalled subscribers from fanout");
            }
        }
        Step::Continue
    }

    fn slash_command(&mut self, text: &[u8], account: &Account, key: &[u8]) -> Step {
        let s = String::from_utf8_lossy(text);
        let mut parts = s[1..].splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
        let _arg = parts.next().unwrap_or("").trim();

        match cmd.as_str() {
            "me" | "emote" => {
                let body = sanitize_chat_text(_arg.as_bytes());
                let ev = chat_event(EventId::Emote, self.flags, 0, account.name.as_bytes(), &body);
                if let Some(wire) = encode(&ev) {
                    let _ = self.node.broadcast(key, &wire, None);
                }
                Step::Continue
            }
            "whoami" => {
                let op = if self.node.is_operator(key, account.id) {
                    " (operator)"
                } else {
                    ""
                };
                let msg = format!("You are {}{op}.", account.name);
                self.send(&chat_event(EventId::Info, 0, 0, b"", msg.as_bytes()))
            }
            _ => self.send(&chat_event(
                EventId::Error,
                0,
                0,
                b"",
                b"That is not a valid command.",
            )),
        }
    }

    /// `SID_CHECKAD` — the client tells us which banner it is showing; we answer with
    /// the next one, or say nothing.
    ///
    /// Because the request carries the client's own cursor, rotation needs no
    /// per-connection state: the answer is a pure function of (previous id, product,
    /// language). See `bnetcc_core::ads`.
    fn check_ad(&mut self, frame: &Frame) -> Step {
        let Some(product) = self.product else {
            return Step::Continue;
        };
        let mut r = frame.reader();
        let parsed = (|| {
            let _platform = r.fourcc()?;
            let _product = r.fourcc()?;
            let previous = r.u32()?;
            Ok::<_, bnetcc_proto::ProtoError>(previous)
        })();
        let Ok(previous) = parsed else {
            return Step::Close;
        };

        // Seed only matters for WarCraft III's random pick; the server token is already
        // per-session and unpredictable enough for choosing a banner.
        let Some(ad) = self
            .node
            .ads
            .next(product, None, previous, u64::from(self.server_token))
        else {
            // Nothing configured for this client. Saying nothing leaves it showing what
            // it has, which is better than sending a banner it cannot render.
            return Step::Continue;
        };

        let extension = bnetcc_proto::bnftp::extension::for_filename(&ad.filename)
            .map_or(0, |t| t.0);
        let mut w = Writer::with_capacity(64);
        w.u32(ad.id)
            .u32(extension)
            .u64(0) // filetime; the client refetches when this changes
            .cstr(ad.filename.as_bytes())
            .cstr(ad.url.as_bytes());
        self.send(&Frame::new(sid::CHECKAD, w.finish()))
    }

    fn channel_list(&mut self) -> Step {
        let mut w = Writer::with_capacity(128);
        for name in self.node.channel_names() {
            w.cstr(name.as_bytes());
        }
        w.cstr(b""); // STRINGLIST terminator
        self.send(&Frame::new(sid::GETCHANNELLIST, w.finish()))
    }

    fn game_list(&mut self) -> Step {
        let mut w = Writer::with_capacity(8);
        if self.node.policy.game_listing.allowed() {
            // No game advertisements yet; an empty list is the honest answer.
            w.u32(0).u32(1);
        } else {
            // Warnet mode: no games, ever. Status 1 = "game doesn't exist".
            w.u32(0).u32(1);
        }
        // NOTE: BNETDocs describes the zero-game case ambiguously — "(UINT32) Number of
        // games [if 0 → a single (UINT32) Status follows instead]". We send count then
        // status. Verify against a real client capture before release; getting this
        // wrong makes the client hang rather than show an error.
        self.send(&Frame::new(sid::GETADVLISTEX, w.finish()))
    }

    fn advertise(&mut self) -> Step {
        let status = if self.node.policy.game_hosting.allowed() {
            advertise_status::OK
        } else {
            // The game's own documented code, so the client shows a real message.
            // Dropping the packet instead would hang the client.
            advertise_status::TYPE_UNAVAILABLE
        };
        let mut w = Writer::with_capacity(4);
        w.u32(status);
        self.send(&Frame::new(sid::STARTADVEX3, w.finish()))
    }
}

fn encode(frame: &Frame) -> Option<Wire> {
    let mut buf = Vec::with_capacity(frame.wire_len());
    encode_frame(frame, &mut buf).ok()?;
    Some(Arc::new(buf))
}

// ---------------------------------------------------------------------------
// Chat gateway
// ---------------------------------------------------------------------------

/// The line-oriented bot gateway.
///
/// First slice: greet, take a username, and echo channel traffic. Enough for a bot to
/// connect and be counted against the per-IP and per-account ceilings, which is the part
/// that had to exist before anything else.
async fn gateway_session(
    stream: TcpStream,
    peer: SocketAddr,
    node: Arc<Node>,
    limits: SessionLimits,
) -> std::io::Result<()> {
    let (mut rd, wr) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Wire>(limits.outbound_queue);
    let writer = spawn_writer(wr, rx);
    let out = Outbound::new(tx.clone());

    let mut greeting = Vec::new();
    encode_line(
        gateway_message(1001, &node.name, Some("Command Center chat gateway")).as_bytes(),
        &mut greeting,
    );
    let _ = out.send(&Arc::new(greeting));

    let mut buf = RecvBuf::with_capacity(512);
    loop {
        let tail = buf.writable_tail(READ_CHUNK);
        let n = match tokio::time::timeout(limits.idle_timeout, rd.read(tail)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                buf.commit(0, READ_CHUNK);
                return Err(e);
            }
            Err(_) => {
                buf.commit(0, READ_CHUNK);
                break;
            }
        };
        buf.commit(n, READ_CHUNK);
        if n == 0 {
            break;
        }

        loop {
            match decode_line(&mut buf, limits.max_line) {
                Ok(Some(line)) => {
                    if line.is_empty() {
                        continue;
                    }
                    let mut reply = Vec::new();
                    encode_line(
                        gateway_message(1018, "INFO", Some(&String::from_utf8_lossy(&line)))
                            .as_bytes(),
                        &mut reply,
                    );
                    if !out.send(&Arc::new(reply)) {
                        warn!(%peer, "gateway outbound queue full; closing");
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    debug!(%peer, error = %e, "gateway line error; closing");
                    drop(tx);
                    let _ = writer.await;
                    return Ok(());
                }
            }
        }
    }

    drop(tx);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_info_frame() -> Frame {
        let mut w = Writer::with_capacity(32);
        w.u32(0)
            .fourcc(bnetcc_proto::FourCc::from_ascii(b"IX86"))
            .fourcc(product::SEXP)
            .u32(0xCD);
        Frame::new(sid::AUTH_INFO, w.finish())
    }

    /// A minimal `SID_AUTH_CHECK` carrying one CD key. `product_value`/`public_value` are
    /// the two fields our uniqueness check actually keys on — see `key_fingerprint`.
    fn auth_check_frame(product_value: u32, public_value: u32) -> Frame {
        let mut w = Writer::with_capacity(96);
        w.u32(0) // client token
            .u32(0) // exe version
            .u32(0) // exe hash
            .u32(1) // key count
            .u32(0) // spawn
            .cstr(b"test.exe 00/00/00 000000")
            .u32(16) // key length
            .u32(product_value)
            .u32(public_value)
            .u32(0) // reserved
            .bytes(&[0u8; 20]) // wire hash; unused by our uniqueness check, see KeyId
            .cstr(b"tester");
        Frame::new(sid::AUTH_CHECK, w.finish())
    }

    async fn send_frame(stream: &mut TcpStream, frame: &Frame) {
        let mut out = Vec::new();
        encode_frame(frame, &mut out).expect("encodable");
        stream.write_all(&out).await.expect("write");
    }

    async fn recv_frame(stream: &mut TcpStream) -> Frame {
        let mut buf = RecvBuf::with_capacity(256);
        loop {
            if let Ok(Some(f)) = decode_frame(&mut buf, DEFAULT_MAX_FRAME) {
                return f;
            }
            let tail = buf.writable_tail(256);
            let n = stream.read(tail).await.expect("read");
            assert_ne!(n, 0, "peer closed before sending a frame");
            buf.commit(n, 256);
        }
    }

    async fn auth_check_status_of(stream: &mut TcpStream, product_value: u32, public_value: u32) -> u32 {
        send_frame(stream, &auth_info_frame()).await;
        let _ = recv_frame(stream).await; // SID_AUTH_INFO reply
        send_frame(stream, &auth_check_frame(product_value, public_value)).await;
        let reply = recv_frame(stream).await;
        assert_eq!(reply.id, sid::AUTH_CHECK);
        reply.reader().u32().expect("status")
    }

    async fn connect(addr: std::net::SocketAddr) -> TcpStream {
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(&[0x01]).await.expect("protocol selector"); // "game client"
        s
    }

    #[tokio::test]
    async fn the_same_cd_key_cannot_open_two_live_sessions() {
        let node = Arc::new(crate::node::test_node());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let limits = SessionLimits::default();
        tokio::spawn({
            let node = Arc::clone(&node);
            async move {
                loop {
                    let (stream, peer) = listener.accept().await.expect("accept");
                    tokio::spawn(handle(stream, peer, Arc::clone(&node), limits));
                }
            }
        });

        let mut a = connect(addr).await;
        assert_eq!(
            auth_check_status_of(&mut a, 1, 42).await,
            auth_check_status::PASSED,
            "first session should claim the key cleanly"
        );

        let mut b = connect(addr).await;
        let status = auth_check_status_of(&mut b, 1, 42).await;
        assert_eq!(
            status & !auth_check_status::SECOND_KEY,
            auth_check_status::KEY_IN_USE,
            "a second live session must not be able to claim the same key"
        );

        // Releasing the first session's connection, and waiting out the registry's
        // cooldown, must free the key back up.
        drop(a);
        tokio::time::sleep(Duration::from_millis(600)).await;
        let mut c = connect(addr).await;
        assert_eq!(
            auth_check_status_of(&mut c, 1, 42).await,
            auth_check_status::PASSED,
            "the key must become claimable again once the holder disconnects"
        );
    }

    #[tokio::test]
    async fn different_cd_keys_do_not_conflict() {
        let node = Arc::new(crate::node::test_node());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let limits = SessionLimits::default();
        tokio::spawn({
            let node = Arc::clone(&node);
            async move {
                loop {
                    let (stream, peer) = listener.accept().await.expect("accept");
                    tokio::spawn(handle(stream, peer, Arc::clone(&node), limits));
                }
            }
        });

        let mut a = connect(addr).await;
        let mut b = connect(addr).await;
        assert_eq!(auth_check_status_of(&mut a, 1, 1).await, auth_check_status::PASSED);
        assert_eq!(auth_check_status_of(&mut b, 1, 2).await, auth_check_status::PASSED);
    }
}
