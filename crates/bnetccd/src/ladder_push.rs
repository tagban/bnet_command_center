//! The ladder standings, pushed to an external website.
//!
//! One JSON snapshot of every ladder — StarCraft, Brood War and Warcraft II (ladder and Iron Man)
//! by rating, Diablo II by experience with its season, WarCraft III's still-closed ladder — that
//! the bnet.cc ladder pages (`web/bnet.cc/`) render. Like the stats push it is outbound-only, so
//! a server behind home NAT needs no forwarded port, and the site keeps showing the last
//! standings it received while the server is down. It is sent at startup, every
//! `[ladder_push] interval_secs`, and shortly after a ladder changes (at most once a minute).
//! The same snapshot is served at the public status endpoint's `/ladder.json`.
//!
//! Only what the games' own ladder screens show is included: StarCraft and Warcraft II players by
//! account name, Diablo II by character name (never the account behind it).

use std::sync::Arc;
use std::time::Duration;

use bnetcc_core::ladder::{standings, League, SortMethod, LADDER_MIN_WINS, MAX_RANK, MIN_GAME_LENGTH};
use bnetcc_proto::d2::status::{EXPANSION, HARDCORE, LADDER};
use serde::Serialize;
use tracing::{info, warn};

use crate::config::LadderPushConfig;
use crate::node::Node;

/// Least time between two pushes a ladder change sets off.
const CHANGE_DEBOUNCE: Duration = Duration::from_secs(60);

/// The Diablo II classes by id, as the ladder names them.
const D2_CLASSES: [&str; 7] = ["Amazon", "Sorceress", "Necromancer", "Paladin", "Barbarian", "Druid", "Assassin"];

/// Everything the ladder pages show.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// The server's name.
    pub server_name: String,
    /// When this was built, seconds since the Unix epoch.
    pub generated: u64,
    /// The lowest rank listed.
    pub max_rank: u32,
    /// Normal-game wins a StarCraft or Warcraft II player needs for the ladder.
    pub ladder_min_wins: u32,
    /// A game counts only when it ran longer than this.
    pub min_game_seconds: u64,
    /// The rated ladders, one per game.
    pub games: Vec<Game>,
    /// The Diablo II realm ladder.
    pub diablo2: Diablo2,
}

/// One game's rated ladders.
#[derive(Debug, Serialize)]
pub struct Game {
    /// Product code: `STAR`, `SEXP`, `W2BN`, `WAR3`, `W3XP`.
    pub product: &'static str,
    /// Its name.
    pub name: &'static str,
    /// Whether its ladder records games yet (WarCraft III's waits for matchmaking).
    pub open: bool,
    /// Its leagues: `ladder`, and `ironman` for Warcraft II.
    pub leagues: Vec<Standings>,
}

/// One league's standings, by rating.
#[derive(Debug, Serialize)]
pub struct Standings {
    /// `ladder` or `ironman`.
    pub league: &'static str,
    /// Ranked players, best first.
    pub players: Vec<Player>,
}

/// A ranked StarCraft or Warcraft II player.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Player {
    /// 1 the best.
    pub rank: u32,
    /// Account name.
    pub name: String,
    /// Ladder wins.
    pub wins: u32,
    /// Ladder losses.
    pub losses: u32,
    /// Ladder disconnects.
    pub disconnects: u32,
    /// Rating.
    pub rating: u32,
    /// Highest rating.
    pub high_rating: u32,
    /// Last ladder game, seconds since the Unix epoch.
    pub last_game: u64,
}

/// The Diablo II ladder.
#[derive(Debug, Serialize)]
pub struct Diablo2 {
    /// The current season.
    pub season: crate::season::Season,
    /// Ladder characters with a rank on their overall or class ladder, most experienced first.
    pub characters: Vec<Character>,
}

