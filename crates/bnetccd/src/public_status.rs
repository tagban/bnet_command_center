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
//!
//! Beyond the totals the feed carries what a website needs for a server page: players online
//! per game, players seen in the last day and games hosted per game, the public channels (ones
//! the operator defined as public or listed; the rest only counted), open games with their game
//! type and map (a password-protected game's name is withheld), Diablo II realm games, and the
//! latest ladder results. Host addresses are never included.

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
    /// When this was built, seconds since the Unix epoch.
    generated: u64,
    /// When the server started, seconds since the Unix epoch.
    started: u64,
    /// Accounts that logged in within the last 24 hours.
    players_24h: usize,
    /// Per game: players online, open games, games hosted in the last 24 hours.
    products: Vec<ProductActivity>,
    /// Who is online and on which game and channel — only with `public_show_users`.
    #[serde(skip_serializing_if = "Option::is_none")]
    user_list: Option<Vec<OnlineUser>>,
    /// Public channels, busiest first.
    channel_list: Vec<PublicChannel>,
    /// The other channels, counted.
    other_channels: ChannelCount,
    /// Open games, newest first.
    game_list: Vec<PublicGame>,
    /// Diablo II realm games, newest first.
    diablo2_games: Vec<Diablo2Game>,
    /// The latest ladder results, newest first.
    recent_ladder: Vec<crate::node::LadderResult>,
}

#[derive(Serialize)]
struct ProductActivity {
    product: String,
    name: &'static str,
    online: usize,
    games_open: usize,
    games_hosted_24h: usize,
}

#[derive(Serialize)]
struct OnlineUser {
    name: String,
    product: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
}

#[derive(Serialize)]
struct PublicChannel {
    name: String,
    users: usize,
}

#[derive(Serialize)]
struct ChannelCount {
    channels: usize,
    users: usize,
}

#[derive(Serialize)]
struct PublicGame {
    /// `None` for a game with a password.
    name: Option<String>,
    product: String,
    kind: &'static str,
    map: Option<String>,
    private: bool,
    minutes: u64,
}

#[derive(Serialize)]
struct Diablo2Game {
    /// `None` for a game with a password.
    name: Option<String>,
    difficulty: &'static str,
    players: usize,
    minutes: u64,
}

/// A product code's name, for the feed.
fn product_name(code: &str) -> &'static str {
    match code {
        "STAR" => "StarCraft",
        "SEXP" => "StarCraft: Brood War",
        "JSTR" => "StarCraft (Japanese)",
        "SSHR" => "StarCraft Shareware",
        "W2BN" => "Warcraft II",
        "DRTL" => "Diablo",
        "DSHR" => "Diablo Shareware",
        "D2DV" => "Diablo II",
        "D2XP" => "Diablo II: Lord of Destruction",
        "WAR3" => "WarCraft III",
        "W3XP" => "WarCraft III: The Frozen Throne",
        "CHAT" => "Chat",
        _ => "Other",
    }
}

/// A StarCraft or Warcraft II game type (BNETDocs' game type list); other games are custom games.
fn game_kind(product: &str, game_type: u16) -> &'static str {
    if !matches!(product, "STAR" | "SEXP" | "JSTR" | "SSHR" | "W2BN") {
        return "Custom";
    }
    match game_type {
        0x02 => "Melee",
        0x03 => "Free for All",
        0x04 => "One on One",
        0x05 => "Capture the Flag",
        0x06 => "Greed",
        0x07 => "Slaughter",
        0x08 => "Sudden Death",
        0x09 => "Ladder",
        0x0A => "Use Map Settings",
        0x0B => "Team Melee",
        0x0C => "Team Free for All",
        0x0D => "Team Capture the Flag",
        0x0F => "Top vs. Bottom",
        0x10 => "Iron Man Ladder",
        _ => "Custom",
    }
}

/// The map a StarCraft or Warcraft II game statstring names: the text after its first carriage
/// return (`…,host\rmap name\r`). WarCraft III's statstring is encoded, so it has none here.
fn statstring_map(product: &str, statstring: &[u8]) -> Option<String> {
    if !matches!(product, "STAR" | "SEXP" | "JSTR" | "SSHR" | "W2BN") {
        return None;
    }
    let text = String::from_utf8_lossy(statstring);
    let map: String = text.split('\r').nth(1)?.chars().filter(|c| !c.is_control()).take(48).collect();
    let map = map.trim().to_string();
    (!map.is_empty()).then_some(map)
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
        generated: crate::now_ms() / 1000,
        started: node.started_unix(),
        players_24h: node.players_seen_since(std::time::Duration::from_secs(24 * 3600)),
        products: Vec::new(),
        user_list: None,
        channel_list: Vec::new(),
        other_channels: ChannelCount { channels: 0, users: 0 },
        game_list: Vec::new(),
        diablo2_games: Vec::new(),
        recent_ladder: node.recent_ladder_results(),
    };
    let mut snap = snap;
    fill_activity(node, show_users, &mut snap);
    serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string())
}

