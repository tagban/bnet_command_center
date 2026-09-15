//! `winbot` — two accounts play each other over and over, one surrendering each game, to test
//! a Command Center server's win/loss records and ladders.
//!
//! Each game: the first account advertises it (`SID_STARTADVEX3`), the second joins
//! (`SID_NOTIFYJOIN`), the host starts it (`SID_STOPADV`), and after `--length` seconds one side
//! surrenders. Both report the game (`SID_GAMERESULT`: the surrenderer its loss, the other its
//! win), leave it and go back to chat, and the bot prints both records. A game has to run longer
//! than two minutes to count, so the default length is 2:05; a shorter `--length` checks that
//! short games do not count.
//!
//! StarCraft and Warcraft II let a player on the ladder only after ten normal-game wins. With
//! `--ladder` or `--iron-man` the bot first plays normal games (the account with fewer wins winning
//! each) until both accounts have ten.
//!
//! ```text
//! winbot --product SEXP                                # normal games, forever
//! winbot --product W2BN --ladder --games 10            # ten ladder games
//! winbot --product STAR --surrender second --length 90 # games too short to count
//! ```
//!
//! WarCraft III (`WAR3`, `W3XP`) logs in and plays the same custom games, but its clients send no
//! `SID_GAMERESULT`: its ladder records games through anonymous matchmaking and the route
//! server, which are not built yet, and custom games are never recorded. Until then its games
//! only exercise logon, hosting and joining.
//!
//! It talks to the server only, never to another game client: the games themselves are not
//! played. Point it at your own server.

mod client;

use std::time::Duration;

use bnetcc_proto::{product, FourCc};
use clap::{Parser, ValueEnum};
use tracing::{info, warn};

use crate::client::{Client, Error};

#[derive(Parser)]
#[command(name = "winbot", about = "Two accounts play each other and one surrenders, to test game records and ladders")]
struct Cli {
    /// The chat server, `host:port`.
    #[arg(long, default_value = "127.0.0.1:6112")]
    server: String,
    /// The game: STAR, SEXP, W2BN, WAR3 or W3XP.
    #[arg(long, default_value = "SEXP")]
    product: String,
    /// The first account (hosts every game), `name:password`. Created if it does not exist.
    #[arg(long, default_value = "WinBotA:winbot")]
    first: String,
    /// The second account (joins every game), `name:password`.
    #[arg(long, default_value = "WinBotB:winbot")]
    second: String,
    /// Games to play; 0 plays until stopped.
    #[arg(long, default_value_t = 0)]
    games: u32,
    /// Seconds from a game's start to the surrender. Games of two minutes or less do not count.
    #[arg(long, default_value_t = 125)]
    length: u64,
    /// Seconds between games.
    #[arg(long, default_value_t = 5)]
    pause: u64,
    /// Play ladder games.
    #[arg(long, conflicts_with = "iron_man")]
    ladder: bool,
    /// Play Iron Man ladder games (Warcraft II).
    #[arg(long)]
    iron_man: bool,
    /// Who surrenders.
    #[arg(long, value_enum, default_value_t = Surrender::Alternate)]
    surrender: Surrender,
    /// The channel the accounts wait in between games.
    #[arg(long, default_value = "Win Bots")]
    channel: String,
}

