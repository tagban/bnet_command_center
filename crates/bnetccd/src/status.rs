//! Optional read-only status UI.
//!
//! A tiny HTTP server (no framework — a hand-rolled GET responder is plenty for a
//! localhost status page) that serves a live snapshot of the node: connection count,
//! who is in which channel, and what games are advertised. Off unless `[status] listen`
//! is set, and intended for `127.0.0.1` only — it exposes operational detail and has no
//! authentication, so never bind it to a public address.
//!
//! `GET /`            → the dashboard (auto-refreshes by polling the JSON below).
//! `GET /status.json` → the snapshot as JSON.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::node::Node;

/// A point-in-time view of the node, serialised to `/status.json`.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// Server display name.
    pub server_name: String,
    /// Seconds since the node started.
    pub uptime_secs: u64,
    /// Total live connections of every kind (game, gateway, BNFTP, pending).
    pub connections: u64,
    /// Users currently sitting in a channel (a subset of `connections`).
    pub online_users: usize,
    /// Active channels, sorted by name.
    pub channels: Vec<ChannelInfo>,
    /// Advertised games, sorted by name.
    pub games: Vec<GameInfo>,
}

/// One active channel and its occupants.
#[derive(Debug, Serialize)]
pub struct ChannelInfo {
    pub name: String,
    pub user_count: usize,
    pub users: Vec<String>,
}

/// One advertised game.
#[derive(Debug, Serialize)]
pub struct GameInfo {
    pub name: String,
    pub host_ip: String,
    pub port: u16,
    pub game_type: u16,
    pub elapsed_secs: u64,
}

/// Serve the status UI until the process ends. Binding failures are logged and non-fatal:
/// the status UI is a convenience, never a reason to take the node down.
pub async fn run(listen: SocketAddr, node: Arc<Node>) {
    let listener = match TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            warn!(addr = %listen, error = %e, "status UI failed to bind; continuing without it");
            return;
        }
    };
    info!(addr = %listen, "status UI listening (read-only; keep it on localhost)");
    loop {
        let (sock, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            handle(sock, &node).await;
        });
    }
}

async fn handle(mut sock: tokio::net::TcpStream, node: &Node) {
    // A status request is tiny; one read of the head is enough to see the request line.
    let mut buf = [0u8; 2048];
    let n = match sock.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let (status, content_type, body) = match path {
        "/status.json" => (
            "200 OK",
            "application/json",
            serde_json::to_string(&node.status_snapshot()).unwrap_or_else(|_| "{}".to_string()),
        ),
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", DASHBOARD.to_string()),
        _ => ("404 Not Found", "text/plain; charset=utf-8", "not found".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(response.as_bytes()).await;
}

/// The dashboard page. Self-contained (no external assets): it polls `/status.json` every
/// two seconds and re-renders. Served same-origin, so the fetch has no CORS concerns.
const DASHBOARD: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>BNET Command Center — Status</title>
<style>
  :root { color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; }
  * { box-sizing: border-box; }
  body { margin:0; font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; background:var(--bg); color:var(--fg); }
  header { padding:16px 20px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:12px; flex-wrap:wrap; }
  header h1 { font-size:16px; margin:0; font-weight:600; }
  header .name { color:var(--accent); }
  header .meta { color:var(--muted); font-size:12px; }
  .stale { color:#e6a15a; }
  main { padding:20px; display:grid; gap:20px; grid-template-columns:repeat(auto-fit,minmax(320px,1fr)); align-items:start; }
  .tiles { display:flex; gap:14px; flex-wrap:wrap; grid-column:1/-1; }
  .tile { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:14px 18px; min-width:130px; }
  .tile .n { font-size:28px; font-weight:700; }
  .tile .l { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.04em; }
  section { background:var(--card); border:1px solid var(--line); border-radius:10px; overflow:hidden; }
  section h2 { margin:0; padding:12px 16px; font-size:13px; border-bottom:1px solid var(--line); display:flex; justify-content:space-between; }
  section h2 .c { color:var(--muted); font-weight:400; }
  table { width:100%; border-collapse:collapse; }
  td,th { text-align:left; padding:8px 16px; border-bottom:1px solid var(--line); vertical-align:top; }
  th { color:var(--muted); font-weight:500; font-size:12px; }
  tr:last-child td { border-bottom:none; }
  .users { color:var(--muted); font-size:12px; }
  .empty { padding:16px; color:var(--muted); }
  .num { font-variant-numeric:tabular-nums; }
</style>
</head>
<body>
<header>
  <h1>BNET Command Center · <span class="name" id="server">…</span></h1>
  <span class="meta" id="meta"></span>
</header>
<main>
  <div class="tiles">
    <div class="tile"><div class="n num" id="t-conns">–</div><div class="l">Connections</div></div>
    <div class="tile"><div class="n num" id="t-users">–</div><div class="l">In channels</div></div>
    <div class="tile"><div class="n num" id="t-channels">–</div><div class="l">Channels</div></div>
    <div class="tile"><div class="n num" id="t-games">–</div><div class="l">Games</div></div>
  </div>
  <section>
    <h2>Channels <span class="c" id="ch-count"></span></h2>
    <div id="channels"><div class="empty">Loading…</div></div>
  </section>
  <section>
    <h2>Games <span class="c" id="gm-count"></span></h2>
    <div id="games"><div class="empty">Loading…</div></div>
  </section>
</main>
<script>
const $ = id => document.getElementById(id);
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
const fmtDur = s => { s=Math.floor(s); const h=Math.floor(s/3600), m=Math.floor(s%3600/60), x=s%60;
  return (h?h+'h ':'')+(h||m?m+'m ':'')+x+'s'; };

async function tick() {
  try {
    const r = await fetch('status.json', {cache:'no-store'});
    const d = await r.json();
    $('server').textContent = d.server_name || 'node';
    $('meta').textContent = 'uptime ' + fmtDur(d.uptime_secs) + ' · updated ' + new Date().toLocaleTimeString();
    $('meta').classList.remove('stale');
    $('t-conns').textContent = d.connections;
    $('t-users').textContent = d.online_users;
    $('t-channels').textContent = d.channels.length;
    $('t-games').textContent = d.games.length;
    $('ch-count').textContent = d.channels.length + ' active';
    $('gm-count').textContent = d.games.length + ' advertised';

    $('channels').innerHTML = d.channels.length ? (
      '<table><thead><tr><th>Channel</th><th>Users</th><th>Names</th></tr></thead><tbody>' +
      d.channels.map(c => '<tr><td>'+esc(c.name)+'</td><td class="num">'+c.user_count+
        '</td><td class="users">'+esc((c.users||[]).join(', '))+'</td></tr>').join('') +
      '</tbody></table>'
    ) : '<div class="empty">No active channels.</div>';

    $('games').innerHTML = d.games.length ? (
      '<table><thead><tr><th>Game</th><th>Host</th><th>Type</th><th>Age</th></tr></thead><tbody>' +
      d.games.map(g => '<tr><td>'+esc(g.name)+'</td><td class="users">'+esc(g.host_ip)+':'+g.port+
        '</td><td class="num">'+g.game_type+'</td><td class="num">'+fmtDur(g.elapsed_secs)+'</td></tr>').join('') +
      '</tbody></table>'
    ) : '<div class="empty">No games advertised.</div>';
  } catch (e) {
    $('meta').textContent = 'server unreachable — retrying…';
    $('meta').classList.add('stale');
  }
}
tick();
setInterval(tick, 2000);
</script>
</body>
</html>
"##;
