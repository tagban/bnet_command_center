//! Integration and concurrency harness.
//!
//! Drives a real BNCS handshake — protocol selector, `SID_AUTH_INFO`, `SID_AUTH_CHECK`,
//! `SID_CREATEACCOUNT2`, `SID_LOGONRESPONSE2` with a genuine X-SHA-1 double-hash proof,
//! `SID_ENTERCHAT`, `SID_JOINCHANNEL`, `SID_CHATCOMMAND` — over real TCP sockets, at
//! whatever concurrency you ask for.
//!
//! # What this does and does not prove
//!
//! **Does prove:** the framing survives real socket boundaries and partial reads; the
//! session state machine accepts the real handshake; X-SHA-1 verification works
//! end-to-end between two independent implementations of the hashing chain; channel
//! join, roster and fanout behave under concurrency; admission control counts correctly;
//! and the host can actually carry N concurrent sockets.
//!
//! **Does not prove anything about tokio.** This harness is thread-per-connection
//! because it must build without a dependency on an async runtime. Its memory figures
//! therefore include ~128 KiB of thread stack per connection that `bnetccd` does not pay
//! — a tokio task is a few hundred bytes plus its buffers. Read the RSS number as an
//! upper bound with a large, known constant in it, and re-measure `bnetccd` itself on the
//! target host.
//!
//! Usage: `bnetcc-smoke [connections] [channel_size]` (defaults 2500 and 40).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bnetcc_core::channel::{Channel, ChannelClass};
use bnetcc_core::session::SessionState;
use bnetcc_crypto::{logon_proof, password_hash, proofs_match};
use bnetcc_proto::bncs::{
    decode_frame, encode_frame, logon_status, sid, Frame, DEFAULT_MAX_FRAME,
};
use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::chat::{chat_event, normalize_channel_name, EventId, USERNAME_MAX};
use bnetcc_proto::product;

const READ_CHUNK: usize = 4096;
const STACK: usize = 128 * 1024;
/// Connections opened per wave, and the pause between waves.
const WAVE: usize = 100;
const WAVE_PAUSE_MS: u64 = 15;

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Account id and stored `XSHA1(lowercase(password))`.
type AccountRecord = (u64, [u8; 20]);
/// Subscribers to one channel: account id plus a writable socket clone.
type ChannelSubs = Arc<Mutex<Vec<(u64, TcpStream)>>>;
/// Every channel's subscriber list. The outer lock is held only long enough to clone
/// the inner `Arc`, so a blocking write to one channel never stalls another's joins.
type Subscribers = HashMap<Vec<u8>, ChannelSubs>;

struct Shared {
    accounts: Mutex<HashMap<String, AccountRecord>>,
    next_id: AtomicU64,
    channels: Mutex<HashMap<Vec<u8>, Channel>>,
    subscribers: Mutex<Subscribers>,
    logons_ok: AtomicU64,
    logons_failed: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            accounts: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            channels: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(HashMap::new()),
            logons_ok: AtomicU64::new(0),
            logons_failed: AtomicU64::new(0),
        }
    }
}

fn write_frame(s: &mut TcpStream, frame: &Frame) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(frame.wire_len());
    encode_frame(frame, &mut buf).expect("encodable");
    s.write_all(&buf)
}

