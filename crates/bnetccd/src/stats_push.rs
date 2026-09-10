//! Push server stats to an external website on an interval.
//!
//! The mirror image of `crate::public_status`: instead of a site pulling `/status.json`, the
//! server POSTs that same JSON to a URL you configure. This is the right model for a node
//! behind residential NAT — it is outbound-only, so there is no extra port to forward and no
//! inbound exposure. Best-effort: a failed or slow post is logged and dropped, on its own
//! task, so it never affects serving.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::config::StatsPushConfig;
use crate::node::Node;

/// Run the stats pusher until the process ends. Returns immediately if no URL is configured.
pub async fn run(node: Arc<Node>, cfg: StatsPushConfig) {
    if cfg.url.trim().is_empty() {
        return;
    }
    let period = Duration::from_secs(cfg.interval_secs.max(5));
    info!(interval_secs = period.as_secs(), "stats push enabled");
    let bearer = (!cfg.token.trim().is_empty()).then(|| cfg.token.clone());

    let mut tick = tokio::time::interval(period);
    loop {
        tick.tick().await;
        let body = crate::public_status::snapshot_json(&node, cfg.include_users);
        if let Err(e) = crate::outbound::post_json(&cfg.url, &body, bearer.as_deref()).await {
            warn!(error = %e, "stats push failed");
        }
    }
}
