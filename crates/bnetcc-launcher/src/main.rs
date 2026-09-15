//! `bnetcc-launcher` — the friendly front door to the Command Center server.
//!
//! Downloading a release and running the server directly works, but a first-time tester on
//! another machine should not have to hand-write a TOML file or hunt for flags. This binary:
//!
//! 1. picks a data directory (the current directory by default) and, on first run, writes a
//!    minimal `bnetccd.toml` there against the *current* config schema;
//! 2. locates the `bnetccd` executable shipped alongside it (same directory), falling back
//!    to `PATH`;
//! 3. prints where things live and how to connect — the BNCS port for game clients and the
//!    admin-panel URL — then launches the server with its output inline.
//!
//! It is deliberately dependency-light (std + clap) and does no protocol work of its own; it
//! only sets the stage and hands off. The server still prints its one-time admin password to
//! this same console on first run, so the tester sees it here.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Parser;

/// The file the launcher writes on first run. Kept minimal and matched to the server's
/// current config schema (`crates/bnetccd/src/config.rs`); `bnetccd.example.toml` documents
/// every other field. `deny_unknown_fields` is on in the server, so stale keys are fatal —
/// only add a key here after confirming it exists there.
const DEFAULT_CONFIG: &str = "\
# Command Center — written by bnetcc-launcher on first run.
# Every field has a default; see bnetccd.example.toml for the full reference.

[server]
name = \"Command Center\"
# gaming | warnet | both
mode = \"gaming\"
motd = \"Welcome to Command Center.\"

[listen]
# BNCS + chat gateway. 0.0.0.0 so clients on other machines can reach this host.
bncs = \"0.0.0.0:6112\"
# Number of accept loops (SO_REUSEPORT). Raise for very high connection rates.
accept_shards = 1

[storage]
# SQLite database; accounts and game records persist here. Empty = in-memory (lost on exit).
path = \"bnetccd.db\"

[status]
# HTTPS admin panel. Loopback-only by default; the panel's Settings page enables remote.
listen = \"127.0.0.1:6114\"

[admins]
# Accounts granted the Blizzard-rep / sysop role (staff commands like /tagban, /ipban).
# Add the account name you log in with, e.g. accounts = [\"YourName\"].
accounts = []
";

/// Executable suffix for the current platform (`.exe` on Windows, empty elsewhere).
const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

/// Exit code the daemon uses to request a relaunch (as opposed to a clean shutdown). Must
/// match `RESTART_EXIT_CODE` in the `bnetccd` binary.
const RESTART_EXIT_CODE: i32 = 75;

#[derive(Parser, Debug)]
#[command(
    name = "bnetcc-launcher",
    version,
    about = "Set up and launch the Command Center server (bnetccd)."
)]
struct Args {
    /// Directory to run in — holds the config, database, and admin/ban files. Created if
    /// missing. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Config file to use (relative to the data directory unless absolute).
    #[arg(long, value_name = "FILE", default_value = "bnetccd.toml")]
    config: PathBuf,

    /// Path to the bnetccd server executable. Defaults to one next to this launcher, then
    /// whatever is on PATH.
    #[arg(long, value_name = "FILE")]
    server_bin: Option<PathBuf>,

    /// Write the default config if missing, print what would run, then exit without
    /// launching the server. Handy in CI or for inspecting the generated config.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bnetcc-launcher: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<ExitCode, String> {
    let data_dir = match &args.data_dir {
        Some(d) => d.clone(),
        None => std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?,
    };
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("cannot create data directory {}: {e}", data_dir.display()))?;

    // Resolve the config path relative to the data dir (unless the user gave an absolute one).
    let config_path = if args.config.is_absolute() {
        args.config.clone()
    } else {
        data_dir.join(&args.config)
    };

    let first_run = !config_path.exists();
    if first_run {
        std::fs::write(&config_path, DEFAULT_CONFIG)
            .map_err(|e| format!("cannot write config {}: {e}", config_path.display()))?;
    }

    let server = locate_server(args.server_bin.as_deref())?;

    print_banner(&data_dir, &config_path, &server, first_run);

    if args.dry_run {
        println!("(dry run — not launching the server)");
        return Ok(ExitCode::SUCCESS);
    }

    // Supervise the server: run it with the data directory as its working directory (so its
    // database and admin/ban files land there) and stdio inherited (so its log — including the
    // one-time admin password on first run — shows up right here). When the admin panel's
    // "Restart" is used, the server exits with RESTART_EXIT_CODE and we relaunch it; any other
    // exit (including a clean Ctrl-C, which also reaches the child) ends the launcher too.
    loop {
        let status = Command::new(&server)
            .arg("--config")
            .arg(&config_path)
            .current_dir(&data_dir)
            .status()
            .map_err(|e| format!("failed to start server {}: {e}", server.display()))?;

        match status.code() {
            Some(RESTART_EXIT_CODE) => {
                println!("\n[launcher] restart requested — relaunching the server…\n");
                continue;
            }
            Some(0) | None => return Ok(ExitCode::SUCCESS),
            Some(code) => return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        }
    }
}

/// Find the `bnetccd` executable: an explicit `--server-bin`, then one next to this launcher,
/// then a bare name for the OS to resolve on `PATH`.
fn locate_server(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("--server-bin {} does not exist", p.display()));
    }
    let bin_name = format!("bnetccd{EXE_SUFFIX}");
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&bin_name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Fall back to PATH resolution by the OS. If it is not there, the spawn fails with a
    // clear message naming the file.
    Ok(PathBuf::from(bin_name))
}

fn print_banner(data_dir: &Path, config_path: &Path, server: &Path, first_run: bool) {
    let line = "=".repeat(60);
    println!("{line}");
    println!("  Command Center — launcher v{}", env!("CARGO_PKG_VERSION"));
    println!("{line}");
    if first_run {
        println!("  First run: wrote a default config.");
        println!("  Edit it to add a staff account under [admins], then restart.");
    }
    println!("  Data directory : {}", data_dir.display());
    println!("  Config         : {}", config_path.display());
    println!("  Server binary  : {}", server.display());
    println!();
    println!("  Game clients connect to : this host on port 6112");
    println!("  Admin panel (this host) : https://127.0.0.1:6114");
    println!("  (self-signed cert — your browser will warn once; the one-time");
    println!("   admin password is printed by the server just below on first run.)");
    println!("{line}");
    println!();
}