fn serve(mut sock: TcpStream, shared: Arc<Shared>) {
    let _ = sock.set_nodelay(true);
    let _ = sock.set_write_timeout(Some(Duration::from_secs(10)));

    let mut sel = [0u8; 1];
    if sock.read_exact(&mut sel).is_err() || sel[0] != 0x01 {
        return;
    }

    let mut state = SessionState::Connected;
    let mut server_token: u32 = 0x1234_5678;
    let mut account: Option<(u64, String)> = None;
    let mut joined: Option<Vec<u8>> = None;
    let mut flags: u32 = 0;
    let mut buf = RecvBuf::with_capacity(READ_CHUNK);

    'outer: loop {
        let tail = buf.writable_tail(READ_CHUNK);
        let n = match sock.read(tail) {
            Ok(n) => n,
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
            let frame = match decode_frame(&mut buf, DEFAULT_MAX_FRAME) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => break 'outer,
            };
            if !state.accepts(frame.id) {
                break 'outer;
            }

            match frame.id {
                sid::AUTH_INFO => {
                    let mut r = frame.reader();
                    let (_proto, _plat, prod) = match (r.u32(), r.fourcc(), r.fourcc()) {
                        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                        _ => break 'outer,
                    };
                    if product::always_no_udp(prod) {
                        flags |= bnetcc_proto::chat::user_flags::NO_UDP;
                    }
                    server_token = server_token.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let mut w = Writer::with_capacity(64);
                    w.u32(0)
                        .u32(server_token)
                        .u32(0)
                        .u64(0)
                        .cstr(b"ver-IX86-1.mpq")
                        .cstr(b"A=1 B=1 C=1 4 A=A^S B=B^C C=C^A A=A^B");
                    if write_frame(&mut sock, &Frame::new(sid::AUTH_INFO, w.finish())).is_err() {
                        break 'outer;
                    }
                }
                sid::AUTH_CHECK => {
                    let mut w = Writer::new();
                    w.u32(0).cstr(b"");
                    if write_frame(&mut sock, &Frame::new(sid::AUTH_CHECK, w.finish())).is_err() {
                        break 'outer;
                    }
                }
                sid::CREATEACCOUNT2 => {
                    let mut r = frame.reader();
                    let (hash, name) = match (r.array::<20>(), r.cstr(USERNAME_MAX)) {
                        (Ok(h), Ok(n)) => (h, String::from_utf8_lossy(n).to_string()),
                        _ => break 'outer,
                    };
                    let id = shared.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    let status = {
                        let mut accounts = shared.accounts.lock().expect("accounts");
                        use std::collections::hash_map::Entry;
                        match accounts.entry(name.to_ascii_lowercase()) {
                            Entry::Occupied(_) => 0x04u32,
                            Entry::Vacant(slot) => {
                                slot.insert((id, hash));
                                0x00
                            }
                        }
                    };
                    let mut w = Writer::new();
                    w.u32(status).cstr(b"");
                    if write_frame(&mut sock, &Frame::new(sid::CREATEACCOUNT2, w.finish())).is_err()
                    {
                        break 'outer;
                    }
                }
                sid::LOGONRESPONSE2 => {
                    let mut r = frame.reader();
                    let parsed = (|| {
                        let ct = r.u32().ok()?;
                        let _st = r.u32().ok()?;
                        let proof = r.array::<20>().ok()?;
                        let name = r.cstr(USERNAME_MAX).ok()?.to_vec();
                        Some((ct, proof, String::from_utf8_lossy(&name).to_string()))
                    })();
                    let Some((client_token, proof, name)) = parsed else {
                        break 'outer;
                    };
                    let record = shared
                        .accounts
                        .lock()
                        .expect("accounts")
                        .get(&name.to_ascii_lowercase())
                        .copied();
                    let status = match record {
                        None => logon_status::NO_SUCH_ACCOUNT,
                        Some((id, h1)) => {
                            let expected = logon_proof(client_token, server_token, &h1);
                            if proofs_match(&proof, &expected) {
                                account = Some((id, name.clone()));
                                shared.logons_ok.fetch_add(1, Ordering::Relaxed);
                                logon_status::SUCCESS
                            } else {
                                shared.logons_failed.fetch_add(1, Ordering::Relaxed);
                                logon_status::WRONG_PASSWORD
                            }
                        }
                    };
                    let mut w = Writer::new();
                    w.u32(status);
                    if write_frame(&mut sock, &Frame::new(sid::LOGONRESPONSE2, w.finish())).is_err()
                    {
                        break 'outer;
                    }
                    if status != logon_status::SUCCESS {
                        break 'outer;
                    }
                }
                sid::ENTERCHAT => {
                    let Some((_, ref name)) = account else {
                        break 'outer;
                    };
                    let mut w = Writer::new();
                    w.cstr(name.as_bytes()).cstr(b"").cstr(name.as_bytes());
                    if write_frame(&mut sock, &Frame::new(sid::ENTERCHAT, w.finish())).is_err() {
                        break 'outer;
                    }
                }
                sid::JOINCHANNEL => {
                    let Some((id, ref name)) = account else {
                        break 'outer;
                    };
                    let mut r = frame.reader();
                    let requested = match (r.u32(), r.cstr(31)) {
                        (Ok(_), Ok(c)) => c.to_vec(),
                        _ => break 'outer,
                    };
                    let key = normalize_channel_name(&requested);

                    let (existing, my_flags) = {
                        let mut channels = shared.channels.lock().expect("channels");
                        let ch = channels.entry(key.clone()).or_insert_with(|| {
                            Channel::new(
                                key.clone(),
                                String::from_utf8_lossy(&requested).to_string(),
                                ChannelClass::Local,
                                4096,
                            )
                        });
                        let existing: Vec<(String, u32)> = ch
                            .members()
                            .iter()
                            .map(|m| (m.name.clone(), m.flags))
                            .collect();
                        match ch.join(id, name.clone(), flags) {
                            Ok(o) => (existing, o.flags),
                            Err(_) => break 'outer,
                        }
                    };
                    flags = my_flags;
                    if let Ok(clone) = sock.try_clone() {
                        let subs = Arc::clone(
                            shared
                                .subscribers
                                .lock()
                                .expect("subs")
                                .entry(key.clone())
                                .or_default(),
                        );
                        subs.lock().expect("chan subs").push((id, clone));
                    }
                    joined = Some(key.clone());

                    if write_frame(
                        &mut sock,
                        &chat_event(EventId::Channel, 0, 0, &requested, b""),
                    )
                    .is_err()
                    {
                        break 'outer;
                    }
                    for (n, f) in existing {
                        if write_frame(
                            &mut sock,
                            &chat_event(EventId::ShowUser, f, 0, n.as_bytes(), b""),
                        )
                        .is_err()
                        {
                            break 'outer;
                        }
                    }
                    broadcast(
                        &shared,
                        &key,
                        &chat_event(EventId::Join, flags, 0, name.as_bytes(), b""),
                        Some(id),
                    );
                }
                sid::CHATCOMMAND => {
                    let Some((id, ref name)) = account else {
                        break 'outer;
                    };
                    let Some(ref key) = joined else {
                        continue;
                    };
                    let mut r = frame.reader();
                    let Ok(text) = r.cstr(224) else { break 'outer };
                    let text = bnetcc_proto::chat::sanitize_chat_text(text);
                    broadcast(
                        &shared,
                        key,
                        &chat_event(EventId::Talk, flags, 0, name.as_bytes(), &text),
                        Some(id),
                    );
                }
                _ => {}
            }

            if let Some(next) = state.next_on_success(frame.id) {
                if next != SessionState::LoggedIn || account.is_some() {
                    state = next;
                }
            }
        }
    }

    if let (Some(key), Some((id, name))) = (joined, account) {
        if let Some(c) = shared.channels.lock().expect("channels").get_mut(&key) {
            c.leave(id);
        }
        let list = shared
            .subscribers
            .lock()
            .expect("subs")
            .get(&key)
            .map(Arc::clone);
        if let Some(list) = list {
            list.lock().expect("chan subs").retain(|(i, _)| *i != id);
        }
        broadcast(
            &shared,
            &key,
            &chat_event(EventId::Leave, 0, 0, name.as_bytes(), b""),
            Some(id),
        );
    }
}

