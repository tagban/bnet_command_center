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
//! `POST /logout`.

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
</table>
<p class="muted">The body is identical to <code>/status.json</code>. Best-effort, on its own task — a slow or failing endpoint never affects the server.</p>

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
    let digest = bnetcc_crypto::password_hash(new);
    let flash = match node.reset_password(id, digest).await {
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
input[type=password], input[type=text], input[type=number], textarea {{ width:100%; padding:10px 12px; border-radius:8px; border:1px solid var(--line); background:#0f1115; color:var(--fg); font-size:14px; font-family:inherit; }}
button {{ margin-top:20px; width:100%; padding:11px; border:none; border-radius:8px; background:var(--accent); color:#04121f; font-weight:600; font-size:14px; cursor:pointer; }}
.err {{ background:rgba(224,106,106,.12); border:1px solid var(--err); color:var(--err); padding:9px 12px; border-radius:8px; font-size:13px; margin-bottom:8px; }}
.ok {{ background:rgba(90,196,125,.12); border:1px solid var(--ok); color:var(--ok); padding:9px 12px; border-radius:8px; font-size:13px; margin-bottom:8px; }}
a {{ color:var(--accent); }} .row {{ display:flex; align-items:center; gap:10px; margin:14px 0; }} .row input {{ width:auto; }}
.row3 {{ display:grid; grid-template-columns:repeat(3,1fr); gap:8px; }}
.hdr {{ margin:24px 0 4px; padding-top:14px; border-top:1px solid var(--line); font-weight:600; font-size:14px; }}
label.cb {{ margin:0; text-transform:none; letter-spacing:0; color:var(--fg); }}
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
        r#"<h1>BNET Command Center</h1><p class="sub">Admin sign-in</p>{}
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
    let motd = html_escape(&cfg_string(&doc, &["server", "motd"], "Welcome to Command Center."));
    let private_max = cfg_int(&doc, &["channels", "private_max"], 0);
    let public_max = cfg_int(&doc, &["channels", "public_max"], 0);
    let clan_max = cfg_int(&doc, &["channels", "clan_max"], 0);
    let max_conn = cfg_int(&doc, &["limits", "max_connections"], 0);
    let game_per_ip = cfg_int(&doc, &["limits", "clients", "game_default", "per_ip"], 8);
    let gw_per_ip = cfg_int(&doc, &["limits", "clients", "gateway", "per_ip"], 1);
    let bnftp_per_ip = cfg_int(&doc, &["limits", "clients", "bnftp", "per_ip"], 4);
    let products = html_escape(&cfg_products_text(&doc));
    let channels_defined = html_escape(&cfg_channels_text(&doc));
    let admins = html_escape(&cfg_admin_list(&doc));
    // Public status
    let public_listen = html_escape(&cfg_string(&doc, &["status", "public_listen"], ""));
    let public_users_ck = if cfg_bool(&doc, &["status", "public_show_users"], false) { "checked" } else { "" };
    // Discord
    let d_url = html_escape(&cfg_string(&doc, &["discord", "webhook_url"], ""));
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
    // Federation role
    let is_node = cfg_bool(&doc, &["federation", "enabled"], false);
    let master_sel = if is_node { "" } else { "selected" };
    let node_sel = if is_node { "selected" } else { "" };
    let hub = html_escape(&cfg_string(&doc, &["federation", "hub"], ""));

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
<p class="sub">Written to the config file; applied after a restart.</p>{flash_html}
<form method="post" action="/settings/config">
<label for="sn">Server name</label>
<input id="sn" name="server_name" type="text" value="{name}" maxlength="64">
<label for="mo">Message of the day</label>
<input id="mo" name="server_motd" type="text" value="{motd}" maxlength="200">
<label for="pm">Channel caps — Private / Public / Clan (0 = unlimited)</label>
<div class="row3">
<input id="pm" name="private_max" type="number" min="0" value="{private_max}">
<input name="public_max" type="number" min="0" value="{public_max}">
<input name="clan_max" type="number" min="0" value="{clan_max}">
</div>
<label for="mc">Max connections (0 = derive from file-descriptor limit)</label>
<input id="mc" name="max_connections" type="number" min="0" value="{max_conn}">
<label>Per-IP limits — game / gateway (telnet) / BNFTP</label>
<div class="row3">
<input name="game_per_ip" type="number" min="0" value="{game_per_ip}">
<input name="gateway_per_ip" type="number" min="0" value="{gw_per_ip}">
<input name="bnftp_per_ip" type="number" min="0" value="{bnftp_per_ip}">
</div>
<label for="po">Per-product overrides — one <code>PRODUCT=limit</code> per line (e.g. <code>DRTL=1</code>)</label>
<textarea id="po" name="product_overrides" rows="3" style="width:100%;box-sizing:border-box" placeholder="DRTL=1&#10;W2BN=8">{products}</textarea>
<label for="ad">Staff accounts (comma or newline separated) — get /tagban, /ipban, /mute</label>
<textarea id="ad" name="admins" rows="2" style="width:100%;box-sizing:border-box">{admins}</textarea>

<div class="hdr">Defined channels</div>
<label for="ch">One per line: <code>name | max | products | public | persist</code></label>
<textarea id="ch" name="channels_defined" rows="4" style="width:100%;box-sizing:border-box" placeholder="War2 BNE | 100 | W2BN | yes | no&#10;Staff | 0 |  | no | yes">{channels_defined}</textarea>
<p class="muted">Blank fields keep the default. <code>products</code> is a comma list of client FourCCs (empty = any). <code>public</code>/<code>persist</code> take yes/no. Other per-channel options (topic, min_flag, telnet) stay in the config file and are preserved on save.</p>

<div class="hdr">Public status endpoint</div>
<label for="pl">Public status address (empty = disabled)</label>
<input id="pl" name="public_listen" type="text" value="{public_listen}" placeholder="0.0.0.0:6116">
<div class="row"><input id="pu" name="public_show_users" type="checkbox" {public_users_ck}><label for="pu" class="cb">List online usernames publicly</label></div>

<div class="hdr">Discord updates</div>
<label for="dw">Webhook URL (empty = disabled)</label>
<input id="dw" name="discord_webhook" type="text" value="{d_url}" placeholder="https://discord.com/api/webhooks/…">
<label>Status interval (mins) / games window (hrs)</label>
<div class="row3">
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

<div class="hdr">Server role</div>
<label for="role">This server is a…</label>
<select id="role" name="role" style="width:100%;padding:10px 12px;border-radius:8px;border:1px solid var(--line);background:#0f1115;color:var(--fg)">
<option value="master" {master_sel}>Master (standalone)</option>
<option value="node" {node_sel}>Node (federated to a master)</option>
</select>
<label for="hub">Master address (for Node mode)</label>
<input id="hub" name="hub" type="text" value="{hub}" placeholder="hub.example.net:7112">
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

    if let Some(v) = req.form.get("server_name") {
        set_cfg(&mut doc, &["server", "name"], toml_edit::value(v.trim()));
    }
    if let Some(v) = req.form.get("server_motd") {
        set_cfg(&mut doc, &["server", "motd"], toml_edit::value(v.trim()));
    }

    // String fields.
    for (field, path) in [
        ("public_listen", &["status", "public_listen"][..]),
        ("discord_webhook", &["discord", "webhook_url"][..]),
        ("stats_url", &["stats_push", "url"][..]),
        ("stats_token", &["stats_push", "token"][..]),
        ("hub", &["federation", "hub"][..]),
    ] {
        if let Some(v) = req.form.get(field) {
            set_cfg(&mut doc, path, toml_edit::value(v.trim()));
        }
    }

    // Integer fields: reject a non-numeric entry with a readable message before touching the
    // document, rather than leaning on the deserialiser's terser error.
    let ints: [(&str, &[&str]); 10] = [
        ("private_max", &["channels", "private_max"]),
        ("public_max", &["channels", "public_max"]),
        ("clan_max", &["channels", "clan_max"]),
        ("max_connections", &["limits", "max_connections"]),
        ("game_per_ip", &["limits", "clients", "game_default", "per_ip"]),
        ("gateway_per_ip", &["limits", "clients", "gateway", "per_ip"]),
        ("bnftp_per_ip", &["limits", "clients", "bnftp", "per_ip"]),
        ("discord_interval", &["discord", "status_interval_mins"]),
        ("discord_window", &["discord", "games_window_hours"]),
        ("stats_interval", &["stats_push", "interval_secs"]),
    ];
    for (field, path) in ints {
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

    // Boolean (checkbox) fields — present in the form only when ticked.
    for (field, path) in [
        ("public_show_users", &["status", "public_show_users"][..]),
        ("discord_status", &["discord", "post_status"][..]),
        ("discord_events", &["discord", "post_events"][..]),
        ("discord_milestones", &["discord", "post_milestones"][..]),
        ("stats_users", &["stats_push", "include_users"][..]),
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

    // Staff accounts (comma/newline separated).
    if let Some(v) = req.form.get("admins") {
        let mut arr = toml_edit::Array::new();
        for name in v.split([',', '\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
            arr.push(name);
        }
        set_cfg(&mut doc, &["admins", "accounts"], toml_edit::value(arr));
    }

    // Per-product per-IP overrides: rebuild the whole products table from the textarea, so a
    // removed line clears its override. Each line is `PRODUCT=limit`.
    if let Some(v) = req.form.get("product_overrides") {
        let mut table = toml_edit::Table::new();
        for line in v.split(['\n', '\r']).map(str::trim).filter(|s| !s.is_empty()) {
            let Some((product, num)) = line.split_once('=') else {
                return html_page(settings_page(
                    admin,
                    config_path,
                    Some((false, &format!("Product override '{line}' must be PRODUCT=number."))),
                ));
            };
            let (product, num) = (product.trim(), num.trim());
            match num.parse::<i64>() {
                Ok(n) if n >= 0 && !product.is_empty() => {
                    let mut entry = toml_edit::Table::new();
                    entry["per_ip"] = toml_edit::value(n);
                    table.insert(product, toml_edit::Item::Table(entry));
                }
                _ => {
                    return html_page(settings_page(
                        admin,
                        config_path,
                        Some((false, &format!("Product override '{line}' must be PRODUCT=number (≥0)."))),
                    ))
                }
            }
        }
        set_cfg(&mut doc, &["limits", "clients", "products"], toml_edit::Item::Table(table));
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

    // Validate the whole document still deserialises as a Config (catches out-of-range values,
    // bad combinations, and any structural mistake) before writing anything.
    let serialized = doc.to_string();
    if let Err(e) = toml::from_str::<Config>(&serialized) {
        return html_page(settings_page(
            admin,
            config_path,
            Some((false, &format!("Rejected — the result is not a valid config: {e}"))),
        ));
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

/// Render `[limits.clients.products]` as `PRODUCT=per_ip` lines for the settings textarea.
fn cfg_products_text(doc: &toml_edit::DocumentMut) -> String {
    cfg_get(doc, &["limits", "clients", "products"])
        .and_then(|i| i.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| {
                    v.as_table_like()
                        .and_then(|pt| pt.get("per_ip"))
                        .and_then(toml_edit::Item::as_integer)
                        .map(|n| format!("{k}={n}"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
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
    cfg_get(doc, &["admins", "accounts"])
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
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
<title>BNET Command Center — Status</title>
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
  <h1>BNET Command Center · <span class="name" id="server">…</span></h1>
  <span class="meta" id="meta"></span>
  <nav><a href="/users">Users</a> · <a href="/settings">Settings</a> · <a href="/help">Help</a> · <a href="/change-password">Password</a></nav>
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
