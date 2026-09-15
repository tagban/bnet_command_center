//! Optional status + admin panel, served over **HTTPS** with password auth.
//!
//! A small hand-rolled HTTP server (no framework) behind TLS. It serves the live status
//! dashboard and a settings page, gated by a session cookie. The password and self-signed
//! cert are managed by [`crate::admin`]: a random password is generated and shown once on
//! first run, and must be changed on first sign-in.
//!
//! **Safety model, in layers:** (1) the OS bind address (`[status] listen`) — keep it on
//! `127.0.0.1` unless you deliberately want remote; (2) the `remote_enabled` toggle, which
//! rejects every non-loopback request until an admin turns it on from localhost; (3) the
//! login session. Each connection handles exactly one request then closes (no keep-alive),
//! which sidesteps request-smuggling classes of bug in the hand-rolled parser.
//!
//! Routes: `GET /` dashboard · `GET /status.json` data · `GET|POST /login` ·
//! `GET|POST /change-password` (forced while `must_change`) · `GET|POST /settings` ·
//! `POST /logout` · `GET /d2` live Diablo II map, with `GET /d2/games.json`, `/d2/level.json` and
//! `/d2/live.json` behind it.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

use bnetcc_proto::chat::user_flags;

use crate::admin::Admin;
use crate::config::Config;
use crate::node::Node;
use crate::storage::UserSummary;

/// Session lifetime before re-login is required.
const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// Failed-login lockout: this many failures within the window blocks further attempts.
const MAX_LOGIN_FAILS: u32 = 5;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
/// Cap on the request bytes we will buffer, so a hostile client cannot exhaust memory.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Server-side session store: opaque token → expiry instant.
type Sessions = Arc<Mutex<HashMap<String, Instant>>>;
/// Per-IP failed-login tracker: ip → (fail count, window start).
type RateLimiter = Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>>;

/// The panel's shared, per-process state, bundled so request handling passes one handle
/// rather than a growing list of arguments. Every field is cheap to clone (an `Arc`).
#[derive(Clone)]
struct Panel {
    node: Arc<Node>,
    admin: Arc<Admin>,
    sessions: Sessions,
    rate: RateLimiter,
    restart: Arc<Notify>,
    config_path: Arc<PathBuf>,
}

/// A point-in-time view of the node, serialised to `/status.json`.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// Server display name.
    pub server_name: String,
    /// Daemon version (`CARGO_PKG_VERSION`).
    pub version: String,
    /// Seconds since the node started.
    pub uptime_secs: u64,
    /// Total live connections of every kind (game, gateway, BNFTP, pending).
    pub connections: u64,
    /// Highest live connection count seen since startup.
    pub peak_connections: u64,
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
    /// Display name of the channel operator, if any.
    pub operator: Option<String>,
}

/// One advertised game.
#[derive(Debug, Serialize)]
pub struct GameInfo {
    pub name: String,
    pub host_ip: String,
    pub port: u16,
    pub game_type: u16,
    /// Whether the game requires a join password.
    pub has_password: bool,
    pub elapsed_secs: u64,
}

/// Serve the status UI until the process ends. Binding failures are logged and non-fatal:
/// the status UI is a convenience, never a reason to take the node down.
/// Serve the admin panel over HTTPS until the process ends. Binding or TLS-setup failures
/// are logged and non-fatal — the panel is a convenience, never a reason to take the node
/// down.
pub async fn run(
    listen: SocketAddr,
    node: Arc<Node>,
    admin: Arc<Admin>,
    restart: Arc<Notify>,
    config_path: PathBuf,
) {
    let config_path = Arc::new(config_path);
    // rustls 0.23 needs a process crypto provider; install one if the app hasn't.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    let tls = match build_tls(&admin) {
        Ok(cfg) => TlsAcceptor::from(Arc::new(cfg)),
        Err(e) => {
            warn!(error = %e, "admin panel: TLS config failed; not serving");
            return;
        }
    };
    let listener = match TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            warn!(addr = %listen, error = %e, "admin panel failed to bind; continuing without it");
            return;
        }
    };
    let remote = if listen.ip().is_loopback() { "localhost-only" } else { "bind is non-loopback; remote gated by the toggle" };
    info!(addr = %listen, %remote, "admin panel listening over HTTPS");
    let panel = Panel {
        node,
        admin,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        rate: Arc::new(Mutex::new(HashMap::new())),
        restart,
        config_path,
    };
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let (tls, panel) = (tls.clone(), panel.clone());
        tokio::spawn(async move {
            let peer_ip = peer.ip();
            // A failed TLS handshake (e.g. someone speaking plain HTTP to the port) just
            // drops — no plaintext is ever served.
            if let Ok(stream) = tls.accept(sock).await {
                handle_conn(stream, peer_ip, &panel).await;
            }
        });
    }
}

/// Build the rustls server config from the admin's PEM cert + key.
fn build_tls(admin: &Admin) -> Result<ServerConfig, String> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut admin.cert_pem().as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut admin.key_pem().as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no private key found in key.pem".to_string())?;
    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())
}

/// One parsed HTTP request (we handle exactly one per connection).
struct Request {
    method: String,
    path: String,
    /// Query-string parameters (e.g. `?offset=100`), decoded.
    query: HashMap<String, String>,
    cookies: HashMap<String, String>,
    form: HashMap<String, String>,
}

/// A response to write back.
struct Response {
    status: &'static str,
    content_type: &'static str,
    body: String,
    set_cookie: Option<String>,
    location: Option<String>,
}

async fn handle_conn<S>(mut stream: S, peer_ip: IpAddr, panel: &Panel)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let Some(req) = read_request(&mut stream).await else {
        return;
    };
    // Remote gate: a non-loopback peer is refused entirely until an admin enables remote
    // access from localhost. This sits in front of auth, so a remote attacker can't even
    // reach the login form while remote is off.
    if !peer_ip.is_loopback() && !panel.admin.remote_enabled() {
        let resp = text("403 Forbidden", "Remote access is disabled on this server.");
        let _ = write_response(&mut stream, &resp).await;
        return;
    }
    let resp = route(&req, peer_ip, panel).await;
    let _ = write_response(&mut stream, &resp).await;
}

/// Route a request to a response. Sync: session/rate stores are plain mutexes and the
/// snapshot is sync, so nothing here awaits.
async fn route(req: &Request, peer_ip: IpAddr, panel: &Panel) -> Response {
    // Rebind the panel's fields as the borrows the handlers already expect. Deref coercion
    // turns `&Arc<T>` into `&T`, so the handler signatures below are unchanged.
    let node: &Node = &panel.node;
    let admin: &Admin = &panel.admin;
    let sessions: &Sessions = &panel.sessions;
    let rate: &RateLimiter = &panel.rate;
    let restart: &Arc<Notify> = &panel.restart;
    let config_path: &Path = &panel.config_path;
    // Public routes (no session required).
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/login") => return html_page(login_page_with("")),
        ("POST", "/login") => return do_login(req, peer_ip, admin, sessions, rate),
        ("POST", "/logout") => return do_logout(req, sessions),
        _ => {}
    }
    // Everything else needs a valid session.
    if !is_authed(req, sessions) {
        return redirect("/login");
    }
    // While the first-run password stands, force the change before anything else.
    if admin.must_change() {
        return match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/change-password") => html_page(change_pw_page(true)),
            ("POST", "/change-password") => do_change_pw(req, admin),
            _ => redirect("/change-password"),
        };
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => html_page(DASHBOARD.to_string()),
        ("GET", "/status.json") => json_response(node),
        ("GET", "/change-password") => html_page(change_pw_page(false)),
        ("POST", "/change-password") => do_change_pw(req, admin),
        ("GET", "/settings") => html_page(settings_page(admin, config_path, None)),
        ("POST", "/settings") => do_settings(req, admin),
        ("POST", "/settings/config") => do_settings_config(req, admin, config_path),
        ("POST", "/restart") => do_restart(restart),
        ("GET", "/help") => html_page(HELP.to_string()),
        ("GET", "/users") => users_page(node, req, None).await,
        ("POST", "/users/flags") => do_user_flags(req, node).await,
        ("POST", "/users/reset") => do_user_reset(req, node).await,
        ("POST", "/users/delete") => do_user_delete(req, node).await,
        ("GET", "/d2") => html_page(D2_MAP.to_string()),
        ("GET", "/d2/season") => season_page(node, None).await,
        ("POST", "/d2/season/end") => do_end_season(node).await,
        ("GET", "/d2/games.json") => d2_json(node, req, D2Query::Games),
        ("GET", "/d2/level.json") => d2_json(node, req, D2Query::Level),
        ("GET", "/d2/live.json") => d2_json(node, req, D2Query::Live),
        _ => text("404 Not Found", "Not found."),
    }
}

/// `POST /restart` — signal the daemon to exit with the restart code. Whether it actually
/// comes back depends on being run under the launcher-supervisor; a standalone daemon just
/// stops. The reply is a self-refreshing page that polls until the panel answers again.
fn do_restart(restart: &Arc<Notify>) -> Response {
    // The main task is selecting on this; it exits the process with RESTART_EXIT_CODE, and
    // the launcher (if supervising) relaunches. notify_one is enough — one waiter.
    restart.notify_one();
    html_page(shell("Restarting — Command Center", RESTARTING_PAGE))
}