/// A Diablo II ladder character.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Character {
    /// Rank on its overall ladder (hardcore or softcore, classic or expansion); `None` past 500.
    pub rank: Option<u32>,
    /// Rank among its class on that ladder; `None` past 500.
    pub class_rank: Option<u32>,
    /// Character name.
    pub name: String,
    /// Class name.
    pub class: &'static str,
    /// Level.
    pub level: u8,
    /// Experience.
    pub experience: u32,
    /// Hardcore.
    pub hardcore: bool,
    /// A hardcore character that has died.
    pub dead: bool,
    /// Lord of Destruction.
    pub expansion: bool,
}

/// Build the snapshot.
pub async fn snapshot(node: &Node) -> Snapshot {
    let mut games = Vec::new();
    for (product, name, leagues) in [
        ("STAR", "StarCraft", &[League::Ladder][..]),
        ("SEXP", "StarCraft: Brood War", &[League::Ladder][..]),
        ("W2BN", "Warcraft II: Battle.net Edition", &[League::Ladder, League::IronMan][..]),
    ] {
        let mut standings_by_league = Vec::new();
        for &league in leagues {
            let rows = standings(node.ladder_rows(product, league).await, SortMethod::Rating);
            standings_by_league.push(Standings { league: league_name(league), players: players(rows) });
        }
        games.push(Game { product, name, open: true, leagues: standings_by_league });
    }
    for (product, name) in [("WAR3", "WarCraft III: Reign of Chaos"), ("W3XP", "WarCraft III: The Frozen Throne")] {
        games.push(Game { product, name, open: false, leagues: Vec::new() });
    }
    Snapshot {
        server_name: node.name.clone(),
        generated: crate::now_ms() / 1000,
        max_rank: MAX_RANK,
        ladder_min_wins: LADDER_MIN_WINS,
        min_game_seconds: MIN_GAME_LENGTH.as_secs(),
        games,
        diablo2: Diablo2 { season: node.d2_season.current(), characters: d2_characters(&node.all_characters().await) },
    }
}

/// The snapshot as JSON.
pub async fn snapshot_json(node: &Node) -> String {
    serde_json::to_string(&snapshot(node).await).unwrap_or_else(|_| "{}".to_string())
}

const fn league_name(league: League) -> &'static str {
    match league {
        League::IronMan => "ironman",
        _ => "ladder",
    }
}

fn players(rows: Vec<bnetcc_core::ladder::LadderRow>) -> Vec<Player> {
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| Player {
            rank: i as u32 + 1,
            name: r.name,
            wins: r.wins,
            losses: r.losses,
            disconnects: r.disconnects,
            rating: r.rating,
            high_rating: r.high_rating,
            last_game: r.last_game,
        })
        .collect()
}

/// Diablo II ladder characters ranked as the realm ranks them (`crate::realm::ladder_reply`:
/// experience, then level, then name), overall and by class within hardcore/softcore and
/// classic/expansion, keeping those ranked on either.
fn d2_characters(all: &[bnetcc_storage::Character]) -> Vec<Character> {
    let mut ladder: Vec<(&bnetcc_storage::Character, u32)> =
        all.iter().filter(|c| c.status & LADDER != 0).map(|c| (c, crate::realm::experience_of(c))).collect();
    ladder.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.level.cmp(&a.0.level)).then_with(|| a.0.name.cmp(&b.0.name)));
    let mut overall = std::collections::HashMap::new();
    let mut by_class = std::collections::HashMap::new();
    ladder
        .into_iter()
        .filter_map(|(c, experience)| {
            let kind = (c.status & HARDCORE != 0, c.status & EXPANSION != 0);
            let rank = {
                let n = overall.entry(kind).or_insert(0u32);
                *n += 1;
                *n
            };
            let class_rank = {
                let n = by_class.entry((kind, c.class)).or_insert(0u32);
                *n += 1;
                *n
            };
            let within = |r: u32| (r <= MAX_RANK).then_some(r);
            (rank <= MAX_RANK || class_rank <= MAX_RANK).then(|| Character {
                rank: within(rank),
                class_rank: within(class_rank),
                name: c.name.clone(),
                class: D2_CLASSES.get(usize::from(c.class)).copied().unwrap_or("Unknown"),
                level: c.level,
                experience,
                hardcore: kind.0,
                dead: kind.0 && c.status & 0x08 != 0,
                expansion: kind.1,
            })
        })
        .collect()
}