/// The per-game, channel and game sections of the feed.
fn fill_activity(node: &Node, show_users: bool, snap: &mut PublicSnapshot) {
    use std::collections::BTreeMap;
    let code = |p: Option<bnetcc_proto::FourCc>| p.map_or_else(|| "CHAT".to_string(), |p| p.to_string());
    let sessions = node.online_sessions();
    let occupancy = node.channel_occupancy();
    let games = node.games();
    let mut products: BTreeMap<String, ProductActivity> = BTreeMap::new();
    fn entry<'a>(products: &'a mut BTreeMap<String, ProductActivity>, p: &str) -> &'a mut ProductActivity {
        products.entry(p.to_string()).or_insert_with(|| ProductActivity { product: p.to_string(), name: product_name(p), online: 0, games_open: 0, games_hosted_24h: 0 })
    }
    for (_, product) in &sessions {
        entry(&mut products, &code(*product)).online += 1;
    }
    for game in &games {
        entry(&mut products, &code(game.product)).games_open += 1;
    }
    for (product, count) in node.games_hosted_since(std::time::Duration::from_secs(24 * 3600)) {
        entry(&mut products, &product).games_hosted_24h = count;
    }
    let mut products: Vec<ProductActivity> = products.into_values().collect();
    products.sort_by(|a, b| b.online.cmp(&a.online).then(b.games_hosted_24h.cmp(&a.games_hosted_24h)).then(a.product.cmp(&b.product)));
    snap.products = products;

    if show_users {
        let channel_of: std::collections::HashMap<String, (String, bool)> = occupancy
            .iter()
            .flat_map(|(name, members, public)| members.iter().map(move |m| (m.to_ascii_lowercase(), (name.clone(), *public))))
            .collect();
        snap.user_list = Some(
            sessions
                .iter()
                .map(|(name, product)| OnlineUser {
                    name: name.clone(),
                    product: code(*product),
                    channel: channel_of.get(&name.to_ascii_lowercase()).filter(|(_, public)| *public).map(|(c, _)| c.clone()),
                })
                .collect(),
        );
    }

    let mut public: Vec<PublicChannel> = Vec::new();
    for (name, members, is_public) in occupancy {
        if is_public {
            public.push(PublicChannel { name, users: members.len() });
        } else if !members.is_empty() {
            snap.other_channels.channels += 1;
            snap.other_channels.users += members.len();
        }
    }
    public.sort_by(|a, b| b.users.cmp(&a.users).then(a.name.cmp(&b.name)));
    snap.channel_list = public;

    let now = std::time::Instant::now();
    let mut list: Vec<(u64, PublicGame)> = games
        .iter()
        .map(|g| {
            let product = code(g.product);
            let private = !g.password.is_empty();
            let age = now.saturating_duration_since(g.created).as_secs();
            let game = PublicGame {
                name: (!private).then(|| String::from_utf8_lossy(&g.name).into_owned()),
                kind: game_kind(&product, g.game_type),
                map: statstring_map(&product, &g.statstring),
                product,
                private,
                minutes: age / 60,
            };
            (age, game)
        })
        .collect();
    list.sort_by_key(|(age, _)| *age);
    snap.game_list = list.into_iter().map(|(_, g)| g).collect();

    if let Some(server) = node.d2_realm.as_ref().and_then(|r| r.game_server.as_ref()) {
        snap.diablo2_games = server
            .public_games()
            .into_iter()
            .map(|(name, private, difficulty, players, age)| Diablo2Game {
                name: (!private).then_some(name),
                difficulty: ["Normal", "Nightmare", "Hell"].get(usize::from(difficulty)).copied().unwrap_or("Normal"),
                players,
                minutes: age / 60,
            })
            .collect();
    }
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
.about { color:var(--muted); font-size:12px; margin-top:28px; border-top:1px solid var(--line); padding-top:14px; }
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
  <p class="about">Command Center is an educational server that keeps classic Battle.net games playable on
  older computers that can no longer connect. On a modern computer, buy <i>Diablo II: Resurrected</i>,
  <i>Warcraft III: Reforged</i>, <i>StarCraft: Remastered</i> and <i>Warcraft II: Remastered</i> &mdash;
  they are worth it. Not affiliated with or endorsed by Blizzard Entertainment; Battle.net, Diablo,
  StarCraft and Warcraft are trademarks or registered trademarks of Blizzard Entertainment, Inc.</p>
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
