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

use crate::admin::Admin;
use crate::node::Node;

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
pub async fn run(listen: SocketAddr, node: Arc<Node>, admin: Arc<Admin>, restart: Arc<Notify>) {
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
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let rate: RateLimiter = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let (tls, node, admin, sessions, rate, restart) = (
            tls.clone(),
            Arc::clone(&node),
            Arc::clone(&admin),
            Arc::clone(&sessions),
            Arc::clone(&rate),
            Arc::clone(&restart),
        );
        tokio::spawn(async move {
            let peer_ip = peer.ip();
            // A failed TLS handshake (e.g. someone speaking plain HTTP to the port) just
            // drops — no plaintext is ever served.
            if let Ok(stream) = tls.accept(sock).await {
                handle_conn(stream, peer_ip, &node, &admin, &sessions, &rate, &restart).await;
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

async fn handle_conn<S>(
    mut stream: S,
    peer_ip: IpAddr,
    node: &Node,
    admin: &Admin,
    sessions: &Sessions,
    rate: &RateLimiter,
    restart: &Arc<Notify>,
) where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let Some(req) = read_request(&mut stream).await else {
        return;
    };
    // Remote gate: a non-loopback peer is refused entirely until an admin enables remote
    // access from localhost. This sits in front of auth, so a remote attacker can't even
    // reach the login form while remote is off.
    if !peer_ip.is_loopback() && !admin.remote_enabled() {
        let resp = text("403 Forbidden", "Remote access is disabled on this server.");
        let _ = write_response(&mut stream, &resp).await;
        return;
    }
    let resp = route(&req, peer_ip, node, admin, sessions, rate, restart);
    let _ = write_response(&mut stream, &resp).await;
}

/// Route a request to a response. Sync: session/rate stores are plain mutexes and the
/// snapshot is sync, so nothing here awaits.
fn route(
    req: &Request,
    peer_ip: IpAddr,
    node: &Node,
    admin: &Admin,
    sessions: &Sessions,
    rate: &RateLimiter,
    restart: &Arc<Notify>,
) -> Response {
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
        ("GET", "/settings") => html_page(settings_page(admin)),
        ("POST", "/settings") => do_settings(req, admin),
        ("POST", "/restart") => do_restart(restart),
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
    let path = raw_path.split('?').next().unwrap_or("/").to_string();

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
    Some(Request { method, path, cookies, form })
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
:root {{ color-scheme: light dark; --bg:#0f1115; --card:#1a1d24; --fg:#e6e8ec; --muted:#9aa0aa; --accent:#5aa9e6; --line:#2a2e37; --err:#e06a6a; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; min-height:100vh; display:flex; align-items:center; justify-content:center; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; }}
.card {{ background:var(--card); border:1px solid var(--line); border-radius:12px; padding:28px; width:min(92vw,380px); }}
h1 {{ font-size:16px; margin:0 0 4px; }}
p.sub {{ color:var(--muted); margin:0 0 18px; font-size:13px; }}
label {{ display:block; font-size:12px; color:var(--muted); margin:14px 0 6px; text-transform:uppercase; letter-spacing:.04em; }}
input[type=password] {{ width:100%; padding:10px 12px; border-radius:8px; border:1px solid var(--line); background:#0f1115; color:var(--fg); font-size:14px; }}
button {{ margin-top:20px; width:100%; padding:11px; border:none; border-radius:8px; background:var(--accent); color:#04121f; font-weight:600; font-size:14px; cursor:pointer; }}
.err {{ background:rgba(224,106,106,.12); border:1px solid var(--err); color:var(--err); padding:9px 12px; border-radius:8px; font-size:13px; margin-bottom:8px; }}
a {{ color:var(--accent); }} .row {{ display:flex; align-items:center; gap:10px; margin:14px 0; }} .row input {{ width:auto; }}
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

fn settings_page(admin: &Admin) -> String {
    let checked = if admin.remote_enabled() { "checked" } else { "" };
    let inner = format!(
        r#"<h1>Settings</h1><p class="sub">Admin panel</p>
<form method="post" action="/settings">
<div class="row"><input id="rm" name="remote" type="checkbox" {checked}><label for="rm" style="margin:0;text-transform:none;letter-spacing:0;color:var(--fg)">Enable remote access (non-localhost)</label></div>
<p class="muted">When off, only localhost can reach this panel — even with the port forwarded. Turn it on only after you have forwarded 6114 and want remote control.</p>
<button type="submit">Save settings</button></form>
<h1 style="margin-top:28px">Server control</h1>
<form method="post" action="/restart" onsubmit="return confirm('Restart the server now? Connected clients will be dropped for a moment.')">
<p class="muted">Restarts the daemon to apply configuration changes. Only comes back automatically when started via the launcher; a standalone server will stop until you start it again.</p>
<button type="submit" style="background:#7a2323;border:1px solid #a33">Restart server</button></form>
<p style="margin-top:18px"><a href="/">Dashboard</a> &middot; <a href="/change-password">Change password</a></p>
<form method="post" action="/logout"><button type="submit" style="background:transparent;border:1px solid var(--line);color:var(--muted)">Sign out</button></form>"#
    );
    shell("Settings — Command Center", &inner)
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