/// Who gives up each game.
#[derive(Clone, Copy, ValueEnum)]
enum Surrender {
    /// The first account one game, the second the next.
    Alternate,
    /// Always the first account.
    First,
    /// Always the second account.
    Second,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let cli = Cli::parse();
    if let Err(e) = run(&cli).await {
        warn!("stopped: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: &Cli) -> Result<(), Error> {
    let text = cli.product.to_ascii_uppercase();
    let bytes: [u8; 4] = text.as_bytes().try_into().map_err(|_| Error(format!("{:?} is not a four-letter product code", cli.product)))?;
    let game = FourCc::from_ascii(&bytes);
    if ![product::STAR, product::SEXP, product::W2BN, product::WAR3, product::W3XP].contains(&game) {
        return Err(Error(format!("{text} is not one of STAR, SEXP, W2BN, WAR3, W3XP")));
    }
    let warcraft3 = game == product::WAR3 || game == product::W3XP;
    let credentials = |s: &str| s.split_once(':').map(|(n, p)| (n.to_string(), p.to_string())).ok_or_else(|| Error(format!("{s:?} is not name:password")));
    let (first_name, first_password) = credentials(&cli.first)?;
    let (second_name, second_password) = credentials(&cli.second)?;
    // Game type and ladder field for STARTADVEX3, and the result's game type.
    let (game_type, ladder, league) = if cli.iron_man {
        (0x10, 3, 3)
    } else if cli.ladder {
        (0x09, 1, 1)
    } else {
        (0x02, 0, 0)
    };

    let mut a = Client::login(&cli.server, game, &first_name, &first_password, &cli.channel).await?;
    let mut b = Client::login(&cli.server, game, &second_name, &second_password, &cli.channel).await?;
    info!(server = %cli.server, product = %game, first = %a.name, second = %b.name, "both logged in");
    if warcraft3 {
        warn!("WarCraft III sends no game results: these games will not change any record");
    }
    if cli.length <= 120 {
        warn!(length = cli.length, "games of two minutes or less do not count");
    }

    let tag = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() % 100_000);
    let mut count = 0u32;
    let mut next_name = || {
        count += 1;
        format!("wb {tag}-{count}")
    };

    // The ladder takes ten normal-game wins: earn them first.
    if league != 0 && !warcraft3 {
        loop {
            let (a_wins, b_wins) = (normal_wins(&mut a).await?, normal_wins(&mut b).await?);
            if a_wins >= LADDER_MIN_WINS && b_wins >= LADDER_MIN_WINS {
                break;
            }
            if cli.length <= 120 {
                return Err(Error(format!(
                    "{} and {} need ten normal wins for the ladder ({a_wins} and {b_wins} now), and games of two minutes or less do not count: use a longer --length",
                    a.account, b.account
                )));
            }
            info!(first = a_wins, second = b_wins, needed = LADDER_MIN_WINS, "warming up: normal games until both have ten wins");
            // The account with fewer wins wins this one.
            play(cli, &mut a, &mut b, &next_name(), (0x02, 0, 0), a_wins >= b_wins, false).await?;
        }
    }

    let mut played = 0u32;
    while cli.games == 0 || played < cli.games {
        let first_surrenders = match cli.surrender {
            Surrender::First => true,
            Surrender::Second => false,
            Surrender::Alternate => played % 2 == 0,
        };
        play(cli, &mut a, &mut b, &next_name(), (game_type, ladder, league), first_surrenders, warcraft3).await?;
        played += 1;
        info!(played, "games played");
    }
    Ok(())
}

/// Normal-game wins a StarCraft or Warcraft II player needs before the ladder.
const LADDER_MIN_WINS: u32 = 10;

/// One game: `a` hosts `name`, `b` joins, the host starts it, and after `--length` one side
/// surrenders. `kind` is the advertised game type, ladder field and result game type.
async fn play(cli: &Cli, a: &mut Client, b: &mut Client, name: &str, kind: (u16, u32, u32), first_surrenders: bool, warcraft3: bool) -> Result<(), Error> {
    let (game_type, ladder, league) = kind;
    a.host(name, game_type, ladder).await?;
    b.join(name).await?;
    a.start().await?;
    let ladder_word = match league {
        1 => "ladder",
        3 => "Iron Man",
        _ => "normal",
    };
    info!(game = %name, kind = ladder_word, length = cli.length, surrenders = if first_surrenders { &a.account } else { &b.account }, "game started");
    // Both stay connected for the game.
    let length = Duration::from_secs(cli.length);
    let (ra, rb) = tokio::join!(a.idle(length), b.idle(length));
    ra?;
    rb?;

    let (a_code, b_code) = if first_surrenders { (2, 1) } else { (1, 2) };
    let slots = [(a.account.clone(), a_code), (b.account.clone(), b_code)];
    let slots: Vec<(&str, u32)> = slots.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    // The surrenderer leaves first.
    let (loser, winner) = if first_surrenders { (a, b) } else { (b, a) };
    for side in [&mut *loser, &mut *winner] {
        if !warcraft3 {
            side.report(league, &slots).await?;
        }
        side.leave(&cli.channel).await?;
    }
    info!(game = %name, winner = %winner.account, "game over");
    if !warcraft3 {
        show_record(loser, league).await?;
        show_record(winner, league).await?;
    }
    let pause = Duration::from_secs(cli.pause);
    let (ra, rb) = tokio::join!(loser.idle(pause), winner.idle(pause));
    ra?;
    rb
}

/// An account's normal-game wins.
async fn normal_wins(client: &mut Client) -> Result<u32, Error> {
    let key = format!(r"Record\{}\0\wins", client.product);
    Ok(client.read(&[key]).await?.first().and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Print an account's normal record, and its ladder record and rank when playing the ladder.
async fn show_record(client: &mut Client, league: u32) -> Result<(), Error> {
    let product = client.product.to_string();
    let mut keys: Vec<String> = ["wins", "losses", "disconnects"].iter().map(|leaf| format!(r"Record\{product}\0\{leaf}")).collect();
    if league != 0 {
        keys.extend(["wins", "losses", "rating", "high rating"].iter().map(|leaf| format!(r"Record\{product}\{league}\{leaf}")));
    }
    let values = client.read(&keys).await?;
    let value = |i: usize| values.get(i).filter(|v| !v.is_empty()).map_or("0", String::as_str).to_string();
    if league == 0 {
        info!(account = %client.account, wins = value(0), losses = value(1), disconnects = value(2), "record");
    } else {
        let account = client.account.clone();
        let rank = client.rank(league, &account).await?.map_or_else(|| "unranked".to_string(), |r| r.to_string());
        info!(
            account = %client.account,
            ladder_wins = value(3),
            ladder_losses = value(4),
            rating = value(5),
            high = value(6),
            rank,
            "record"
        );
    }
    Ok(())
}
