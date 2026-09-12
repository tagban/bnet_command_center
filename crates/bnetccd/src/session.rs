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

use bnetcc_core::limits::{ClientClass, FloodTracker, FloodVerdict, KeyId};
use bnetcc_core::policy::FloodPenalty;
use bnetcc_core::session::SessionState;
use bnetcc_storage::model::Credential;
use bnetcc_crypto::{logon_proof, proofs_match, xsha1_bytes};
use bnetcc_proto::bncs::{
    advertise_status, auth_check_status, decode_frame, encode_frame, logon_status, sid, Frame,
    ProtocolSelector, DEFAULT_MAX_FRAME, nls_status};
use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::chat::{
    chat_event, normalize_channel_name, sanitize_chat_text, user_flags, EventId,
    CHANNEL_NAME_MAX, CHAT_TEXT_MAX, USERNAME_MAX,
};
use bnetcc_proto::bnftp;
use bnetcc_proto::error::FourCc;
use bnetcc_proto::line::{decode_line, encode_line, gateway_message};
use bnetcc_proto::product;
use bnetcc_proto::statstring;
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
    debug!(%peer, "accepted TCP connection");

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
            if let Err(reason) = node.admit(ClientClass::Bnftp, ip) {
                debug!(%peer, ?reason, "BNFTP connection refused by admission control");
                return;
            }
            let result = bnftp_session(stream, peer, Arc::clone(&node), limits).await;
            node.release(ClientClass::Bnftp, ip);
            if let Err(e) = result {
                debug!(%peer, error = %e, "BNFTP session ended");
            }
        }
        None => {
            // Unknown selector: close without responding, exactly as real Battle.net does.
            debug!(%peer, selector, "unknown protocol selector");
        }
    }
}

/// The version-check MPQ filename to advertise for a client's platform.
///
/// The file is platform-specific: a PowerPC Mac client (`PMAC`/`XMAC`) cannot run
/// CheckRevision against the Windows (`IX86`) MPQ. The client fetches exactly the name we
/// send here over BNFTP, so an operator supplies one per platform they want to admit.
///
/// We use PvPGN's `<PLATFORM>ver1.mpq` naming (`IX86ver1.mpq`, `PMACver1.mpq`,
/// `XMACver1.mpq`) — the scheme under which a stock PvPGN `files/` directory ships all
/// three platforms. An unrecognised or absent platform falls back to `IX86`.
/// The CheckRevision MPQ name to hand a client. Two naming conventions exist and the
/// client parses the name it is given: the original `IX86ver1.mpq` (StarCraft, Diablo,
/// Warcraft II) and the later `ver-IX86-1.mpq` that Diablo II and newer WarCraft III expect.
///
/// **WarCraft III straddles both**, keyed by the version byte (from PvPGN's versioncheck
/// config): 1.26 and older (`<= 0x1A`) use the old `IX86ver1.mpq`, while 1.27+ use
/// `ver-IX86-1.mpq`. Handing a client the wrong one makes it fail its version check and, on
/// the BNFTP v2 path, re-request in a loop until it gives up. Diablo II uses the `ver-` form.
/// Both files ship in the operator's files directory. `version_byte` is `None` where the
/// game version is not known (e.g. a bare BNFTP fallback), which selects the modern name.
fn version_mpq_name(
    platform: Option<FourCc>,
    product: Option<FourCc>,
    version_byte: Option<u32>,
) -> String {
    let plat = platform
        .map(|p| {
            let a = p.as_ascii();
            if a.iter().all(u8::is_ascii_graphic) {
                String::from_utf8_lossy(&a).into_owned()
            } else {
                "IX86".to_string()
            }
        })
        .unwrap_or_else(|| "IX86".to_string());
    // WarCraft III's naming depends on the patch level.
    if matches!(product, Some(p) if p == product::WAR3 || p == product::W3XP) {
        return match version_byte {
            Some(v) if v <= 0x1A => format!("{plat}ver1.mpq"),
            _ => format!("ver-{plat}-1.mpq"),
        };
    }
    let modern = matches!(product, Some(p) if p == product::D2DV || p == product::D2XP);
    if modern {
        format!("ver-{plat}-1.mpq")
    } else {
        format!("{plat}ver1.mpq")
    }
}

/// The BNI icon file to advertise for a client's product. StarCraft/Brood War and
/// WarCraft III have their own icon packs; everything else (Diablo, Warcraft II BNE, the
/// old Mac clients) uses the shared `icons.bni`. An operator supplies these in the BNFTP
/// files directory.
fn icon_file_name(product: Option<FourCc>) -> &'static [u8] {
    match product {
        Some(p) if p == product::STAR || p == product::SEXP => b"icons_STAR.bni",
        Some(p) if p == product::WAR3 || p == product::W3XP => b"icons-WAR3.bni",
        _ => b"icons.bni",
    }
}

/// Convert a Unix time to a Windows FILETIME (100-nanosecond ticks since 1601-01-01).
fn unix_to_filetime(t: SystemTime) -> u64 {
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600; // 1601→1970 in seconds.
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    (secs + EPOCH_DIFF_SECS).saturating_mul(10_000_000)
}

/// Serve one BNFTP file request: the client asks for a filename, we send a header and the
/// file bytes from the operator's configured directory. This is how a real classic client
/// fetches the version-check MPQ it needs to pass CheckRevision, plus `icons.bni`, `tos.txt`
/// and ad images. One file per connection, then the client closes.
///
/// Only files in the configured directory are served, and only names that pass
/// [`bnftp::sanitize_filename`] — the security boundary against path traversal from an
/// unauthenticated peer.
async fn bnftp_session(
    mut stream: TcpStream,
    peer: SocketAddr,
    node: Arc<Node>,
    limits: SessionLimits,
) -> std::io::Result<()> {
    let mut chunk = [0u8; 512];

    // --- Phase 1: the initial request. v1 carries the filename; v2 (WarCraft III) is just a
    // 20-byte header and the filename arrives later (phase 3). ---
    let mut buf = Vec::with_capacity(64);
    let request = loop {
        match bnftp::decode_request(&buf) {
            Ok(Some(req)) => break req,
            Ok(None) => {}
            Err(e) => {
                warn!(%peer, error = %e, bytes = %hex_preview(&buf), "malformed BNFTP request");
                return Ok(());
            }
        }
        if buf.len() > bnftp::MAX_REQUEST {
            debug!(%peer, "BNFTP request exceeded the size cap");
            return Ok(());
        }
        let n = tokio::time::timeout(limits.handshake_timeout, stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "bnftp request"))??;
        if n == 0 {
            return Ok(()); // client closed before completing the request
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let Some(dir) = node.files_dir.clone() else {
        debug!(%peer, "BNFTP request but no files directory is configured; refusing");
        return Ok(());
    };

    // Resolve the requested filename and the response framing. v2 is a three-phase handshake
    // (docs/PROTOCOL-NOTES.md §5a, BNETDocs doc 6): the client sent its 20-byte header and is
    // now WAITING for a u32 server-token challenge before it sends the filename. Skipping the
    // challenge is what deadlocked every WarCraft III login. The token is arbitrary (a private
    // server does not gate file access on the CD key) and the client names the file itself, so
    // every WC3 patch is served the file it asks for with no per-version guessing.
    let is_v2 = request.version == bnftp::VERSION_2;
    let requested: Vec<u8> = if is_v2 {
        // Phase 2: send the challenge.
        stream.write_all(&rand::random::<u32>().to_le_bytes()).await?;
        stream.flush().await?;
        // Phase 3: read the client's response; the filename is at its tail.
        let mut cbuf = Vec::with_capacity(96);
        loop {
            match bnftp::decode_v2_challenge_filename(&cbuf) {
                Ok(Some(name)) => break name,
                Ok(None) => {}
                Err(e) => {
                    warn!(%peer, error = %e, bytes = %hex_preview(&cbuf), "bad BNFTP v2 challenge response");
                    return Ok(());
                }
            }
            if cbuf.len() > bnftp::MAX_REQUEST {
                debug!(%peer, "BNFTP v2 challenge response exceeded the size cap");
                return Ok(());
            }
            let n = tokio::time::timeout(limits.handshake_timeout, stream.read(&mut chunk))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "bnftp v2 challenge"))??;
            if n == 0 {
                return Ok(());
            }
            cbuf.extend_from_slice(&chunk[..n]);
        }
    } else {
        request.filename.clone()
    };

    // Defensive fallback: if a v2 client somehow named nothing, serve the product's
    // version-check MPQ (the file we advertise in SID_AUTH_INFO).
    let requested = if is_v2 && requested.is_empty() {
        // The BNFTP request carries no game version byte, so fall back to the modern name.
        version_mpq_name(Some(request.platform), Some(request.product), None).into_bytes()
    } else {
        requested
    };

    let Some(name) = bnftp::sanitize_filename(&requested) else {
        warn!(%peer, filename = %String::from_utf8_lossy(&requested), "BNFTP filename rejected by the traversal guard");
        return Ok(());
    };
    if is_v2 {
        info!(%peer, product = %request.product, file = %name, "BNFTP v2 request");
    }

    let path = dir.join(name);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            warn!(%peer, file = %name, error = %e, "BNFTP file not found or unreadable");
            return Ok(());
        }
    };
    let filetime = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .map_or(0, unix_to_filetime);

    let file_size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let header = bnftp::ResponseHeader {
        kind: 0,
        file_size,
        ad_id: request.ad_id,
        ad_extension: request.ad_extension,
        filetime,
        filename: name.as_bytes().to_vec(),
    };
    // Phase 4 (v2) / the single response (v1). v2 frames the header with a u32 length and no
    // "type" word. Log after the flush so a truncated transfer surfaces as a WARN rather than
    // a misleading success line.
    let response_header = if is_v2 {
        bnftp::encode_response_header_v2(&header)
    } else {
        bnftp::encode_response_header(&header)
    };
    let send = async {
        stream.write_all(&response_header).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await
    };
    match send.await {
        Ok(()) => info!(%peer, file = %name, bytes = bytes.len(), v2 = is_v2, "BNFTP file delivered"),
        Err(e) => warn!(%peer, file = %name, bytes = bytes.len(), error = %e, "BNFTP transfer failed mid-send"),
    }
    Ok(())
}