/// Push the snapshot until the process ends: now, every `interval_secs`, and a minute or less
/// after a ladder changes. A failed push is logged and tried again next time.
pub async fn run(node: Arc<Node>, cfg: LadderPushConfig) {
    let period = Duration::from_secs(cfg.interval_secs.max(60));
    let bearer = (!cfg.token.trim().is_empty()).then(|| cfg.token.clone());
    info!(interval_secs = period.as_secs(), "ladder push enabled");
    let mut tick = tokio::time::interval(period);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            () = node.ladder_changed.notified() => tokio::time::sleep(CHANGE_DEBOUNCE).await,
        }
        let body = snapshot_json(&node).await;
        match crate::outbound::post_json(&cfg.url, &body, bearer.as_deref()).await {
            Ok(()) => tick.reset(),
            Err(e) => warn!(error = %e, "ladder push failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(name: &str, class: u8, status: u8, level: u8, experience: u32) -> bnetcc_storage::Character {
        let save = d2_formats::d2s::Save::new(name, class, status, 0, &[(12, u32::from(level)), (13, experience)]).to_bytes();
        bnetcc_storage::Character { account: 1, name: name.into(), class, status, level, progression: 0, created_at: 0, last_played: 0, save: Some(save) }
    }

    #[test]
    fn diablo_two_characters_rank_overall_and_by_class_within_their_ladder() {
        let all = vec![
            character("Zon", 0, LADDER | EXPANSION, 40, 9_000_000),
            character("Barb", 4, LADDER | EXPANSION, 50, 20_000_000),
            character("Barb2", 4, LADDER | EXPANSION, 30, 2_000_000),
            character("Ghost", 4, LADDER | EXPANSION | HARDCORE | 0x08, 60, 50_000_000),
            character("Normal", 4, EXPANSION, 90, 90_000_000),
        ];
        let listed = d2_characters(&all);
        let summary: Vec<_> = listed.iter().map(|c| (c.name.as_str(), c.rank, c.class_rank, c.class, c.hardcore, c.dead)).collect();
        assert_eq!(
            summary,
            [
                ("Ghost", Some(1), Some(1), "Barbarian", true, true),
                ("Barb", Some(1), Some(1), "Barbarian", false, false),
                ("Zon", Some(2), Some(1), "Amazon", false, false),
                ("Barb2", Some(3), Some(2), "Barbarian", false, false),
            ],
            "hardcore ranks apart; non-ladder characters are left out"
        );
        assert_eq!(listed[1].experience, 20_000_000);
    }

    #[tokio::test]
    async fn the_snapshot_lists_every_ladder() {
        use bnetcc_storage::model::Credential;
        let node = crate::node::test_node();
        let acct = node.create_account("Raynor", Credential::Xsha1 { digest: [1u8; 20] }).await.unwrap();
        node.record_ladder_game(acct.id, "W2BN", League::IronMan, crate::storage::GameOutcome::Win, 1000).await.unwrap();
        let snap = snapshot(&node).await;
        let products: Vec<_> = snap.games.iter().map(|g| (g.product, g.open, g.leagues.len())).collect();
        assert_eq!(products, [("STAR", true, 1), ("SEXP", true, 1), ("W2BN", true, 2), ("WAR3", false, 0), ("W3XP", false, 0)]);
        let iron = &snap.games[2].leagues[1];
        assert_eq!(iron.league, "ironman");
        assert_eq!((iron.players[0].rank, iron.players[0].name.as_str(), iron.players[0].rating), (1, "Raynor", 1016));
        assert!(snap.games[2].leagues[0].players.is_empty());
        assert_eq!((snap.max_rank, snap.ladder_min_wins, snap.min_game_seconds, snap.diablo2.season.number), (500, 10, 120, 1));
        let json = snapshot_json(&node).await;
        assert!(json.contains(r#""league":"ironman","players":[{"rank":1,"name":"Raynor""#), "{json}");
    }
}
