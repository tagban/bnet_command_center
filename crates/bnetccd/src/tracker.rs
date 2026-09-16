//! PvPGN-compatible server tracking.
//!
//! Two independent halves, each optional:
//!
//! * **Advertise** — periodically beacon this server to public PvPGN trackers (e.g.
//!   `tracker.pvpgn.org:6114`) so it appears on their lists.
//! * **Host** — accept those same beacons from other servers on UDP `:6114`, and serve a
//!   public HTML/JSON list of them over HTTP, so operators can list themselves with us.
//!
//! The wire format is the PvPGN tracking protocol (BNETDocs document 35): a fixed 464-byte
//! UDP datagram, big-endian, packet version `0x0002`, ANSI strings null-padded (not
//! terminated). This codec is written from that public spec — not from PvPGN's GPL
//! `tracker.cpp` (see docs/LEGAL.md).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tracing::{info, warn};

use crate::config::TrackerConfig;
use crate::node::Node;

/// Tracking protocol packet version this implements.
const PACKET_VERSION: u16 = 2;
/// Total wire size of a tracking packet.
const PACKET_SIZE: usize = 464;

/// A decoded tracking packet (the dynamic stats plus the server's identity strings).
#[derive(Debug, Clone)]
struct TrackPacket {
    server_port: u16,
    flags: u32,
    software: String,
    version: String,
    platform: String,
    server_desc: String,
    location: String,
    url: String,
    contact_name: String,
    contact_email: String,
    active_users: u32,
    active_channels: u32,
    active_games: u32,
    uptime: u32,
    total_games: u32,
    total_logins: u32,
}

/// Write `s` into an `n`-byte field: truncated to fit, then null-padded (not terminated).
fn put_str(buf: &mut Vec<u8>, s: &str, n: usize) {
    let bytes = s.as_bytes();
    let take = bytes.len().min(n);
    buf.extend_from_slice(&bytes[..take]);
    buf.resize(buf.len() + (n - take), 0);
}

/// Read an `n`-byte field at `off` (advancing it): drop trailing nulls, decode as text.
fn get_str(buf: &[u8], off: &mut usize, n: usize) -> String {
    let raw = &buf[*off..*off + n];
    *off += n;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(n);
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

impl TrackPacket {
    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(PACKET_SIZE);
        b.extend_from_slice(&PACKET_VERSION.to_be_bytes());
        b.extend_from_slice(&self.server_port.to_be_bytes());
        b.extend_from_slice(&self.flags.to_be_bytes());
        put_str(&mut b, &self.software, 32);
        put_str(&mut b, &self.version, 16);
        put_str(&mut b, &self.platform, 32);
        put_str(&mut b, &self.server_desc, 64);
        put_str(&mut b, &self.location, 64);
        put_str(&mut b, &self.url, 96);
        put_str(&mut b, &self.contact_name, 64);
        put_str(&mut b, &self.contact_email, 64);
        for v in [
            self.active_users,
            self.active_channels,
            self.active_games,
            self.uptime,
            self.total_games,
            self.total_logins,
        ] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        debug_assert_eq!(b.len(), PACKET_SIZE);
        b
    }

    fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_SIZE {
            return None;
        }
        if be_u16(buf, 0) != PACKET_VERSION {
            return None;
        }
        let server_port = be_u16(buf, 2);
        let flags = be_u32(buf, 4);
        let mut o = 8;
        let software = get_str(buf, &mut o, 32);
        let version = get_str(buf, &mut o, 16);
        let platform = get_str(buf, &mut o, 32);
        let server_desc = get_str(buf, &mut o, 64);
        let location = get_str(buf, &mut o, 64);
        let url = get_str(buf, &mut o, 96);
        let contact_name = get_str(buf, &mut o, 64);
        let contact_email = get_str(buf, &mut o, 64);
        let u32_at = |o: &mut usize| {
            let v = be_u32(buf, *o);
            *o += 4;
            v
        };
        Some(Self {
            server_port,
            flags,
            software,
            version,
            platform,
            server_desc,
            location,
            url,
            contact_name,
            contact_email,
            active_users: u32_at(&mut o),
            active_channels: u32_at(&mut o),
            active_games: u32_at(&mut o),
            uptime: u32_at(&mut o),
            total_games: u32_at(&mut o),
            total_logins: u32_at(&mut o),
        })
    }
}

fn be_u16(buf: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([buf[o], buf[o + 1]])
}

fn be_u32(buf: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
}

// ---------------------------------------------------------------------------
// Advertise (outbound beacon)
// ---------------------------------------------------------------------------

