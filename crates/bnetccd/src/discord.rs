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

use tracing::{info, warn};

use crate::config::DiscordConfig;
use crate::node::Node;

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

/// Announce a newly-advertised game to the separate games webhook. Best-effort and
/// fire-and-forget: `session::advertise` spawns this so a slow webhook never delays hosting.
/// `webhook_url` is the caller's `Node::games_announce_webhook` (already known non-empty).
pub async fn post_game(webhook_url: String, host: String, game: String, product: String) {
    post(&webhook_url, &format!("🎮 **{host}** is hosting **{game}** ({product})")).await;
}

/// Post `content` to the webhook. Best-effort: logs and returns on any failure.
async fn post(webhook_url: &str, content: &str) {
    let body = serde_json::json!({ "content": content }).to_string();
    if let Err(e) = crate::outbound::post_json(webhook_url, &body, None).await {
        warn!(error = %e, "Discord webhook post failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_formats_by_magnitude() {
        assert_eq!(fmt_uptime(30), "0m");
        assert_eq!(fmt_uptime(600), "10m");
        assert_eq!(fmt_uptime(3700), "1h 1m");
        assert_eq!(fmt_uptime(90_000), "1d 1h");
    }
}