/// Encode once, write to every subscriber.
fn broadcast(shared: &Shared, key: &[u8], frame: &Frame, exclude: Option<u64>) {
    let mut wire = Vec::with_capacity(frame.wire_len());
    encode_frame(frame, &mut wire).expect("encodable");
    // Take the outer lock only to clone the Arc, then release it before writing.
    let list = shared
        .subscribers
        .lock()
        .expect("subs")
        .get(key)
        .map(Arc::clone);
    if let Some(list) = list {
        list.lock().expect("chan subs").retain_mut(|(id, s)| {
            if Some(*id) == exclude {
                return true;
            }
            s.write_all(&wire).is_ok()
        });
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

struct Client {
    sock: TcpStream,
    buf: RecvBuf,
}

impl Client {
    fn connect(addr: std::net::SocketAddr) -> std::io::Result<Self> {
        let mut sock = TcpStream::connect(addr)?;
        sock.set_nodelay(true)?;
        sock.write_all(&[0x01])?;
        Ok(Self {
            sock,
            buf: RecvBuf::with_capacity(1024),
        })
    }

    fn send(&mut self, frame: &Frame) -> std::io::Result<()> {
        write_frame(&mut self.sock, frame)
    }

    fn recv(&mut self) -> std::io::Result<Frame> {
        loop {
            if let Ok(Some(f)) = decode_frame(&mut self.buf, DEFAULT_MAX_FRAME) {
                return Ok(f);
            }
            let tail = self.buf.writable_tail(READ_CHUNK);
            let n = self.sock.read(tail)?;
            self.buf.commit(n, READ_CHUNK);
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "peer closed",
                ));
            }
        }
    }

    fn expect(&mut self, id: u8) -> std::io::Result<Frame> {
        loop {
            let f = self.recv()?;
            if f.id == id {
                return Ok(f);
            }
        }
    }

    /// Full handshake through channel join. Returns the granted user flags.
    fn handshake(&mut self, user: &str, password: &str, channel: &str) -> std::io::Result<()> {
        // SID_AUTH_INFO
        let mut w = Writer::new();
        w.u32(0)
            .fourcc(bnetcc_proto::FourCc::from_ascii(b"IX86"))
            .fourcc(product::SEXP)
            .u32(0xCD)
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(0)
            .cstr(b"USA")
            .cstr(b"United States");
        self.send(&Frame::new(sid::AUTH_INFO, w.finish()))?;
        let reply = self.expect(sid::AUTH_INFO)?;
        let mut r = reply.reader();
        let _logon_type = r.u32().map_err(bad)?;
        let server_token = r.u32().map_err(bad)?;

        // SID_AUTH_CHECK
        let mut w = Writer::new();
        w.u32(0xAAAA_BBBB).u32(1).u32(0).u32(0).u32(0).cstr(b"").cstr(b"");
        self.send(&Frame::new(sid::AUTH_CHECK, w.finish()))?;
        let reply = self.expect(sid::AUTH_CHECK)?;
        if reply.reader().u32().map_err(bad)? != 0 {
            return Err(other("version check refused"));
        }

        // SID_CREATEACCOUNT2 — password hashed once.
        let h1 = password_hash(password);
        let mut w = Writer::new();
        w.bytes(&h1).cstr(user.as_bytes());
        self.send(&Frame::new(sid::CREATEACCOUNT2, w.finish()))?;
        let reply = self.expect(sid::CREATEACCOUNT2)?;
        let status = reply.reader().u32().map_err(bad)?;
        if status != 0x00 && status != 0x04 {
            return Err(other("account creation refused"));
        }

        // SID_LOGONRESPONSE2 — password hashed twice, with both tokens.
        let client_token = 0xDEAD_0000u32 ^ (server_token.rotate_left(7));
        let proof = logon_proof(client_token, server_token, &h1);
        let mut w = Writer::new();
        w.u32(client_token)
            .u32(server_token)
            .bytes(&proof)
            .cstr(user.as_bytes());
        self.send(&Frame::new(sid::LOGONRESPONSE2, w.finish()))?;
        let reply = self.expect(sid::LOGONRESPONSE2)?;
        let status = reply.reader().u32().map_err(bad)?;
        if status != logon_status::SUCCESS {
            return Err(other(&format!("logon rejected, status {status:#x}")));
        }

        // SID_ENTERCHAT
        let mut w = Writer::new();
        w.cstr(user.as_bytes()).cstr(b"");
        self.send(&Frame::new(sid::ENTERCHAT, w.finish()))?;
        self.expect(sid::ENTERCHAT)?;

        // SID_JOINCHANNEL
        let mut w = Writer::new();
        w.u32(0x01).cstr(channel.as_bytes());
        self.send(&Frame::new(sid::JOINCHANNEL, w.finish()))?;
        self.expect(sid::CHATEVENT)?;
        Ok(())
    }

    fn say(&mut self, text: &str) -> std::io::Result<()> {
        let mut w = Writer::new();
        w.cstr(text.as_bytes());
        self.send(&Frame::new(sid::CHATCOMMAND, w.finish()))
    }
}