/// Beacon this server to the configured trackers on an interval. Returns immediately if no
/// targets are configured.
pub async fn advertise(node: Arc<Node>, bncs_port: u16, cfg: TrackerConfig) {
    if cfg.advertise_to.is_empty() {
        return;
    }
    let sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "tracker: could not open a UDP socket to advertise; disabled");
            return;
        }
    };
    let period = Duration::from_secs(cfg.advertise_interval_secs.max(30));
    info!(targets = ?cfg.advertise_to, interval_secs = period.as_secs(), "tracker: advertising this server");
    let mut tick = tokio::time::interval(period);
    loop {
        tick.tick().await;
        let packet = build_beacon(&node, bncs_port, &cfg).encode();
        for target in &cfg.advertise_to {
            let target = if target.contains(':') { target.clone() } else { format!("{target}:6114") };
            match sock.send_to(&packet, &target).await {
                Ok(_) => {}
                Err(e) => warn!(target = %target, error = %e, "tracker: beacon send failed"),
            }
        }
    }
}

fn build_beacon(node: &Node, bncs_port: u16, cfg: &TrackerConfig) -> TrackPacket {
    let uptime = u32::try_from(node.uptime_secs()).unwrap_or(u32::MAX);
    TrackPacket {
        server_port: bncs_port,
        flags: 0,
        software: "Command Center".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        server_desc: if cfg.description.is_empty() { node.name.clone() } else { cfg.description.clone() },
        location: String::new(),
        url: cfg.url.clone(),
        contact_name: cfg.contact_name.clone(),
        contact_email: cfg.contact_email.clone(),
        active_users: u32::try_from(node.online_count()).unwrap_or(u32::MAX),
        active_channels: u32::try_from(node.channel_names().len()).unwrap_or(u32::MAX),
        active_games: u32::try_from(node.games().len()).unwrap_or(u32::MAX),
        uptime,
        total_games: u32::try_from(node.total_games()).unwrap_or(u32::MAX),
        total_logins: u32::try_from(node.total_logins()).unwrap_or(u32::MAX),
    }
}

// ---------------------------------------------------------------------------
// Host (receive beacons + serve the list)
// ---------------------------------------------------------------------------

/// One tracked server, as last reported.
#[derive(Clone, Serialize)]
struct TrackedServer {
    address: String,
    software: String,
    version: String,
    platform: String,
    description: String,
    url: String,
    users: u32,
    channels: u32,
    games: u32,
    uptime_secs: u32,
    #[serde(skip)]
    last_seen: Instant,
    seconds_ago: u64,
}

type Registry = Arc<Mutex<HashMap<(IpAddr, u16), TrackedServer>>>;

/// Run our own tracker: a UDP receiver plus an HTTP list page. `udp_listen`/`http_listen`
/// come from config; either empty disables that half.
pub async fn host(udp_listen: String, http_listen: String, prune_after_secs: u64) {
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let prune_after = Duration::from_secs(prune_after_secs.max(60));

    if let Ok(addr) = udp_listen.parse::<SocketAddr>() {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { receive_beacons(addr, registry, prune_after).await });
    } else if !udp_listen.is_empty() {
        warn!(listen = %udp_listen, "tracker: invalid host_listen; not receiving beacons");
    }

    if let Ok(addr) = http_listen.parse::<SocketAddr>() {
        serve_list(addr, registry, prune_after).await;
    } else if !http_listen.is_empty() {
        warn!(listen = %http_listen, "tracker: invalid list_listen; not serving the list");
    }
}

async fn receive_beacons(addr: SocketAddr, registry: Registry, prune_after: Duration) {
    let sock = match UdpSocket::bind(addr).await {
        Ok(s) => s,
        Err(e) => {
            warn!(%addr, error = %e, "tracker: could not bind UDP receiver");
            return;
        }
    };
    info!(%addr, "tracker: receiving server beacons");
    let mut buf = [0u8; PACKET_SIZE + 64];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        let Some(pkt) = TrackPacket::decode(&buf[..n]) else { continue };
        let ip = from.ip();
        let entry = TrackedServer {
            address: format!("{ip}:{}", pkt.server_port),
            software: pkt.software,
            version: pkt.version,
            platform: pkt.platform,
            description: pkt.server_desc,
            url: pkt.url,
            users: pkt.active_users,
            channels: pkt.active_channels,
            games: pkt.active_games,
            uptime_secs: pkt.uptime,
            last_seen: Instant::now(),
            seconds_ago: 0,
        };
        let mut reg = registry.lock().expect("tracker registry");
        reg.insert((ip, pkt.server_port), entry);
        reg.retain(|_, s| s.last_seen.elapsed() < prune_after);
    }
}

