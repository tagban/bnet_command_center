//! Public, read-only status endpoint.
//!
//! Deliberately separate from the admin panel (`crate::status`): that one is HTTPS,
//! password-gated, and localhost-by-default because it can *change* the server. This one is
//! plain HTTP, unauthenticated, and exposes only safe aggregate stats so it can be pointed at
//! the open internet and embedded anywhere (a `Access-Control-Allow-Origin: *` header lets a
//! site like bnet.cc `fetch()` the JSON cross-origin).
//!
//! It serves two routes:
//! * `GET /status.json` — the machine-readable feed to hook into.
//! * `GET /` — a small self-contained HTML page that renders that feed and refreshes.
//! * `GET /ladder.json` — every ladder's standings (`crate::ladder_push`), what a game's own
//!   ladder screen shows anyone.
//!
//! The one privacy lever is the online-users list: included only when the operator turns it
//! on (`[status] public_show_users`). Counts, uptime, channel/game totals and the MOTD are
//! always safe to show; who is online is a per-operator choice, off by default.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::node::Node;

/// Cap on the request bytes we read before parsing the request line — this endpoint only
/// needs the method and path, never a body.
const MAX_REQUEST_BYTES: usize = 8192;

/// The public status feed. Aggregates only, plus an optional online-users list.
#[derive(Serialize)]
struct PublicSnapshot {
    server_name: String,
    motd: String,
    version: &'static str,
    uptime_secs: u64,
    /// Total live connections of every kind (game, gateway, BNFTP, pending).
    connections: u64,
    /// Sessions that are logged in.
    users_online: usize,
    /// Highest live connection count since startup.
    peak_connections: u64,
    /// Number of active channels.
    channels: usize,
    /// Number of advertised games.
    games: usize,
    /// Online display names — present only when the operator enabled `public_show_users`.
    #[serde(skip_serializing_if = "Option::is_none")]
    users: Option<Vec<String>>,
}

/// Serve the public status endpoint until the process ends. A bind failure is logged and
/// non-fatal — the public page is a convenience, never a reason to take the node down.
pub async fn run(listen: SocketAddr, node: Arc<Node>, show_users: bool) {
    let listener = match TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            warn!(addr = %listen, error = %e, "public status page failed to bind; continuing without it");
            return;
        }
    };
    info!(addr = %listen, show_users, "public status page listening over HTTP");
    loop {
        let (mut sock, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            let Some(path) = read_path(&mut sock).await else {
                return;
            };
            let response = match path.as_str() {
                "/status.json" => json_response(&node, show_users),
                "/ladder.json" => {
                    http_response("200 OK", "application/json; charset=utf-8", &crate::ladder_push::snapshot_json(&node).await, true)
                }
                "/" | "/index.html" => http_response("200 OK", "text/html; charset=utf-8", HTML, false),
                _ => http_response("404 Not Found", "text/plain; charset=utf-8", "Not found.", false),
            };
            let _ = sock.write_all(response.as_bytes()).await;
        });
    }
}

/// Read just enough of the request to get the path from the request line.
async fn read_path(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(p) = buf.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&buf[..p]);
            let path = line.split_whitespace().nth(1)?;
            return Some(path.split('?').next().unwrap_or("/").to_string());
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_REQUEST_BYTES {
            return None;
        }
    }
}

/// Build the public status feed as a JSON string. Shared by the public endpoint and the
/// outbound stats push (`crate::stats_push`).
pub(crate) fn snapshot_json(node: &Node, show_users: bool) -> String {
    let snap = PublicSnapshot {
        server_name: node.name.clone(),
        motd: node.motd.clone(),
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: node.uptime_secs(),
        connections: node.connection_count(),
        users_online: node.online_count(),
        peak_connections: node.peak_connections(),
        channels: node.channel_names().len(),
        games: node.games().len(),
        users: show_users.then(|| node.online_names()),
    };
    serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string())
}

fn json_response(node: &Node, show_users: bool) -> String {
    http_response("200 OK", "application/json; charset=utf-8", &snapshot_json(node, show_users), true)
}

/// Build an HTTP/1.1 response. `cors` adds `Access-Control-Allow-Origin: *` so the JSON feed
/// can be fetched from another origin. `Connection: close` keeps this one-request-per-socket.
fn http_response(status: &str, content_type: &str, body: &str, cors: bool) -> String {
    let cors_header = if cors { "Access-Control-Allow-Origin: *\r\n" } else { "" };
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         {cors_header}\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    )
}