/// A short hex preview of a frame body, for debug logging while reverse-engineering a
/// client's wire format. Capped so a large frame does not flood the log.
fn hex_preview(body: &[u8]) -> String {
    const MAX: usize = 64;
    let shown = &body[..body.len().min(MAX)];
    let mut s = String::with_capacity(shown.len() * 2 + 3);
    for b in shown {
        s.push_str(&format!("{b:02x}"));
    }
    if body.len() > MAX {
        s.push_str("...");
    }
    s
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

/// Protocol-independent result of a create-account attempt, mapped by each of
/// `SID_CREATEACCOUNT` / `SID_CREATEACCOUNT2` into the reply its own wire format expects.
enum CreateOutcome {
    Created,
    NameTaken,
    Invalid,
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
    /// The platform FourCC from `SID_AUTH_INFO` (`IX86`, `PMAC`, `XMAC`). Used, with the
    /// product and version byte, for optional version restriction — see `auth_check`.
    platform: Option<FourCc>,
    /// The version byte from `SID_AUTH_INFO`. Logged always; enforced only when
    /// `versions.restrict` is on — see `auth_check`.
    version_byte: Option<u32>,
    /// This user's statstring, built from the product at `SID_AUTH_INFO`, echoed in the
    /// `EID_SHOWUSER`/`EID_JOIN` events other clients use to render them.
    statstring: Vec<u8>,
    /// CD keys claimed by `auth_check`, released on disconnect regardless of whether
    /// this session ever logged in.
    claimed_keys: Vec<KeyId>,
    account: Option<Account>,
    /// The session's server-wide-unique display name — the account name, or `Name#2`/`#3`…
    /// when that account is already online. Assigned at logon (claimed from the node's name
    /// registry), released on disconnect. This, not the account name, is what other users
    /// see in chat, so several logins of one account appear distinctly and coexist.
    display_name: String,
    channel: Option<Vec<u8>>,
    flags: u32,
    /// The game-hosting port the client announced via `SID_NETGAMEPORT`; defaults to the
    /// Battle.net game port until then. Used in the game directory so joiners can reach
    /// the host peer-to-peer.
    game_port: u16,
    /// Per-connection chat flood budget (token bucket). Every inbound chat line spends a
    /// token; over-budget lines are penalised per policy before they can fan out, so one
    /// flooder cannot multiply into channel-wide load. See `bnetcc_core::FloodTracker`.
    flood: FloodTracker,
    /// Epoch-millis until which this session is muted for flooding (`0` = not muted).
    muted_until_ms: u64,
    /// Fired by staff moderation (a tag ban or IP ban) to force this session off. The read
    /// loop selects on it and breaks, so cleanup runs normally — unlike aborting the task.
    kill: Arc<tokio::sync::Notify>,
    /// Whether this session currently has a game advertised. Set on the first `STARTADVEX3`
    /// and cleared when the game stops, so re-advertisements (state updates) do not re-count
    /// the game in the hosted-games metric.
    hosting_game: bool,
    /// An NLS logon in flight: set by `SID_AUTH_ACCOUNTLOGON`, consumed by the proof.
    srp: Option<SrpPending>,
}

/// The server-side state of one WarCraft III logon between the challenge (0x53) and the
/// proof (0x54). Dropped as soon as the proof is answered, whichever way it went.
struct SrpPending {
    /// The account being logged into (realm-qualified name).
    account: Account,
    /// The name the client typed, which is what its `M1` hashes.
    bare_name: String,
    salt: [u8; 32],
    verifier: [u8; 32],
    /// Our private exponent for this logon only.
    b: [u8; 32],
    client_public: [u8; 32],
    server_public: [u8; 32],
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

    let flood = FloodTracker::new(node.policy.game_flood, now_ms());
    let mut s = Bncs {
        node,
        out: Outbound::new(tx.clone()),
        peer,
        class: ClientClass::GamePending,
        state: SessionState::Connected,
        server_token: next_server_token(),
        product: None,
        platform: None,
        version_byte: None,
        statstring: Vec::new(),
        claimed_keys: Vec::new(),
        account: None,
        display_name: String::new(),
        channel: None,
        flags: 0,
        game_port: 6112,
        flood,
        muted_until_ms: 0,
        kill: Arc::new(tokio::sync::Notify::new()),
        hosting_game: false,
        srp: None,
    };

    // Real Battle.net (and Atlas) send SID_PING (0x25) as soon as a game client connects,
    // carrying a cookie the client echoes back; the client uses it for latency display and
    // some clients/bots wait for it before continuing the handshake. Send it up front so
    // we match that behaviour rather than leaving such a client waiting.
    {
        let mut w = Writer::with_capacity(4);
        w.u32(s.server_token);
        if let Step::Close = s.send(&Frame::new(sid::PING, w.finish())) {
            drop(tx);
            let _ = writer.await;
            return (Ok(()), s.class);
        }
    }

    let mut buf = RecvBuf::with_capacity(READ_CHUNK);
    loop {
        // The short handshake deadline guards only the automated pre-login phase; once the
        // version check passes, a human may be at the login screen. But a session that has
        // passed the version check has already claimed its CD key at SID_AUTH_CHECK, so if it
        // then goes silent (a dead/zombie socket) it holds that key for the whole deadline —
        // blocking relogin with "key in use". Bound the not-yet-logged-in window to a few
        // minutes (ample for a human to type a password) rather than the full idle timeout, so
        // a zombie releases its key promptly; only a fully logged-in session gets the long idle
        // timeout. TCP keepalive (set on accept) reaps truly-dead sockets even sooner.
        let deadline = if s.state.in_automated_handshake() {
            limits.handshake_timeout
        } else if !s.state.authenticated() {
            Duration::from_secs(180)
        } else {
            limits.idle_timeout
        };

        let tail = buf.writable_tail(READ_CHUNK);
        let n = tokio::select! {
            // Staff forced this session off (tag/IP ban). Break to run normal cleanup; any
            // removal notice already queued flushes as the writer drains before shutdown.
            () = s.kill.notified() => {
                buf.commit(0, READ_CHUNK);
                debug!(peer = %s.peer, "session closed by staff moderation");
                break;
            }
            r = tokio::time::timeout(deadline, rd.read(tail)) => match r {
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
            },
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
                if s.state.authenticated() {
                    // Post-login, an unrecognised packet is tolerated: real clients emit a
                    // long tail of optional packets (news, profile, realm, WC3 extras) and
                    // dropping the connection over one is hostile. Ignore it and continue.
                    // Pre-login the strict close below stands — that is the CVE-class guard.
                    debug!(
                        peer = %s.peer,
                        id = %format!("{:#04x}", frame.id),
                        state = s.state.name(),
                        len = frame.body.len(),
                        body = %hex_preview(&frame.body),
                        "ignoring unrecognised packet (authenticated)"
                    );
                    continue;
                }
                debug!(
                    peer = %s.peer,
                    id = %format!("{:#04x}", frame.id),
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
        // Temporary WC3 handshake trace (docs/WARCRAFT3.md §7): log where a WC3 session ended
        // so a client that stalls after SID_AUTH_INFO is diagnosable — `state=versioning` means
        // it never sent SID_AUTH_CHECK (it rejected our AUTH_INFO reply), `state=authenticating`
        // means it got stuck in the NLS logon, `logged-in`+ means login succeeded.
        if matches!(self.product, Some(p) if matches!(product::auth_family(p), Some(product::AuthFamily::Srp))) {
            info!(
                peer = %self.peer,
                product = ?self.product,
                state = self.state.name(),
                account = ?self.account.as_ref().map(|a| a.name.as_str()),
                "WC3 session ended"
            );
        }
        let now = now_ms();
        for key in self.claimed_keys.drain(..) {
            self.node.release_key(key, now);
        }
        // A game must never outlive the connection hosting it.
        if let Some(account) = &self.account {
            self.node.withdraw_game(account.id);
        }
        if let Some(key) = self.channel.take() {
            let leaver = self.display_name.clone();
            let new_op = self.node.leave_channel(&key, &leaver);
            let leave = chat_event(EventId::Leave, self.flags, 0, leaver.as_bytes(), b"");
            // Coalesced: a mass disconnect enqueues N leaves that flush as one batched write
            // per subscriber, instead of N broadcasts that overflow a bounded outbound queue.
            if let Some(bytes) = encode_vec(&leave) {
                self.node.enqueue_leave(&key, bytes);
            }
            if let Some(op) = new_op {
                debug!(channel = ?String::from_utf8_lossy(&key), new_operator = op, "operator inherited");
            }
        }
        // Free this session's display name so its `#N` slot can be reused, and drop it from
        // the moderation directory.
        if !self.display_name.is_empty() {
            self.node.unregister_session(&self.display_name);
            self.node.release_name(&self.display_name);
        }
    }

    async fn handle(&mut self, frame: &Frame) -> Step {
        debug!(
            peer = %self.peer,
            id = %format!("{:#04x}", frame.id),
            len = frame.body.len(),
            state = self.state.name(),
            body = %hex_preview(&frame.body),
            "recv BNCS frame"
        );
        // Temporary WarCraft III handshake trace (docs/WARCRAFT3.md §7): a real patched WC3
        // client stalls somewhere after SID_AUTH_INFO and we have no capture of where. Log every
        // frame from a WC3/W3XP session at INFO — scoped to the SRP auth family, so the
        // DRTL/STAR/CHAT bot fleet does not flood the log — so the next real connection shows the
        // exact packet sequence and where it stops. Remove once the WC3 login is confirmed.
        if matches!(self.product, Some(p) if matches!(product::auth_family(p), Some(product::AuthFamily::Srp))) {
            info!(
                peer = %self.peer,
                id = %format!("{:#04x}", frame.id),
                len = frame.body.len(),
                state = self.state.name(),
                body = %hex_preview(&frame.body),
                "WC3 frame in"
            );
        }
        let step = match frame.id {
            sid::AUTH_INFO => self.auth_info(frame),
            sid::AUTH_CHECK => self.auth_check(frame),
            sid::STARTVERSIONING => self.start_versioning(frame),
            sid::REPORTVERSION => self.report_version(frame),
            sid::LOGONRESPONSE => self.logon(frame, sid::LOGONRESPONSE).await,
            sid::LOGONRESPONSE2 => self.logon(frame, sid::LOGONRESPONSE2).await,
            // WarCraft III's NLS/SRP logon (docs/WARCRAFT3.md §3).
            sid::AUTH_ACCOUNTCREATE => self.auth_account_create(frame).await,
            sid::AUTH_ACCOUNTLOGON => self.auth_account_logon(frame).await,
            sid::AUTH_ACCOUNTLOGONPROOF => self.auth_account_logon_proof(frame).await,
            sid::CREATEACCOUNT => self.create_account_legacy(frame).await,
            sid::CREATEACCOUNT2 => self.create_account2(frame).await,
            sid::ENTERCHAT => self.enter_chat(frame),
            sid::JOINCHANNEL => self.join_channel(frame),
            sid::LEAVECHAT => self.leave_chat(),
            sid::CHATCOMMAND => self.chat_command(frame).await,
            sid::GETCHANNELLIST => self.channel_list(),
            sid::GETADVLISTEX => self.game_list(frame),
            sid::CHECKAD => self.check_ad(frame),
            sid::STARTADVEX3 => self.advertise(frame),
            // A game ended (STOPADV is also sent spuriously on logoff, which is harmless —
            // withdrawing a game the host does not have is a no-op).
            sid::STOPADV | sid::LEAVEGAME => self.stop_advertising(),
            sid::NETGAMEPORT => self.net_game_port(frame),
            // Map authentication: a host sends the map's size, SHA-1 and filename to be
            // "authenticated" before the game can start. We approve every map — see the
            // handler; refusing leaves the host stuck on "Unable to authenticate map".
            sid::CHECKDATAFILE2 => self.check_data_file2(frame),
            // Post-game result report — the data behind win/loss records and the ladder.
            // Currently capture-only while the wire format is decoded; see the handler.
            sid::GAMERESULT => self.game_result(frame).await,
            // Legacy CD-key checks (old-logon flow). We accept the key — CD-key uniqueness
            // is enforced on the modern AUTH_CHECK path via KeyRegistry, not here.
            sid::CDKEY => self.cd_key_reply(sid::CDKEY),
            sid::CDKEY2 => self.cd_key_reply(sid::CDKEY2),
            // The client asks for a file's modification time (TOS, bnserver.ini, icons)
            // to decide whether to re-download it over BNFTP. It waits for this reply, so
            // never leave it unanswered.
            sid::GETFILETIME => self.get_filetime(frame),
            // Icon file negotiation: tell the client which BNI icon file to fetch (per
            // product) and its filetime; the client BNFTP-downloads it and renders user
            // icons from it. War2 and others show blank icons without this.
            sid::GETICONDATA => self.get_icon_data(),
            // Profile reads. Returns empty for every key for now — see the handler; this
            // keeps CVE-2004-2705 shut (no account's keys are ever returned) while letting
            // the client's login-screen and profile reads complete instead of hanging.
            sid::READUSERDATA => self.read_user_data(frame).await,
            // Friends and news are not stored yet; both get an empty, well-formed reply so
            // the client's panels settle instead of waiting (WarCraft III asks for both
            // right after logon). Serving real data is future work (docs/ROADMAP.md).
            sid::FRIENDSLIST => self.friends_list(),
            sid::NEWS_INFO => self.news_info(),
            // Keepalive, the UDP detection reply, legacy-logon informational packets, and
            // advertisement telemetry — all accepted but not acted on.
            sid::NULL
            | sid::PING
            | sid::UDPPINGRESPONSE
            | sid::NOTIFYJOIN
            | sid::CLICKAD
            | sid::CLIENTID
            | sid::CLIENTID2
            | sid::LOCALEINFO
            | sid::SYSTEMINFO
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

    /// Kick off the login-time UDP ping to this client (see [`crate::udp::ping_client`]).
    /// Classic clients bind UDP `:6112` and wait for the server to ping them before they
    /// un-grey Create/Join. Called once per client from whichever login flow it uses:
    /// `auth_info` (modern) or `start_versioning` (legacy). Fire-and-forget; skips products
    /// that never run the check and hosts sharing ours (handled inside `ping_client`).
    fn spawn_udp_ping(&self) {
        if matches!(self.product, Some(p) if product::always_no_udp(p)) {
            return;
        }
        if let Some(sock) = &self.node.udp_socket {
            let sock = Arc::clone(sock);
            let ip = self.peer.ip();
            tokio::spawn(crate::udp::ping_client(sock, ip));
        }
    }

    fn auth_info(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let _protocol = r.u32()?;
            let platform = r.fourcc()?;
            let product = r.fourcc()?;
            let version_byte = r.u32()?;
            Ok::<_, bnetcc_proto::ProtoError>((platform, product, version_byte))
        })();
        let Ok((platform, product, version_byte)) = parsed else {
            return Step::Close;
        };
        self.product = Some(product);
        self.platform = Some(platform);
        self.version_byte = Some(version_byte);
        self.statstring = statstring::build_default(product);
        // Whether this is *enforced* depends on config (versions.restrict); either way it
        // is logged, so an operator can see what real client builds are connecting before
        // deciding what to allow. Enforcement itself happens in `auth_check`.
        info!(
            peer = %self.peer,
            product = %product,
            platform = %platform,
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
        // A WarCraft III node can be configured for the legacy X-SHA-1 logon instead of
        // NLS/SRP (config `server.wc3_logon = "legacy"`). Legacy advertises logon type 0 and
        // omits the 128-byte RSA signature the NLS path carries — the signature WC3 verifies
        // after the proof and that only a patched/loadered client accepts (docs/LEGAL.md §3,
        // docs/WARCRAFT3.md §3.7). The 0x53/0x54 handlers stay wired either way.
        let nls = srp && !self.node.wc3_legacy_logon;
        let logon_type: u32 = if nls { 0x02 } else { 0x00 };

        let mpq = version_mpq_name(self.platform, self.product, self.version_byte);
        // The file's real modification time, so a client that caches the MPQ by filetime
        // (WarCraft III does) decides correctly whether to re-fetch it; 0 if we lack the file.
        let mpq_filetime = self.node.file_mtime(mpq.as_bytes()).map_or(0, unix_to_filetime);
        let mut w = Writer::with_capacity(64);
        w.u32(logon_type)
            .u32(self.server_token)
            // Non-zero UDP value so the client runs its UDP connectivity test (which the
            // UDP listener answers). A zero here tells the client not to bother, leaving
            // game hosting disabled. See crate::udp.
            .u32(self.server_token)
            .u64(mpq_filetime) // CheckRevision MPQ filetime
            .cstr(mpq.as_bytes())
            // CheckRevision value string. We fail *open* on the version check (we never
            // recompute the hash), so any well-formed formula the client can run is fine —
            // but WarCraft III should get the real `ver-IX86-1.mpq` formula that a stock
            // server sends (confirmed against a bnetdocs / PvPGN Pro capture), not a
            // placeholder. Other products keep the neutral placeholder until each one's real
            // formula is captured; a wrong formula still costs nothing here.
            .cstr(if matches!(product, product::WAR3 | product::W3XP) {
                b"B=454282227 C=2370009462 A=2264812340 4 A=A^S B=B-C C=C-A A=A+B".as_slice()
            } else {
                b"A=1 B=1 C=1 4 A=A^S B=B^C C=C^A A=A^B".as_slice()
            });
        if srp {
            // The `SID_AUTH_INFO` reply for WarCraft III carries a 128-byte RSA signature
            // field — a W3XP/WAR3 *product* field the client's parser expects regardless of
            // logon type, so it must be present on the legacy path (logon type 0) too or the
            // client rejects the reply as an "invalid Battle.net server" before the version
            // check. We cannot produce a valid signature (no Blizzard key); on the NLS path
            // the client *verifies* it and only a patched client accepts our zeros
            // (docs/LEGAL.md §3). The legacy path advertises logon type 0, under which the
            // client does not verify this field.
            w.bytes(&[0u8; 128]);
        }
        let step = self.send(&Frame::new(sid::AUTH_INFO, w.finish()));
        // Modern-flow clients also wait for the server's UDP ping to un-grey Create/Join.
        // Guarded against same-host self-loops inside `spawn_udp_ping`/`ping_client`.
        self.spawn_udp_ping();
        step
    }

    /// Two checks live here. **Version restriction** is opt-in (`versions.restrict`):
    /// off by default so nobody is locked out, and when on it rejects a client whose
    /// product/platform/version-byte (captured in `auth_info`) is not on the allowlist,
    /// with `0x100` "old version". **CD-key uniqueness** is always on: one live session
    /// per key, the real gate on a bot fleet (addresses are cheap, keys are not — see
    /// `docs/WARNET.md`).
    ///
    /// The request wire layout parsed below was confirmed against a real BNLS-assisted
    /// client connecting to Atlas on 2026-09-09 (see `docs/PROTOCOL-NOTES.md` §3), for the
    /// single-key StarCraft/Brood War case. The two-key WarCraft III case is extrapolated,
    /// not yet captured. A parse failure still fails *open* — log and accept, skipping only
    /// the key check — because refusing every logon over a misparsed frame is worse than
    /// not enforcing uniqueness; version restriction, when on, is applied before the parse
    /// and so is unaffected.
    fn auth_check(&mut self, frame: &Frame) -> Step {
        // Version restriction comes first: it depends only on what auth_info already
        // captured, not on parsing this frame, so it holds even if the body is malformed.
        if let (Some(product), Some(platform), Some(version_byte)) =
            (self.product, self.platform, self.version_byte)
        {
            if !self.node.version_policy.allows(product, platform, version_byte) {
                info!(
                    peer = %self.peer,
                    product = %product,
                    platform = %platform,
                    version_byte = %format!("{version_byte:#010x}"),
                    "rejected by version restriction"
                );
                let mut w = Writer::with_capacity(16);
                // 0x100 "old version" carries a patch-MPQ filename; we have none, so send
                // an empty string. The client shows an upgrade-required message.
                w.u32(auth_check_status::OLD_VERSION).cstr(b"");
                return self.send(&Frame::new(sid::AUTH_CHECK, w.finish()));
            }
        }

        let mut r = frame.reader();
        let parsed = (|| {
            let _client_token = r.u32()?;
            let _exe_version = r.u32()?;
            let _exe_hash = r.u32()?;
            let key_count = r.u32()?;
            let _spawn = r.u32()?;
            // The per-key block comes BEFORE the EXE info string, not after it. Confirmed
            // against a real client (BNLS-assisted bot) connecting to Atlas, 2026-09-09:
            //   client_token, exe_version, exe_hash, key_count, spawn,
            //   [key_length, product_value, public_value, reserved, hash:20]*,
            //   exe_info:cstr, owner:cstr
            let mut keys = Vec::with_capacity(key_count.min(2) as usize);
            for _ in 0..key_count.min(2) {
                let _key_length = r.u32()?;
                let product_value = r.u32()?;
                let public_value = r.u32()?;
                let _reserved = r.u32()?;
                let _hash: [u8; 20] = r.array()?;
                keys.push(key_fingerprint(product_value, public_value));
            }
            let _exe_info = r.cstr(256)?;
            let _owner = r.cstr(64)?;
            Ok::<_, bnetcc_proto::ProtoError>(keys)
        })();

        // Operator opted out of one-session-per-key (e.g. a test fleet on one key).
        if !self.node.cd_key_uniqueness {
            return self.auth_check_ok();
        }

        let keys = match parsed {
            Ok(keys) => keys,
            Err(e) => {
                warn!(
                    peer = %self.peer,
                    error = %e,
                    "could not parse SID_AUTH_CHECK; accepting without a CD-key uniqueness \
                     check (see this function's fail-open note)"
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
                    // Refusals were previously silent server-side (the holder name only went to
                    // the client). Log it: a blank holder means the key is held by a session
                    // that claimed it but never finished logging in — i.e. a lingering zombie,
                    // not a real second player. See the pre-login deadline note above.
                    warn!(
                        peer = %self.peer,
                        product = ?self.product,
                        holder = %if holder.is_empty() { "<unconfirmed pre-login claim>" } else { holder.as_str() },
                        "CD-key refused: already in use (one live session per key)"
                    );
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

    /// Handle a logon proof. `reply_id` distinguishes the two forms, which share the same
    /// request layout (client token, server token, 20-byte double-hash proof, username)
    /// and verification but differ in their reply's result codes:
    /// - `SID_LOGONRESPONSE2` (0x3A): 0x00 success · 0x01 no account · 0x02 wrong password.
    /// - `SID_LOGONRESPONSE` (0x29, legacy): 0x00 **failure** · 0x01 **success**.
    async fn logon(&mut self, frame: &Frame, reply_id: u8) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let client_token = r.u32()?;
            let client_server_token = r.u32()?;
            let proof: [u8; 20] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((client_token, client_server_token, proof, username))
        })();
        let Ok((client_token, client_server_token, proof, username)) = parsed else {
            return Step::Close;
        };
        let name = String::from_utf8_lossy(&username).to_string();

        // Which server token the proof was computed with:
        // - Modern LOGONRESPONSE2 (0x3A): the token WE issued in SID_AUTH_INFO. Never trust
        //   the client's echo here — pinning it to our value is the replay protection.
        // - Legacy LOGONRESPONSE (0x29): the old flow issues no server token, so the client
        //   uses whatever it put in the packet (real old clients send 0). Verify with that,
        //   or the proof can never match and every legacy logon fails as "wrong password".
        let server_token = if reply_id == sid::LOGONRESPONSE {
            client_server_token
        } else {
            self.server_token
        };

        let ok = match self.node.account(&name).await {
            None => false,
            Some(account) => {
                // An X-SHA-1 proof can only be checked against an X-SHA-1 credential. A
                // WarCraft III (SRP) account is invisible to this flow — and it lives in
                // its own realm anyway, so the name would not have matched.
                let proof_ok = match &account.credential {
                    Credential::Xsha1 { digest } => {
                        proofs_match(&proof, &logon_proof(client_token, server_token, digest))
                    }
                    Credential::Srp { .. } => false,
                };
                if !proof_ok {
                    false
                } else if self.node.bans.is_tag_banned(&account.name) {
                    // A staff `/tagban` refuses any account whose name matches the banned
                    // substring, even with the correct password. Reported as a plain logon
                    // failure — the protocol has no "you are banned" status here.
                    info!(peer = %self.peer, account = %account.name, "logon refused: name is tag-banned");
                    false
                } else {
                    self.finish_logon(account).await;
                    true
                }
            }
        };

        let mut w = Writer::with_capacity(8);
        if reply_id == sid::LOGONRESPONSE {
            // Legacy: 1 = success, 0 = failure (no distinct no-account/wrong-password).
            w.u32(u32::from(ok));
        } else {
            w.u32(if ok {
                logon_status::SUCCESS
            } else {
                logon_status::NO_SUCH_ACCOUNT
            });
        }
        self.send(&Frame::new(reply_id, w.finish()))
    }

    /// Everything a successful logon does after the proof checks out, shared by the
    /// X-SHA-1 flows and the WarCraft III NLS flow: key-holder bookkeeping, admin and
    /// stored flags, the logon timestamp, the server-wide-unique display name, and the
    /// moderation registry. Sets `self.account`, which is what lets the state machine
    /// advance to `LoggedIn`.
    async fn finish_logon(&mut self, account: Account) {
        info!(peer = %self.peer, account = %account.name, "logon accepted");
        for &key in &self.claimed_keys {
            self.node.record_key_holder_name(key, account.name.clone());
        }
        // Sysops carry the Battle.net Administrator flag (the sysop icon) in every channel
        // they enter — not the Blizzard-representative flag, which is Blizzard's own staff
        // marker and the wrong identity for a private-server operator. A custom operator icon
        // (bnet.cc) can later be mapped to this flag in the product icons.bni.
        if self.node.admins.is_admin(&account.name) {
            self.flags |= user_flags::ADMIN;
            info!(peer = %self.peer, account = %account.name, "administrator logged on");
        }
        // Apply any admin-assigned flags stored on the account (staff, speaker,
        // special guest), masked to the assignable set. This is how the panel's
        // per-account flag/staff changes take effect — at the next logon.
        self.flags |= self.node.user_flags(account.id).await;
        // Note the logon time for the admin user list (fire-and-forget).
        self.node.record_login(account.id, now_ms() / 1000);
        // Claim a server-wide-unique display name (Name, or Name#2/#3… if this exact account
        // is already online elsewhere) for the life of this session.
        //
        // WarCraft III accounts are stored and shown realm-qualified as `Name@<realm>` — the
        // way Battle.net has shown WC3 users since the game launched (`Name@Azeroth`, etc.).
        // The realm suffix is the account's identity here, so a WC3 `Tagban@bncc` and an
        // X-SHA-1 `Tagban` are distinct names that coexist without a `#N` collision. See
        // docs/WARCRAFT3.md §3.6.
        self.display_name = self.node.claim_name(&account.name);
        // Register with the moderation directory so staff can reach this session
        // (resolve its IP, or force it off) from any channel.
        self.node.register_session(
            &self.display_name,
            self.peer.ip(),
            self.out.clone(),
            Arc::clone(&self.kill),
        );
        self.account = Some(account);
    }

    /// The bare name a WarCraft III client typed, as a string, or `None` if it contains
    /// `@`. The realm is the server's business: the client's verifier and proof hash the
    /// typed name exactly, so a name typed *with* a suffix could never match a verifier
    /// created without one. Refusing `@` outright keeps that from ever looking like a
    /// wrong password. See `docs/WARCRAFT3.md` §3.6.
    fn realm_bare_name(&self, typed: &[u8]) -> Option<String> {
        let typed = String::from_utf8_lossy(typed).into_owned();
        (!typed.contains('@')).then_some(typed)
    }

    /// The realm-qualified account name for a bare WarCraft III name.
    fn qualified_name(&self, bare: &str) -> String {
        format!("{bare}@{}", self.node.realm)
    }

    /// `SID_AUTH_ACCOUNTCREATE` (0x52): a WarCraft III client registers a salt and
    /// verifier for a name. The account is created in this node's realm as `Name@<realm>`,
    /// so it can never collide with an X-SHA-1 account of the same spelling. Reply is a
    /// single status DWORD (`nls_status::CREATE_*`); creation does not log the client in,
    /// it sends `SID_AUTH_ACCOUNTLOGON` next.
    async fn auth_account_create(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let salt: [u8; 32] = r.array()?;
            let verifier: [u8; 32] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((salt, verifier, username))
        })();
        let Ok((salt, verifier, username)) = parsed else {
            return Step::Close;
        };
        let status = match self.realm_bare_name(&username) {
            None => nls_status::CREATE_ILLEGAL_CHAR,
            Some(bare) => {
                let qualified = self.qualified_name(&bare);
                match self
                    .node
                    .create_account(&qualified, Credential::Srp { salt, verifier })
                    .await
                {
                    Ok(account) => {
                        info!(peer = %self.peer, account = %account.name, "account created (NLS)");
                        nls_status::CREATE_OK
                    }
                    Err(crate::storage::CreateAccountError::NameTaken) => nls_status::CREATE_NAME_EXISTS,
                    Err(crate::storage::CreateAccountError::Invalid(why)) => {
                        debug!(peer = %self.peer, name = %qualified, reason = %why, "NLS account creation refused");
                        if why.contains("shorter") {
                            nls_status::CREATE_TOO_SHORT
                        } else {
                            nls_status::CREATE_ILLEGAL_CHAR
                        }
                    }
                    Err(crate::storage::CreateAccountError::Backend(why)) => {
                        warn!(peer = %self.peer, reason = %why, "NLS account creation failed in storage");
                        return Step::Close;
                    }
                }
            }
        };
        let mut w = Writer::with_capacity(4);
        w.u32(status);
        self.send(&Frame::new(sid::AUTH_ACCOUNTCREATE, w.finish()))
    }

    /// `SID_AUTH_ACCOUNTLOGON` (0x53): the client sends its public key `A` and the name;
    /// we answer with the account's salt and our public key `B`. The reply is always 72
    /// bytes — status, `s[32]`, `B[32]` — zeroed on failure, which is what the client
    /// expects. A fresh `b` is drawn per logon and kept only until the proof is answered.
    async fn auth_account_logon(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let client_public: [u8; 32] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((client_public, username))
        })();
        let Ok((client_public, username)) = parsed else {
            return Step::Close;
        };
        // A new challenge invalidates any earlier one.
        self.srp = None;

        let mut w = Writer::with_capacity(72);
        let account = match self.realm_bare_name(&username) {
            Some(bare) => self.node.account(&self.qualified_name(&bare)).await.map(|a| (bare, a)),
            None => None,
        };
        match account {
            Some((bare_name, account)) => {
                let Credential::Srp { salt, verifier } = account.credential.clone() else {
                    // Cannot happen by construction (only SRP accounts carry a realm), but
                    // refuse rather than guess if storage ever hands us one.
                    w.u32(nls_status::LOGON_NO_ACCOUNT).bytes(&[0u8; 64]);
                    return self.send(&Frame::new(sid::AUTH_ACCOUNTLOGON, w.finish()));
                };
                let b: [u8; 32] = rand::random();
                // The modular exponentiation is CPU work; keep it off the reactor
                // (docs/ARCHITECTURE.md §3).
                let server_public =
                    match tokio::task::spawn_blocking(move || bnetcc_crypto::nls::server_public(&verifier, &b)).await
                    {
                        Ok(v) => v,
                        Err(_) => return Step::Close,
                    };
                self.srp = Some(SrpPending {
                    account,
                    bare_name,
                    salt,
                    verifier,
                    b,
                    client_public,
                    server_public,
                });
                info!(peer = %self.peer, name = %String::from_utf8_lossy(&username), "NLS logon (0x53): challenge issued");
                w.u32(nls_status::LOGON_OK).bytes(&salt).bytes(&server_public);
            }
            None => {
                info!(peer = %self.peer, name = %String::from_utf8_lossy(&username), "NLS logon (0x53): no such account");
                w.u32(nls_status::LOGON_NO_ACCOUNT).bytes(&[0u8; 64]);
            }
        }
        self.send(&Frame::new(sid::AUTH_ACCOUNTLOGON, w.finish()))
    }

    /// `SID_AUTH_ACCOUNTLOGONPROOF` (0x54): the client's `M1`. Verified against the
    /// challenge from `auth_account_logon`; on success the reply carries our `M2` and the
    /// session is logged in. Reply: status, `M2[20]` (zeroed on failure), info string.
    async fn auth_account_logon_proof(&mut self, frame: &Frame) -> Step {
        let Ok(client_proof) = frame.reader().array::<20>() else {
            return Step::Close;
        };
        // A proof with no outstanding challenge is a protocol violation.
        let Some(p) = self.srp.take() else {
            debug!(peer = %self.peer, "NLS proof without a challenge; closing");
            return Step::Close;
        };
        let verdict = tokio::task::spawn_blocking(move || {
            bnetcc_crypto::nls::server_verify(
                &p.bare_name,
                &p.salt,
                &p.verifier,
                &p.b,
                &p.client_public,
                &p.server_public,
                &client_proof,
            )
            .map(|m2| (m2, p.account))
        })
        .await;

        let mut w = Writer::with_capacity(32);
        match verdict {
            Ok(Some((server_proof, account))) => {
                if self.node.bans.is_tag_banned(&account.name) {
                    // Same rule as the X-SHA-1 flow, but WarCraft III can show a reason.
                    info!(peer = %self.peer, account = %account.name, "NLS logon refused: name is tag-banned");
                    w.u32(nls_status::PROOF_CUSTOM_ERROR)
                        .bytes(&[0u8; 20])
                        .cstr(b"This account is banned from this server.");
                } else {
                    self.finish_logon(account).await;
                    // Status + M2 and *nothing else*. The additional-info string is only
                    // sent with a custom error (0x0F). A real WarCraft III client validates
                    // the exact `0x54` body length: for a non-error status it expects exactly
                    // 24 bytes (4 + 20) and drops the connection if a trailing byte follows.
                    // A bnetdocs (PvPGN Pro) capture confirmed the 24-byte form; our earlier
                    // trailing empty-string null was the post-logon "connection lost" drop.
                    w.u32(nls_status::PROOF_OK).bytes(&server_proof);
                }
            }
            Ok(None) => {
                info!(peer = %self.peer, "NLS logon (0x54): wrong password (M1 mismatch)");
                w.u32(nls_status::PROOF_WRONG_PASSWORD).bytes(&[0u8; 20]);
            }
            Err(_) => return Step::Close,
        }
        self.send(&Frame::new(sid::AUTH_ACCOUNTLOGONPROOF, w.finish()))
    }

    /// `SID_STARTVERSIONING` (0x06), the legacy flow's opener. Payload is platform,
    /// product and version byte; the reply is the MPQ filetime, filename and
    /// check-revision formula, mirroring what `SID_AUTH_INFO` returns in the modern flow.
    fn start_versioning(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        if let Ok((platform, product, version_byte)) = (|| {
            let platform = r.fourcc()?;
            let product = r.fourcc()?;
            let version_byte = r.u32()?;
            Ok::<_, bnetcc_proto::ProtoError>((platform, product, version_byte))
        })() {
            self.platform = Some(platform);
            self.product = Some(product);
            self.version_byte = Some(version_byte);
            self.statstring = statstring::build_default(product);
            if product::always_no_udp(product) {
                // Diablo (DRTL/DSHR) never runs the UDP check, so its users always carry the
                // No-UDP flag. This is the legacy-flow counterpart to the same line in
                // `auth_info`; without it a Diablo user — which only ever logs in through
                // this path — would be shown to others without the flag. See
                // `product::always_no_udp` and `spawn_udp_ping` (which skips the ping here).
                self.flags |= user_flags::NO_UDP;
            }
            info!(
                peer = %self.peer,
                product = %product,
                platform = %platform,
                version_byte = %format!("{version_byte:#010x}"),
                "SID_STARTVERSIONING (legacy)"
            );
        }
        let mpq = version_mpq_name(self.platform, self.product, self.version_byte);
        let mpq_filetime = self.node.file_mtime(mpq.as_bytes()).map_or(0, unix_to_filetime);
        let mut w = Writer::with_capacity(64);
        w.u64(mpq_filetime) // MPQ filetime
            .cstr(mpq.as_bytes())
            .cstr(b"A=1 B=1 C=1 4 A=A^S B=B^C C=C^A A=A^B");
        let step = self.send(&Frame::new(sid::STARTVERSIONING, w.finish()));
        // Legacy-flow clients (StarCraft, Warcraft II BNE, Diablo) bind UDP :6112 and wait
        // for the server to ping them; without it Create/Join stay greyed. Confirmed against
        // a real W2BN client, 2026-09-09. Guarded against same-host self-loops.
        self.spawn_udp_ping();
        step
    }

    /// `SID_REPORTVERSION` (0x07), the legacy version/CD-key check. Enforces version
    /// restriction if configured (like `auth_check`), otherwise accepts. Result: 0x02 is
    /// success; the trailing string is a patch path, empty when none is needed.
    fn report_version(&mut self, _frame: &Frame) -> Step {
        if let (Some(product), Some(platform), Some(version_byte)) =
            (self.product, self.platform, self.version_byte)
        {
            if !self.node.version_policy.allows(product, platform, version_byte) {
                info!(
                    peer = %self.peer,
                    product = %product,
                    platform = %platform,
                    "rejected by version restriction (legacy)"
                );
                return self.report_version_reply(0x0000); // 0 = failed version check
            }
        }
        self.report_version_reply(0x0002) // 2 = success
    }

    /// Build a `SID_REPORTVERSION` reply. The response is `(UINT32) Result`, `(STRING)`
    /// patch path, then a trailing `(UINT8)` — the last byte is documented on BNETDocs and
    /// omitting it makes some old clients under-read the packet and report the version as
    /// unsupported even on a success result. Patch path is empty (no patch to apply).
    fn report_version_reply(&mut self, result: u32) -> Step {
        let mut w = Writer::with_capacity(8);
        w.u32(result).cstr(b"").u8(0);
        self.send(&Frame::new(sid::REPORTVERSION, w.finish()))
    }

    /// `SID_GETFILETIME` (0x33): the client asks for a file's modification time so it can
    /// decide whether its cached copy is current or must be re-downloaded over BNFTP. The
    /// reply echoes the request id and filename with the file's FILETIME (0 if we do not
    /// have the file). The client blocks on this during the TOS/config step of login.
    fn get_filetime(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let parsed = (|| {
            let request_id = r.u32()?;
            let unknown = r.u32()?;
            let filename = r.cstr(bnftp::MAX_FILENAME)?.to_vec();
            Ok::<_, bnetcc_proto::ProtoError>((request_id, unknown, filename))
        })();
        let Ok((request_id, unknown, filename)) = parsed else {
            return Step::Close;
        };
        let filetime = self.node.file_mtime(&filename).map_or(0, unix_to_filetime);
        let mut w = Writer::with_capacity(32);
        w.u32(request_id).u32(unknown).u64(filetime).cstr(&filename);
        self.send(&Frame::new(sid::GETFILETIME, w.finish()))
    }

    /// `SID_READUSERDATA` (0x26): the client requests profile keys for one or more
    /// accounts. Request is `(DWORD)` account count, `(DWORD)` key count, `(DWORD)` request
    /// id, then the account names and key names. The reply echoes the counts and id, then
    /// `accounts * keys` value strings.
    ///
    /// **Security:** this is CVE-2004-2705's exact vector — a server that returns any key of
    /// any account leaks password digests. Every value here goes through
    /// `bnetcc_storage::attr::AttrSchema::filter_readable` at the `Actor::Other` level (in
    /// the storage actor), so only world-readable keys — game records, profile fields,
    /// non-secret system keys — are ever returned; the password digest and unknown/private
    /// keys resolve to an empty cell by construction, not by this handler remembering to
    /// check. Requested keys the account does not have also come back empty.
    async fn read_user_data(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        // The tuple is a one-off parse result threaded straight into the handler below;
        // naming a struct for it would not earn its keep.
        #[allow(clippy::type_complexity)]
        let parsed = (|| -> Result<(usize, usize, u32, Vec<String>, Vec<String>), bnetcc_proto::ProtoError> {
            let accounts = (r.u32()? as usize).min(64);
            let keys = (r.u32()? as usize).min(64);
            let request_id = r.u32()?;
            let mut account_names = Vec::with_capacity(accounts);
            for _ in 0..accounts {
                account_names.push(String::from_utf8_lossy(r.cstr(64)?).into_owned());
            }
            let mut key_names = Vec::with_capacity(keys);
            for _ in 0..keys {
                key_names.push(String::from_utf8_lossy(r.cstr(256)?).into_owned());
            }
            Ok((accounts, keys, request_id, account_names, key_names))
        })();
        let Ok((accounts, keys, request_id, account_names, key_names)) = parsed else {
            return Step::Close;
        };
        let attr_keys: Vec<bnetcc_storage::attr::AttrKey> =
            key_names.iter().map(|k| bnetcc_storage::attr::AttrKey::new(k)).collect();
        let mut w = Writer::with_capacity(32 + accounts * keys);
        w.u32(accounts as u32).u32(keys as u32).u32(request_id);
        for name in &account_names {
            let readable = self.node.read_readable_attrs(name, attr_keys.clone()).await;
            for key in &attr_keys {
                w.cstr(readable.get(key).map_or(b"".as_slice(), |v| v.as_bytes()));
            }
        }
        self.send(&Frame::new(sid::READUSERDATA, w.finish()))
    }

    /// `SID_GETICONDATA` (0x2D): tell the client which BNI icon file to use for this
    /// product and its filetime. The client then BNFTP-downloads the file (if its cached
    /// copy is stale) and renders each user's icon from it based on flags/statstring.
    /// Reply is `(FILETIME)` then `(STRING)` filename.
    fn get_icon_data(&mut self) -> Step {
        let name = icon_file_name(self.product);
        let filetime = self.node.file_mtime(name).map_or(0, unix_to_filetime);
        let mut w = Writer::with_capacity(32);
        w.u64(filetime).cstr(name);
        self.send(&Frame::new(sid::GETICONDATA, w.finish()))
    }

    /// Reply to a legacy `SID_CDKEY` / `SID_CDKEY2` CD-key check. Response is `(UINT32)`
    /// Result (0x01 = Ok) then `(STRING)` Key owner. We accept the key here — legacy CD-key
    /// *uniqueness* is not yet enforced on this path (the modern `SID_AUTH_CHECK` path
    /// does that via `KeyRegistry`); wiring it here is future work (see docs/ROADMAP.md).
    /// `reply_id` echoes whichever of the two packets the client sent.
    fn cd_key_reply(&mut self, reply_id: u8) -> Step {
        const RESULT_OK: u32 = 0x01;
        let owner = self.account.as_ref().map_or(String::new(), |a| a.name.clone());
        let mut w = Writer::with_capacity(16);
        w.u32(RESULT_OK).cstr(owner.as_bytes());
        self.send(&Frame::new(reply_id, w.finish()))
    }

    /// Parse a create-account request (20-byte password hash then username — the shared
    /// shape of both `SID_CREATEACCOUNT` and `SID_CREATEACCOUNT2`) and attempt the create.
    ///
    /// Returns `Ok(outcome)` with a protocol-independent result the caller formats into
    /// the reply its packet expects, or `Err(Step::Close)` when the frame is malformed or
    /// storage failed hard.
    async fn do_create_account(&mut self, frame: &Frame) -> Result<CreateOutcome, Step> {
        let mut r = frame.reader();
        let parsed = (|| {
            let hash: [u8; 20] = r.array()?;
            let username = read_username(&mut r)?;
            Ok::<_, bnetcc_proto::ProtoError>((hash, username))
        })();
        let Ok((hash, username)) = parsed else {
            return Err(Step::Close);
        };
        let name = String::from_utf8_lossy(&username).to_string();

        // Both packets carry the password hashed **once** (unlike the logon proof, which
        // is a double hash), so storing what arrives is correct.
        // `@` designates a realm-scoped account (WarCraft III's namespace — see
        // docs/WARCRAFT3.md §3.6); an X-SHA-1 client cannot register into a realm.
        if name.contains('@') {
            debug!(peer = %self.peer, name = %name, "account creation refused: '@' is reserved for realms");
            return Ok(CreateOutcome::Invalid);
        }
        match self.node.create_account(&name, Credential::Xsha1 { digest: hash }).await {
            Ok(account) => {
                // Creation does not log you in; the client sends a logon next.
                info!(peer = %self.peer, account = %account.name, "account created");
                Ok(CreateOutcome::Created)
            }
            Err(crate::storage::CreateAccountError::NameTaken) => Ok(CreateOutcome::NameTaken),
            Err(crate::storage::CreateAccountError::Invalid(why)) => {
                debug!(peer = %self.peer, name = %name, reason = %why, "account creation refused");
                Ok(CreateOutcome::Invalid)
            }
            Err(crate::storage::CreateAccountError::Backend(why)) => {
                warn!(peer = %self.peer, reason = %why, "account creation failed in storage");
                Err(Step::Close)
            }
        }
    }

    /// `SID_CREATEACCOUNT2` (0x3D): reply carries a status code and an (empty) reason
    /// string. Codes beyond 0x00/0x04 are unverified against a real client — see
    /// `docs/PROTOCOL-NOTES.md`.
    async fn create_account2(&mut self, frame: &Frame) -> Step {
        let outcome = match self.do_create_account(frame).await {
            Ok(o) => o,
            Err(step) => return step,
        };
        let status: u32 = match outcome {
            CreateOutcome::Created => 0x00,
            CreateOutcome::NameTaken => 0x04,
            CreateOutcome::Invalid => 0x02,
        };
        let mut w = Writer::with_capacity(8);
        w.u32(status).cstr(b"");
        self.send(&Frame::new(sid::CREATEACCOUNT2, w.finish()))
    }

    /// `SID_CREATEACCOUNT` (0x2A): the older form, reply is a bare result DWORD. The
    /// success/failure values are unverified against a real old client (the one client
    /// tested against this path ignores the value and simply reconnects); we use the same
    /// 0x00-is-success convention as the rest of the protocol family. See
    /// `docs/PROTOCOL-NOTES.md`.
    async fn create_account_legacy(&mut self, frame: &Frame) -> Step {
        let outcome = match self.do_create_account(frame).await {
            Ok(o) => o,
            Err(step) => return step,
        };
        let result: u32 = match outcome {
            CreateOutcome::Created => 0x00,
            CreateOutcome::NameTaken | CreateOutcome::Invalid => 0x01,
        };
        let mut w = Writer::with_capacity(4);
        w.u32(result);
        self.send(&Frame::new(sid::CREATEACCOUNT, w.finish()))
    }

    fn enter_chat(&mut self, _frame: &Frame) -> Step {
        if self.account.is_none() {
            return Step::Close;
        }
        // The unique display name (with any #N), so a duplicate login sees itself correctly.
        let mut w = Writer::with_capacity(64);
        w.cstr(self.display_name.as_bytes())
            .cstr(self.statstring.as_slice())
            .cstr(self.display_name.as_bytes());
        if matches!(self.send(&Frame::new(sid::ENTERCHAT, w.finish())), Step::Close) {
            return Step::Close;
        }
        // Server MOTD, once, at chat entry — not on every channel join. A per-channel MOTD
        // is a separate future feature (see docs/ROADMAP.md).
        self.send(&chat_event(EventId::Info, 0, 0, b"", self.node.motd.as_bytes()))
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
        self.do_join(&requested, &account)
    }

    /// Join a channel by name, from either `SID_JOINCHANNEL` or a `/join` command.
    fn do_join(&mut self, requested: &[u8], account: &Account) -> Step {
        // If the client named no channel, fall back to its product's default channel (when one
        // is configured); otherwise honour exactly what it asked for.
        let product = self.product.map(|p| p.to_string());
        let requested: Vec<u8> = if String::from_utf8_lossy(requested).trim().is_empty() {
            product
                .as_deref()
                .and_then(|p| self.node.channel_rules.default_channel(p))
                .map_or_else(|| requested.to_vec(), |chan| chan.as_bytes().to_vec())
        } else {
            requested.to_vec()
        };
        let requested = requested.as_slice();

        // Leave the current channel first; joining your current channel returns you to
        // the previous one on real Battle.net, but leaving unconditionally is the
        // behaviour clients cope with and is far easier to reason about.
        if let Some(prev) = self.channel.take() {
            if prev == normalize_channel_name(requested) {
                // Re-joining the same channel: nothing to do.
                self.channel = Some(prev);
                return Step::Continue;
            }
            self.leave_current(&prev);
        }

        match self.node.join_channel(
            requested,
            account.id,
            &self.display_name,
            self.flags,
            product.as_deref(),
            self.statstring.clone(),
            self.out.clone(),
        ) {
            Err(denial) => {
                let event = match denial {
                    bnetcc_core::JoinDenial::Full => EventId::ChannelFull,
                    bnetcc_core::JoinDenial::Banned | bnetcc_core::JoinDenial::Restricted => {
                        EventId::ChannelRestricted
                    }
                    bnetcc_core::JoinDenial::AlreadyPresent => return Step::Continue,
                };
                self.send(&chat_event(event, 0, 0, b"", requested))
            }
            Ok((joined, existing)) => {
                self.flags = joined.outcome.flags;
                self.channel = Some(joined.key.clone());

                // The join snapshot — "you are in channel X" (EID_CHANNEL) then one
                // EID_SHOWUSER per occupant already present plus one for us — is coalesced
                // into a SINGLE queued write. Sent as separate frames it would put one item
                // per occupant into the bounded outbound queue, so a channel near the queue
                // depth (~64) overflowed the *joiner's* queue mid-list and capped channel
                // size. One buffer is one queue item regardless of occupant count, and the
                // client still parses the individual frames within it.
                //
                // For EID_CHANNEL the channel name goes in the *text* field, not the
                // username field — confirmed against a real client (it reads the name from
                // text and shows blank otherwise). Each EID_SHOWUSER carries the user's
                // statstring so the client can render product and icon.
                let mut snapshot = Vec::with_capacity(64 + existing.len() * 48);
                let _ = encode_frame(
                    &chat_event(EventId::Channel, joined.flags, 0, b"", joined.display.as_bytes()),
                    &mut snapshot,
                );
                for occ in &existing {
                    let _ = encode_frame(
                        &chat_event(
                            EventId::ShowUser,
                            occ.flags,
                            0,
                            occ.name.as_bytes(),
                            &occ.statstring,
                        ),
                        &mut snapshot,
                    );
                }
                let _ = encode_frame(
                    &chat_event(
                        EventId::ShowUser,
                        self.flags,
                        0,
                        self.display_name.as_bytes(),
                        &self.statstring,
                    ),
                    &mut snapshot,
                );
                if !self.out.send(&Arc::new(snapshot)) {
                    return Step::Close;
                }
                // Tell everyone else we arrived, with our statstring so their lists
                // render us.
                let ev = chat_event(
                    EventId::Join,
                    self.flags,
                    0,
                    self.display_name.as_bytes(),
                    &self.statstring,
                );
                if let Some(wire) = encode(&ev) {
                    let _ = self.node.broadcast(&joined.key, &wire, Some(&self.display_name), None);
                }
                // 4. A per-channel topic/greeting, if this channel defines one.
                if let Some(topic) = &joined.topic {
                    return self.send(&chat_event(EventId::Info, 0, 0, b"", topic.as_bytes()));
                }
                Step::Continue
            }
        }
    }

    fn leave_current(&mut self, key: &[u8]) {
        self.node.leave_channel(key, &self.display_name);
        let ev = chat_event(EventId::Leave, self.flags, 0, self.display_name.as_bytes(), b"");
        if let Some(bytes) = encode_vec(&ev) {
            self.node.enqueue_leave(key, bytes);
        }
    }

    /// `SID_LEAVECHAT` (0x10): the client is leaving the chat environment (often to switch
    /// channels or enter a game). Leave the current channel but keep the connection —
    /// there is no reply, and the client typically joins another channel next.
    fn leave_chat(&mut self) -> Step {
        if let Some(key) = self.channel.take() {
            self.leave_current(&key);
        }
        Step::Continue
    }

    async fn chat_command(&mut self, frame: &Frame) -> Step {
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
        // Flood control. Every chat line (talk, emote, or slash command) spends a token;
        // over-budget lines are penalised here, before they can fan out to the channel, so
        // one flooder cannot amplify into channel-wide load.
        let now = now_ms();
        if now < self.muted_until_ms {
            return Step::Continue; // muted: drop silently until the window passes
        }
        if let FloodVerdict::Exceeded(penalty) = self.flood.check(now) {
            return self.apply_flood_penalty(penalty, now);
        }
        if text.first() == Some(&b'/') {
            return self.slash_command(&text, &account, &key).await;
        }

        // Re-check membership before fanning out: an operator may have kicked us since we
        // joined, and a removed session must not keep talking to the channel.
        if !self.ensure_member(&key) {
            return Step::Continue;
        }
        // Server-wide mute (a staff `/mute`): the line is silently dropped, matching how a
        // flood mute is handled above. Enforced here, after membership, so a muted account
        // still counts as present but cannot broadcast.
        if self.node.bans.is_muted(&account.name, now) {
            return Step::Continue;
        }
        let ev = chat_event(EventId::Talk, self.flags, 0, self.display_name.as_bytes(), &text);
        if let Some(wire) = encode(&ev) {
            // Real Battle.net does not echo your own channel talk back to you. `sender_base`
            // lets recipients who have squelched this account drop the line.
            let stalled = self.node.broadcast(&key, &wire, Some(&self.display_name), Some(&account.name));
            if !stalled.is_empty() {
                debug!(count = stalled.len(), "dropped stalled subscribers from fanout");
            }
        }
        Step::Continue
    }

    /// Confirm we are still a member of `key`; if not (e.g. we were kicked), clear our local
    /// channel state, tell the user, and return `false` so the caller drops the action.
    fn ensure_member(&mut self, key: &[u8]) -> bool {
        if self.node.is_member(key, &self.display_name) {
            return true;
        }
        self.channel = None;
        let _ = self.send(&chat_event(
            EventId::Error,
            0,
            0,
            b"",
            b"You are no longer in a channel.",
        ));
        false
    }

    /// Apply the flood penalty for an over-budget chat line and return the step to take.
    /// `DropMessage` and `Mute` warn the sender (real Battle.net drops silently, which is
    /// maddening to debug); `Disconnect` sends `SID_FLOODDETECTED` and closes.
    fn apply_flood_penalty(&mut self, penalty: FloodPenalty, now: u64) -> Step {
        match penalty {
            FloodPenalty::DropMessage => self.send(&chat_event(
                EventId::Error,
                0,
                0,
                b"",
                b"You are talking too fast; slow down.",
            )),
            FloodPenalty::Mute { seconds } => {
                self.muted_until_ms = now.saturating_add(u64::from(seconds) * 1000);
                self.send(&chat_event(
                    EventId::Error,
                    0,
                    0,
                    b"",
                    b"You have been temporarily muted for flooding.",
                ))
            }
            FloodPenalty::Disconnect => {
                let _ = self.send(&Frame::empty(sid::FLOODDETECTED));
                Step::Close
            }
        }
    }

    async fn slash_command(&mut self, text: &[u8], account: &Account, key: &[u8]) -> Step {
        let s = String::from_utf8_lossy(text);
        let mut parts = s[1..].splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
        let _arg = parts.next().unwrap_or("").trim();

        match cmd.as_str() {
            "join" | "j" => {
                let target = _arg;
                if target.is_empty() {
                    return self.send(&chat_event(
                        EventId::Error,
                        0,
                        0,
                        b"",
                        b"Usage: /join <channel>",
                    ));
                }
                let account = account.clone();
                self.do_join(target.as_bytes(), &account)
            }
            "me" | "emote" => {
                if !self.ensure_member(key) {
                    return Step::Continue;
                }
                if self.node.bans.is_muted(&account.name, now_ms()) {
                    return Step::Continue;
                }
                let body = sanitize_chat_text(_arg.as_bytes());
                let ev = chat_event(EventId::Emote, self.flags, 0, self.display_name.as_bytes(), &body);
                if let Some(wire) = encode(&ev) {
                    let _ = self.node.broadcast(key, &wire, None, Some(&account.name));
                }
                Step::Continue
            }
            "kick" => self.op_kick(_arg, account, key),
            "ban" => self.op_ban(_arg, account, key),
            "unban" => self.op_unban(_arg, account, key).await,
            "designate" | "heir" => self.op_designate(_arg, account, key),
            // Personal ignore — any user. Hides a chosen account's channel chat from you.
            "squelch" | "ignore" => self.squelch(_arg),
            "unsquelch" | "unignore" => self.unsquelch(_arg),
            // Staff-only (Blizzard-rep / sysop) moderation.
            "tagban" => self.staff_tagban(_arg, account),
            "tagunban" => self.staff_tagunban(_arg, account),
            "ipban" => self.staff_ipban(_arg, account),
            "ipunban" => self.staff_ipunban(_arg, account),
            "mute" => self.staff_mute(_arg, account),
            "unmute" => self.staff_unmute(_arg, account),
            "bans" => self.staff_bans(),
            "whoami" => {
                let op = if self.node.is_operator(key, account.id) {
                    " (operator)"
                } else {
                    ""
                };
                let msg = format!("You are {}{op}.", account.name);
                self.send(&chat_event(EventId::Info, 0, 0, b"", msg.as_bytes()))
            }
            "ver" | "version" => {
                let msg = format!(
                    "{} — bnetccd v{}",
                    self.node.name,
                    env!("CARGO_PKG_VERSION")
                );
                self.send(&chat_event(EventId::Info, 0, 0, b"", msg.as_bytes()))
            }
            "who" | "users" => {
                // With no argument, list the current channel; otherwise the named one.
                let target_key = if _arg.is_empty() {
                    key.to_vec()
                } else {
                    normalize_channel_name(_arg.as_bytes())
                };
                let names = self.node.channel_occupant_names(&target_key);
                if names.is_empty() {
                    return self.send(&chat_event(
                        EventId::Info,
                        0,
                        0,
                        b"",
                        b"No such channel, or it is empty.",
                    ));
                }
                let header = format!("Users in channel ({}): {}", names.len(), names.join(", "));
                self.send(&chat_event(EventId::Info, 0, 0, b"", header.as_bytes()))
            }
            "help" | "?" => {
                // Keep this list in sync with the arms above. Whisper/friends commands are
                // still pending (they need cross-session routing) — see docs/ROADMAP.md.
                let mut lines = vec![
                    "Commands: /help, /join <channel>, /me <action>, /who [channel], /whoami, /ver",
                    "Personal: /squelch <user>, /unsquelch <user>",
                    "Operator: /kick <user>, /ban <user>, /unban <user>, /designate <user>",
                ];
                // Only staff see the staff commands listed.
                if self.node.admins.is_admin(&account.name) {
                    lines.push(
                        "Staff: /tagban <text>, /tagunban <text>, /ipban <user> [hrs], \
                         /ipunban <ip>, /mute <user> [hrs], /unmute <user>, /bans",
                    );
                }
                for line in lines {
                    if matches!(
                        self.send(&chat_event(EventId::Info, 0, 0, b"", line.as_bytes())),
                        Step::Close
                    ) {
                        return Step::Close;
                    }
                }
                Step::Continue
            }
            _ => self.send(&chat_event(
                EventId::Error,
                0,
                0,
                b"",
                b"That is not a valid command. Type /help for the list.",
            )),
        }
    }

    /// `/kick <user>` — operator removes a user from the current channel.
    fn op_kick(&self, target: &str, actor: &Account, key: &[u8]) -> Step {
        if target.is_empty() {
            return self.send(&chat_event(EventId::Error, 0, 0, b"", b"Usage: /kick <user>"));
        }
        match self.node.channel_kick(key, actor.id, target) {
            crate::node::ModResult::NotOperator => self.op_error("You are not the channel operator."),
            crate::node::ModResult::NotFound => self.op_error("No such user in this channel."),
            crate::node::ModResult::CannotTargetSelf => self.op_error("You cannot kick yourself."),
            crate::node::ModResult::Ok { target_out, new_operator } => {
                self.after_removal(key, target, target_out, new_operator, "kicked");
                self.send(&chat_event(
                    EventId::Info,
                    0,
                    0,
                    b"",
                    format!("You kicked {target}.").as_bytes(),
                ))
            }
        }
    }

    /// `/ban <user>` — operator removes a user and blocks their account from rejoining.
    fn op_ban(&self, target: &str, actor: &Account, key: &[u8]) -> Step {
        if target.is_empty() {
            return self.send(&chat_event(EventId::Error, 0, 0, b"", b"Usage: /ban <user>"));
        }
        match self.node.channel_ban(key, actor.id, target) {
            crate::node::ModResult::NotOperator => self.op_error("You are not the channel operator."),
            crate::node::ModResult::NotFound => self.op_error("No such user in this channel."),
            crate::node::ModResult::CannotTargetSelf => self.op_error("You cannot ban yourself."),
            crate::node::ModResult::Ok { target_out, new_operator } => {
                self.after_removal(key, target, target_out, new_operator, "banned");
                self.send(&chat_event(
                    EventId::Info,
                    0,
                    0,
                    b"",
                    format!("You banned {target}.").as_bytes(),
                ))
            }
        }
    }

    /// `/unban <user>` — operator lifts a channel ban. The target is not present, so its
    /// account is resolved from storage by name.
    async fn op_unban(&self, target: &str, actor: &Account, key: &[u8]) -> Step {
        if target.is_empty() {
            return self.send(&chat_event(EventId::Error, 0, 0, b"", b"Usage: /unban <user>"));
        }
        let Some(acct) = self.node.account(target).await else {
            return self.op_error("No such account.");
        };
        match self.node.channel_unban(key, actor.id, acct.id) {
            crate::node::ModResult::NotOperator => self.op_error("You are not the channel operator."),
            crate::node::ModResult::NotFound => self.op_error("No such channel."),
            _ => self.send(&chat_event(
                EventId::Info,
                0,
                0,
                b"",
                format!("Unbanned {target}.").as_bytes(),
            )),
        }
    }

    /// `/designate <user>` — operator nominates a present user as heir to operator.
    fn op_designate(&self, target: &str, actor: &Account, key: &[u8]) -> Step {
        if target.is_empty() {
            return self.send(&chat_event(EventId::Error, 0, 0, b"", b"Usage: /designate <user>"));
        }
        match self.node.channel_designate(key, actor.id, target) {
            crate::node::ModResult::NotOperator => self.op_error("You are not the channel operator."),
            crate::node::ModResult::NotFound => self.op_error("No such user in this channel."),
            crate::node::ModResult::CannotTargetSelf => self.op_error("You cannot designate yourself."),
            _ => self.send(&chat_event(
                EventId::Info,
                0,
                0,
                b"",
                format!("{target} will inherit operator when you leave.").as_bytes(),
            )),
        }
    }

    /// Send an operator-command error line to the actor.
    fn op_error(&self, msg: &str) -> Step {
        self.send(&chat_event(EventId::Error, 0, 0, b"", msg.as_bytes()))
    }

    /// Shared tail of a kick/ban: notify the removed user via their own connection, announce
    /// the departure to the channel so rosters drop them, and note any operator succession.
    fn after_removal(
        &self,
        key: &[u8],
        target: &str,
        target_out: Option<crate::node::Outbound>,
        new_operator: Option<bnetcc_core::channel::AccountId>,
        verb: &str,
    ) {
        if let Some(out) = target_out {
            let msg = format!("You were {verb} from the channel by {}.", self.display_name);
            if let Some(wire) = encode(&chat_event(EventId::Error, 0, 0, b"", msg.as_bytes())) {
                let _ = out.send(&wire);
            }
        }
        if let Some(bytes) = encode_vec(&chat_event(EventId::Leave, 0, 0, target.as_bytes(), b"")) {
            self.node.enqueue_leave(key, bytes);
        }
        if let Some(op) = new_operator {
            debug!(new_operator = op, "operator inherited after a removal");
        }
    }

    // -- personal ignore (any user) ---------------------------------------------

    /// `/squelch <user>` — personally ignore an account: its channel chat is hidden from you
    /// (all of its sessions). Reversible with `/unsquelch`. Not a moderation action.
    fn squelch(&self, arg: &str) -> Step {
        let target = base_name(arg.trim());
        if target.is_empty() {
            // No argument: show who this session is currently ignoring.
            let ignored = self.out.ignores().list();
            return if ignored.is_empty() {
                self.info("You are not squelching anyone. Usage: /squelch <user>")
            } else {
                self.info(&format!("Squelched: {}", ignored.join(", ")))
            };
        }
        if target.eq_ignore_ascii_case(base_name(&self.display_name)) {
            return self.op_error("You cannot squelch yourself.");
        }
        if self.out.ignores().add(target) {
            self.info(&format!("You will no longer see messages from {target}."))
        } else {
            self.info(&format!("{target} is already squelched."))
        }
    }

    /// `/unsquelch <user>` — stop ignoring an account.
    fn unsquelch(&self, arg: &str) -> Step {
        let target = base_name(arg.trim());
        if target.is_empty() {
            return self.op_error("Usage: /unsquelch <user>");
        }
        if self.out.ignores().remove(target) {
            self.info(&format!("You will now see messages from {target} again."))
        } else {
            self.info(&format!("{target} was not squelched."))
        }
    }

    // -- staff moderation (Blizzard-rep / sysop only) ---------------------------

    /// Whether this session holds staff privilege. True for the effective Administrator flag,
    /// which covers both the configured `[admins]` list and any panel-granted staff flag
    /// stored on the account — both are OR-ed into `self.flags` at logon.
    fn require_staff(&self) -> bool {
        self.flags & user_flags::ADMIN != 0
    }

    /// The refusal shown to a non-staff account that tries a staff command.
    fn staff_denied(&self) -> Step {
        self.op_error("That command is restricted to server administrators.")
    }

    /// Send an informational line to the actor.
    fn info(&self, msg: &str) -> Step {
        self.send(&chat_event(EventId::Info, 0, 0, b"", msg.as_bytes()))
    }

    /// `/tagban <text>` — refuse any account whose name contains `text` (e.g. a clan prefix
    /// like `BNU-`), and disconnect everyone online who already matches. Persists.
    fn staff_tagban(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let sub = arg.trim();
        if sub.is_empty() {
            return self.op_error("Usage: /tagban <text>  (bans any name containing <text>)");
        }
        if !self.node.bans.add_tag(sub) {
            return self.op_error(&format!("'{sub}' is already tag-banned."));
        }
        let count = match removal_notice("You have been removed from this server by an administrator.") {
            Some(w) => self.node.disconnect_name_matches(&sub.to_ascii_lowercase(), &w),
            None => 0,
        };
        info!(admin = %account.name, tag = %sub, removed = count, "tag banned");
        self.info(&format!(
            "Tag-banned '{sub}': {count} online user(s) removed; matching logins are now refused."
        ))
    }

    /// `/tagunban <text>` — lift a tag ban.
    fn staff_tagunban(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let sub = arg.trim();
        if sub.is_empty() {
            return self.op_error("Usage: /tagunban <text>");
        }
        if self.node.bans.remove_tag(sub) {
            info!(admin = %account.name, tag = %sub, "tag unbanned");
            self.info(&format!("Tag ban '{sub}' lifted."))
        } else {
            self.op_error(&format!("'{sub}' is not tag-banned."))
        }
    }

    /// `/ipban <user> [hours]` — ban an online user's address for `hours` (default 24; `0`
    /// or `perm` = permanent) and disconnect every session on it. Persists.
    fn staff_ipban(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let mut it = arg.split_whitespace();
        let Some(user) = it.next() else {
            return self.op_error("Usage: /ipban <user> [hours]  (0 or 'perm' = permanent)");
        };
        let Some(ip) = self.node.session_ip(user) else {
            return self.op_error("No such user online.");
        };
        if ip == self.peer.ip() {
            return self.op_error("That is your own address; refusing to ban yourself.");
        }
        let now = now_ms();
        let (hours_opt, human) = match parse_ban_duration(it.next(), 24) {
            Some(d) => d,
            None => return self.op_error("Hours must be a whole number, 0, or 'perm'."),
        };
        let expiry = match hours_opt {
            Some(hours) => now.saturating_add(hours.saturating_mul(3_600_000)),
            None => crate::moderation::NEVER,
        };
        self.node.bans.ban_ip(ip, expiry, now);
        let count = match removal_notice("You have been banned from this server by an administrator.") {
            Some(w) => self.node.disconnect_ip(ip, &w),
            None => 0,
        };
        info!(admin = %account.name, %ip, removed = count, "ip banned");
        self.info(&format!("Banned {ip} {human}: {count} session(s) disconnected."))
    }

    /// `/ipunban <ip>` — lift an IP ban.
    fn staff_ipunban(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let Ok(ip) = arg.trim().parse::<std::net::IpAddr>() else {
            return self.op_error("Usage: /ipunban <ip address>");
        };
        if self.node.bans.unban_ip(ip, now_ms()) {
            info!(admin = %account.name, %ip, "ip unbanned");
            self.info(&format!("IP ban on {ip} lifted."))
        } else {
            self.op_error(&format!("{ip} is not banned."))
        }
    }

    /// `/mute <user> [hours]` — silence an account's chat across the whole server (default
    /// indefinite; a number sets hours). The user stays connected; their lines are dropped.
    fn staff_mute(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let mut it = arg.split_whitespace();
        let Some(user) = it.next() else {
            return self.op_error("Usage: /mute <user> [hours]  (no hours = indefinite)");
        };
        let base = base_name(user).to_string();
        let now = now_ms();
        // Default: indefinite (until /unmute). A mute is reversible and does not disconnect,
        // so a permanent default is safe here (unlike /ipban's 24h default).
        let (hours_opt, human) = match parse_ban_duration(it.next(), 0) {
            Some(d) => d,
            None => return self.op_error("Hours must be a whole number, 0, or 'perm'."),
        };
        let (expiry, human) = match hours_opt {
            Some(hours) => (now.saturating_add(hours.saturating_mul(3_600_000)), human),
            None => (crate::moderation::NEVER, "indefinitely".to_string()),
        };
        self.node.bans.mute(&base, expiry, now);
        info!(admin = %account.name, user = %base, "muted");
        self.info(&format!("Muted {base} {human}."))
    }

    /// `/unmute <user>` — lift a server mute.
    fn staff_unmute(&self, arg: &str, account: &Account) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let base = base_name(arg.trim());
        if base.is_empty() {
            return self.op_error("Usage: /unmute <user>");
        }
        if self.node.bans.unmute(base, now_ms()) {
            info!(admin = %account.name, user = %base, "unmuted");
            self.info(&format!("{base} is no longer muted."))
        } else {
            self.op_error(&format!("{base} is not muted."))
        }
    }

    /// `/bans` — list the active tag bans, IP bans, and mutes.
    fn staff_bans(&self) -> Step {
        if !self.require_staff() {
            return self.staff_denied();
        }
        let now = now_ms();
        let snap = self.node.bans.snapshot(now);
        let mut lines = Vec::new();
        if snap.tags.is_empty() && snap.ip_bans.is_empty() && snap.mutes.is_empty() {
            lines.push("No active bans or mutes.".to_string());
        } else {
            if !snap.tags.is_empty() {
                lines.push(format!("Tag bans: {}", snap.tags.join(", ")));
            }
            for (ip, exp) in &snap.ip_bans {
                lines.push(format!("IP ban: {ip} ({})", fmt_remaining(*exp, now)));
            }
            for (name, exp) in &snap.mutes {
                lines.push(format!("Mute: {name} ({})", fmt_remaining(*exp, now)));
            }
        }
        for line in lines {
            if matches!(self.info(&line), Step::Close) {
                return Step::Close;
            }
        }
        Step::Continue
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

    /// `SID_FRIENDSLIST` (0x65): the client asks for its friends list. Friends are not
    /// stored yet (docs/ROADMAP.md), so the reply is an empty list — `(UINT8) count = 0`.
    fn friends_list(&mut self) -> Step {
        let mut w = Writer::with_capacity(1);
        w.u8(0);
        self.send(&Frame::new(sid::FRIENDSLIST, w.finish()))
    }

    /// `SID_NEWS_INFO` (0x46): the client asks for news newer than a timestamp. There is
    /// no news feed; reply with zero entries so the login-screen news panel completes.
    /// Layout: `(UINT8) entries`, `(UINT32) last logon`, `(UINT32) oldest`, `(UINT32) newest`.
    fn news_info(&mut self) -> Step {
        let mut w = Writer::with_capacity(13);
        w.u8(0).u32(0).u32(0).u32(0);
        self.send(&Frame::new(sid::NEWS_INFO, w.finish()))
    }

    /// `SID_GETADVLISTEX` (0x09): return the games this node knows about, filtered the way
    /// a client expects: only its **own product** (a WarCraft III client cannot parse a
    /// StarCraft statstring, nor the reverse), by **exact name** when one is given (this
    /// is how a joiner fetches one host's address — WarCraft III sends the name with a
    /// count of 1), and capped at the requested **count**.
    ///
    /// Request: type `u16` + sub-type `u16` (WarCraft III: one `u32` of game flags), a
    /// viewing filter/mask `u32`, reserved `u32`, count `u32` (`0` = no cap), then name,
    /// password and statstring strings. A missing or short body lists everything for the
    /// product. Empty-list status is `0x00` for a plain listing and `0x01` "doesn't
    /// exist" for a named lookup that found nothing (or when listing is off: warnet mode).
    ///
    /// Reply entries embed a `sockaddr_in` (address family, big-endian port, host IP) so
    /// a joiner can reach the host peer-to-peer, then status, elapsed seconds, and the
    /// name/password/statstring strings. ⚠️ Layout verified against BNETDocs and two
    /// independent implementations, not yet against a captured real client.
    fn game_list(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let (count, wanted) = (|| {
            let _type_or_flags = r.u32()?;
            let _filter_or_mask = r.u32()?;
            let _reserved = r.u32()?;
            let count = r.u32()?;
            let name = r.cstr(CHANNEL_NAME_MAX)?.to_vec();
            Ok::<_, bnetcc_proto::ProtoError>((count, name))
        })()
        .unwrap_or((0, Vec::new()));
        let wanted = (!wanted.is_empty()).then(|| wanted.to_ascii_lowercase());
        let cap = if count == 0 { usize::MAX } else { count as usize };

        // Warnet mode never lists games; otherwise show what hosts have advertised.
        let games: Vec<crate::node::GameAd> = if self.node.policy.game_listing.allowed() {
            self.node
                .games()
                .into_iter()
                .filter(|g| g.product == self.product)
                .filter(|g| wanted.as_ref().map_or(true, |w| g.name.to_ascii_lowercase() == *w))
                .take(cap)
                .collect()
        } else {
            Vec::new()
        };

        let mut w = Writer::with_capacity(64);
        w.u32(games.len() as u32);
        if games.is_empty() {
            // Zero-game case: a status DWORD stands in for the (absent) entries.
            let status = if wanted.is_some() || !self.node.policy.game_listing.allowed() {
                0x01 // "game does not exist"
            } else {
                0x00 // OK, nothing to list
            };
            w.u32(status);
            return self.send(&Frame::new(sid::GETADVLISTEX, w.finish()));
        }
        for g in &games {
            let elapsed = u32::try_from(g.created.elapsed().as_secs()).unwrap_or(u32::MAX);
            w.u16(g.game_type)
                .u16(g.parameter)
                .u32(0) // language id
                .u16(2) // AF_INET
                .u16(g.port.to_be()) // port, network byte order
                .bytes(&g.host_ip.octets()) // host IP, network order
                .u32(0) // sin_zero
                .u32(0) // sin_zero
                .u32(g.state) // game status
                .u32(elapsed)
                .cstr(&g.name)
                .cstr(&g.password)
                .cstr(&g.statstring);
        }
        self.send(&Frame::new(sid::GETADVLISTEX, w.finish()))
    }

    /// `SID_STARTADVEX3` (0x1C): a client advertises a game it is hosting. We register it
    /// in the node's directory so `SID_GETADVLISTEX` can hand it to other clients.
    ///
    /// ⚠️ Request layout unverified against a real client — see `game_list`.
    fn advertise(&mut self, frame: &Frame) -> Step {
        if !self.node.policy.game_hosting.allowed() {
            // Warnet mode: hosting is off. Reply with the game's own documented code so the
            // client shows a real message rather than hanging on a dropped packet.
            let mut w = Writer::with_capacity(4);
            w.u32(advertise_status::TYPE_UNAVAILABLE);
            return self.send(&Frame::new(sid::STARTADVEX3, w.finish()));
        }
        let Some(account) = self.account.clone() else {
            return Step::Close;
        };

        let mut r = frame.reader();
        let parsed = (|| {
            let state = r.u32()?;
            let _uptime = r.u32()?;
            let game_type = r.u16()?;
            let parameter = r.u16()?;
            let _unknown = r.u32()?;
            let _ladder = r.u32()?;
            let name = r.cstr(CHANNEL_NAME_MAX)?.to_vec();
            let password = r.cstr(CHANNEL_NAME_MAX)?.to_vec();
            let statstring = r.cstr(512)?.to_vec();
            Ok::<_, bnetcc_proto::ProtoError>((state, game_type, parameter, name, password, statstring))
        })();
        let Ok((state, game_type, parameter, name, password, statstring)) = parsed else {
            return Step::Close;
        };

        let host_ip = match self.peer.ip() {
            std::net::IpAddr::V4(v4) => v4,
            std::net::IpAddr::V6(_) => std::net::Ipv4Addr::UNSPECIFIED,
        };
        let ad = crate::node::GameAd {
            name,
            password,
            statstring,
            game_type,
            parameter,
            state,
            product: self.product,
            port: self.game_port,
            host_ip,
            host: account.id,
            created: std::time::Instant::now(),
        };
        let ok = self.node.advertise_game(ad);
        if ok {
            info!(peer = %self.peer, account = %account.name, "game advertised");
            // Count a game only on its first advertisement; STARTADVEX3 re-fires for state
            // updates while the game is live.
            if !self.hosting_game {
                self.hosting_game = true;
                let product = self.product.map_or_else(|| "unknown".to_string(), |p| p.to_string());
                self.node.record_hosted_game(&product);
            }
        }
        let mut w = Writer::with_capacity(4);
        w.u32(if ok {
            advertise_status::OK
        } else {
            advertise_status::NAME_TAKEN
        });
        self.send(&Frame::new(sid::STARTADVEX3, w.finish()))
    }

    /// `SID_STOPADV` (0x02) / `SID_LEAVEGAME` (0x1F): the host's game is over. Remove it
    /// from the directory so it stops appearing in the game list.
    fn stop_advertising(&mut self) -> Step {
        if let Some(account) = &self.account {
            self.node.withdraw_game(account.id);
        }
        // The game is over; a later STARTADVEX3 starts a new one and counts again.
        self.hosting_game = false;
        Step::Continue
    }

    /// `SID_NETGAMEPORT` (0x45): the client announces the port it will host games on.
    fn net_game_port(&mut self, frame: &Frame) -> Step {
        if let Ok(port) = frame.reader().u16() {
            self.game_port = port;
        }
        Step::Continue
    }

    /// `SID_CHECKDATAFILE2` (0x3C): when hosting a game the client asks the server to
    /// authenticate the map file. Request: `(UINT32)` size, `(20 bytes)` SHA-1, `(STRING)`
    /// filename. We approve every map — there is no Blizzard map-hash database to check
    /// against, gating custom maps is not wanted, and leaving this unanswered strands the
    /// host on "Unable to authenticate map" (confirmed against a real W2BN host, 2026-09-09).
    /// Reply is a single `(UINT32)` result; `1` = approved, matching PvPGN.
    fn check_data_file2(&mut self, frame: &Frame) -> Step {
        let mut r = frame.reader();
        let name = (|| -> Result<String, bnetcc_proto::ProtoError> {
            let _size = r.u32()?;
            let _hash: [u8; 20] = r.array()?;
            Ok(String::from_utf8_lossy(r.cstr(260)?).into_owned())
        })()
        .unwrap_or_default();
        debug!(peer = %self.peer, map = %name, "SID_CHECKDATAFILE2: approving map");
        let mut w = Writer::with_capacity(4);
        w.u32(1); // 1 = approved
        self.send(&Frame::new(sid::CHECKDATAFILE2, w.finish()))
    }

    /// `SID_GAMERESULT` (0x2C): a host reports the outcome of a finished game. Body layout,
    /// decoded from a real W2BN host (2026-09-09): `(u32)` header, `(u32)` slot count,
    /// `(u32)[count]` per-slot result codes (1 win, 2 loss, 3 draw, 4 disconnect, 0 empty),
    /// `(cstring)[count]` player names, then a human-readable score-screen string.
    ///
    /// We record **only the reporting account's own slot**, matched by name: a client must
    /// not be able to report outcomes for other players (the classic ladder-forgery vector —
    /// every player's client reports its own game). The outcome increments the PvPGN-style
    /// `Record\<product>\0\wins`/`losses`/`draws`/`disconnects` counters, served back via
    /// `SID_READUSERDATA`. No reply is expected. A solo game reports a draw, not a loss.
    async fn game_result(&mut self, frame: &Frame) -> Step {
        let Some(account) = self.account.clone() else {
            return Step::Continue;
        };
        let Some(product) = self.product.map(|p| p.to_string()) else {
            return Step::Continue;
        };
        let mut r = frame.reader();
        let parsed = (|| -> Result<(Vec<u32>, Vec<String>), bnetcc_proto::ProtoError> {
            let _header = r.u32()?;
            let count = (r.u32()? as usize).min(16);
            let mut results = Vec::with_capacity(count);
            for _ in 0..count {
                results.push(r.u32()?);
            }
            let mut names = Vec::with_capacity(count);
            for _ in 0..count {
                names.push(String::from_utf8_lossy(r.cstr(64)?).into_owned());
            }
            Ok((results, names))
        })();
        let Ok((results, names)) = parsed else {
            debug!(peer = %self.peer, "malformed SID_GAMERESULT; ignoring");
            return Step::Continue;
        };
        let outcome = names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&account.name))
            .and_then(|i| results.get(i).copied())
            .and_then(crate::storage::GameOutcome::from_code);
        match outcome {
            Some(o) => match self.node.record_game(account.id, &product, o).await {
                Ok(total) => info!(
                    peer = %self.peer,
                    account = %account.name,
                    product = %product,
                    outcome = ?o,
                    new_total = total,
                    "recorded game result"
                ),
                Err(e) => {
                    warn!(peer = %self.peer, error = %e, "failed to record game result")
                }
            },
            None => debug!(
                peer = %self.peer,
                account = %account.name,
                "SID_GAMERESULT carried no recordable result for this player"
            ),
        }
        Step::Continue
    }
}

