//! `bnetccd` — the Command Center node daemon.

#![forbid(unsafe_code)]

mod admin;
mod config;
mod discord;
mod moderation;
mod node;
mod outbound;
mod public_status;
mod realm;
mod session;
mod stats_push;
mod status;
mod tracker;
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

/// Exit code the daemon uses to ask its supervisor (the launcher) to relaunch it, as opposed
/// to a normal shutdown (0). Must match `RESTART_EXIT_CODE` in `bnetcc-launcher`.
const RESTART_EXIT_CODE: i32 = 75;

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

    match runtime.block_on(run(cfg, args.config)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cfg: Config, config_path: PathBuf) -> Result<(), String> {
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

    // Staff bans persist to a JSON file next to the account database (in-memory storage keeps
    // them in memory only). Same derivation as the admin panel's `bnetccd-admin/`.
    let bans_path = if cfg.storage.path.is_empty() {
        None
    } else {
        Some(
            std::path::Path::new(&cfg.storage.path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map_or_else(|| PathBuf::from("bnetccd-bans.json"), |p| p.join("bnetccd-bans.json")),
        )
    };

    let node = Arc::new(Node::new(
        node::NodeConfig {
            policy: policy.clone(),
            name: cfg.server.name.clone(),
            motd: cfg.server.motd.clone(),
            realm: cfg.server.realm.clone(),
            wc3_legacy_logon: cfg.server.wc3_legacy_logon()?,
            games_announce_webhook: {
                let url = cfg.discord.games_webhook_url.trim();
                (!url.is_empty()).then(|| url.to_string())
            },
            // The realm shares warnet mode's rule for the WarCraft III listeners: a chat-only
            // server offers no game infrastructure at all.
            d2_realm: (cfg.diablo2.realm && policy.mode != bnetcc_core::policy::ServerMode::Warnet)
                .then(|| node::D2Realm {
                    name: cfg.server.realm.clone(),
                    description: cfg.diablo2.description.clone(),
                    address: {
                        let a = cfg.diablo2.address.trim();
                        (!a.is_empty()).then(|| a.to_string())
                    },
                    max_characters: cfg.diablo2.max_characters as usize,
                }),
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
            bans_path,
        },
        storage,
    ));

    // Signalled by the admin panel's "Restart server" button; the main task selects on it
    // below and exits with RESTART_EXIT_CODE so the launcher-supervisor relaunches us.
    let restart = Arc::new(tokio::sync::Notify::new());

    // Optional Discord webhook updates. The updater runs on its own task; `discord_cfg` is
    // kept for the start/stop/restart event posts around this function.
    let discord_cfg = cfg.discord.clone();
    if !discord_cfg.webhook_url.trim().is_empty() {
        tokio::spawn(discord::run(Arc::clone(&node), discord_cfg.clone()));
    }

    // Optional stats push to an external website (outbound-only, no forwarded port needed).
    if !cfg.stats_push.url.trim().is_empty() {
        tokio::spawn(stats_push::run(Arc::clone(&node), cfg.stats_push.clone()));
    }

    // Optional PvPGN-compatible tracking: advertise this server to public trackers, and/or
    // host our own tracker (UDP beacon receiver) + public server-list page.
    if !cfg.tracker.advertise_to.is_empty() {
        tokio::spawn(tracker::advertise(
            Arc::clone(&node),
            cfg.listen.bncs.port(),
            cfg.tracker.clone(),
        ));
    }
    if !cfg.tracker.host_listen.trim().is_empty() || !cfg.tracker.list_listen.trim().is_empty() {
        tokio::spawn(tracker::host(
            cfg.tracker.host_listen.clone(),
            cfg.tracker.list_listen.clone(),
            cfg.tracker.prune_after_secs,
        ));
    }

    // Optional HTTPS admin panel. Off unless configured; a bad address, admin-secret error,
    // or bind failure is logged and never blocks the node from serving clients. The admin
    // credentials + self-signed cert live in `bnetccd-admin/` next to the account database.
    if !cfg.status.listen.is_empty() {
        match cfg.status.listen.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                let admin_dir = std::path::Path::new(&cfg.storage.path)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map_or_else(|| PathBuf::from("bnetccd-admin"), |p| p.join("bnetccd-admin"));
                match admin::Admin::load_or_init(&admin_dir) {
                    Ok(a) => {
                        tokio::spawn(status::run(
                            addr,
                            Arc::clone(&node),
                            Arc::new(a),
                            Arc::clone(&restart),
                            config_path.clone(),
                        ));
                    }
                    Err(e) => warn!(
                        dir = %admin_dir.display(),
                        error = %e,
                        "could not initialise admin panel credentials; panel disabled"
                    ),
                }
            }
            Err(e) => warn!(
                listen = %cfg.status.listen,
                error = %e,
                "invalid status.listen; admin panel disabled"
            ),
        }
    }

    // Optional public, read-only status page + JSON feed (plain HTTP, unauthenticated, safe
    // to expose). Separate from the admin panel above so public traffic never touches it.
    if !cfg.status.public_listen.is_empty() {
        match cfg.status.public_listen.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                tokio::spawn(public_status::run(
                    addr,
                    Arc::clone(&node),
                    cfg.status.public_show_users,
                ));
            }
            Err(e) => warn!(
                listen = %cfg.status.public_listen,
                error = %e,
                "invalid status.public_listen; public status page disabled"
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

    // Socket sharding: bind N SO_REUSEPORT listeners so several accept loops share the port
    // and the kernel spreads new connections across them (each with its own accept backlog).
    // `accept_shards = 1` is the ordinary single-listener case.
    let requested_shards = cfg.listen.accept_shards.clamp(1, 64);
    // SO_REUSEPORT sharding is a Unix feature; on other platforms a single listener is used.
    let shards = if cfg!(unix) {
        requested_shards
    } else {
        if requested_shards > 1 {
            warn!(
                requested = requested_shards,
                "accept_shards > 1 is not supported on this platform (no SO_REUSEPORT); using 1"
            );
        }
        1
    };
    let mut listeners = Vec::with_capacity(shards);
    for _ in 0..shards {
        listeners.push(
            reuseport_listener(cfg.listen.bncs)
                .map_err(|e| format!("cannot bind {}: {e}", cfg.listen.bncs))?,
        );
    }
    info!(addr = %cfg.listen.bncs, shards, "listening for BNCS and chat-gateway clients");

    if cfg.federation.enabled {
        // Phase 2. The link is one outbound mTLS connection to the hub; a node never
        // accepts inbound federation traffic, which is what lets a node behind NAT
        // participate. See docs/FEDERATION.md.
        warn!(hub = %cfg.federation.hub, "federation is configured but not yet implemented");
    }

    // Coalesced leave notifications flush on this cadence, so departures still propagate on
    // an idle channel (an active one also flushes on each broadcast). 75ms keeps the delay
    // imperceptible while collapsing a mass-disconnect burst into a handful of batched writes.
    {
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(75));
            loop {
                tick.tick().await;
                node.flush_pending();
            }
        });
    }

    for listener in listeners {
        tokio::spawn(accept_loop(listener, Arc::clone(&node), limits, max_connections));
    }

    // Wait for either a shutdown (Ctrl-C) or a panel-requested restart.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!(connections = node.connection_count(), "shutdown signal received; stopping");
            discord::post_event(&discord_cfg, &format!("🔴 **{}** is shutting down.", node.name)).await;
            Ok(())
        }
        () = restart.notified() => {
            // Exit with the agreed code so the launcher-supervisor relaunches us with the
            // (possibly just-edited) config. A standalone daemon simply exits.
            info!(connections = node.connection_count(), "restart requested via admin panel; exiting for relaunch");
            discord::post_event(&discord_cfg, &format!("🔄 **{}** is restarting.", node.name)).await;
            std::process::exit(RESTART_EXIT_CODE);
        }
    }
}