/// The Help & Reference page: interfaces/ports, configuration options, and the planned
/// Discord integration. Static reference content, so it is a self-contained constant.
const HELP: &str = r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Help — Command Center</title>
<style>
:root { color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --ok:#5ac47d; --warn:#e6a15a; }
* { box-sizing:border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:14px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }
header { padding:16px 20px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:12px; }
header h1 { font-size:16px; margin:0; }
header nav { margin-left:auto; } header nav a { color:var(--accent); text-decoration:none; font-size:13px; }
main { max-width:820px; margin:0 auto; padding:24px 20px 48px; }
h2 { font-size:15px; margin:28px 0 8px; border-bottom:1px solid var(--line); padding-bottom:6px; }
h2:first-of-type { margin-top:8px; }
table { width:100%; border-collapse:collapse; margin:8px 0; background:var(--card); border:1px solid var(--line); border-radius:8px; overflow:hidden; }
td,th { text-align:left; padding:8px 12px; border-bottom:1px solid var(--line); vertical-align:top; }
th { color:var(--muted); font-weight:500; font-size:12px; }
tr:last-child td { border-bottom:none; }
code { background:#0f1115; border:1px solid var(--line); border-radius:4px; padding:1px 5px; font-size:12px; }
.muted { color:var(--muted); }
.pill { display:inline-block; font-size:11px; padding:1px 7px; border-radius:10px; border:1px solid var(--warn); color:var(--warn); margin-left:8px; }
.note { background:rgba(90,169,230,.08); border:1px solid var(--line); border-radius:8px; padding:10px 14px; margin:10px 0; }
ul { margin:6px 0; padding-left:20px; }
</style></head><body>
<header><h1>Help &amp; Reference</h1><nav><a href="/">Dashboard</a> · <a href="/users">Users</a> · <a href="/settings">Settings</a></nav></header>
<main>

<h2>Interfaces &amp; ports</h2>
<p class="muted">Ports are configurable in <code>bnetccd.toml</code>; the values below are the defaults this server ships with.</p>
<table>
<tr><th>Interface</th><th>Address</th><th>What it is</th></tr>
<tr><td><b>Game &amp; chat (BNCS)</b></td><td>TCP + UDP <code>0.0.0.0:6112</code></td><td>Where Battle.net clients and chat-gateway bots connect. The UDP side is the login-time game check. This is the port players point their client at.</td></tr>
<tr><td><b>Admin panel</b></td><td>HTTPS <code>127.0.0.1:6114</code></td><td>This site. Password-gated; it can change the server (settings, users, restart). Loopback-only unless you enable remote access on the Settings page, because it is privileged — forward it only behind that toggle.</td></tr>
<tr><td><b>Public status</b></td><td>HTTP <code>0.0.0.0:6116</code></td><td>Read-only, no login, safe to expose. Forward this port to publish server stats. See below.</td></tr>
</table>

<h2>Public status endpoint</h2>
<p>Two routes on the public port, for anyone (no login):</p>
<ul>
<li><code>/status.json</code> — a JSON feed (server name, MOTD, uptime, connections, users online, peak, channel/game counts). It sends <code>Access-Control-Allow-Origin: *</code>, so a site like bnet.cc can <code>fetch()</code> it directly and render your own widget.</li>
<li><code>/</code> — a ready-made status page that renders the feed and refreshes every few seconds.</li>
</ul>
<p class="muted">Set <code>[status] public_show_users = true</code> to also list who is online; it is off by default (counts only).</p>

<h2>Push stats to your own site</h2>
<p>Instead of your site pulling the feed, the server can <b>POST</b> the same JSON to a URL you configure, on an interval. This is outbound-only, so a server behind a home router needs no extra forwarded port for it.</p>
<table>
<tr><th>Config key</th><th>Effect</th></tr>
<tr><td><code>[stats_push] url</code></td><td>Your site's ingest endpoint (https). Empty = disabled.</td></tr>
<tr><td><code>interval_secs</code></td><td>Seconds between pushes (minimum 5; default 60).</td></tr>
<tr><td><code>token</code></td><td>Optional; sent as <code>Authorization: Bearer &lt;token&gt;</code> so your endpoint can authenticate the POST.</td></tr>
<tr><td><code>include_users</code></td><td>Whether the pushed JSON includes the online-usernames list.</td></tr>
<tr><td><code>[ladder_push] url</code></td><td>Your site's ladder ingest endpoint (https), e.g. <code>ladder-push.php</code> from <code>web/bnet.cc/</code>. Empty = disabled. The standings are also at the public endpoint's <code>/ladder.json</code>.</td></tr>
<tr><td><code>token</code></td><td>Bearer token for the ladder POST; empty uses <code>[stats_push] token</code>.</td></tr>
<tr><td><code>interval_secs</code></td><td>Seconds between ladder pushes when nothing changes (minimum 60; default 300). A ladder game or a season's end sends one within a minute.</td></tr>
</table>
<p class="muted">The body is identical to <code>/status.json</code>. Best-effort, on its own task — a slow or failing endpoint never affects the server.</p>

<h2>Server tracking (PvPGN lists)</h2>
<p>Optional, config-file only. <b>Advertise</b> this server to public PvPGN trackers so it appears on their lists (a UDP beacon on port 6114), and/or <b>host your own list</b>: receive other servers' beacons and publish them.</p>
<table>
<tr><th>Config key</th><th>Effect</th></tr>
<tr><td><code>[tracker] advertise_to</code></td><td>Trackers to beacon to (e.g. <code>["tracker.pvpgn.org"]</code>). Empty = off.</td></tr>
<tr><td><code>description / url / contact_*</code></td><td>What our beacon reports (description defaults to the server name).</td></tr>
<tr><td><code>host_listen</code></td><td>UDP address to receive other servers' beacons, e.g. <code>0.0.0.0:6114</code>. Empty = off.</td></tr>
<tr><td><code>list_listen</code></td><td>HTTP address for the public list page + <code>/servers.json</code>, e.g. <code>0.0.0.0:8080</code> (port 80 needs root; forward 80→8080).</td></tr>
</table>

<h2>Configuration (<code>bnetccd.toml</code>)</h2>
<p class="muted">Edit on the <a href="/settings">Settings</a> page (which writes the file and keeps a <code>.bak</code>) or by hand. Most changes apply on the next restart; the Restart button is on Settings.</p>
<table>
<tr><th>Section / key</th><th>Meaning</th></tr>
<tr><td><code>[server] name / motd</code></td><td>Server display name and message of the day.</td></tr>
<tr><td><code>[server] mode</code></td><td><code>gaming</code>, <code>warnet</code> (chat/bots only), or <code>both</code>.</td></tr>
<tr><td><code>[listen] bncs</code></td><td>The game/chat bind address (port 6112).</td></tr>
<tr><td><code>[listen] accept_shards</code></td><td>Parallel accept loops for very high connection rates (Unix only).</td></tr>
<tr><td><code>[status] listen</code></td><td>Admin panel address (empty = disabled).</td></tr>
<tr><td><code>[status] public_listen</code></td><td>Public status address (empty = disabled).</td></tr>
<tr><td><code>[status] public_show_users</code></td><td>Whether the public feed lists online usernames.</td></tr>
<tr><td><code>[channels] private_max / public_max / clan_max</code></td><td>Per-category user caps (<code>0</code> = unlimited).</td></tr>
<tr><td><code>[channels] auto_op_private</code></td><td>Whether the first arrival to a private channel becomes operator.</td></tr>
<tr><td><code>[admins] accounts</code></td><td>Staff accounts — they get the Blizzard-rep icon and the <code>/tagban</code>, <code>/ipban</code>, <code>/mute</code>, <code>/kick</code>, <code>/ban</code> commands. Can also be granted per-account on the <a href="/users">Users</a> page.</td></tr>
<tr><td><code>[limits] max_connections</code></td><td>Global connection ceiling (<code>0</code> = derive from the file-descriptor limit).</td></tr>
<tr><td><code>[limits.clients.gateway] per_ip</code></td><td>Telnet/chat-gateway connections per IP (keyless path — default 1).</td></tr>
<tr><td><code>[limits.clients.game_default] per_ip</code></td><td>Default game connections per IP (default 8); override one product with <code>[limits.clients.products.DRTL] per_ip = 1</code>.</td></tr>
<tr><td><code>[storage] path</code></td><td>SQLite database file (empty = in-memory, lost on restart).</td></tr>
</table>

<h2>Chat commands</h2>
<ul>
<li><b>Anyone:</b> <code>/help</code>, <code>/join</code>, <code>/me</code>, <code>/who</code>, <code>/whoami</code>, <code>/squelch</code> &amp; <code>/unsquelch</code> (personal ignore).</li>
<li><b>Channel operator:</b> <code>/kick</code>, <code>/ban</code>, <code>/unban</code>, <code>/designate</code>.</li>
<li><b>Staff:</b> <code>/tagban &lt;text&gt;</code>, <code>/ipban &lt;user&gt; [hrs]</code>, <code>/mute &lt;user&gt; [hrs]</code>, and their inverses, plus <code>/bans</code>.</li>
</ul>

<h2>Discord updates</h2>
<p>Create a Discord channel <b>webhook</b> (Channel → Edit → Integrations → Webhooks) and put its URL in <code>[discord] webhook_url</code>; the server posts to that channel. The URL is a secret — it lives only in your config. Set it to empty (the default) to disable Discord entirely.</p>
<table>
<tr><th>Config key</th><th>Effect</th></tr>
<tr><td><code>webhook_url</code></td><td>The Discord webhook URL. Empty = Discord off.</td></tr>
<tr><td><code>status_interval_mins</code></td><td>Minutes between periodic status posts (default 30).</td></tr>
<tr><td><code>games_window_hours</code></td><td>Window for the "games hosted per client" figure (default 6).</td></tr>
<tr><td><code>post_status</code></td><td>Post the periodic summary: users online, channels, live games, uptime, and games hosted per client over the window.</td></tr>
<tr><td><code>post_events</code></td><td>Post on server start, shutdown, and restart.</td></tr>
<tr><td><code>post_milestones</code></td><td>Post on a new peak-connections record.</td></tr>
</table>
<p class="muted">Each post type is switchable, so you can send just the periodic summary, only events, or everything. Posts are best-effort — a slow or failed post is dropped, never blocking the server.</p>

</main></body></html>"##;

/// How many accounts one page of the user list shows.
const USERS_PAGE_SIZE: u32 = 50;

/// Products a per-IP override can be set for from the panel — the classic BNCS product codes.
/// A fixed pick-list keeps the editor typo-proof: no hand-typed FourCCs (which is how a bad
/// value like "TELNET" once got in). Telnet/chat clients are limited by the gateway per-IP
/// field, not here.
const KNOWN_PRODUCTS: &[(&str, &str)] = &[
    ("STAR", "StarCraft"),
    ("SEXP", "Brood War"),
    ("SSHR", "StarCraft Shareware"),
    ("JSTR", "StarCraft (Japan)"),
    ("DRTL", "Diablo"),
    ("DSHR", "Diablo Shareware"),
    ("D2DV", "Diablo II"),
    ("D2XP", "Diablo II: LoD"),
    ("W2BN", "Warcraft II BNE"),
    ("WAR3", "Warcraft III: RoC"),
    ("W3XP", "Warcraft III: TFT"),
];

/// `GET /users` — the user-management list, one page at a time.
async fn users_page(node: &Node, req: &Request, flash: Option<(bool, &str)>) -> Response {
    let offset: u64 = req.query.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    // Fetch one extra to learn whether a further page exists, without a second count query.
    let mut users = node.list_users(offset, USERS_PAGE_SIZE + 1).await;
    let has_next = users.len() as u32 > USERS_PAGE_SIZE;
    users.truncate(USERS_PAGE_SIZE as usize);
    html_page(render_users(&users, offset, has_next, flash))
}

/// `POST /users/flags` — set an account's assignable flags from the row's checkboxes.
async fn do_user_flags(req: &Request, node: &Node) -> Response {
    let Some(id) = req.form.get("id").and_then(|s| s.parse::<u64>().ok()) else {
        return users_page(node, req, Some((false, "Missing account id."))).await;
    };
    let mut flags = 0u32;
    if req.form.get("staff").map(String::as_str) == Some("on") {
        flags |= user_flags::ADMIN | user_flags::BLIZZARD_REP;
    }
    if req.form.get("speaker").map(String::as_str) == Some("on") {
        flags |= user_flags::SPEAKER;
    }
    if req.form.get("guest").map(String::as_str) == Some("on") {
        flags |= user_flags::SPECIAL_GUEST;
    }
    let flash = match node.set_user_flags(id, flags).await {
        Ok(()) => (true, "Flags saved — they take effect at the user's next login.".to_string()),
        Err(e) => (false, format!("Could not save flags: {e}")),
    };
    users_page(node, req, Some((flash.0, &flash.1))).await
}

/// `POST /users/reset` — reset an account's password to an admin-supplied plaintext, which
/// is hashed to the X-SHA-1 digest a client would send at logon (never stored in the clear).
async fn do_user_reset(req: &Request, node: &Node) -> Response {
    let Some(id) = req.form.get("id").and_then(|s| s.parse::<u64>().ok()) else {
        return users_page(node, req, Some((false, "Missing account id."))).await;
    };
    let new = req.form.get("new_password").map(String::as_str).unwrap_or("");
    if new.is_empty() {
        return users_page(node, req, Some((false, "The new password cannot be empty."))).await;
    }
    let flash = match node.reset_password(id, new).await {
        Ok(()) => (true, "Password reset. Give the user the new password.".to_string()),
        Err(e) => (false, format!("Could not reset password: {e}")),
    };
    users_page(node, req, Some((flash.0, &flash.1))).await
}

/// `POST /users/delete` — permanently delete an account.
async fn do_user_delete(req: &Request, node: &Node) -> Response {
    let Some(id) = req.form.get("id").and_then(|s| s.parse::<u64>().ok()) else {
        return users_page(node, req, Some((false, "Missing account id."))).await;
    };
    let flash = match node.delete_user(id).await {
        Ok(()) => (true, "Account deleted.".to_string()),
        Err(e) => (false, format!("Could not delete account: {e}")),
    };
    users_page(node, req, Some((flash.0, &flash.1))).await
}

/// `GET /d2/season` — the Diablo II ladder season: its number, when it began, how many
/// characters are on the ladder, and the button that ends it.
async fn season_page(node: &Node, flash: Option<(bool, &str)>) -> Response {
    use bnetcc_proto::d2::status::{HARDCORE, LADDER};
    let season = node.d2_season.current();
    let ladder: Vec<_> = node.all_characters().await.into_iter().filter(|c| c.status & LADDER != 0).collect();
    let hardcore = ladder.iter().filter(|c| c.status & HARDCORE != 0).count();
    html_page(render_season(season, ladder.len() - hardcore, hardcore, flash))
}

/// `POST /d2/season/end` — end the season and start the next.
async fn do_end_season(node: &Node) -> Response {
    let flash = match node.end_ladder_season().await {
        Ok((ended, begun, softcore, hardcore)) => (
            true,
            format!(
                "Season {} is over: {softcore} softcore and {hardcore} hardcore characters are now non-ladder. Season {} has begun.",
                ended.number, begun.number
            ),
        ),
        Err(e) => (false, format!("The season was not ended: {e}")),
    };
    season_page(node, Some((flash.0, &flash.1))).await
}

/// The ladder season page.
fn render_season(season: crate::season::Season, softcore: usize, hardcore: usize, flash: Option<(bool, &str)>) -> String {
    let flash_html = match flash {
        Some((true, m)) => format!(r#"<p class="ok">{}</p>"#, html_escape(m)),
        Some((false, m)) => err_banner(m),
        None => String::new(),
    };
    let number = season.number;
    let next = number + 1;
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Diablo II ladder — Command Center</title>
<style>
:root {{ color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --err:#e06a6a; --ok:#5ac47d; --danger:#a33; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
header {{ padding:16px 20px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:12px; flex-wrap:wrap; }}
header h1 {{ font-size:16px; margin:0; }}
header nav {{ margin-left:auto; }} header nav a {{ color:var(--accent); text-decoration:none; font-size:13px; }}
main {{ padding:20px; max-width:720px; }}
.card {{ background:var(--card); border:1px solid var(--line); border-radius:10px; padding:16px 18px; margin-bottom:16px; }}
.big {{ font-size:28px; font-weight:600; font-variant-numeric:tabular-nums; }}
.sub {{ color:var(--muted); font-size:13px; }}
.row {{ display:flex; gap:32px; flex-wrap:wrap; margin-top:10px; }}
button {{ padding:8px 14px; border:none; border-radius:6px; background:var(--danger); color:#fff; font-weight:600; font-size:13px; cursor:pointer; }}
.err {{ background:rgba(224,106,106,.12); border:1px solid var(--err); color:var(--err); padding:9px 12px; border-radius:8px; margin-bottom:12px; }}
.ok {{ background:rgba(90,196,125,.12); border:1px solid var(--ok); color:var(--ok); padding:9px 12px; border-radius:8px; margin-bottom:12px; }}
</style></head><body>
<header><h1>Diablo II ladder</h1><nav><a href="/">Dashboard</a> · <a href="/d2">D2 map</a> · <a href="/users">Users</a> · <a href="/help">Help</a></nav></header>
<main>
{flash_html}
<div class="card">
  <div class="sub">Current season</div>
  <div class="big">Season {number}</div>
  <div class="sub">Began {began}</div>
  <div class="row">
    <div><div class="big">{softcore}</div><div class="sub">softcore ladder characters</div></div>
    <div><div class="big">{hardcore}</div><div class="sub">hardcore ladder characters</div></div>
  </div>
</div>
<div class="card">
  <p>Ending the season turns every ladder character, softcore and hardcore, into a normal character. They keep their levels, items and progress. The ladder starts empty, and characters created as ladder characters from then on belong to season {next}.</p>
  <form method="post" action="/d2/season/end" onsubmit="return confirm('End season {number}? Every ladder character becomes a normal character. This cannot be undone.')">
    <button type="submit">End season {number}</button>
  </form>
</div>
</main></body></html>"##,
        began = ago(season.started),
    )
}

/// Render "N{unit} ago" for a timestamp `secs` epoch-seconds in the past, avoiding a date
/// dependency. `0` (unset) renders as "never".
fn ago(secs: u64) -> String {
    if secs == 0 {
        return "never".to_string();
    }
    let now = crate::now_ms() / 1000;
    let d = now.saturating_sub(secs);
    if d < 90 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

/// Build the user-management page for one loaded slice of accounts.
fn render_users(users: &[UserSummary], offset: u64, has_next: bool, flash: Option<(bool, &str)>) -> String {
    let flash_html = match flash {
        Some((true, m)) => format!(r#"<p class="ok">{}</p>"#, html_escape(m)),
        Some((false, m)) => err_banner(m),
        None => String::new(),
    };
    let mut rows = String::new();
    if users.is_empty() {
        rows.push_str(r#"<tr><td colspan="5" class="empty">No accounts on this page.</td></tr>"#);
    }
    for u in users {
        let staff = u.flags & user_flags::ADMIN != 0;
        let speaker = u.flags & user_flags::SPEAKER != 0;
        let guest = u.flags & user_flags::SPECIAL_GUEST != 0;
        let ck = |on: bool| if on { "checked" } else { "" };
        let name = html_escape(&u.name);
        rows.push_str(&format!(
            r#"<tr>
<td><b>{name}</b>{badge}<div class="sub2">created {created}</div></td>
<td>{last}</td>
<td class="num">{wins}/{losses}</td>
<td>
  <form method="post" action="/users/flags" class="inline">
    <input type="hidden" name="id" value="{id}">
    <label><input type="checkbox" name="staff" {cs}> Staff</label>
    <label><input type="checkbox" name="speaker" {csp}> Speaker</label>
    <label><input type="checkbox" name="guest" {cg}> Guest</label>
    <button type="submit">Save</button>
  </form>
</td>
<td>
  <form method="post" action="/users/reset" class="inline" onsubmit="return confirm('Reset the password for {name}?')">
    <input type="hidden" name="id" value="{id}">
    <input type="password" name="new_password" placeholder="new password" autocomplete="new-password">
    <button type="submit">Reset</button>
  </form>
  <form method="post" action="/users/delete" class="inline" onsubmit="return confirm('Permanently delete {name}? This cannot be undone.')">
    <input type="hidden" name="id" value="{id}">
    <button type="submit" class="danger">Delete</button>
  </form>
</td>
</tr>"#,
            badge = if staff { r#" <span class="tag">staff</span>"# } else { "" },
            created = ago(u.created_at),
            last = ago(u.last_login.unwrap_or(0)),
            wins = u.wins,
            losses = u.losses,
            id = u.id,
            cs = ck(staff),
            csp = ck(speaker),
            cg = ck(guest),
        ));
    }

    let prev = offset.saturating_sub(u64::from(USERS_PAGE_SIZE));
    let prev_link = if offset > 0 {
        format!(r#"<a href="/users?offset={prev}">‹ Prev</a>"#)
    } else {
        r#"<span class="disabled">‹ Prev</span>"#.to_string()
    };
    let next_off = offset + u64::from(USERS_PAGE_SIZE);
    let next_link = if has_next {
        format!(r#"<a href="/users?offset={next_off}">Next ›</a>"#)
    } else {
        r#"<span class="disabled">Next ›</span>"#.to_string()
    };

    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Users — Command Center</title>
<style>
:root {{ color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --err:#e06a6a; --ok:#5ac47d; --danger:#a33; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
header {{ padding:16px 20px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:12px; }}
header h1 {{ font-size:16px; margin:0; }}
header nav {{ margin-left:auto; }} header nav a {{ color:var(--accent); text-decoration:none; font-size:13px; }}
main {{ padding:20px; }}
table {{ width:100%; border-collapse:collapse; background:var(--card); border:1px solid var(--line); border-radius:10px; overflow:hidden; }}
td,th {{ text-align:left; padding:10px 14px; border-bottom:1px solid var(--line); vertical-align:middle; }}
th {{ color:var(--muted); font-weight:500; font-size:12px; text-transform:uppercase; letter-spacing:.04em; }}
tr:last-child td {{ border-bottom:none; }}
.sub2 {{ color:var(--muted); font-size:12px; }}
.num {{ font-variant-numeric:tabular-nums; }}
.empty {{ color:var(--muted); padding:16px; }}
.tag {{ background:rgba(90,169,230,.15); color:var(--accent); border:1px solid var(--accent); border-radius:4px; font-size:11px; padding:1px 5px; margin-left:6px; }}
form.inline {{ display:inline-flex; align-items:center; gap:6px; margin:0 8px 0 0; }}
form.inline label {{ color:var(--fg); font-size:12px; display:inline-flex; align-items:center; gap:3px; margin:0; text-transform:none; letter-spacing:0; }}
input[type=password] {{ padding:5px 8px; border-radius:6px; border:1px solid var(--line); background:#0f1115; color:var(--fg); width:130px; }}
button {{ padding:5px 10px; border:none; border-radius:6px; background:var(--accent); color:#04121f; font-weight:600; font-size:12px; cursor:pointer; }}
button.danger {{ background:var(--danger); color:#fff; }}
.err {{ background:rgba(224,106,106,.12); border:1px solid var(--err); color:var(--err); padding:9px 12px; border-radius:8px; margin-bottom:12px; }}
.ok {{ background:rgba(90,196,125,.12); border:1px solid var(--ok); color:var(--ok); padding:9px 12px; border-radius:8px; margin-bottom:12px; }}
.pager {{ margin-top:16px; display:flex; gap:16px; align-items:center; }}
.pager a {{ color:var(--accent); text-decoration:none; }} .pager .disabled {{ color:var(--muted); }}
</style></head><body>
<header><h1>Users</h1><nav><a href="/">Dashboard</a> · <a href="/settings">Settings</a> · <a href="/help">Help</a></nav></header>
<main>
{flash_html}
<table>
<tr><th>Account</th><th>Last login</th><th>W/L</th><th>Flags (apply at next login)</th><th>Password / Delete</th></tr>
{rows}
</table>
<div class="pager">{prev_link}{next_link}<span class="sub2">showing from #{start}</span></div>
</main></body></html>"##,
        start = offset + 1,
    )
}

fn do_login(
    req: &Request,
    peer_ip: IpAddr,
    admin: &Admin,
    sessions: &Sessions,
    rate: &RateLimiter,
) -> Response {
    if is_rate_limited(rate, peer_ip) {
        return html_page(login_page_with("Too many attempts — wait a minute and try again."));
    }
    let password = req.form.get("password").map(String::as_str).unwrap_or("");
    if admin.verify_password(password) {
        clear_rate(rate, peer_ip);
        let token = new_token();
        sessions
            .lock()
            .expect("sessions lock")
            .insert(token.clone(), Instant::now() + SESSION_TTL);
        let cookie = format!(
            "session={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
            SESSION_TTL.as_secs()
        );
        Response {
            status: "303 See Other",
            content_type: "text/html; charset=utf-8",
            body: String::new(),
            set_cookie: Some(cookie),
            location: Some("/".to_string()),
        }
    } else {
        record_fail(rate, peer_ip);
        html_page(login_page_with("Incorrect password."))
    }
}

fn do_logout(req: &Request, sessions: &Sessions) -> Response {
    if let Some(tok) = req.cookies.get("session") {
        sessions.lock().expect("sessions lock").remove(tok);
    }
    let mut r = redirect("/login");
    r.set_cookie = Some("session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0".to_string());
    r
}

fn do_change_pw(req: &Request, admin: &Admin) -> Response {
    let new = req.form.get("new_password").map(String::as_str).unwrap_or("");
    let confirm = req.form.get("confirm").map(String::as_str).unwrap_or("");
    if new != confirm {
        return html_page(change_pw_page_with(admin.must_change(), "The two passwords did not match."));
    }
    match admin.set_password(new) {
        Ok(()) => redirect("/"),
        Err(e) => html_page(change_pw_page_with(admin.must_change(), e)),
    }
}

fn do_settings(req: &Request, admin: &Admin) -> Response {
    // An HTML checkbox only appears in the form when checked.
    let want_remote = req.form.get("remote").map(String::as_str) == Some("on");
    admin.set_remote_enabled(want_remote);
    redirect("/settings")
}

fn is_authed(req: &Request, sessions: &Sessions) -> bool {
    let Some(tok) = req.cookies.get("session") else {
        return false;
    };
    let mut s = sessions.lock().expect("sessions lock");
    match s.get(tok).copied() {
        Some(exp) if exp > Instant::now() => true,
        Some(_) => {
            s.remove(tok);
            false
        }
        None => false,
    }
}

fn is_rate_limited(rate: &RateLimiter, ip: IpAddr) -> bool {
    let mut r = rate.lock().expect("rate lock");
    match r.get(&ip) {
        Some(&(fails, start)) if start.elapsed() < LOGIN_WINDOW => fails >= MAX_LOGIN_FAILS,
        _ => {
            r.remove(&ip);
            false
        }
    }
}

fn record_fail(rate: &RateLimiter, ip: IpAddr) {
    let mut r = rate.lock().expect("rate lock");
    let entry = r.entry(ip).or_insert((0, Instant::now()));
    if entry.1.elapsed() >= LOGIN_WINDOW {
        *entry = (0, Instant::now());
    }
    entry.0 += 1;
}

fn clear_rate(rate: &RateLimiter, ip: IpAddr) {
    rate.lock().expect("rate lock").remove(&ip);
}

/// A 256-bit random session token, hex-encoded, from the OS CSPRNG.
fn new_token() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn html_page(body: String) -> Response {
    Response {
        status: "200 OK",
        content_type: "text/html; charset=utf-8",
        body,
        set_cookie: None,
        location: None,
    }
}

fn json_response(node: &Node) -> Response {
    Response {
        status: "200 OK",
        content_type: "application/json",
        body: serde_json::to_string(&node.status_snapshot()).unwrap_or_else(|_| "{}".to_string()),
        set_cookie: None,
        location: None,
    }
}

/// The live Diablo II map page (`GET /d2`).
const D2_MAP: &str = include_str!("d2_map.html");

/// Which map data a `/d2/*.json` request asks for.
enum D2Query {
    Games,
    Level,
    Live,
}

/// The live map's data from the game server: every game, a level's collision map and marks
/// (`?game=&level=`), or what stands in a level now. An empty list when no game server runs.
fn d2_json(node: &Node, req: &Request, query: D2Query) -> Response {
    let server = node.d2_realm.as_ref().and_then(|r| r.game_server.as_ref());
    let number = |key: &str| req.query.get(key).and_then(|v| v.parse::<i64>().ok());
    let target = number("game").and_then(|g| u16::try_from(g).ok()).zip(number("level").and_then(|l| i32::try_from(l).ok()));
    let body = match (query, server) {
        (D2Query::Games, None) => Some("[]".to_string()),
        (D2Query::Games, Some(gs)) => serde_json::to_string(&gs.map_games()).ok(),
        (D2Query::Level, Some(gs)) => target.and_then(|(g, l)| gs.map_level(g, l)).and_then(|m| serde_json::to_string(&m).ok()),
        (D2Query::Live, Some(gs)) => target.and_then(|(g, l)| gs.map_live(g, l)).and_then(|m| serde_json::to_string(&m).ok()),
        (_, None) => None,
    };
    match body {
        Some(body) => Response { status: "200 OK", content_type: "application/json", body, set_cookie: None, location: None },
        None => text("404 Not Found", "No such game or level."),
    }
}

fn text(status: &'static str, body: &str) -> Response {
    Response {
        status,
        content_type: "text/plain; charset=utf-8",
        body: body.to_string(),
        set_cookie: None,
        location: None,
    }
}

fn redirect(to: &str) -> Response {
    Response {
        status: "303 See Other",
        content_type: "text/html; charset=utf-8",
        body: String::new(),
        set_cookie: None,
        location: Some(to.to_string()),
    }
}

async fn write_response<S>(stream: &mut S, resp: &Response)
where
    S: AsyncWriteExt + Unpin,
{
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n",
        resp.status,
        resp.content_type,
        resp.body.len()
    );
    if let Some(loc) = &resp.location {
        head.push_str(&format!("Location: {loc}\r\n"));
    }
    if let Some(cookie) = &resp.set_cookie {
        head.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(resp.body.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Read exactly one HTTP request: headers up to the blank line, then `Content-Length` body
/// bytes. One request per connection (no keep-alive), so there is nothing to smuggle.
async fn read_request<S>(stream: &mut S) -> Option<Request>
where
    S: AsyncReadExt + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_REQUEST_BYTES {
            return None;
        }
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let mut rl = lines.next()?.split_whitespace();
    let method = rl.next()?.to_string();
    let raw_path = rl.next()?.to_string();
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_form(q)),
        None => (raw_path.clone(), HashMap::new()),
    };

    let mut content_length = 0usize;
    let mut cookies = HashMap::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = v.trim().parse().unwrap_or(0),
            "cookie" => {
                for pair in v.split(';') {
                    if let Some((ck, cv)) = pair.trim().split_once('=') {
                        cookies.insert(ck.trim().to_string(), cv.trim().to_string());
                    }
                }
            }
            _ => {}
        }
    }
    content_length = content_length.min(MAX_REQUEST_BYTES);

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > MAX_REQUEST_BYTES {
            return None;
        }
    }
    body.truncate(content_length);
    let form = if method == "POST" {
        parse_form(&String::from_utf8_lossy(&body))
    } else {
        HashMap::new()
    };
    Some(Request { method, path, query, cookies, form })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_form(body: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        m.insert(url_decode(k), url_decode(v));
    }
    m
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => match (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Minimal HTML-escape for the few dynamic strings we interpolate into pages.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Shared page shell (CSS + centered card) for the auth/settings pages.
fn shell(title: &str, inner: &str) -> String {
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title>
<style>
:root {{ color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --err:#e06a6a; --ok:#5ac47d; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; min-height:100vh; display:flex; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
.card {{ background:var(--card); border:1px solid var(--line); border-radius:12px; padding:28px; width:min(92vw,440px); margin:auto; }}
h1 {{ font-size:16px; margin:0 0 4px; }}
p.sub {{ color:var(--muted); margin:0 0 18px; font-size:13px; }}
label {{ display:block; font-size:12px; color:var(--muted); margin:14px 0 6px; text-transform:uppercase; letter-spacing:.04em; }}
input[type=password], input[type=text], input[type=number], textarea, select {{ width:100%; padding:10px 12px; border-radius:8px; border:1px solid var(--line); background:#0f1115; color:var(--fg); font-size:14px; font-family:inherit; }}
button {{ margin-top:20px; width:100%; padding:11px; border:none; border-radius:8px; background:var(--accent); color:#04121f; font-weight:600; font-size:14px; cursor:pointer; }}
.err {{ background:rgba(224,106,106,.12); border:1px solid var(--err); color:var(--err); padding:9px 12px; border-radius:8px; font-size:13px; margin-bottom:8px; }}
.ok {{ background:rgba(90,196,125,.12); border:1px solid var(--ok); color:var(--ok); padding:9px 12px; border-radius:8px; font-size:13px; margin-bottom:8px; }}
.warn {{ background:rgba(224,170,90,.12); border:1px solid #e0aa5a; color:#e0aa5a; padding:9px 12px; border-radius:8px; font-size:12px; margin:6px 0; }}
a {{ color:var(--accent); }} .row {{ display:flex; align-items:center; gap:10px; margin:14px 0; }} .row input {{ width:auto; }}
.row2 {{ display:grid; grid-template-columns:repeat(2,1fr); gap:8px; }}
.row3 {{ display:grid; grid-template-columns:repeat(3,1fr); gap:8px; }}
.row4 {{ display:grid; grid-template-columns:repeat(4,1fr); gap:8px; }}
.gh {{ display:grid; grid-template-columns:repeat(4,1fr); gap:8px; font-size:11px; color:var(--muted); text-transform:uppercase; letter-spacing:.04em; margin:10px 0 4px; }}
.gh span:first-child {{ text-transform:none; letter-spacing:0; }}
.hdr {{ margin:24px 0 4px; padding-top:14px; border-top:1px solid var(--line); font-weight:600; font-size:14px; }}
label.cb {{ margin:0; text-transform:none; letter-spacing:0; color:var(--fg); }}
.prodgrid {{ display:grid; grid-template-columns:repeat(2,1fr); gap:6px 12px; }}
label.prod {{ display:flex; align-items:center; justify-content:space-between; gap:8px; margin:0; text-transform:none; letter-spacing:0; color:var(--fg); font-size:13px; }}
label.prod input {{ width:64px; }}
code {{ background:#0f1115; border:1px solid var(--line); border-radius:4px; padding:1px 5px; font-size:12px; }}
.muted {{ color:var(--muted); font-size:12px; }}
</style></head><body><div class="card">{inner}</div></body></html>"##
    )
}

fn err_banner(msg: &str) -> String {
    if msg.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="err">{}</div>"#, html_escape(msg))
    }
}

fn login_page_with(err: &str) -> String {
    let inner = format!(
        r#"<h1>Command Center</h1><p class="sub">Admin sign-in</p>{}
<form method="post" action="/login">
<label for="pw">Password</label>
<input id="pw" name="password" type="password" autofocus autocomplete="current-password">
<button type="submit">Sign in</button></form>"#,
        err_banner(err)
    );
    shell("Sign in — Command Center", &inner)
}

fn change_pw_page(must_change: bool) -> String {
    change_pw_page_with(must_change, "")
}

fn change_pw_page_with(must_change: bool, err: &str) -> String {
    let sub = if must_change {
        "Set a new password to continue — the first-run password must be changed."
    } else {
        "Change your admin password."
    };
    let inner = format!(
        r#"<h1>Change password</h1><p class="sub">{sub}</p>{}
<form method="post" action="/change-password">
<label for="np">New password</label>
<input id="np" name="new_password" type="password" autofocus autocomplete="new-password">
<label for="cf">Confirm</label>
<input id="cf" name="confirm" type="password" autocomplete="new-password">
<button type="submit">Save</button></form>
<p class="muted" style="margin-top:16px">Up to 32 characters.</p>"#,
        err_banner(err)
    );
    shell("Change password — Command Center", &inner)
}

/// The settings page. `flash` is `(ok, message)` shown after a config save.
fn settings_page(admin: &Admin, config_path: &Path, flash: Option<(bool, &str)>) -> String {
    let checked = if admin.remote_enabled() { "checked" } else { "" };
    let doc = load_config_doc(config_path);
    // Current config values (from the file), each falling back to the config default so the
    // form is never blank on a fresh install.
    let name = html_escape(&cfg_string(&doc, &["server", "name"], "Command Center"));
    let motd = html_escape(&cfg_string(&doc, &["server", "motd"], "Welcome to Command Center, an educational server for older computers. On a modern computer, buy Diablo II: Resurrected, Warcraft III: Reforged, StarCraft: Remastered or Warcraft II: Remastered."));
    // Server mode <select>.
    let mode = cfg_string(&doc, &["server", "mode"], "gaming");
    let mode_opts = select_options(
        &mode,
        &[("gaming", "Gaming (games + chat)"), ("warnet", "Warnet (chat/bots only)"), ("both", "Both")],
    );
    // WarCraft III logon method <select>.
    let wc3_logon = cfg_string(&doc, &["server", "wc3_logon"], "nls");
    let wc3_opts = select_options(
        &wc3_logon,
        &[("nls", "NLS / SRP (default)"), ("legacy", "Legacy X-SHA-1 (StarCraft-hash loader)")],
    );
    // Diablo II closed realm.
    let d2_realm_ck = if cfg_bool(&doc, &["diablo2", "realm"], true) { "checked" } else { "" };
    let d2_desc = html_escape(&cfg_string(&doc, &["diablo2", "description"], "Diablo II closed realm"));
    let d2_addr = html_escape(&cfg_string(&doc, &["diablo2", "address"], ""));
    let d2_max = cfg_int(&doc, &["diablo2", "max_characters"], 18);
    // Listeners.
    let bncs = html_escape(&cfg_string(&doc, &["listen", "bncs"], "0.0.0.0:6112"));
    let accept_shards = cfg_int(&doc, &["listen", "accept_shards"], 1);
    let admin_listen = html_escape(&cfg_string(&doc, &["listen", "admin"], "127.0.0.1:6115"));
    let panel_listen = html_escape(&cfg_string(&doc, &["status", "listen"], "127.0.0.1:6114"));
    // Channel caps.
    let private_max = cfg_int(&doc, &["channels", "private_max"], 0);
    let public_max = cfg_int(&doc, &["channels", "public_max"], 0);
    let clan_max = cfg_int(&doc, &["channels", "clan_max"], 0);
    // Channel behaviour.
    let auto_op_ck = if cfg_bool(&doc, &["channels", "auto_op_private"], false) { "checked" } else { "" };
    let telnet_access = cfg_string(&doc, &["channels", "telnet_access"], "public");
    let telnet_opts = select_options(
        &telnet_access,
        &[("public", "Public channels only"), ("none", "No channels"), ("all", "Any channel")],
    );
    let default_channels = html_escape(&cfg_default_channel_text(&doc));
    // Global resource limits.
    let max_conn = cfg_int(&doc, &["limits", "max_connections"], 0);
    let max_frame = cfg_int(&doc, &["limits", "max_frame_bytes"], 8192);
    let max_line = cfg_int(&doc, &["limits", "max_line_bytes"], 1024);
    let outbound_queue = cfg_int(&doc, &["limits", "outbound_queue"], 64);
    let handshake_to = cfg_int(&doc, &["limits", "handshake_timeout_secs"], 30);
    let idle_to = cfg_int(&doc, &["limits", "idle_timeout_secs"], 1200);
    let cd_key_ck = if cfg_bool(&doc, &["limits", "cd_key_uniqueness"], true) { "checked" } else { "" };
    let allowlist = html_escape(&cfg_str_array(&doc, &["limits", "gateway_allowlist"]));
    // Per-client-type limits. per_ip keeps a concrete effective default; per_account / global
    // are blank when unset (an unset field keeps the mode default rather than pinning a value).
    let game_per_ip = cfg_int(&doc, &["limits", "clients", "game_default", "per_ip"], 8);
    let game_per_acct = cfg_opt_int(&doc, &["limits", "clients", "game_default", "per_account"]);
    let game_global = cfg_opt_int(&doc, &["limits", "clients", "game_default", "global"]);
    let gw_per_ip = cfg_int(&doc, &["limits", "clients", "gateway", "per_ip"], 1);
    let gw_per_acct = cfg_opt_int(&doc, &["limits", "clients", "gateway", "per_account"]);
    let gw_global = cfg_opt_int(&doc, &["limits", "clients", "gateway", "global"]);
    let bnftp_per_ip = cfg_int(&doc, &["limits", "clients", "bnftp", "per_ip"], 4);
    let bnftp_per_acct = cfg_opt_int(&doc, &["limits", "clients", "bnftp", "per_account"]);
    let bnftp_global = cfg_opt_int(&doc, &["limits", "clients", "bnftp", "global"]);
    let pend_per_ip = cfg_opt_int(&doc, &["limits", "clients", "game_pending", "per_ip"]);
    let pend_per_acct = cfg_opt_int(&doc, &["limits", "clients", "game_pending", "per_account"]);
    let pend_global = cfg_opt_int(&doc, &["limits", "clients", "game_pending", "global"]);
    let products_grid = KNOWN_PRODUCTS
        .iter()
        .map(|(code, name)| {
            let v = cfg_int(&doc, &["limits", "clients", "products", code, "per_ip"], -1);
            let val = if v >= 0 { v.to_string() } else { String::new() };
            format!(
                r#"<label class="prod"><span>{name} <code>{code}</code></span><input name="prod_{code}" type="number" min="0" value="{val}" placeholder="—"></label>"#
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let channels_defined = html_escape(&cfg_channels_text(&doc));
    let admins = html_escape(&cfg_admin_list(&doc));
    // Storage / files / versions.
    let storage_path = html_escape(&cfg_string(&doc, &["storage", "path"], "bnetccd.db"));
    let files_dir = html_escape(&cfg_string(&doc, &["files", "dir"], ""));
    let versions_restrict_ck = if cfg_bool(&doc, &["versions", "restrict"], false) { "checked" } else { "" };
    let versions_text = html_escape(&cfg_versions_text(&doc));
    // Public status
    let public_listen = html_escape(&cfg_string(&doc, &["status", "public_listen"], ""));
    let public_users_ck = if cfg_bool(&doc, &["status", "public_show_users"], false) { "checked" } else { "" };
    // Discord
    let d_url = html_escape(&cfg_string(&doc, &["discord", "webhook_url"], ""));
    let d_games = html_escape(&cfg_string(&doc, &["discord", "games_webhook_url"], ""));
    let d_interval = cfg_int(&doc, &["discord", "status_interval_mins"], 30);
    let d_window = cfg_int(&doc, &["discord", "games_window_hours"], 6);
    let d_status_ck = if cfg_bool(&doc, &["discord", "post_status"], true) { "checked" } else { "" };
    let d_events_ck = if cfg_bool(&doc, &["discord", "post_events"], true) { "checked" } else { "" };
    let d_miles_ck = if cfg_bool(&doc, &["discord", "post_milestones"], true) { "checked" } else { "" };
    // Stats push
    let s_url = html_escape(&cfg_string(&doc, &["stats_push", "url"], ""));
    let s_interval = cfg_int(&doc, &["stats_push", "interval_secs"], 60);
    let s_token = html_escape(&cfg_string(&doc, &["stats_push", "token"], ""));
    let s_users_ck = if cfg_bool(&doc, &["stats_push", "include_users"], false) { "checked" } else { "" };
    // Server tracking (advertise us / host a list).
    let t_advertise = html_escape(&cfg_str_array(&doc, &["tracker", "advertise_to"]));
    let t_desc = html_escape(&cfg_string(&doc, &["tracker", "description"], ""));
    let t_url = html_escape(&cfg_string(&doc, &["tracker", "url"], ""));
    let t_cname = html_escape(&cfg_string(&doc, &["tracker", "contact_name"], ""));
    let t_cemail = html_escape(&cfg_string(&doc, &["tracker", "contact_email"], ""));
    let t_interval = cfg_int(&doc, &["tracker", "advertise_interval_secs"], 300);
    let t_host = html_escape(&cfg_string(&doc, &["tracker", "host_listen"], ""));
    let t_list = html_escape(&cfg_string(&doc, &["tracker", "list_listen"], ""));
    let t_prune = cfg_int(&doc, &["tracker", "prune_after_secs"], 600);
    // Federation role
    let is_node = cfg_bool(&doc, &["federation", "enabled"], false);
    let master_sel = if is_node { "" } else { "selected" };
    let node_sel = if is_node { "selected" } else { "" };
    let hub = html_escape(&cfg_string(&doc, &["federation", "hub"], ""));
    let identity_key = html_escape(&cfg_string(&doc, &["federation", "identity_key"], "node.key"));
    let offline_login_ck = if cfg_bool(&doc, &["federation", "offline_login"], false) { "checked" } else { "" };

    let flash_html = match flash {
        Some((true, m)) => format!(r#"<p class="ok">{}</p>"#, html_escape(m)),
        Some((false, m)) => err_banner(m),
        None => String::new(),
    };

    let inner = format!(
        r#"<h1>Settings</h1><p class="sub">Admin panel</p>
<form method="post" action="/settings">
<div class="row"><input id="rm" name="remote" type="checkbox" {checked}><label for="rm" style="margin:0;text-transform:none;letter-spacing:0;color:var(--fg)">Enable remote access (non-localhost)</label></div>
<p class="muted">When off, only localhost can reach this panel — even with the port forwarded. Turn it on only after you have forwarded 6114 and want remote control.</p>
<button type="submit">Save remote setting</button></form>

<h1 style="margin-top:28px">Server configuration</h1>
<p class="sub">Everything configurable lives here. Written to the config file; applied after a restart.</p>{flash_html}
<form method="post" action="/settings/config">

<div class="hdr">Server identity</div>
<label for="sn">Server name</label>
<input id="sn" name="server_name" type="text" value="{name}" maxlength="64">
<label for="md">Mode</label>
<select id="md" name="server_mode">{mode_opts}</select>
<p class="muted"><b>Gaming</b>: games, realms, ladder + chat. <b>Warnet</b>: chat/bots only — game hosting refused, realm/WC3 listeners don't bind. <b>Both</b>: games available, limits split by class.</p>
<label for="mo">Message of the day</label>
<input id="mo" name="server_motd" type="text" value="{motd}" maxlength="200">
<label for="w3">WarCraft III logon</label>
<select id="w3" name="server_wc3_logon">{wc3_opts}</select>
<p class="muted">NLS is what a normal WarCraft III client uses. <b>Legacy</b> advertises the X-SHA-1 logon for a StarCraft-hash loader — leave it on NLS unless you run one. See docs/WARCRAFT3.md §3.7.</p>

<div class="hdr">Diablo II realm</div>
<div class="row"><input id="d2r" name="d2_realm" type="checkbox" {d2_realm_ck}><label for="d2r" class="cb">Offer the closed realm (private characters on the Battle.net button)</label></div>
<label for="d2d">Realm description (its name is the realm name above: <code>Name@realm</code>)</label>
<input id="d2d" name="d2_description" type="text" value="{d2_desc}" maxlength="64">
<label for="d2a">Realm address clients dial (empty = the address they reached this server on)</label>
<input id="d2a" name="d2_address" type="text" value="{d2_addr}" placeholder="bnet.example.net or 203.0.113.4">
<p class="muted">The realm shares the BNCS port — no extra port to forward. LAN players always get the LAN address; set this to your public address or hostname for players on the internet. Game creation needs a Diablo II game server, which is not built in yet; see docs/DIABLO2.md.</p>
<label for="d2m">Characters per account (1–18)</label>
<input id="d2m" name="d2_max_characters" type="number" min="1" max="18" value="{d2_max}">

<div class="hdr">Network &amp; listeners</div>
<div class="warn">These need a restart and can lock you out or move ports. Change them only when you know the address is reachable.</div>
<label for="lb">Game / chat-gateway address (BNCS)</label>
<input id="lb" name="listen_bncs" type="text" value="{bncs}" placeholder="0.0.0.0:6112">
<label for="as">Accept-loop shards (1 = single listener; higher spreads accept across cores, Unix only)</label>
<input id="as" name="accept_shards" type="number" min="1" max="64" value="{accept_shards}">
<label for="la">Health / metrics address</label>
<input id="la" name="listen_admin" type="text" value="{admin_listen}" placeholder="127.0.0.1:6115">
<label for="ps">Admin panel address (this page)</label>
<input id="ps" name="status_listen" type="text" value="{panel_listen}" placeholder="127.0.0.1:6114">
<p class="muted">Changing the admin panel address moves this very page — after the restart, reload it at the new address. Keep it loopback unless you also turn on remote access above.</p>

<div class="hdr">Channels</div>
<label for="pm">Channel caps — Private / Public / Clan (0 = unlimited)</label>
<div class="row3">
<input id="pm" name="private_max" type="number" min="0" value="{private_max}">
<input name="public_max" type="number" min="0" value="{public_max}">
<input name="clan_max" type="number" min="0" value="{clan_max}">
</div>
<div class="row"><input id="ao" name="auto_op_private" type="checkbox" {auto_op_ck}><label for="ao" class="cb">Auto-op the first arrival to a private channel</label></div>
<label for="ta">Telnet / chat-gateway channel access (default; per-channel overrides win)</label>
<select id="ta" name="telnet_access">{telnet_opts}</select>
<label for="dc">Default channel per product — one per line: <code>PRODUCT = Channel name</code></label>
<textarea id="dc" name="default_channels" rows="3" style="width:100%;box-sizing:border-box" placeholder="STAR = StarCraft&#10;W2BN = War2 BNE">{default_channels}</textarea>
<p class="muted">Where a client lands when it joins without naming a channel. A client that names a channel is honoured as-is.</p>
<label for="ch">Defined channels — one per line: <code>name | max | products | public | persist</code></label>
<textarea id="ch" name="channels_defined" rows="4" style="width:100%;box-sizing:border-box" placeholder="War2 BNE | 100 | W2BN | yes | no&#10;Staff | 0 |  | no | yes">{channels_defined}</textarea>
<p class="muted">Blank fields keep the default. <code>products</code> is a comma list of client FourCCs (empty = any). <code>public</code>/<code>persist</code> take yes/no. Other per-channel options (topic, min_flag, telnet, game_only) stay in the config file and are preserved on save.</p>

<div class="hdr">Connection limits</div>
<label for="mc">Max connections (0 = derive from file-descriptor limit)</label>
<input id="mc" name="max_connections" type="number" min="0" value="{max_conn}">
<label>Max frame bytes / max line bytes / outbound queue (frames)</label>
<div class="row3">
<input name="max_frame_bytes" type="number" min="16" value="{max_frame}">
<input name="max_line_bytes" type="number" min="1" value="{max_line}">
<input name="outbound_queue" type="number" min="1" value="{outbound_queue}">
</div>
<label>Handshake timeout / idle timeout (seconds)</label>
<div class="row2">
<input name="handshake_timeout_secs" type="number" min="1" value="{handshake_to}">
<input name="idle_timeout_secs" type="number" min="1" value="{idle_to}">
</div>
<div class="row"><input id="ck" name="cd_key_uniqueness" type="checkbox" {cd_key_ck}><label for="ck" class="cb">Enforce one live session per CD key</label></div>
<label for="al">Gateway allow-list — IPs exempt from per-IP caps (comma/newline)</label>
<textarea id="al" name="gateway_allowlist" rows="2" style="width:100%;box-sizing:border-box" placeholder="203.0.113.4, 198.51.100.9">{allowlist}</textarea>

<label>Per-client-type limits (blank per-account / global = keep the mode default)</label>
<div class="gh"><span>Client</span><span>per IP</span><span>per acct</span><span>global</span></div>
<div class="row4">
<span style="align-self:center;font-size:13px;color:var(--fg)">Game</span>
<input name="game_per_ip" type="number" min="0" value="{game_per_ip}">
<input name="game_per_account" type="number" min="0" value="{game_per_acct}" placeholder="—">
<input name="game_global" type="number" min="0" value="{game_global}" placeholder="—">
</div>
<div class="row4">
<span style="align-self:center;font-size:13px;color:var(--fg)">Gateway</span>
<input name="gateway_per_ip" type="number" min="0" value="{gw_per_ip}">
<input name="gateway_per_account" type="number" min="0" value="{gw_per_acct}" placeholder="—">
<input name="gateway_global" type="number" min="0" value="{gw_global}" placeholder="—">
</div>
<div class="row4">
<span style="align-self:center;font-size:13px;color:var(--fg)">BNFTP</span>
<input name="bnftp_per_ip" type="number" min="0" value="{bnftp_per_ip}">
<input name="bnftp_per_account" type="number" min="0" value="{bnftp_per_acct}" placeholder="—">
<input name="bnftp_global" type="number" min="0" value="{bnftp_global}" placeholder="—">
</div>
<div class="row4">
<span style="align-self:center;font-size:13px;color:var(--fg)">Pending</span>
<input name="pending_per_ip" type="number" min="0" value="{pend_per_ip}" placeholder="—">
<input name="pending_per_account" type="number" min="0" value="{pend_per_acct}" placeholder="—">
<input name="pending_global" type="number" min="0" value="{pend_global}" placeholder="—">
</div>
<label>Per-IP limit per game (blank = no override, use the game default)</label>
<div class="prodgrid">{products_grid}</div>

<div class="hdr">Staff accounts</div>
<label for="ad">Comma or newline separated — get /tagban, /ipban, /mute and the staff icon</label>
<textarea id="ad" name="admins" rows="2" style="width:100%;box-sizing:border-box">{admins}</textarea>

<div class="hdr">Storage &amp; files</div>
<div class="warn">Changing the database path needs a restart and points the server at a different accounts file.</div>
<label for="sp">SQLite database path (empty = in-memory, lost on restart)</label>
<input id="sp" name="storage_path" type="text" value="{storage_path}" placeholder="bnetccd.db">
<label for="fd">BNFTP files directory (empty = BNFTP serving off; real classic clients need it)</label>
<input id="fd" name="files_dir" type="text" value="{files_dir}" placeholder="/path/to/files">

<div class="hdr">Client version restriction</div>
<div class="row"><input id="vr" name="versions_restrict" type="checkbox" {versions_restrict_ck}><label for="vr" class="cb">Restrict which client versions may connect</label></div>
<label for="va">Allowed versions — one per line: <code>PRODUCT PLATFORM = byte, byte</code></label>
<textarea id="va" name="versions_allowed" rows="3" style="width:100%;box-sizing:border-box" placeholder="SEXP IX86 = 211&#10;W2BN IX86 = 79">{versions_text}</textarea>
<p class="muted">Consulted only when restriction is on. Products/platforms are four-character codes (SEXP, IX86, PMAC); version bytes are small numbers (Brood War is 211).</p>

<div class="hdr">Public status endpoint</div>
<label for="pl">Public status address (empty = disabled)</label>
<input id="pl" name="public_listen" type="text" value="{public_listen}" placeholder="0.0.0.0:6116">
<div class="row"><input id="pu" name="public_show_users" type="checkbox" {public_users_ck}><label for="pu" class="cb">List online usernames publicly</label></div>

<div class="hdr">Discord updates</div>
<label for="dw">Webhook URL (empty = disabled)</label>
<input id="dw" name="discord_webhook" type="text" value="{d_url}" placeholder="https://discord.com/api/webhooks/…">
<label for="dg">Games webhook URL — separate channel for game announcements (empty = off)</label>
<input id="dg" name="discord_games_webhook" type="text" value="{d_games}" placeholder="https://discord.com/api/webhooks/…">
<label>Status interval (mins) / games window (hrs)</label>
<div class="row2">
<input name="discord_interval" type="number" min="1" value="{d_interval}">
<input name="discord_window" type="number" min="1" value="{d_window}">
</div>
<div class="row"><input id="ds" name="discord_status" type="checkbox" {d_status_ck}><label for="ds" class="cb">Post periodic status</label></div>
<div class="row"><input id="de" name="discord_events" type="checkbox" {d_events_ck}><label for="de" class="cb">Post start / stop / restart events</label></div>
<div class="row"><input id="dm" name="discord_milestones" type="checkbox" {d_miles_ck}><label for="dm" class="cb">Post milestones (new peak)</label></div>

<div class="hdr">Push stats to your site</div>
<label for="su">Endpoint URL (empty = disabled)</label>
<input id="su" name="stats_url" type="text" value="{s_url}" placeholder="https://mysite.com/ingest">
<label for="si">Interval (seconds)</label>
<input id="si" name="stats_interval" type="number" min="5" value="{s_interval}">
<label for="st">Bearer token (optional)</label>
<input id="st" name="stats_token" type="text" value="{s_token}">
<div class="row"><input id="siu" name="stats_users" type="checkbox" {s_users_ck}><label for="siu" class="cb">Include online usernames in the push</label></div>

<div class="hdr">Server tracking</div>
<label for="tad">Advertise to trackers — host or host:port (comma/newline; empty = don't advertise)</label>
<textarea id="tad" name="tracker_advertise_to" rows="2" style="width:100%;box-sizing:border-box" placeholder="tracker.pvpgn.org, track.muleslow.net">{t_advertise}</textarea>
<label for="tds">Description (empty = use the server name)</label>
<input id="tds" name="tracker_description" type="text" value="{t_desc}">
<label for="tu">URL / contact name / contact email (shown on lists)</label>
<input id="tu" name="tracker_url" type="text" value="{t_url}" placeholder="https://us.bnet.cc">
<div class="row2">
<input name="tracker_contact_name" type="text" value="{t_cname}" placeholder="Contact name">
<input name="tracker_contact_email" type="text" value="{t_cemail}" placeholder="Contact email">
</div>
<label for="ti">Advertise interval (seconds, min 30)</label>
<input id="ti" name="tracker_interval" type="number" min="30" value="{t_interval}">
<label for="th">Host a tracker — receive beacons (UDP) / serve the list (HTTP); both empty = off</label>
<div class="row2">
<input id="th" name="tracker_host_listen" type="text" value="{t_host}" placeholder="0.0.0.0:6114">
<input name="tracker_list_listen" type="text" value="{t_list}" placeholder="0.0.0.0:8080">
</div>
<label for="tp">Prune a listed server after (seconds without a beacon)</label>
<input id="tp" name="tracker_prune" type="number" min="1" value="{t_prune}">
<p class="muted">Port 80 needs root; bind a high port (e.g. 8080) and forward 80&#8594;8080 at your router.</p>

<div class="hdr">Server role</div>
<label for="role">This server is a…</label>
<select id="role" name="role">
<option value="master" {master_sel}>Master (standalone)</option>
<option value="node" {node_sel}>Node (federated to a master)</option>
</select>
<label for="hub">Master address (for Node mode)</label>
<input id="hub" name="hub" type="text" value="{hub}" placeholder="hub.example.net:7112">
<label for="ik">Identity key path</label>
<input id="ik" name="identity_key" type="text" value="{identity_key}" placeholder="node.key">
<div class="row"><input id="ol" name="offline_login" type="checkbox" {offline_login_ck}><label for="ol" class="cb">Allow cached-credential logins while the hub is unreachable</label></div>
<p class="muted">Federation is not active yet (Phase 2) — this only records the intended role/master; a Node will run standalone until federation ships.</p>

<button type="submit" style="margin-top:14px">Save configuration</button></form>
<p class="muted">Changes take effect after a restart. A backup of the previous config is kept as <code>{bak}</code>, and an invalid value is rejected before anything is written.</p>

<h1 style="margin-top:28px">Server control</h1>
<form method="post" action="/restart" onsubmit="return confirm('Restart the server now? Connected clients will be dropped for a moment.')">
<p class="muted">Restarts the daemon to apply configuration changes. Only comes back automatically when started via the launcher; a standalone server will stop until you start it again.</p>
<button type="submit" style="background:#7a2323;border:1px solid #a33">Restart server</button></form>
<p style="margin-top:18px"><a href="/">Dashboard</a> &middot; <a href="/help">Help</a> &middot; <a href="/change-password">Change password</a></p>
<form method="post" action="/logout"><button type="submit" style="background:transparent;border:1px solid var(--line);color:var(--muted)">Sign out</button></form>"#,
        bak = html_escape(&format!("{}.bak", config_path.display())),
    );
    shell("Settings — Command Center", &inner)
}

/// `POST /settings/config` — apply the edited server settings to the config file.
///
/// Every change is staged in a comment-preserving document, the whole result is validated by
/// deserialising it as a [`Config`], the previous file is copied to `<path>.bak`, and only
/// then is the new file written. A malformed value therefore never reaches disk, so it can
/// never brick the next restart.
fn do_settings_config(req: &Request, admin: &Admin, config_path: &Path) -> Response {
    let mut doc = load_config_doc(config_path);

    // Simple string fields, written verbatim (trimmed). `server_mode` and `telnet_access`
    // come from <select>s; their values are validated as part of the whole-document check
    // below (a bad mode, e.g., is caught by `Config::validate`).
    for &(field, path) in &[
        ("server_name", &["server", "name"][..]),
        ("server_mode", &["server", "mode"][..]),
        ("server_wc3_logon", &["server", "wc3_logon"][..]),
        ("server_motd", &["server", "motd"][..]),
        ("discord_games_webhook", &["discord", "games_webhook_url"][..]),
        ("d2_description", &["diablo2", "description"][..]),
        ("d2_address", &["diablo2", "address"][..]),
        ("listen_bncs", &["listen", "bncs"][..]),
        ("listen_admin", &["listen", "admin"][..]),
        ("status_listen", &["status", "listen"][..]),
        ("telnet_access", &["channels", "telnet_access"][..]),
        ("storage_path", &["storage", "path"][..]),
        ("files_dir", &["files", "dir"][..]),
        ("public_listen", &["status", "public_listen"][..]),
        ("discord_webhook", &["discord", "webhook_url"][..]),
        ("stats_url", &["stats_push", "url"][..]),
        ("stats_token", &["stats_push", "token"][..]),
        ("tracker_description", &["tracker", "description"][..]),
        ("tracker_url", &["tracker", "url"][..]),
        ("tracker_contact_name", &["tracker", "contact_name"][..]),
        ("tracker_contact_email", &["tracker", "contact_email"][..]),
        ("tracker_host_listen", &["tracker", "host_listen"][..]),
        ("tracker_list_listen", &["tracker", "list_listen"][..]),
        ("hub", &["federation", "hub"][..]),
        ("identity_key", &["federation", "identity_key"][..]),
    ] {
        if let Some(v) = req.form.get(field) {
            set_cfg(&mut doc, path, toml_edit::value(v.trim()));
        }
    }

    // Required integer fields (always present): reject a non-numeric entry with a readable
    // message before touching the document, rather than leaning on the deserialiser's terser
    // error. A slice (not a fixed array) so entries can be added without counting them.
    let ints: &[(&str, &[&str])] = &[
        ("private_max", &["channels", "private_max"]),
        ("public_max", &["channels", "public_max"]),
        ("clan_max", &["channels", "clan_max"]),
        ("accept_shards", &["listen", "accept_shards"]),
        ("max_connections", &["limits", "max_connections"]),
        ("max_frame_bytes", &["limits", "max_frame_bytes"]),
        ("max_line_bytes", &["limits", "max_line_bytes"]),
        ("outbound_queue", &["limits", "outbound_queue"]),
        ("handshake_timeout_secs", &["limits", "handshake_timeout_secs"]),
        ("idle_timeout_secs", &["limits", "idle_timeout_secs"]),
        ("game_per_ip", &["limits", "clients", "game_default", "per_ip"]),
        ("gateway_per_ip", &["limits", "clients", "gateway", "per_ip"]),
        ("bnftp_per_ip", &["limits", "clients", "bnftp", "per_ip"]),
        ("discord_interval", &["discord", "status_interval_mins"]),
        ("discord_window", &["discord", "games_window_hours"]),
        ("stats_interval", &["stats_push", "interval_secs"]),
        ("tracker_interval", &["tracker", "advertise_interval_secs"]),
        ("tracker_prune", &["tracker", "prune_after_secs"]),
        ("d2_max_characters", &["diablo2", "max_characters"]),
    ];
    for &(field, path) in ints {
        if let Some(raw) = req.form.get(field) {
            let raw = raw.trim();
            match raw.parse::<i64>() {
                Ok(n) if n >= 0 => set_cfg(&mut doc, path, toml_edit::value(n)),
                _ => {
                    return html_page(settings_page(
                        admin,
                        config_path,
                        Some((false, &format!("'{field}' must be a whole number ≥ 0 (got '{raw}')."))),
                    ))
                }
            }
        }
    }

    // Optional integer fields: blank clears the key (so the built-in / mode default applies
    // again) rather than pinning a value; a number sets it.
    let opt_ints: &[(&str, &[&str])] = &[
        ("game_per_account", &["limits", "clients", "game_default", "per_account"]),
        ("game_global", &["limits", "clients", "game_default", "global"]),
        ("gateway_per_account", &["limits", "clients", "gateway", "per_account"]),
        ("gateway_global", &["limits", "clients", "gateway", "global"]),
        ("bnftp_per_account", &["limits", "clients", "bnftp", "per_account"]),
        ("bnftp_global", &["limits", "clients", "bnftp", "global"]),
        ("pending_per_ip", &["limits", "clients", "game_pending", "per_ip"]),
        ("pending_per_account", &["limits", "clients", "game_pending", "per_account"]),
        ("pending_global", &["limits", "clients", "game_pending", "global"]),
    ];
    for &(field, path) in opt_ints {
        if let Some(raw) = req.form.get(field) {
            let raw = raw.trim();
            if raw.is_empty() {
                unset_cfg(&mut doc, path);
            } else {
                match raw.parse::<i64>() {
                    Ok(n) if n >= 0 => set_cfg(&mut doc, path, toml_edit::value(n)),
                    _ => {
                        return html_page(settings_page(
                            admin,
                            config_path,
                            Some((false, &format!("'{field}' must be a whole number ≥ 0 or blank (got '{raw}')."))),
                        ))
                    }
                }
            }
        }
    }

    // Boolean (checkbox) fields — present in the form only when ticked.
    for (field, path) in [
        ("public_show_users", &["status", "public_show_users"][..]),
        ("discord_status", &["discord", "post_status"][..]),
        ("discord_events", &["discord", "post_events"][..]),
        ("discord_milestones", &["discord", "post_milestones"][..]),
        ("stats_users", &["stats_push", "include_users"][..]),
        ("auto_op_private", &["channels", "auto_op_private"][..]),
        ("cd_key_uniqueness", &["limits", "cd_key_uniqueness"][..]),
        ("versions_restrict", &["versions", "restrict"][..]),
        ("offline_login", &["federation", "offline_login"][..]),
        ("d2_realm", &["diablo2", "realm"][..]),
    ] {
        set_cfg(&mut doc, path, toml_edit::value(checkbox(&req.form, field)));
    }

    // Server role: Master = federation off, Node = federation on (needs a hub, enforced by
    // the Config validation below with a friendlier pre-check).
    let is_node = req.form.get("role").map(String::as_str) == Some("node");
    set_cfg(&mut doc, &["federation", "enabled"], toml_edit::value(is_node));
    if is_node && req.form.get("hub").map_or(true, |h| h.trim().is_empty()) {
        return html_page(settings_page(
            admin,
            config_path,
            Some((false, "Node mode needs a master (hub) address.")),
        ));
    }

    // String-array fields (comma/newline separated).
    for &(field, path) in &[
        ("admins", &["admins", "accounts"][..]),
        ("gateway_allowlist", &["limits", "gateway_allowlist"][..]),
        ("tracker_advertise_to", &["tracker", "advertise_to"][..]),
    ] {
        if let Some(v) = req.form.get(field) {
            set_cfg(&mut doc, path, toml_edit::value(str_array_field(v)));
        }
    }

    // Per-product per-IP overrides come from a fixed pick-list of known products (so no one
    // can hand-type a bad FourCC like "TELNET"). Rebuild the products table: keep any existing
    // override for a product NOT in the list (a hand-added exotic one) verbatim, and for each
    // known product start from its existing entry so a per_account/global set in the file
    // survives editing per_ip here; a blank per_ip clears just that key.
    {
        let mut table = toml_edit::Table::new();
        if let Some(existing) =
            cfg_get(&doc, &["limits", "clients", "products"]).and_then(toml_edit::Item::as_table)
        {
            for (code, item) in existing.iter() {
                if !KNOWN_PRODUCTS.iter().any(|(k, _)| k.eq_ignore_ascii_case(code)) {
                    table.insert(code, item.clone());
                }
            }
        }
        for (code, _) in KNOWN_PRODUCTS {
            let raw = req.form.get(&format!("prod_{code}")).map(|s| s.trim()).unwrap_or("");
            let mut entry = cfg_get(&doc, &["limits", "clients", "products", code])
                .and_then(toml_edit::Item::as_table)
                .cloned()
                .unwrap_or_default();
            if raw.is_empty() {
                entry.remove("per_ip");
            } else {
                match raw.parse::<i64>() {
                    Ok(n) if n >= 0 => entry["per_ip"] = toml_edit::value(n),
                    _ => {
                        return html_page(settings_page(
                            admin,
                            config_path,
                            Some((false, &format!("Per-IP limit for {code} must be a whole number ≥ 0."))),
                        ))
                    }
                }
            }
            // Drop the product entirely when nothing is left, so no empty tables accumulate.
            if !entry.is_empty() {
                table.insert(code, toml_edit::Item::Table(entry));
            }
        }
        set_cfg(&mut doc, &["limits", "clients", "products"], toml_edit::Item::Table(table));
    }

    // Per-product default channel: rebuild `[channels.default_channel]` from `PRODUCT = Channel`
    // lines. An empty box clears the table.
    if let Some(v) = req.form.get("default_channels") {
        let mut table = toml_edit::Table::new();
        for line in v.split(['\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
            let Some((prod, chan)) = line.split_once('=') else {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Default-channel line '{line}' needs 'PRODUCT = Channel name'."))),
                ));
            };
            let prod = prod.trim().to_ascii_uppercase();
            let chan = chan.trim();
            if prod.is_empty() || chan.is_empty() {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Default-channel line '{line}' needs a product and a channel name."))),
                ));
            }
            table.insert(&prod, toml_edit::value(chan));
        }
        set_cfg(&mut doc, &["channels", "default_channel"], toml_edit::Item::Table(table));
    }

    // Version allow-list: rebuild `[versions.allowed]` from `PRODUCT PLATFORM = byte, byte`
    // lines. An empty box clears it. Bad FourCCs and the "restrict on with no entries" case
    // are caught by `version_policy()` in the validation block below.
    if let Some(v) = req.form.get("versions_allowed") {
        let mut allowed = toml_edit::Table::new();
        for line in v.split(['\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
            let Some((lhs, rhs)) = line.split_once('=') else {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Version line '{line}' needs 'PRODUCT PLATFORM = byte, byte'."))),
                ));
            };
            let mut names = lhs.split_whitespace();
            let (Some(prod), Some(plat), None) = (names.next(), names.next(), names.next()) else {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Version line '{line}': the left of '=' must be exactly a product and a platform code."))),
                ));
            };
            let mut arr = toml_edit::Array::new();
            for b in rhs.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match b.parse::<i64>() {
                    Ok(n) if n >= 0 => arr.push(n),
                    _ => {
                        return html_page(settings_page(
                            admin,
                            config_path,
                            Some((false, &format!("Version line '{line}': '{b}' must be a whole number ≥ 0."))),
                        ))
                    }
                }
            }
            let prod_up = prod.to_ascii_uppercase();
            let plat_up = plat.to_ascii_uppercase();
            let prod_tbl = allowed
                .entry(prod_up.as_str())
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
            if !prod_tbl.is_table() {
                *prod_tbl = toml_edit::Item::Table(toml_edit::Table::new());
            }
            prod_tbl
                .as_table_mut()
                .expect("just ensured a table")[plat_up.as_str()] = toml_edit::value(arr);
        }
        set_cfg(&mut doc, &["versions", "allowed"], toml_edit::Item::Table(allowed));
    }

    // Defined channels: rebuild `[[channels.defined]]` from the textarea. Each existing
    // channel is looked up by name and its table reused, so options the editor doesn't show
    // (topic, min_flag, telnet, game_only) survive; a channel dropped from the box is removed.
    if let Some(v) = req.form.get("channels_defined") {
        let mut existing: HashMap<String, toml_edit::Table> = HashMap::new();
        if let Some(aot) = cfg_get(&doc, &["channels", "defined"]).and_then(toml_edit::Item::as_array_of_tables) {
            for t in aot.iter() {
                if let Some(name) = t.get("name").and_then(toml_edit::Item::as_str) {
                    existing.insert(name.to_ascii_lowercase(), t.clone());
                }
            }
        }
        let mut aot = toml_edit::ArrayOfTables::new();
        for line in v.split(['\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
            let mut parts = line.split('|').map(str::trim);
            let name = parts.next().unwrap_or("");
            if name.is_empty() {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Channel line '{line}' needs a name before the first '|'."))),
                ));
            }
            let mut t = existing.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default();
            t["name"] = toml_edit::value(name);
            // max_users
            match parts.next().unwrap_or("") {
                "" => { t.remove("max_users"); }
                m => match m.parse::<i64>() {
                    Ok(n) if n >= 0 => t["max_users"] = toml_edit::value(n),
                    _ => {
                        return html_page(settings_page(
                            admin,
                            config_path,
                            Some((false, &format!("Channel '{name}': max must be a whole number ≥ 0."))),
                        ))
                    }
                },
            }
            // products (comma list)
            let prods = parts.next().unwrap_or("");
            if prods.is_empty() {
                t.remove("products");
            } else {
                let mut arr = toml_edit::Array::new();
                for p in prods.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    arr.push(p.to_ascii_uppercase());
                }
                t["products"] = toml_edit::value(arr);
            }
            // public / persist (yes/no; blank leaves default)
            for (field, raw) in [("public", parts.next()), ("persist", parts.next())] {
                match raw.and_then(parse_yesno) {
                    Some(b) => t[field] = toml_edit::value(b),
                    None => { t.remove(field); }
                }
            }
            aot.push(t);
        }
        set_cfg(&mut doc, &["channels", "defined"], toml_edit::Item::ArrayOfTables(aot));
    }

    // Validate the whole document before writing, running exactly the checks the daemon runs
    // at startup so a value that would crash the next start is rejected here instead. Parsing
    // as a `Config` catches structural/type/range errors (a bad socket address, a negative
    // where unsigned is expected); `validate()` adds mode, policy (product FourCCs),
    // outbound_queue, frame size and federation-needs-a-hub; `version_policy()` adds the
    // version-code checks; `channel_rules()` adds telnet_access and the channel definitions.
    let serialized = doc.to_string();
    let parsed = match toml::from_str::<Config>(&serialized) {
        Ok(cfg) => cfg,
        Err(e) => {
            return html_page(settings_page(
                admin,
                config_path,
                Some((false, &format!("Rejected — not a valid config: {e}"))),
            ))
        }
    };
    if let Err(e) = parsed.validate() {
        return html_page(settings_page(admin, config_path, Some((false, &format!("Rejected — {e}")))));
    }
    if let Err(e) = parsed.version_policy() {
        return html_page(settings_page(admin, config_path, Some((false, &format!("Rejected — {e}")))));
    }
    if let Err(e) = parsed.channel_rules() {
        return html_page(settings_page(admin, config_path, Some((false, &format!("Rejected — {e}")))));
    }

    // Back up the current file (best-effort — absent on a first-ever save), then write.
    let _ = std::fs::copy(config_path, config_path.with_extension("toml.bak"));
    if let Err(e) = std::fs::write(config_path, serialized.as_bytes()) {
        return html_page(settings_page(
            admin,
            config_path,
            Some((false, &format!("Could not write the config file: {e}"))),
        ));
    }

    html_page(settings_page(
        admin,
        config_path,
        Some((true, "Saved. Restart the server (below) to apply these changes.")),
    ))
}

/// Read the config file into an editable, comment-preserving document, or an empty one if it
/// does not exist or fails to parse (a fresh save then writes a minimal valid file).
fn load_config_doc(path: &Path) -> toml_edit::DocumentMut {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
        .unwrap_or_default()
}

/// Follow a dotted path of table keys to an item, if present.
fn cfg_get<'a>(doc: &'a toml_edit::DocumentMut, path: &[&str]) -> Option<&'a toml_edit::Item> {
    let (last, tables) = path.split_last()?;
    let mut tbl = doc.as_table();
    for seg in tables {
        tbl = tbl.get(seg)?.as_table()?;
    }
    tbl.get(last)
}

fn cfg_string(doc: &toml_edit::DocumentMut, path: &[&str], default: &str) -> String {
    cfg_get(doc, path)
        .and_then(|i| i.as_str())
        .map_or_else(|| default.to_string(), str::to_string)
}

fn cfg_int(doc: &toml_edit::DocumentMut, path: &[&str], default: i64) -> i64 {
    cfg_get(doc, path).and_then(|i| i.as_integer()).unwrap_or(default)
}

fn cfg_bool(doc: &toml_edit::DocumentMut, path: &[&str], default: bool) -> bool {
    cfg_get(doc, path).and_then(|i| i.as_bool()).unwrap_or(default)
}

/// `on`/absent checkbox helper: an HTML checkbox is present in the form only when ticked.
fn checkbox(form: &HashMap<String, String>, field: &str) -> bool {
    form.get(field).map(String::as_str) == Some("on")
}

/// Render `[[channels.defined]]` as one `name | max | products | public | persist` line per
/// channel, for the settings textarea.
fn cfg_channels_text(doc: &toml_edit::DocumentMut) -> String {
    let Some(aot) = cfg_get(doc, &["channels", "defined"]).and_then(toml_edit::Item::as_array_of_tables) else {
        return String::new();
    };
    aot.iter()
        .map(|t| {
            let name = t.get("name").and_then(toml_edit::Item::as_str).unwrap_or("");
            let max = t.get("max_users").and_then(toml_edit::Item::as_integer).map(|n| n.to_string()).unwrap_or_default();
            let products = t
                .get("products")
                .and_then(toml_edit::Item::as_array)
                .map(|a| a.iter().filter_map(toml_edit::Value::as_str).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            let yn = |k: &str| match t.get(k).and_then(toml_edit::Item::as_bool) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "",
            };
            format!("{name} | {max} | {products} | {} | {}", yn("public"), yn("persist"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a yes/no field: `yes`/`y`/`true` → Some(true), `no`/`n`/`false` → Some(false),
/// blank/other → None (leave the setting at its default).
fn parse_yesno(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" => Some(true),
        "no" | "n" | "false" => Some(false),
        _ => None,
    }
}

fn cfg_admin_list(doc: &toml_edit::DocumentMut) -> String {
    cfg_str_array(doc, &["admins", "accounts"])
}

/// Read a TOML array of strings at `path` as a comma-joined string, for a text field.
fn cfg_str_array(doc: &toml_edit::DocumentMut, path: &[&str]) -> String {
    cfg_get(doc, path)
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(toml_edit::Value::as_str).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// Parse a comma/newline-separated field into a TOML string array, dropping blanks.
fn str_array_field(raw: &str) -> toml_edit::Array {
    let mut arr = toml_edit::Array::new();
    for item in raw.split([',', '\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
        arr.push(item);
    }
    arr
}

/// Set `path` (dotted table keys ending in the key to set) to `value`, creating any missing
/// intermediate tables. An intermediate that is somehow not a table is replaced with one, so
/// this never panics on a hand-edited file.
fn set_cfg(doc: &mut toml_edit::DocumentMut, path: &[&str], value: toml_edit::Item) {
    let (last, tables) = path.split_last().expect("config path is non-empty");
    let mut tbl = doc.as_table_mut();
    for seg in tables {
        let entry = tbl
            .entry(seg)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if !entry.is_table() {
            *entry = toml_edit::Item::Table(toml_edit::Table::new());
        }
        tbl = entry.as_table_mut().expect("just ensured a table");
    }
    tbl[last] = value;
}

/// Remove the key at `path` if it (and every table on the way) exists. Used for optional
/// fields where a blank form entry means "unset, fall back to the default".
fn unset_cfg(doc: &mut toml_edit::DocumentMut, path: &[&str]) {
    let Some((last, tables)) = path.split_last() else { return };
    let mut tbl = doc.as_table_mut();
    for seg in tables {
        match tbl.get_mut(seg).and_then(toml_edit::Item::as_table_mut) {
            Some(t) => tbl = t,
            None => return,
        }
    }
    tbl.remove(last);
}

/// Read an optional integer field as a string: the value if present, else blank (so the form
/// shows an empty box rather than a fabricated default it would then pin on save).
fn cfg_opt_int(doc: &toml_edit::DocumentMut, path: &[&str]) -> String {
    cfg_get(doc, path)
        .and_then(toml_edit::Item::as_integer)
        .map(|n| n.to_string())
        .unwrap_or_default()
}

/// Render `<option>`s for a `<select>`, marking the one matching `current` (case-insensitive)
/// as selected. `opts` is `(value, label)` pairs.
fn select_options(current: &str, opts: &[(&str, &str)]) -> String {
    opts.iter()
        .map(|(val, label)| {
            let sel = if current.eq_ignore_ascii_case(val) { " selected" } else { "" };
            format!(r#"<option value="{val}"{sel}>{label}</option>"#)
        })
        .collect()
}

/// Render `[channels.default_channel]` as `PRODUCT = Channel` lines for the settings textarea.
/// Reads any table-like representation so a hand-written inline table round-trips safely.
fn cfg_default_channel_text(doc: &toml_edit::DocumentMut) -> String {
    let Some(t) = cfg_get(doc, &["channels", "default_channel"]).and_then(toml_edit::Item::as_table_like)
    else {
        return String::new();
    };
    t.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| format!("{k} = {s}")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render `[versions.allowed]` as `PRODUCT PLATFORM = byte, byte` lines for the settings
/// textarea. Reads any table-like representation so an inline table round-trips safely.
fn cfg_versions_text(doc: &toml_edit::DocumentMut) -> String {
    let Some(t) = cfg_get(doc, &["versions", "allowed"]).and_then(toml_edit::Item::as_table_like)
    else {
        return String::new();
    };
    let mut lines = Vec::new();
    for (prod, platforms) in t.iter() {
        let Some(pt) = platforms.as_table_like() else { continue };
        for (plat, bytes) in pt.iter() {
            let list = bytes
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(toml_edit::Value::as_integer)
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            lines.push(format!("{prod} {plat} = {list}"));
        }
    }
    lines.join("\n")
}

/// Shown after a restart is requested: it polls the panel and reloads once it answers again
/// (which it will only do if the launcher-supervisor relaunched the daemon).
const RESTARTING_PAGE: &str = r#"<h1>Restarting…</h1>
<p class="sub">The server is restarting to apply changes.</p>
<p class="muted" id="msg">Waiting for it to come back…</p>
<script>
async function poll() {
  try {
    const r = await fetch('/status.json', {cache:'no-store'});
    if (r.ok) { location.href = '/'; return; }
  } catch (e) {}
  setTimeout(poll, 1500);
}
setTimeout(poll, 2500);
</script>"#;

/// The dashboard page. Self-contained (no external assets): it polls `/status.json` every
/// two seconds and re-renders. Served same-origin, so the fetch has no CORS concerns.
const DASHBOARD: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Command Center — Status</title>
<style>
  :root { color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; }
  * { box-sizing: border-box; }
  body { margin:0; font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; background:var(--bg); color:var(--fg); }
  header { padding:16px 20px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:12px; flex-wrap:wrap; }
  header h1 { font-size:16px; margin:0; font-weight:600; }
  header .name { color:var(--accent); }
  header .meta { color:var(--muted); font-size:12px; }
  header nav { margin-left:auto; }
  header nav a { color:var(--accent); text-decoration:none; font-size:13px; }
  header nav a:hover { text-decoration:underline; }
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
  <h1>Command Center · <span class="name" id="server">…</span></h1>
  <span class="meta" id="meta"></span>
  <nav><a href="/d2">D2 map</a> · <a href="/d2/season">D2 ladder</a> · <a href="/users">Users</a> · <a href="/settings">Settings</a> · <a href="/help">Help</a> · <a href="/change-password">Password</a></nav>
</header>
<main>
  <div class="tiles">
    <div class="tile"><div class="n num" id="t-conns">–</div><div class="l">Connections</div></div>
    <div class="tile"><div class="n num" id="t-peak">–</div><div class="l">Peak</div></div>
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
    $('meta').textContent = 'v' + (d.version || '?') + ' · uptime ' + fmtDur(d.uptime_secs) +
      ' · updated ' + new Date().toLocaleTimeString();
    $('meta').classList.remove('stale');
    $('t-conns').textContent = d.connections;
    $('t-peak').textContent = d.peak_connections;
    $('t-users').textContent = d.online_users;
    $('t-channels').textContent = d.channels.length;
    $('t-games').textContent = d.games.length;
    $('ch-count').textContent = d.channels.length + ' active';
    $('gm-count').textContent = d.games.length + ' advertised';

    $('channels').innerHTML = d.channels.length ? (
      '<table><thead><tr><th>Channel</th><th>Users</th><th>Operator</th><th>Names</th></tr></thead><tbody>' +
      d.channels.map(c => '<tr><td>'+esc(c.name)+'</td><td class="num">'+c.user_count+
        '</td><td class="users">'+(c.operator?esc(c.operator):'—')+
        '</td><td class="users">'+esc((c.users||[]).join(', '))+'</td></tr>').join('') +
      '</tbody></table>'
    ) : '<div class="empty">No active channels.</div>';

    $('games').innerHTML = d.games.length ? (
      '<table><thead><tr><th>Game</th><th>Host</th><th>Type</th><th>Age</th></tr></thead><tbody>' +
      d.games.map(g => '<tr><td>'+esc(g.name)+(g.has_password?' 🔒':'')+
        '</td><td class="users">'+esc(g.host_ip)+':'+g.port+
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

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("bnetccd-settings-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    fn post(form: &[(&str, &str)]) -> Request {
        Request {
            method: "POST".into(),
            path: "/settings/config".into(),
            query: HashMap::new(),
            cookies: HashMap::new(),
            form: form.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        }
    }

    /// A full submission round-trips: it saves, the file reloads as a valid `Config` with the
    /// expected values, hand-written comments and unknown per-product overrides survive, and
    /// the textarea readers reproduce what was saved.
    #[test]
    fn a_full_settings_post_round_trips() {
        let dir = temp_dir("roundtrip");
        let admin = Admin::load_or_init(&dir).expect("admin");
        let cfg_path = dir.join("bnetccd.toml");
        // A hand-written starting file: a comment to preserve, an unknown (exotic) product
        // override to preserve, and a known product with a per_account we must not clobber.
        std::fs::write(
            &cfg_path,
            "# keep this comment\n\
             [server]\nname = \"Old\"\n\n\
             [limits.clients.products.ZZZZ]\nper_ip = 5\n\n\
             [limits.clients.products.WAR3]\nper_ip = 2\nper_account = 1\n",
        )
        .expect("write");

        let resp = do_settings_config(
            &post(&[
                ("server_name", "New Name"),
                ("server_mode", "warnet"),
                ("server_motd", "hi"),
                ("listen_bncs", "0.0.0.0:6112"),
                ("status_listen", "127.0.0.1:6114"),
                ("telnet_access", "all"),
                ("storage_path", "bnetccd.db"),
                ("role", "master"),
                ("max_frame_bytes", "8192"),
                ("outbound_queue", "64"),
                ("gateway_per_ip", "1"),
                ("game_per_ip", "8"),
                ("bnftp_per_ip", "4"),
                ("gateway_global", "256"),   // optional int -> set
                ("game_per_account", ""),    // optional int -> unset (never set here)
                ("pending_per_ip", ""),      // optional int -> unset
                ("prod_WAR3", "3"),          // change per_ip; per_account must survive
                ("prod_STAR", ""),           // no override
                ("auto_op_private", "on"),
                ("cd_key_uniqueness", "on"),
                ("versions_restrict", "on"),
                ("versions_allowed", "SEXP IX86 = 211, 210\nW2BN IX86 = 79"),
                ("default_channels", "STAR = StarCraft\nW2BN = War2 BNE"),
                ("gateway_allowlist", "203.0.113.4, 198.51.100.9"),
                ("tracker_advertise_to", "tracker.pvpgn.org, track.muleslow.net"),
                ("tracker_interval", "300"),
                ("tracker_prune", "600"),
                ("admins", "Alice, Bob"),
            ]),
            &admin,
            &cfg_path,
        );
        assert!(resp.body.contains("Saved"), "expected a success flash, got: {}", resp.body);

        // Reloads as a valid Config with the edited values.
        let cfg = Config::load(&cfg_path).expect("reloads as valid config");
        assert_eq!(cfg.server.name, "New Name");
        assert_eq!(cfg.server.mode, "warnet");
        assert_eq!(cfg.channels.telnet_access, "all");
        assert!(cfg.channels.auto_op_private);
        assert!(cfg.limits.cd_key_uniqueness);
        assert!(cfg.versions.restrict);
        assert_eq!(cfg.limits.gateway_allowlist.len(), 2);
        assert_eq!(cfg.tracker.advertise_to, vec!["tracker.pvpgn.org", "track.muleslow.net"]);
        assert_eq!(cfg.admins.accounts, vec!["Alice", "Bob"]);
        assert_eq!(cfg.channels.default_channel.get("STAR").map(String::as_str), Some("StarCraft"));

        // The known product's per_ip changed but its per_account survived; the unknown
        // product override survived untouched.
        let war3 = cfg.limits.clients.products.get("WAR3").expect("WAR3 override");
        assert_eq!(war3.per_ip, Some(3));
        assert_eq!(war3.per_account, Some(1), "per_account must survive editing per_ip");
        let zzzz = cfg.limits.clients.products.get("ZZZZ").expect("exotic override kept");
        assert_eq!(zzzz.per_ip, Some(5));

        // The optional gateway.global was set; version policy resolves.
        assert_eq!(cfg.limits.clients.gateway.global, Some(256));
        cfg.version_policy().expect("version policy resolves");
        cfg.channel_rules().expect("channel rules resolve");

        // The hand-written comment survived the comment-preserving edit.
        let on_disk = std::fs::read_to_string(&cfg_path).expect("read back");
        assert!(on_disk.contains("# keep this comment"), "comment lost:\n{on_disk}");

        // The textarea readers reproduce what was saved (so a re-open + re-save is stable).
        let doc = load_config_doc(&cfg_path);
        assert_eq!(cfg_versions_text(&doc), "SEXP IX86 = 211, 210\nW2BN IX86 = 79");
        assert_eq!(cfg_default_channel_text(&doc), "STAR = StarCraft\nW2BN = War2 BNE");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bad value is rejected before anything is written, and the on-disk file is untouched.
    #[test]
    fn an_invalid_value_is_rejected_without_writing() {
        let dir = temp_dir("reject");
        let admin = Admin::load_or_init(&dir).expect("admin");
        let cfg_path = dir.join("bnetccd.toml");
        std::fs::write(&cfg_path, "[server]\nname = \"Keep\"\n").expect("write");

        // restrict on with no version entries would refuse every client -> version_policy err.
        let resp = do_settings_config(
            &post(&[("versions_restrict", "on"), ("versions_allowed", "")]),
            &admin,
            &cfg_path,
        );
        assert!(resp.body.contains("Rejected"), "expected rejection, got: {}", resp.body);
        // File is unchanged.
        let on_disk = std::fs::read_to_string(&cfg_path).expect("read");
        assert!(on_disk.contains("name = \"Keep\""), "file must be untouched: {on_disk}");

        // A bad mode is rejected too.
        let resp = do_settings_config(&post(&[("server_mode", "nonsense")]), &admin, &cfg_path);
        assert!(resp.body.contains("Rejected"), "bad mode should be rejected: {}", resp.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn helpers_behave() {
        assert_eq!(
            select_options("both", &[("gaming", "G"), ("both", "B")]),
            r#"<option value="gaming">G</option><option value="both" selected>B</option>"#
        );
        // Optional-int unset then set on a fresh doc.
        let mut doc = toml_edit::DocumentMut::new();
        set_cfg(&mut doc, &["limits", "clients", "gateway", "global"], toml_edit::value(9i64));
        assert_eq!(cfg_opt_int(&doc, &["limits", "clients", "gateway", "global"]), "9");
        unset_cfg(&mut doc, &["limits", "clients", "gateway", "global"]);
        assert_eq!(cfg_opt_int(&doc, &["limits", "clients", "gateway", "global"]), "");
        // Unsetting through a missing path is a no-op, not a panic.
        unset_cfg(&mut doc, &["nope", "missing", "key"]);
    }
}

#[cfg(test)]
mod season_tests {
    use super::*;

    fn body(r: &Response) -> String {
        r.body.clone()
    }

    /// The ladder page shows the season and its ladder characters; its button ends the season,
    /// which empties the ladder and shows the next season.
    #[tokio::test]
    async fn the_season_page_ends_the_season() {
        use bnetcc_proto::d2::status::{HARDCORE, LADDER};
        use bnetcc_storage::model::Credential;
        let node = crate::node::test_node();
        let acct = node.create_account("Owner", Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
        for (name, status) in [("Soft", LADDER), ("Hard", LADDER | HARDCORE), ("Plain", 0)] {
            let c = bnetcc_storage::Character { account: acct.id, name: name.into(), class: 1, status, level: 1, progression: 0, created_at: 0, last_played: 0, save: None };
            node.create_character(c).await.unwrap();
        }
        let page = body(&season_page(&node, None).await);
        assert!(page.contains("Season 1<") && page.contains(r#"<div class="big">1</div><div class="sub">softcore"#), "{page}");
        assert!(page.contains(r#"<div class="big">1</div><div class="sub">hardcore"#));

        let ended = body(&do_end_season(&node).await);
        assert!(ended.contains("Season 1 is over: 1 softcore and 1 hardcore characters are now non-ladder. Season 2 has begun."), "{ended}");
        assert!(ended.contains("Season 2<") && ended.contains(r#"<div class="big">0</div><div class="sub">softcore"#));
        assert!(node.all_characters().await.iter().all(|c| c.status & LADDER == 0));
    }
}
