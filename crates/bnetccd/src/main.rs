//! `bnetccd` — the Command Center node daemon.

#![forbid(unsafe_code)]

mod config;
mod node;
mod session;
mod status;
mod storage;
mod udp;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::node::Node;
use crate::session::SessionLimits;

#[derive(Parser, Debug)]
#[command(name = "bnetccd", version, about = "Command Center node daemon")]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "bnetccd.toml")]
    config: PathBuf,

    /// Validate the configuration and exit without binding anything.
    #[arg(long)]
    check: bool,
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let cfg = if args.config.exists() {
        match Config::load(&args.config) {
            Ok(c) => c,
            Err(e) => {
                error!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else {
        warn!(
            path = %args.config.display(),
            "no config file found; running with defaults"
        );
        Config::default()
    };

    if args.check {
        info!("configuration is valid");
        return std::process::ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            error!("cannot start the async runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(cfg)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cfg: Config) -> Result<(), String> {
    let mode = cfg.server.parsed_mode()?;
    let policy = cfg.policy()?;

    let fd_limit = file_descriptor_limit();
    let max_connections = cfg.effective_max_connections(fd_limit);

    // Log the effective ceiling loudly. PvPGN ships a hard-coded 1000 and silently
    // refuses connections past it; operators who never found that knob concluded the
    // software could not scale. Nobody should have to guess.
    info!(
        mode = mode.as_str(),
        fd_limit,
        max_connections,
        game_hosting = ?policy.game_hosting,
        chat_ordering = ?policy.chat_ordering,
        gateway_per_ip = policy.clients.gateway.per_ip,
        game_per_ip = policy.clients.game_default.per_ip,
        "starting bnetccd"
    );
    if max_connections < 2000 {
        warn!(
            max_connections,
            fd_limit,
            "connection ceiling is below 2000; raise the file descriptor limit \
             (ulimit -n, LimitNOFILE=, or kern.maxfilesperproc on macOS) \
             or set limits.max_connections explicitly"
        );
    }

    let storage_backend: Box<dyn bnetcc_storage::Storage + Send> = if cfg.storage.path.is_empty()
    {
        warn!(
            "no storage.path configured; accounts are in-memory only and will not \
             survive a restart"
        );
        Box::new(bnetcc_storage::memory::MemoryStorage::new())
    } else {
        bnetcc_storage_sqlite::SqliteStorage::open(std::path::Path::new(&cfg.storage.path))
            .map(|s| Box::new(s) as Box<dyn bnetcc_storage::Storage + Send>)
            .map_err(|e| format!("cannot open storage.path {}: {e}", cfg.storage.path))?
    };
    let storage = storage::spawn(storage_backend);

    let version_policy = cfg.version_policy()?;
    if cfg.versions.restrict {
        let combos: usize = cfg.versions.allowed.values().map(BTreeMap::len).sum();
        info!(
            product_platform_combinations = combos,
            "client version restriction is ON; only listed product/platform/version-byte \
             combinations will be admitted"
        );
    }

    let files_dir = if cfg.files.dir.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&cfg.files.dir))
    };
    if let Some(dir) = &files_dir {
        info!(dir = %dir.display(), "serving operator-supplied files over BNFTP");
    }

    // UDP :6112 for the login-time UDP check that lets classic clients host/join games.
    // Best-effort — a bind failure only means clients keep the No-UDP flag (games stay
    // greyed), which is no worse than not having it.
    let udp_socket = match tokio::net::UdpSocket::bind(cfg.listen.bncs).await {
        Ok(s) => {
            info!(addr = %cfg.listen.bncs, "listening for the game UDP check");
            let socket = Arc::new(s);
            // The receive loop answers clients that probe first; the socket is also handed
            // to the node so sessions can send the login-time PKT_SERVERPING that un-greys
            // Create/Join on clients that wait for the server to ping them.
            tokio::spawn(udp::run(Arc::clone(&socket)));
            Some(socket)
        }
        Err(e) => {
            warn!(error = %e, "could not bind UDP; classic clients will show No-UDP (games greyed)");
            None
        }
    };

    let node = Arc::new(Node::new(
        node::NodeConfig {
            policy: policy.clone(),
            name: cfg.server.name.clone(),
            motd: cfg.server.motd.clone(),
            gateway_allowlist: cfg.limits.gateway_allowlist.clone(),
            version_policy,
            files_dir,
            admins: cfg.admins.clone(),
            auto_op_private: cfg.channels.auto_op_private,
            channel_rules: cfg.channel_rules()?,
            cd_key_uniqueness: cfg.limits.cd_key_uniqueness,
            channel_caps: node::ChannelCaps {
                private: cfg.channels.private_max,
                public: cfg.channels.public_max,
                clan: cfg.channels.clan_max,
            },
            udp_socket,
        },
        storage,
    ));

    // Optional read-only status UI. Off unless configured; a bad address or bind failure is
    // logged and never blocks the node from serving clients.
    if !cfg.status.listen.is_empty() {
        match cfg.status.listen.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                tokio::spawn(status::run(addr, Arc::clone(&node)));
            }
            Err(e) => warn!(
                listen = %cfg.status.listen,
                error = %e,
                "invalid status.listen; status UI disabled"
            ),
        }
    }

    let limits = SessionLimits {
        max_frame: cfg.limits.max_frame_bytes,
        max_line: cfg.limits.max_line_bytes,
        outbound_queue: cfg.limits.outbound_queue,
        handshake_timeout: Duration::from_secs(cfg.limits.handshake_timeout_secs),
        idle_timeout: Duration::from_secs(cfg.limits.idle_timeout_secs),
    };

    let listener = TcpListener::bind(cfg.listen.bncs)
        .await
        .map_err(|e| format!("cannot bind {}: {e}", cfg.listen.bncs))?;
    info!(addr = %cfg.listen.bncs, "listening for BNCS and chat-gateway clients");

    if cfg.federation.enabled {
        // Phase 2. The link is one outbound mTLS connection to the hub; a node never
        // accepts inbound federation traffic, which is what lets a node behind NAT
        // participate. See docs/FEDERATION.md.
        warn!(hub = %cfg.federation.hub, "federation is configured but not yet implemented");
    }

    let mut shutdown = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        if node.connection_count() >= u64::from(max_connections) {
                            // Refuse at the door rather than accepting and failing later.
                            drop(stream);
                            continue;
                        }
                        let node = Arc::clone(&node);
                        tokio::spawn(async move {
                            session::handle(stream, peer, node, limits).await;
                        });
                    }
                    Err(e) => {
                        // A per-connection accept error (EMFILE, a peer that vanished)
                        // must never end the accept loop.
                        warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
            _ = &mut shutdown => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    info!(connections = node.connection_count(), "stopping");
    Ok(())
}

/// Best-effort file descriptor limit for this process.
///
/// Reads `/proc/self/limits` on Linux. On macOS and Windows there is no dependency-free
/// way to ask, so we return a conservative value and rely on the startup warning to tell
/// the operator to set `limits.max_connections` explicitly.
///
/// TODO: replace with the `rlimit` crate, which handles all three platforms and can also
/// raise the soft limit to the hard limit at startup. Tracked in `docs/ROADMAP.md`.
fn file_descriptor_limit() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/self/limits") {
            for line in text.lines() {
                if line.starts_with("Max open files") {
                    if let Some(soft) = line.split_whitespace().nth(3) {
                        if let Ok(n) = soft.parse::<u64>() {
                            return n;
                        }
                        if soft == "unlimited" {
                            return 1_048_576;
                        }
                    }
                }
            }
        }
    }
    1024
}