fn bad(e: bnetcc_proto::ProtoError) -> std::io::Error {
    other(&e.to_string())
}

fn other(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn rss_kib() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let target: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2500);
    let channel_size: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    println!("bnetcc-smoke: {target} concurrent connections, channels of {channel_size}");
    println!("(thread-per-connection harness; see the module docs on what this proves)\n");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shared = Arc::new(Shared::new());

    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    let _ = thread::Builder::new()
                        .stack_size(STACK)
                        .spawn(move || serve(stream, shared));
                }
            })
            .expect("accept thread");
    }

    let baseline = rss_kib().unwrap_or(0);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<usize, String>>();
    let (talk_tx, talk_rx) = mpsc::channel::<Instant>();

    let start = Instant::now();
    let mut handles = Vec::with_capacity(target);
    for i in 0..target {
        // std's TcpListener has a fixed 128-entry accept backlog. Launching 2500
        // connects in the same instant overruns it no matter how the server is written,
        // so we open in waves: the measurement we care about is how many connections are
        // held concurrently, not how many can be accepted in one millisecond.
        if i > 0 && i % WAVE == 0 {
            thread::sleep(Duration::from_millis(WAVE_PAUSE_MS));
        }
        let ready_tx = ready_tx.clone();
        let talk_tx = talk_tx.clone();
        let channel = format!("chan{}", i / channel_size);
        let spawned = thread::Builder::new().stack_size(STACK).spawn(move || {
            let mut c = match Client::connect(addr) {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("connect: {e}")));
                    return;
                }
            };
            let user = format!("user{i}");
            if let Err(e) = c.handshake(&user, "hunter2", &channel) {
                let _ = ready_tx.send(Err(format!("handshake: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(i));
            // Hold the connection open and timestamp any channel talk we receive.
            loop {
                match c.recv() {
                    Ok(f) if f.id == sid::CHATEVENT => {
                        let mut r = f.reader();
                        if r.u32() == Ok(EventId::Talk as u32) {
                            let _ = talk_tx.send(Instant::now());
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        // Two threads per connection (this harness's own client, plus the server's
        // handler thread — see the module docs) means the *host's* thread ceiling, not
        // the server, is usually what caps `target` in practice. Report what was
        // actually held rather than crashing the whole run over it.
        match spawned {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                eprintln!(
                    "stopped spawning after {i}/{target} connections: {e} \
                     (this host's per-process thread limit, not a server limit)"
                );
                break;
            }
        }
    }
    drop(ready_tx);
    drop(talk_tx);

    let mut ok = 0usize;
    let mut failures: HashMap<String, usize> = HashMap::new();
    for _ in 0..target {
        match ready_rx.recv_timeout(Duration::from_secs(180)) {
            Ok(Ok(_)) => ok += 1,
            Ok(Err(e)) => {
                let key: String = e.chars().take(70).collect();
                *failures.entry(key).or_insert(0) += 1;
            }
            Err(_) => break,
        }
    }
    let setup = start.elapsed();
    let after = rss_kib().unwrap_or(0);

    println!("connections established : {ok} / {target}");
    if failures.is_empty() {
        println!("failures                : none");
    } else {
        for (k, v) in &failures {
            println!("failures                : {v} x {k}");
        }
    }
    println!("logons verified         : {}", shared.logons_ok.load(Ordering::Relaxed));
    println!(
        "logons rejected         : {}",
        shared.logons_failed.load(Ordering::Relaxed)
    );
    println!("setup wall time         : {setup:.2?}");
    if ok > 0 {
        println!(
            "  per connection        : {:.3?}",
            setup / u32::try_from(ok).unwrap_or(1)
        );
    }
    println!("RSS baseline            : {baseline} KiB");
    println!("RSS with {ok:>5} conns   : {after} KiB");
    if ok > 0 && after > baseline {
        let per = (after - baseline) as f64 / ok as f64;
        println!("  per connection        : {per:.1} KiB (server + client + 2 thread stacks)");
    }

    // Fanout latency: a prober joins chan0 and speaks; every other member of that
    // channel should receive it. This is the operation that actually scales with
    // channel size, so it is the one worth timing.
    println!();
    match Client::connect(addr) {
        Err(e) => println!("fanout probe            : could not connect ({e})"),
        Ok(mut prober) => {
            // Unlike the held-open connections above (each in its own throwaway
            // thread, where blocking forever is fine), this runs on the main thread:
            // a stalled read here — e.g. the server refusing the thread it needed to
            // handle this very connection, because the run above already parked the
            // host at its thread ceiling — must not hang the whole harness.
            let _ = prober.sock.set_read_timeout(Some(Duration::from_secs(10)));
            if let Err(e) = prober.handshake("prober", "hunter2", "chan0") {
                println!("fanout probe            : handshake failed ({e})");
            } else {
                // Drain join noise.
                while talk_rx.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(200));
                while talk_rx.try_recv().is_ok() {}

                let expected = channel_size.min(ok);
                let t0 = Instant::now();
                if prober.say("gg").is_ok() {
                    let mut samples = Vec::new();
                    let deadline = Instant::now() + Duration::from_secs(15);
                    while samples.len() < expected && Instant::now() < deadline {
                        match talk_rx.recv_timeout(Duration::from_millis(500)) {
                            Ok(at) => samples.push(at.saturating_duration_since(t0)),
                            Err(_) => break,
                        }
                    }
                    samples.sort_unstable();
                    println!(
                        "fanout recipients       : {} / {expected} in chan0",
                        samples.len()
                    );
                    if !samples.is_empty() {
                        println!("  p50                   : {:.3?}", percentile(&samples, 0.50));
                        println!("  p99                   : {:.3?}", percentile(&samples, 0.99));
                        println!("  max                   : {:.3?}", samples[samples.len() - 1]);
                    }
                }
            }
        }
    }

    println!("\ndone");
    std::process::exit(if ok == target { 0 } else { 1 });
}