/// Build a listening socket with `SO_REUSEADDR` (plus `SO_REUSEPORT` on Unix) so several
/// accept loops can share one port; the kernel load-balances new connections across them. A
/// backlog of 1024 is requested per shard (the OS clamps it to `somaxconn`).
///
/// `SO_REUSEPORT` does not exist on Windows, so it is set only on Unix; the shard count is
/// forced to 1 elsewhere (see the shard loop in [`run`]).
fn reuseport_listener(addr: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
    let sock = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    sock.listen(1024)?;
    TcpListener::from_std(std::net::TcpListener::from(sock))
}

/// Wall-clock milliseconds since the Unix epoch, for ban-expiry checks.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One accept loop for a (possibly sharded) listener: accept, apply the global connection
/// ceiling, and spawn a session task per connection. A per-connection accept error (EMFILE,
/// a peer that vanished) is logged and never ends the loop.
async fn accept_loop(
    listener: TcpListener,
    node: Arc<Node>,
    limits: SessionLimits,
    max_connections: u32,
) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                // Shed banned addresses at the door — before a session task is spawned — so a
                // staff `/ipban` sheds an attacker's reconnects cheaply. This inherently
                // covers every session and alt on that address.
                if node.bans.is_ip_banned(peer.ip(), now_ms()) {
                    drop(stream);
                    continue;
                }
                if node.connection_count() >= u64::from(max_connections) {
                    // Refuse at the door rather than accepting and failing later.
                    drop(stream);
                    continue;
                }
                // Enable TCP keepalive so a peer that vanishes without a clean close (a killed
                // bot, a NAT/router drop) is detected and its session reaped in a couple of
                // minutes instead of lingering until the idle timeout. A zombie session holds
                // its CD-key claim (→ spurious "key in use" on the client's reconnect) and a
                // file descriptor (→ "too many open files" under a reconnecting fleet); reaping
                // it promptly releases both. Best-effort — a failure to set it is harmless.
                {
                    use socket2::{SockRef, TcpKeepalive};
                    let ka = TcpKeepalive::new()
                        .with_time(Duration::from_secs(60))
                        .with_interval(Duration::from_secs(15));
                    let _ = SockRef::from(&stream).set_tcp_keepalive(&ka);
                }
                let node = Arc::clone(&node);
                tokio::spawn(async move {
                    session::handle(stream, peer, node, limits).await;
                });
            }
            Err(e) => {
                warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
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