fn encode(frame: &Frame) -> Option<Wire> {
    encode_vec(frame).map(Arc::new)
}

/// Encode a frame to owned bytes (not yet wrapped in an `Arc`). Used where the bytes are
/// buffered for later coalescing — see [`crate::node::Node::enqueue_leave`].
fn encode_vec(frame: &Frame) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(frame.wire_len());
    encode_frame(frame, &mut buf).ok()?;
    Some(buf)
}

/// The base account name behind a display name — strips any `#N` coexistence suffix, so
/// moderation and squelch target the account rather than one particular session.
fn base_name(name: &str) -> &str {
    name.split('#').next().unwrap_or(name)
}

/// A pre-encoded error line delivered to a session just before staff force it off.
fn removal_notice(msg: &str) -> Option<Wire> {
    encode(&chat_event(EventId::Error, 0, 0, b"", msg.as_bytes()))
}

/// Parse an optional `[hours]` duration argument for `/ipban` and `/mute`.
///
/// Returns `Some((Some(hours), human))` for a finite duration, `Some((None, "permanently"))`
/// for a permanent ban (`0`, `perm`, or `permanent`), or `None` if the token is malformed.
/// When the token is absent, `default_hours` applies (`0` meaning permanent).
fn parse_ban_duration(token: Option<&str>, default_hours: u64) -> Option<(Option<u64>, String)> {
    let permanent = || (None, "permanently".to_string());
    match token {
        None if default_hours == 0 => Some(permanent()),
        None => Some((Some(default_hours), format!("for {default_hours}h"))),
        Some(t)
            if t == "0" || t.eq_ignore_ascii_case("perm") || t.eq_ignore_ascii_case("permanent") =>
        {
            Some(permanent())
        }
        Some(t) => t.parse::<u64>().ok().map(|h| (Some(h), format!("for {h}h"))),
    }
}

