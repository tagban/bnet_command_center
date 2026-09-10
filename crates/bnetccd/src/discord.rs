//! Best-effort Discord webhook updates.
//!
//! Enabled only when `[discord] webhook_url` is set. Everything here is best-effort: a failed
//! or slow post is logged and dropped, never retried in a way that could back up, and it runs
//! on its own task so it cannot stall the server. The webhook URL is a secret the operator
//! supplies in config — it is never logged.
//!
//! Rather than pull in a full HTTP client, this posts over the `tokio-rustls` stack already
//! in the tree: a TLS connection to the webhook host, one `POST` with a JSON body, and a peek
//! at the status line. Discord replies `204 No Content` on success.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tracing::{info, warn};

use crate::config::DiscordConfig;
use crate::node::Node;

/// Overall deadline for one webhook post (connect + TLS + write + read).
const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// Run the periodic Discord updater until the process ends. Returns immediately if Discord is
/// not configured.
pub async fn run(node: Arc<Node>, cfg: DiscordConfig) {
    if cfg.webhook_url.trim().is_empty() {
        return;
    }
    info!(
        interval_mins = cfg.status_interval_mins,
        games_window_hours = cfg.games_window_hours,
        "Discord updates enabled"
    );

    if cfg.post_events {
        post(&cfg.webhook_url, &format!("🟢 **{}** is online.", node.name)).await;
    }

    let mut last_peak = node.peak_connections();
    let period = Duration::from_secs(cfg.status_interval_mins.max(1) * 60);
    let mut tick = tokio::time::interval(period);
    tick.tick().await; // the first tick fires immediately; skip it so we don't post at once

    loop {
        tick.tick().await;

        if cfg.post_milestones {
            let peak = node.peak_connections();
            if peak > last_peak {
                post(
                    &cfg.webhook_url,
                    &format!("📈 New peak: **{peak}** concurrent connections."),
                )
                .await;
            }
            last_peak = last_peak.max(peak);
        }

        if cfg.post_status {
            post(&cfg.webhook_url, &status_message(&node, cfg.games_window_hours)).await;
        }
    }
}

/// Post a one-off event (server start/stop/restart) if events are enabled. Used by `main` on
/// shutdown, where a long hang must not delay exit — hence the short post timeout inside.
pub async fn post_event(cfg: &DiscordConfig, msg: &str) {
    if cfg.webhook_url.trim().is_empty() || !cfg.post_events {
        return;
    }
    post(&cfg.webhook_url, msg).await;
}

/// The periodic status line.
fn status_message(node: &Node, window_hours: u64) -> String {
    let games = node.games_hosted_since(Duration::from_secs(window_hours.max(1) * 3600));
    let games_line = if games.is_empty() {
        "none".to_string()
    } else {
        games.iter().map(|(p, c)| format!("{p} {c}")).collect::<Vec<_>>().join(", ")
    };
    format!(
        "**{name}** — {users} online · {conns} connections · {channels} channels · {live} games live · up {uptime}\n\
         Games hosted (last {window}h): {games_line}",
        name = node.name,
        users = node.online_count(),
        conns = node.connection_count(),
        channels = node.channel_names().len(),
        live = node.games().len(),
        uptime = fmt_uptime(node.uptime_secs()),
        window = window_hours,
    )
}

fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Post `content` to the webhook. Best-effort: logs and returns on any failure.
async fn post(webhook_url: &str, content: &str) {
    match timeout(POST_TIMEOUT, do_post(webhook_url, content)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Discord webhook post failed"),
        Err(_) => warn!("Discord webhook post timed out"),
    }
}

async fn do_post(webhook_url: &str, content: &str) -> Result<(), String> {
    let (host, path) = split_url(webhook_url).ok_or("webhook_url is not a valid https URL")?;
    let body = serde_json::json!({ "content": content }).to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: bnetccd\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    );

    let tcp = TcpStream::connect((host.as_str(), 443))
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let connector = TlsConnector::from(Arc::new(client_config()));
    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| format!("server name: {e}"))?;
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls: {e}"))?;

    tls.write_all(request.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
    tls.flush().await.map_err(|e| format!("flush: {e}"))?;

    // Read just the status line to confirm the post landed (Discord replies 204).
    let mut buf = [0u8; 256];
    let n = tls.read(&mut buf).await.map_err(|e| format!("read: {e}"))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let status_ok = head.starts_with("HTTP/1.1 2") || head.starts_with("HTTP/1.0 2");
    if status_ok {
        Ok(())
    } else {
        let line = head.lines().next().unwrap_or("").trim();
        Err(format!("unexpected response: {line}"))
    }
}

/// Build a rustls client config trusting the Mozilla roots. Cheap enough to build per post,
/// which happens at most a few times an hour.
fn client_config() -> rustls::ClientConfig {
    // `ClientConfig::builder()` needs a process crypto provider; install one if nothing has.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Split an `https://host/path…` URL into (host, path). Returns `None` for anything that is
/// not https. Query strings stay on the path.
fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    match rest.split_once('/') {
        Some((host, path)) => Some((host.to_string(), format!("/{path}"))),
        None => Some((rest.to_string(), "/".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_discord_webhook_url() {
        let (host, path) = split_url("https://discord.com/api/webhooks/123/abc").unwrap();
        assert_eq!(host, "discord.com");
        assert_eq!(path, "/api/webhooks/123/abc");
    }

    #[test]
    fn rejects_non_https() {
        assert!(split_url("http://discord.com/x").is_none());
        assert!(split_url("discord.com/x").is_none());
    }

    #[test]
    fn uptime_formats_by_magnitude() {
        assert_eq!(fmt_uptime(30), "0m");
        assert_eq!(fmt_uptime(600), "10m");
        assert_eq!(fmt_uptime(3700), "1h 1m");
        assert_eq!(fmt_uptime(90_000), "1d 1h");
    }
}