/// Snapshot the live servers (pruned, newest first) with their `seconds_ago` filled in.
fn snapshot(registry: &Registry, prune_after: Duration) -> Vec<TrackedServer> {
    let mut list: Vec<TrackedServer> = registry
        .lock()
        .expect("tracker registry")
        .values()
        .filter(|s| s.last_seen.elapsed() < prune_after)
        .map(|s| {
            let mut s = s.clone();
            s.seconds_ago = s.last_seen.elapsed().as_secs();
            s
        })
        .collect();
    list.sort_by(|a, b| b.users.cmp(&a.users).then(a.address.cmp(&b.address)));
    list
}

async fn serve_list(addr: SocketAddr, registry: Registry, prune_after: Duration) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!(%addr, error = %e, "tracker: could not bind list HTTP server");
            return;
        }
    };
    info!(%addr, "tracker: serving the public server list over HTTP");
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            let path = read_path(&mut sock).await.unwrap_or_default();
            let servers = snapshot(&registry, prune_after);
            let resp = match path.as_str() {
                "/servers.json" => {
                    let body = serde_json::to_string(&servers).unwrap_or_else(|_| "[]".into());
                    http_response("application/json; charset=utf-8", &body)
                }
                _ => http_response("text/html; charset=utf-8", &list_html(&servers)),
            };
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}

async fn read_path(sock: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(p) = buf.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&buf[..p]);
            return line.split_whitespace().nth(1).map(|p| p.split('?').next().unwrap_or("/").to_string());
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 || buf.len() > 8192 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn http_response(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn list_html(servers: &[TrackedServer]) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let rows = if servers.is_empty() {
        "<tr><td colspan=\"5\" class=\"empty\">No servers listed right now.</td></tr>".to_string()
    } else {
        servers
            .iter()
            .map(|s| {
                format!(
                    "<tr><td><b>{}</b><div class=\"sub\">{} · {} {}</div></td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}s ago</td></tr>",
                    esc(&s.description),
                    esc(&s.address),
                    esc(&s.software),
                    esc(&s.version),
                    esc(&s.url),
                    s.users,
                    s.games,
                    s.seconds_ago,
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Server List</title>
<style>
:root {{ color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; }}
body {{ margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
.wrap {{ max-width:860px; margin:0 auto; padding:28px 20px; }}
h1 {{ font-size:20px; margin:0 0 16px; }}
table {{ width:100%; border-collapse:collapse; background:var(--card); border:1px solid var(--line); border-radius:10px; overflow:hidden; }}
td,th {{ text-align:left; padding:10px 14px; border-bottom:1px solid var(--line); }}
th {{ color:var(--muted); font-weight:500; font-size:12px; text-transform:uppercase; letter-spacing:.04em; }}
tr:last-child td {{ border-bottom:none; }}
.sub {{ color:var(--muted); font-size:12px; }} .num {{ font-variant-numeric:tabular-nums; }}
.empty {{ color:var(--muted); }} .foot {{ color:var(--muted); font-size:12px; margin-top:16px; }}
</style></head><body><div class="wrap">
<h1>Battle.net Server List</h1>
<table>
<tr><th>Server</th><th>URL</th><th>Users</th><th>Games</th><th>Seen</th></tr>
{rows}
</table>
<p class="foot">Servers self-report via the PvPGN tracking protocol (UDP 6114). Machine-readable feed at <code>/servers.json</code>.</p>
<script>setTimeout(function(){{location.reload()}}, 30000);</script>
</div></body></html>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trips_at_the_fixed_size() {
        let p = TrackPacket {
            server_port: 6112,
            flags: 0,
            software: "bnetccd".into(),
            version: "0.2.3".into(),
            platform: "macos".into(),
            server_desc: "Command Center".into(),
            location: "US".into(),
            url: "https://bnet.cc".into(),
            contact_name: "op".into(),
            contact_email: "op@bnet.cc".into(),
            active_users: 12,
            active_channels: 3,
            active_games: 2,
            uptime: 3600,
            total_games: 0,
            total_logins: 0,
        };
        let bytes = p.encode();
        assert_eq!(bytes.len(), PACKET_SIZE, "the tracking packet is a fixed 464 bytes");
        let d = TrackPacket::decode(&bytes).expect("decodes");
        assert_eq!(d.server_port, 6112);
        assert_eq!(d.software, "bnetccd");
        assert_eq!(d.version, "0.2.3");
        assert_eq!(d.server_desc, "Command Center");
        assert_eq!(d.url, "https://bnet.cc");
        assert_eq!(d.active_users, 12);
        assert_eq!(d.uptime, 3600);
    }

    #[test]
    fn a_wrong_version_or_short_packet_is_rejected() {
        assert!(TrackPacket::decode(&[0u8; 10]).is_none(), "too short");
        let mut bytes = vec![0u8; PACKET_SIZE];
        bytes[0] = 0xFF; // packet version 0xFF00, not 2
        assert!(TrackPacket::decode(&bytes).is_none(), "unknown version");
    }
}