/// The self-contained status page: it fetches `/status.json` and re-renders every few
/// seconds. No external assets, so it works behind any reverse proxy or none.
const HTML: &str = r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Server Status</title>
<style>
:root { color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --ok:#5ac47d; }
* { box-sizing:border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }
.wrap { max-width:760px; margin:0 auto; padding:32px 20px; }
h1 { font-size:22px; margin:0 0 2px; }
h1 .dot { display:inline-block; width:10px; height:10px; border-radius:50%; background:var(--muted); margin-right:8px; vertical-align:middle; }
h1 .dot.up { background:var(--ok); }
.motd { color:var(--muted); margin:0 0 24px; }
.tiles { display:grid; grid-template-columns:repeat(auto-fit,minmax(150px,1fr)); gap:14px; }
.tile { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:16px 18px; }
.tile .n { font-size:30px; font-weight:700; font-variant-numeric:tabular-nums; }
.tile .l { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.04em; }
.users { margin-top:24px; background:var(--card); border:1px solid var(--line); border-radius:12px; padding:16px 18px; }
.users h2 { font-size:13px; color:var(--muted); text-transform:uppercase; letter-spacing:.04em; margin:0 0 10px; }
.users .list { display:flex; flex-wrap:wrap; gap:6px; }
.users .u { background:rgba(90,169,230,.12); border:1px solid var(--line); border-radius:6px; padding:2px 8px; font-size:13px; }
.foot { color:var(--muted); font-size:12px; margin-top:20px; }
.err { color:#e06a6a; }
</style></head><body>
<div class="wrap">
  <h1><span class="dot" id="dot"></span><span id="name">Server Status</span></h1>
  <p class="motd" id="motd"></p>
  <div class="tiles">
    <div class="tile"><div class="n" id="users">–</div><div class="l">Users online</div></div>
    <div class="tile"><div class="n" id="conns">–</div><div class="l">Connections</div></div>
    <div class="tile"><div class="n" id="channels">–</div><div class="l">Channels</div></div>
    <div class="tile"><div class="n" id="games">–</div><div class="l">Games</div></div>
    <div class="tile"><div class="n" id="uptime">–</div><div class="l">Uptime</div></div>
    <div class="tile"><div class="n" id="peak">–</div><div class="l">Peak</div></div>
  </div>
  <div class="users" id="userbox" hidden>
    <h2>Online now</h2>
    <div class="list" id="userlist"></div>
  </div>
  <p class="foot" id="foot">Loading…</p>
</div>
<script>
function fmtUptime(s){
  const d=Math.floor(s/86400), h=Math.floor(s%86400/3600), m=Math.floor(s%3600/60);
  if(d>0) return d+"d "+h+"h"; if(h>0) return h+"h "+m+"m"; return m+"m";
}
async function tick(){
  try{
    const r=await fetch("status.json",{cache:"no-store"});
    const j=await r.json();
    document.getElementById("name").textContent=j.server_name||"Server Status";
    document.getElementById("motd").textContent=j.motd||"";
    document.getElementById("dot").classList.add("up");
    document.getElementById("users").textContent=j.users_online;
    document.getElementById("conns").textContent=j.connections;
    document.getElementById("channels").textContent=j.channels;
    document.getElementById("games").textContent=j.games;
    document.getElementById("uptime").textContent=fmtUptime(j.uptime_secs);
    document.getElementById("peak").textContent=j.peak_connections;
    const box=document.getElementById("userbox");
    if(Array.isArray(j.users)){
      box.hidden=false;
      const list=document.getElementById("userlist");
      list.innerHTML="";
      if(j.users.length===0){ list.textContent="Nobody online."; }
      else for(const u of j.users){ const s=document.createElement("span"); s.className="u"; s.textContent=u; list.appendChild(s); }
    } else { box.hidden=true; }
    document.getElementById("foot").textContent="Updated "+new Date().toLocaleTimeString();
  }catch(e){
    document.getElementById("dot").classList.remove("up");
    document.getElementById("foot").innerHTML='<span class="err">Server unreachable.</span>';
  }
}
tick(); setInterval(tick, 5000);
</script>
</body></html>"##;