/// Human-readable remaining time for a ban expiry (epoch-millis), for the `/bans` listing.
fn fmt_remaining(expiry_ms: u64, now_ms: u64) -> String {
    if expiry_ms == crate::moderation::NEVER {
        return "permanent".to_string();
    }
    let secs = expiry_ms.saturating_sub(now_ms) / 1000;
    if secs >= 3600 {
        format!("{}h {}m left", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m left", secs / 60)
    }
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
        auth_info_frame_for(product::SEXP)
    }

    fn auth_info_frame_for(p: bnetcc_proto::FourCc) -> Frame {
        let mut w = Writer::with_capacity(32);
        w.u32(0)
            .fourcc(bnetcc_proto::FourCc::from_ascii(b"IX86"))
            .fourcc(p)
            .u32(0xCD);
        Frame::new(sid::AUTH_INFO, w.finish())
    }

    /// A minimal `SID_AUTH_CHECK` carrying one CD key. `product_value`/`public_value` are
    /// the two fields our uniqueness check actually keys on — see `key_fingerprint`.
    fn auth_check_frame(product_value: u32, public_value: u32) -> Frame {
        // Field order matches the parser (verified against a real client): the key block
        // comes before the EXE info string, not after it.
        let mut w = Writer::with_capacity(96);
        w.u32(0) // client token
            .u32(0) // exe version
            .u32(0) // exe hash
            .u32(1) // key count
            .u32(0) // spawn
            .u32(16) // key length
            .u32(product_value)
            .u32(public_value)
            .u32(0) // reserved
            .bytes(&[0u8; 20]) // wire hash; unused by our uniqueness check, see KeyId
            .cstr(b"test.exe 00/00/00 000000")
            .cstr(b"tester");
        Frame::new(sid::AUTH_CHECK, w.finish())
    }

    async fn send_frame(stream: &mut TcpStream, frame: &Frame) {
        let mut out = Vec::new();
        encode_frame(frame, &mut out).expect("encodable");
        stream.write_all(&out).await.expect("write");
    }

    /// Read the next frame, skipping server-initiated notifications the request/reply
    /// tests do not care about: `SID_PING` (sent on connect and unprompted) and
    /// `SID_CHATEVENT` (MOTD, joins, and other async chat events).
    async fn recv_frame(stream: &mut TcpStream) -> Frame {
        loop {
            // Read *exactly* one frame. TCP may deliver several frames in a single read, so a
            // helper that buffered locally and returned the first would discard the surplus
            // bytes when its buffer dropped — a timing-dependent flake under load (the next
            // call would read fresh and miss the already-consumed frame). Reading the 4-byte
            // header, then exactly `len - 4` body bytes (`len` includes the header), never
            // consumes past the frame boundary, so no state has to persist between calls.
            let mut header = [0u8; bnetcc_proto::bncs::HEADER_LEN];
            stream.read_exact(&mut header).await.expect("read frame header");
            assert_eq!(header[0], bnetcc_proto::bncs::MAGIC, "bad frame magic");
            let id = header[1];
            let total = u16::from_le_bytes([header[2], header[3]]) as usize;
            let mut body = vec![0u8; total.saturating_sub(bnetcc_proto::bncs::HEADER_LEN)];
            stream.read_exact(&mut body).await.expect("read frame body");
            // Skip keepalives and asynchronous chat events, as before.
            if id == sid::PING || id == sid::CHATEVENT {
                continue;
            }
            return Frame { id, body }
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

    /// Spawn a server on an ephemeral port and return its address.
    async fn spawn_server() -> std::net::SocketAddr {
        let node = Arc::new(crate::node::test_node());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let limits = SessionLimits::default();
        tokio::spawn(async move {
            loop {
                let (stream, peer) = listener.accept().await.expect("accept");
                tokio::spawn(handle(stream, peer, Arc::clone(&node), limits));
            }
        });
        addr
    }

    /// A test server whose WarCraft III clients are offered the legacy X-SHA-1 logon
    /// (`server.wc3_logon = "legacy"`) instead of NLS/SRP.
    async fn spawn_server_legacy_wc3() -> std::net::SocketAddr {
        let node = Arc::new(crate::node::test_node_with(|c| c.wc3_legacy_logon = true));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let limits = SessionLimits::default();
        tokio::spawn(async move {
            loop {
                let (stream, peer) = listener.accept().await.expect("accept");
                tokio::spawn(handle(stream, peer, Arc::clone(&node), limits));
            }
        });
        addr
    }

    /// With `server.wc3_logon = "legacy"`, a WarCraft III client is offered the X-SHA-1
    /// logon — logon type 0 and no 128-byte RSA signature — and logs in over
    /// `SID_LOGONRESPONSE2` against the plain X-SHA-1 account namespace, the same flow
    /// StarCraft uses and the one a StarCraft-hash loader drives. This is the escape from
    /// the NLS-only signature an unpatched client cannot satisfy (docs/WARCRAFT3.md §3.7).
    #[tokio::test]
    async fn warcraft_three_uses_the_legacy_xsha1_logon_when_configured() {
        const CLIENT_TOKEN: u32 = 0xCAFE_BABE;
        let addr = spawn_server_legacy_wc3().await;
        let mut s = connect(addr).await;

        send_frame(&mut s, &auth_info_frame_for(product::W3XP)).await;
        let info = recv_frame(&mut s).await;
        assert_eq!(info.id, sid::AUTH_INFO);
        let mut ir = info.reader();
        assert_eq!(ir.u32().unwrap(), 0x00, "legacy WC3 is offered logon type 0, not NLS");
        let server_token = ir.u32().unwrap();
        assert!(
            info.body.ends_with(&[0u8; 128]),
            "the 128-byte signature field stays present (a W3XP product field); logon type 0 \
             tells the client not to verify it"
        );

        send_frame(&mut s, &auth_check_frame(1, 777)).await;
        assert_eq!(recv_frame(&mut s).await.reader().u32().unwrap(), auth_check_status::PASSED);

        // Create and log in with the StarCraft-style double hash, into the plain namespace.
        let h1 = bnetcc_crypto::password_hash("hunter2");
        let mut cw = Writer::with_capacity(32);
        cw.bytes(&h1).cstr(b"Tagban");
        send_frame(&mut s, &Frame::new(sid::CREATEACCOUNT2, cw.finish())).await;
        assert_eq!(recv_frame(&mut s).await.reader().u32().unwrap(), 0x00, "account creation");

        let proof = bnetcc_crypto::logon_proof(CLIENT_TOKEN, server_token, &h1);
        let mut lw = Writer::with_capacity(32);
        lw.u32(CLIENT_TOKEN).u32(server_token).bytes(&proof).cstr(b"Tagban");
        send_frame(&mut s, &Frame::new(sid::LOGONRESPONSE2, lw.finish())).await;
        assert_eq!(recv_frame(&mut s).await.reader().u32().unwrap(), logon_status::SUCCESS, "logon");

        // Logged in as the plain X-SHA-1 name, not the SRP realm name `Tagban@bncc`.
        assert_eq!(enter_chat_name(&mut s).await, "Tagban");
    }

    /// Drive a full modern-flow login as a game test client: handshake, create the
    /// account, log in, enter chat. Leaves the stream logged in and ready for game or
    /// channel packets. Returns the connected stream.
    async fn login(addr: std::net::SocketAddr, user: &str, password: &str, key: u32) -> TcpStream {
        const CLIENT_TOKEN: u32 = 0xCAFE_BABE;
        let mut s = connect(addr).await;

        // Version handshake. Each caller passes a distinct `key` so concurrent logins in
        // one test do not collide on the one-session-per-CD-key rule.
        send_frame(&mut s, &auth_info_frame()).await;
        let info = recv_frame(&mut s).await;
        assert_eq!(info.id, sid::AUTH_INFO);
        let mut ir = info.reader();
        let _logon_type = ir.u32().unwrap();
        let server_token = ir.u32().unwrap();
        send_frame(&mut s, &auth_check_frame(1, key)).await;
        let check = recv_frame(&mut s).await;
        assert_eq!(check.reader().u32().unwrap(), auth_check_status::PASSED);

        // Create the account (single-hash), then log in with the double-hash proof.
        let h1 = bnetcc_crypto::password_hash(password);
        let mut cw = Writer::with_capacity(32);
        cw.bytes(&h1).cstr(user.as_bytes());
        send_frame(&mut s, &Frame::new(sid::CREATEACCOUNT2, cw.finish())).await;
        let created = recv_frame(&mut s).await;
        assert_eq!(created.reader().u32().unwrap(), 0x00, "account creation");

        let proof = bnetcc_crypto::logon_proof(CLIENT_TOKEN, server_token, &h1);
        let mut lw = Writer::with_capacity(32);
        lw.u32(CLIENT_TOKEN).u32(server_token).bytes(&proof).cstr(user.as_bytes());
        send_frame(&mut s, &Frame::new(sid::LOGONRESPONSE2, lw.finish())).await;
        let logon = recv_frame(&mut s).await;
        assert_eq!(logon.reader().u32().unwrap(), logon_status::SUCCESS, "logon");

        send_frame(&mut s, &Frame::new(sid::ENTERCHAT, Writer::new().finish())).await;
        let _enter = recv_frame(&mut s).await; // ENTERCHAT reply (+ MOTD)
        s
    }

    /// Build a `SID_STARTADVEX3` request for a game called `name`.
    fn start_adv_frame(name: &str) -> Frame {
        let mut w = Writer::with_capacity(64);
        w.u32(0) // state
            .u32(0) // uptime
            .u16(0x02) // game type
            .u16(0) // parameter
            .u32(1) // unknown
            .u32(0) // ladder
            .cstr(name.as_bytes())
            .cstr(b"") // password
            .cstr(b"0x01 0x02 map.scm"); // statstring
        Frame::new(sid::STARTADVEX3, w.finish())
    }

    /// Ask for the game list and return each game's name.
    async fn list_game_names(s: &mut TcpStream) -> Vec<String> {
        list_game_names_for(s, "").await.0
    }

    /// Ask for the game list, optionally for one game by name (as a joiner does). Returns
    /// the names and, when the list was empty, the status DWORD.
    async fn list_game_names_for(s: &mut TcpStream, name: &str) -> (Vec<String>, Option<u32>) {
        let mut q = Writer::with_capacity(32);
        q.u32(0) // type / WC3 flags
            .u32(0) // filter / mask
            .u32(0) // reserved
            .u32(20) // count
            .cstr(name.as_bytes())
            .cstr(b"")
            .cstr(b"");
        send_frame(s, &Frame::new(sid::GETADVLISTEX, q.finish())).await;
        let reply = recv_frame(s).await;
        assert_eq!(reply.id, sid::GETADVLISTEX);
        let mut r = reply.reader();
        let count = r.u32().expect("count");
        if count == 0 {
            return (Vec::new(), Some(r.u32().expect("status")));
        }
        let mut names = Vec::new();
        for _ in 0..count {
            let _game_type = r.u16().unwrap();
            let _parameter = r.u16().unwrap();
            let _lang = r.u32().unwrap();
            let _af = r.u16().unwrap();
            let _port = r.u16().unwrap();
            let _ip: [u8; 4] = r.array().unwrap();
            let _z0 = r.u32().unwrap();
            let _z1 = r.u32().unwrap();
            let _status = r.u32().unwrap();
            let _elapsed = r.u32().unwrap();
            let name = r.cstr(256).unwrap().to_vec();
            let _password = r.cstr(256).unwrap();
            let _statstring = r.cstr(512).unwrap();
            names.push(String::from_utf8_lossy(&name).into_owned());
        }
        (names, None)
    }

    // ---- WarCraft III (NLS/SRP, realm accounts) ------------------------------------

    /// Connect and pass the version check as a Frozen Throne client.
    async fn wc3_handshake(addr: std::net::SocketAddr, key: u32) -> TcpStream {
        let mut s = connect(addr).await;
        send_frame(&mut s, &auth_info_frame_for(product::W3XP)).await;
        let info = recv_frame(&mut s).await;
        assert_eq!(info.id, sid::AUTH_INFO);
        assert_eq!(info.reader().u32().unwrap(), 0x02, "WC3 gets NLS v2 logon type");
        assert!(info.body.ends_with(&[0u8; 128]), "the 128-byte signature slot is present");
        send_frame(&mut s, &auth_check_frame(1, key)).await;
        let check = recv_frame(&mut s).await;
        assert_eq!(check.reader().u32().unwrap(), auth_check_status::PASSED);
        s
    }

    /// `SID_AUTH_ACCOUNTCREATE` with a client-computed salt and verifier; returns the status.
    async fn wc3_create(s: &mut TcpStream, user: &str, password: &str) -> u32 {
        let salt = [7u8; 32];
        let verifier = bnetcc_crypto::nls::verifier(user, password, &salt);
        let mut w = Writer::with_capacity(80);
        w.bytes(&salt).bytes(&verifier).cstr(user.as_bytes());
        send_frame(s, &Frame::new(sid::AUTH_ACCOUNTCREATE, w.finish())).await;
        let reply = recv_frame(s).await;
        assert_eq!(reply.id, sid::AUTH_ACCOUNTCREATE);
        reply.reader().u32().unwrap()
    }

    /// The 0x53/0x54 exchange. `Err(status)` names the packet's failure status; on success
    /// the server's `M2` has been checked against the client's own computation.
    async fn wc3_logon(s: &mut TcpStream, user: &str, password: &str) -> Result<(), u32> {
        let a = [3u8; 32];
        let client_public = bnetcc_crypto::nls::client_public(&a);
        let mut w = Writer::with_capacity(64);
        w.bytes(&client_public).cstr(user.as_bytes());
        send_frame(s, &Frame::new(sid::AUTH_ACCOUNTLOGON, w.finish())).await;
        let reply = recv_frame(s).await;
        assert_eq!(reply.id, sid::AUTH_ACCOUNTLOGON);
        assert_eq!(reply.body.len(), 4 + 32 + 32, "0x53 reply is always status + s + B");
        let mut r = reply.reader();
        let status = r.u32().unwrap();
        let salt: [u8; 32] = r.array().unwrap();
        let server_public: [u8; 32] = r.array().unwrap();
        if status != nls_status::LOGON_OK {
            assert_eq!(salt, [0u8; 32]);
            assert_eq!(server_public, [0u8; 32]);
            return Err(status);
        }
        let (m1, key) =
            bnetcc_crypto::nls::client_proof(user, password, &salt, &a, &server_public).expect("B != 0");
        let mut w = Writer::with_capacity(20);
        w.bytes(&m1);
        send_frame(s, &Frame::new(sid::AUTH_ACCOUNTLOGONPROOF, w.finish())).await;
        let reply = recv_frame(s).await;
        assert_eq!(reply.id, sid::AUTH_ACCOUNTLOGONPROOF);
        // Non-error 0x54 is exactly status + M2, no trailing string (a real WC3 client drops
        // on any extra byte); only the custom-error status (0x0F) carries a message.
        assert_eq!(reply.body.len(), 4 + 20, "0x54 (ok / wrong-password) is status + M2 only");
        let mut r = reply.reader();
        let status = r.u32().unwrap();
        let m2: [u8; 20] = r.array().unwrap();
        if status != nls_status::PROOF_OK {
            assert_eq!(m2, [0u8; 20]);
            return Err(status);
        }
        assert_eq!(
            m2,
            bnetcc_crypto::nls::server_proof_from_key(&client_public, &m1, &key),
            "the server must prove it knows K"
        );
        Ok(())
    }

    /// `SID_ENTERCHAT`; returns the unique name the server assigned.
    async fn enter_chat_name(s: &mut TcpStream) -> String {
        send_frame(s, &Frame::new(sid::ENTERCHAT, Writer::new().finish())).await;
        let reply = recv_frame(s).await;
        assert_eq!(reply.id, sid::ENTERCHAT);
        String::from_utf8_lossy(reply.reader().cstr(64).unwrap()).into_owned()
    }

    #[test]
    fn warcraft_three_and_diablo_two_get_the_ver_dash_mpq_name() {
        let ix86 = Some(bnetcc_proto::FourCc::from_ascii(b"IX86"));
        // WarCraft III depends on the patch: 1.27+ (>=0x1B) gets the ver- name, 1.26 (0x1A)
        // and older get the classic name; unknown version defaults to the modern name.
        assert_eq!(version_mpq_name(ix86, Some(product::W3XP), Some(0x1B)), "ver-IX86-1.mpq");
        assert_eq!(version_mpq_name(ix86, Some(product::WAR3), Some(0x1C)), "ver-IX86-1.mpq");
        assert_eq!(version_mpq_name(ix86, Some(product::W3XP), Some(0x1A)), "IX86ver1.mpq");
        assert_eq!(version_mpq_name(ix86, Some(product::WAR3), None), "ver-IX86-1.mpq");
        // Diablo II always uses the ver- name; the classic games use the old name.
        assert_eq!(version_mpq_name(ix86, Some(product::D2XP), None), "ver-IX86-1.mpq");
        assert_eq!(version_mpq_name(ix86, Some(product::SEXP), None), "IX86ver1.mpq");
        assert_eq!(version_mpq_name(ix86, Some(product::W2BN), None), "IX86ver1.mpq");
        assert_eq!(version_mpq_name(None, None, None), "IX86ver1.mpq");
    }

    #[tokio::test]
    async fn a_warcraft_three_client_creates_a_realm_account_and_logs_in() {
        let addr = spawn_server().await;
        let mut s = wc3_handshake(addr, 501).await;

        // No account yet: the challenge is refused (zeroed), the client then creates one.
        assert_eq!(wc3_logon(&mut s, "Tagban", "hunter2").await, Err(nls_status::LOGON_NO_ACCOUNT));
        assert_eq!(wc3_create(&mut s, "Tagban", "hunter2").await, nls_status::CREATE_OK);
        assert_eq!(
            wc3_create(&mut s, "tagban", "other").await,
            nls_status::CREATE_NAME_EXISTS,
            "realm names are case-insensitive"
        );
        assert_eq!(wc3_create(&mut s, "a", "x").await, nls_status::CREATE_TOO_SHORT);

        // Now the real thing, typed in a different case (the client upper-cases before
        // hashing, and the realm lookup is case-insensitive). A typed realm suffix is
        // refused as "no such account": the proof would hash it and never match anyway.
        assert_eq!(wc3_logon(&mut s, "TAGBAN@BNCC", "hunter2").await, Err(nls_status::LOGON_NO_ACCOUNT));
        assert_eq!(wc3_logon(&mut s, "TAGBAN", "hunter2").await, Ok(()));
        // Shown realm-qualified, the way Battle.net has shown WC3 users since launch
        // (Name@Azeroth). This keeps them distinct from any X-SHA-1 account of the same name.
        assert_eq!(enter_chat_name(&mut s).await, "Tagban@bncc");
    }

    #[tokio::test]
    async fn a_wrong_warcraft_three_password_is_refused_at_the_proof() {
        let addr = spawn_server().await;
        let mut s = wc3_handshake(addr, 502).await;
        assert_eq!(wc3_create(&mut s, "Zealot", "right").await, nls_status::CREATE_OK);
        assert_eq!(wc3_logon(&mut s, "Zealot", "wrong").await, Err(nls_status::PROOF_WRONG_PASSWORD));
        // Still not logged in: chat entry is a protocol violation and closes the stream.
        assert_eq!(wc3_logon(&mut s, "Zealot", "right").await, Ok(()));
        assert_eq!(enter_chat_name(&mut s).await, "Zealot@bncc");
    }

    #[tokio::test]
    async fn realm_accounts_and_xsha1_accounts_with_the_same_name_coexist() {
        let addr = spawn_server().await;
        // A Brood War player registers "Zealot" the X-SHA-1 way …
        let _sc = login(addr, "Zealot", "pw", 503).await;
        // … and a WarCraft III player registers "Zealot" too: a distinct account in the realm.
        // Both are allowed because they render distinctly — "Zealot" vs "Zealot@bncc" — so they
        // never collide as a chat name.
        let mut w3 = wc3_handshake(addr, 504).await;
        assert_eq!(wc3_create(&mut w3, "Zealot", "pw2").await, nls_status::CREATE_OK);
        assert_eq!(wc3_logon(&mut w3, "Zealot", "pw2").await, Ok(()));
        assert_eq!(enter_chat_name(&mut w3).await, "Zealot@bncc");

        // An X-SHA-1 client still cannot put a literal '@' in a created name (reserved for realms).
        let mut fresh = connect(addr).await;
        send_frame(&mut fresh, &auth_info_frame()).await;
        let _ = recv_frame(&mut fresh).await;
        send_frame(&mut fresh, &auth_check_frame(1, 505)).await;
        let _ = recv_frame(&mut fresh).await;
        let mut cw = Writer::with_capacity(32);
        cw.bytes(&[0u8; 20]).cstr(b"Grunt@bncc");
        send_frame(&mut fresh, &Frame::new(sid::CREATEACCOUNT2, cw.finish())).await;
        assert_eq!(
            recv_frame(&mut fresh).await.reader().u32().unwrap(),
            0x02,
            "'@' is reserved for realms"
        );
    }

    #[tokio::test]
    async fn the_game_list_is_per_product_and_answers_a_lookup_by_name() {
        let addr = spawn_server().await;
        let mut host = wc3_handshake(addr, 506).await;
        assert_eq!(wc3_create(&mut host, "Host", "pw").await, nls_status::CREATE_OK);
        assert_eq!(wc3_logon(&mut host, "Host", "pw").await, Ok(()));
        enter_chat_name(&mut host).await;
        send_frame(&mut host, &start_adv_frame("DotA 6.83 -apem")).await;
        assert_eq!(recv_frame(&mut host).await.reader().u32().unwrap(), advertise_status::OK);

        // A Brood War client never sees a WarCraft III game …
        let mut sc = login(addr, "Marine", "pw", 507).await;
        assert_eq!(list_game_names_for(&mut sc, "").await, (Vec::new(), Some(0x00)));

        // … a Frozen Throne client does, and can fetch it by name as a joiner.
        let mut joiner = wc3_handshake(addr, 508).await;
        assert_eq!(wc3_create(&mut joiner, "Joiner", "pw").await, nls_status::CREATE_OK);
        assert_eq!(wc3_logon(&mut joiner, "Joiner", "pw").await, Ok(()));
        enter_chat_name(&mut joiner).await;
        assert_eq!(list_game_names(&mut joiner).await, vec!["DotA 6.83 -apem".to_string()]);
        assert_eq!(
            list_game_names_for(&mut joiner, "dota 6.83 -APEM").await,
            (vec!["DotA 6.83 -apem".to_string()], None)
        );
        assert_eq!(
            list_game_names_for(&mut joiner, "nope").await,
            (Vec::new(), Some(0x01)),
            "a named lookup that misses says the game doesn't exist"
        );
    }

    #[tokio::test]
    async fn a_hosted_game_appears_in_the_list_then_disappears_when_ended() {
        let addr = spawn_server().await;
        let mut host = login(addr, "HostBot", "pw", 1).await;

        // No games yet.
        assert!(list_game_names(&mut host).await.is_empty());

        // Advertise a game; it should appear in the list.
        send_frame(&mut host, &start_adv_frame("My Melee Game")).await;
        let adv = recv_frame(&mut host).await;
        assert_eq!(adv.id, sid::STARTADVEX3);
        assert_eq!(adv.reader().u32().unwrap(), advertise_status::OK);

        let names = list_game_names(&mut host).await;
        assert_eq!(names, vec!["My Melee Game".to_string()], "hosted game should be listed");

        // A second client sees it too.
        let mut viewer = login(addr, "Viewer", "pw", 2).await;
        assert_eq!(list_game_names(&mut viewer).await, vec!["My Melee Game".to_string()]);

        // Ending it (SID_STOPADV) removes it from the directory.
        send_frame(&mut host, &Frame::new(sid::STOPADV, Writer::new().finish())).await;
        // STOPADV has no reply; give the server a moment, then re-list.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            list_game_names(&mut viewer).await.is_empty(),
            "an ended game must not remain in the list"
        );
    }

    #[tokio::test]
    async fn a_hosts_game_is_withdrawn_when_the_host_disconnects() {
        let addr = spawn_server().await;
        let mut host = login(addr, "Ghost", "pw", 1).await;
        send_frame(&mut host, &start_adv_frame("Abandoned")).await;
        let _ = recv_frame(&mut host).await;

        let mut viewer = login(addr, "Watcher", "pw", 2).await;
        assert_eq!(list_game_names(&mut viewer).await, vec!["Abandoned".to_string()]);

        // Host vanishes without sending STOPADV — the game must still be cleaned up.
        drop(host);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            list_game_names(&mut viewer).await.is_empty(),
            "a game must not outlive the connection hosting it"
        );
    }

    /// Drive a full **legacy-flow** (Diablo I) login as a real `DRTL` client would:
    /// `SID_STARTVERSIONING` → `SID_REPORTVERSION` → icon negotiation → legacy account
    /// creation → `SID_LOGONRESPONSE` (0x29) → `SID_ENTERCHAT`. Asserts the Diablo-specific
    /// replies (the `icons.bni` filename and the level-1 Warrior default statstring) as it
    /// goes. Returns the logged-in stream, in chat and ready to join a channel.
    async fn diablo_login(addr: std::net::SocketAddr, user: &str, password: &str) -> TcpStream {
        const CLIENT_TOKEN: u32 = 0x1234_5678;
        let mut s = connect(addr).await;

        // SID_STARTVERSIONING (0x06): platform, product, version byte (Diablo reports 0x2A).
        let mut vw = Writer::with_capacity(16);
        vw.fourcc(bnetcc_proto::FourCc::from_ascii(b"IX86"))
            .fourcc(product::DRTL)
            .u32(0x2A);
        send_frame(&mut s, &Frame::new(sid::STARTVERSIONING, vw.finish())).await;
        assert_eq!(recv_frame(&mut s).await.id, sid::STARTVERSIONING, "version reply");

        // SID_REPORTVERSION (0x07): the handler ignores the body; result 0x02 == success.
        send_frame(&mut s, &Frame::new(sid::REPORTVERSION, Writer::new().finish())).await;
        let rv = recv_frame(&mut s).await;
        assert_eq!(rv.id, sid::REPORTVERSION);
        assert_eq!(rv.reader().u32().unwrap(), 0x02, "legacy version check should pass");

        // SID_GETICONDATA (0x2D): Diablo must be handed `icons.bni`, not `icons_STAR.bni`.
        send_frame(&mut s, &Frame::new(sid::GETICONDATA, Writer::new().finish())).await;
        let icon = recv_frame(&mut s).await;
        assert_eq!(icon.id, sid::GETICONDATA);
        let mut ir = icon.reader();
        let _filetime = ir.u64().unwrap();
        assert_eq!(ir.cstr(64).unwrap(), b"icons.bni", "Diablo icon file selection");

        // Legacy account creation (SID_CREATEACCOUNT, 0x2A): single-hash password + name.
        let h1 = bnetcc_crypto::password_hash(password);
        let mut cw = Writer::with_capacity(32);
        cw.bytes(&h1).cstr(user.as_bytes());
        send_frame(&mut s, &Frame::new(sid::CREATEACCOUNT, cw.finish())).await;
        assert_eq!(
            recv_frame(&mut s).await.reader().u32().unwrap(),
            0x00,
            "legacy account creation"
        );

        // SID_LOGONRESPONSE (0x29): the legacy flow issues no server token, so the client
        // computes its proof with the server token it puts in the packet — real clients send 0.
        let proof = bnetcc_crypto::logon_proof(CLIENT_TOKEN, 0, &h1);
        let mut lw = Writer::with_capacity(32);
        lw.u32(CLIENT_TOKEN).u32(0).bytes(&proof).cstr(user.as_bytes());
        send_frame(&mut s, &Frame::new(sid::LOGONRESPONSE, lw.finish())).await;
        let logon = recv_frame(&mut s).await;
        assert_eq!(logon.id, sid::LOGONRESPONSE);
        assert_eq!(logon.reader().u32().unwrap(), 1, "legacy logon: 1 == success");

        // SID_ENTERCHAT: the reply's second string is our own statstring — the Diablo default.
        send_frame(&mut s, &Frame::new(sid::ENTERCHAT, Writer::new().finish())).await;
        let enter = recv_frame(&mut s).await;
        assert_eq!(enter.id, sid::ENTERCHAT);
        let mut er = enter.reader();
        let _name = er.cstr(64).unwrap();
        assert_eq!(
            er.cstr(64).unwrap(),
            bnetcc_proto::statstring::layout::DIABLO_DEFAULT,
            "a fresh Diablo user gets the level-1 Warrior default statstring"
        );
        s
    }

    /// Send `SID_JOINCHANNEL` with the first-join flag for `channel`. The reply (a snapshot
    /// of chat events) is left on the socket for the caller to read as it needs.
    async fn join_channel_first(stream: &mut TcpStream, channel: &str) {
        let mut w = Writer::with_capacity(32);
        w.u32(0x01).cstr(channel.as_bytes()); // 0x01 = first join
        send_frame(stream, &Frame::new(sid::JOINCHANNEL, w.finish())).await;
    }

    /// Read the next `SID_CHATEVENT`, skipping keepalives. Returns (event id, flags,
    /// username, text). Panics on any non-chat, non-ping frame — the tests only call it
    /// where a chat event is expected.
    async fn recv_chatevent(stream: &mut TcpStream) -> (u32, u32, Vec<u8>, Vec<u8>) {
        loop {
            let mut header = [0u8; bnetcc_proto::bncs::HEADER_LEN];
            stream.read_exact(&mut header).await.expect("chat header");
            assert_eq!(header[0], bnetcc_proto::bncs::MAGIC, "bad frame magic");
            let id = header[1];
            let total = u16::from_le_bytes([header[2], header[3]]) as usize;
            let mut body = vec![0u8; total.saturating_sub(bnetcc_proto::bncs::HEADER_LEN)];
            stream.read_exact(&mut body).await.expect("chat body");
            if id == sid::PING {
                continue;
            }
            assert_eq!(id, sid::CHATEVENT, "expected a chat event");
            let frame = Frame { id, body };
            let mut r = frame.reader();
            let event = r.u32().unwrap();
            let flags = r.u32().unwrap();
            let _ping = r.u32().unwrap();
            let _ip = r.u32().unwrap();
            let _acct = r.u32().unwrap();
            let _reg = r.u32().unwrap();
            let username = r.cstr(64).unwrap().to_vec();
            let text = r.cstr(512).unwrap().to_vec();
            return (event, flags, username, text);
        }
    }

    #[tokio::test]
    async fn a_diablo_client_completes_the_legacy_login() {
        // The whole legacy handshake succeeds end-to-end and the Diablo-specific asserts
        // inside the helper (icon file, default statstring) hold.
        let addr = spawn_server().await;
        let _s = diablo_login(addr, "Wanderer", "pw").await;
    }

    #[tokio::test]
    async fn a_diablo_user_carries_no_udp_and_diablo_stats_in_channel() {
        let addr = spawn_server().await;

        // A modern StarCraft client joins a channel first...
        let mut modern = login(addr, "Watcher", "pw", 7).await;
        join_channel_first(&mut modern, "Diablo").await;
        // ...and its own SHOWUSER in the join snapshot must NOT carry No-UDP (the control:
        // SEXP completes the UDP check, so it is never forced No-UDP).
        let watcher_flags = loop {
            let (event, flags, name, _) = recv_chatevent(&mut modern).await;
            if event == EventId::ShowUser as u32 && name.as_slice() == b"Watcher" {
                break flags;
            }
        };
        assert_eq!(watcher_flags & user_flags::NO_UDP, 0, "SEXP is not a No-UDP product");

        // A Diablo client logs in through the legacy flow and joins the same channel.
        let mut diablo = diablo_login(addr, "Diabloer", "pw").await;
        join_channel_first(&mut diablo, "Diablo").await;

        // The modern client sees the Diablo user arrive (EID_JOIN) carrying its flags and
        // statstring: No-UDP must be set, and the statstring must be the Diablo default.
        let (_event, flags, _name, text) = loop {
            let ev = recv_chatevent(&mut modern).await;
            if ev.0 == EventId::Join as u32 && ev.2.as_slice() == b"Diabloer" {
                break ev;
            }
        };
        assert_ne!(flags & user_flags::NO_UDP, 0, "Diablo users always carry the No-UDP flag");
        assert_eq!(
            text.as_slice(),
            bnetcc_proto::statstring::layout::DIABLO_DEFAULT,
            "the Diablo default statstring reaches other clients in the channel"
        );
    }
}
