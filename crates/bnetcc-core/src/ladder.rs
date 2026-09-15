//! The StarCraft / Warcraft II ladder: leagues, records, ratings and standings.
//!
//! What the protocol fixes (BNETDocs `SID_GAMERESULT`, `SID_GETLADDERDATA`,
//! `SID_FINDLADDERUSER`): a result names its league (0 normal, 1 ladder, 3 Iron Man); standings
//! are 0-based ranks sorted by highest rating, most wins or most games; a player with no rank is
//! `0xFFFFFFFF`.
//!
//! ⚠️ **Provisional, pending tagban's decision:** the rating arithmetic. Blizzard's classic ladder
//! pages are gone and no permissive source gives the formula, so [`rating_after`] is plain Elo
//! from [`START_RATING`] with [`K_FACTOR`] until the formula is chosen. Every rating change goes
//! through that one function. The classic eligibility rule (ten normal-game wins before playing
//! on the ladder, [`LADDER_MIN_WINS`]) is enforced by the chat server (tagban, 2026-09-14): a
//! player with fewer normal wins cannot host a ladder game and its ladder results do not count.

pub mod war3;

/// A game's league, from `SID_GAMERESULT`'s game type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum League {
    /// A normal game (`0`).
    Normal,
    /// A ladder game (`1`).
    Ladder,
    /// An Iron Man ladder game (`3`).
    IronMan,
}

impl League {
    /// From a game type or `SID_GETLADDERDATA` league value; anything else counts as normal.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => Self::Ladder,
            3 => Self::IronMan,
            _ => Self::Normal,
        }
    }

    /// The `Record\<product>\<n>` index.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Ladder => 1,
            Self::IronMan => 3,
        }
    }
}

/// A new ladder player's rating (provisional).
pub const START_RATING: u32 = 1000;
/// Elo's K for [`rating_after`] (provisional).
pub const K_FACTOR: f64 = 32.0;
/// Normal-game wins a StarCraft or Warcraft II player needs before playing on the ladder, as on
/// classic Battle.net. Diablo II's ladder has no such rule.
pub const LADDER_MIN_WINS: u32 = 10;
/// How long a game must last for its result to count, ladder or not: longer than two minutes
/// (tagban, 2026-09-14). A player who surrenders or leaves a counted game loses it, and the other
/// side, reporting its win, wins it.
pub const MIN_GAME_LENGTH: std::time::Duration = std::time::Duration::from_secs(120);

/// The lowest rank a ladder lists: ranks run from 1, the best, to this; everyone below is
/// unranked. Classic Battle.net went to 5,000; this server will not see that many players
/// (tagban, 2026-09-14). Every ladder — StarCraft, Warcraft II, Diablo II, WarCraft III — uses it.
pub const MAX_RANK: u32 = 500;

/// How a game went for one player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Won.
    Win,
    /// Lost.
    Loss,
    /// Drew.
    Draw,
    /// Disconnected — scored as a loss.
    Disconnect,
}

/// A player's rating after a game against opponents rated `opponent` on average (provisional
/// Elo; see the module notes). Never below 1.
#[must_use]
pub fn rating_after(rating: u32, opponent: u32, outcome: Outcome) -> u32 {
    let expected = 1.0 / (1.0 + 10f64.powf((f64::from(opponent) - f64::from(rating)) / 400.0));
    let score = match outcome {
        Outcome::Win => 1.0,
        Outcome::Draw => 0.5,
        Outcome::Loss | Outcome::Disconnect => 0.0,
    };
    (f64::from(rating) + K_FACTOR * (score - expected)).round().max(1.0) as u32
}

/// `SID_GETLADDERDATA`'s sort methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMethod {
    /// `0`: highest rating.
    Rating,
    /// `1`: fastest climbers — unused by the client; sorted as rating.
    Climbers,
    /// `2`: most wins.
    Wins,
    /// `3`: most games played.
    Games,
}

impl SortMethod {
    /// From the wire value; unknown values sort by rating.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => Self::Climbers,
            2 => Self::Wins,
            3 => Self::Games,
            _ => Self::Rating,
        }
    }
}

/// One player's ladder record in one league.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LadderRow {
    /// Account name.
    pub name: String,
    /// Wins.
    pub wins: u32,
    /// Losses.
    pub losses: u32,
    /// Disconnects.
    pub disconnects: u32,
    /// Current rating.
    pub rating: u32,
    /// Highest rating reached.
    pub high_rating: u32,
    /// Last game, seconds since the Unix epoch.
    pub last_game: u64,
}

impl LadderRow {
    /// Games played.
    #[must_use]
    pub const fn games(&self) -> u32 {
        self.wins + self.losses + self.disconnects
    }
}

/// Standings: players with a ladder game, in `sort` order (ties by rating, then name), the top
/// [`MAX_RANK`] only. A player's rank is its index + 1; the ladder packets carry the index.
#[must_use]
pub fn standings(mut rows: Vec<LadderRow>, sort: SortMethod) -> Vec<LadderRow> {
    rows.retain(|r| r.games() > 0);
    rows.sort_by(|a, b| {
        let key = |r: &LadderRow| match sort {
            SortMethod::Rating | SortMethod::Climbers => r.rating,
            SortMethod::Wins => r.wins,
            SortMethod::Games => r.games(),
        };
        key(b).cmp(&key(a)).then(b.rating.cmp(&a.rating)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows.truncate(MAX_RANK as usize);
    rows
}

/// A player's index in `standings` (its rank − 1), `None` if unranked.
#[must_use]
pub fn rank_of(standings: &[LadderRow], name: &str) -> Option<u32> {
    standings.iter().position(|r| r.name.eq_ignore_ascii_case(name)).map(|i| i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, wins: u32, losses: u32, rating: u32) -> LadderRow {
        LadderRow { name: name.into(), wins, losses, disconnects: 0, rating, high_rating: rating, last_game: 0 }
    }

    #[test]
    fn leagues_follow_the_game_type() {
        assert_eq!(League::from_code(0), League::Normal);
        assert_eq!(League::from_code(1).index(), 1);
        assert_eq!(League::from_code(3).index(), 3);
        assert_eq!(League::from_code(9), League::Normal);
    }

    #[test]
    fn ratings_move_by_the_expected_score() {
        assert_eq!(rating_after(1000, 1000, Outcome::Win), 1016);
        assert_eq!(rating_after(1000, 1000, Outcome::Loss), 984);
        assert_eq!(rating_after(1000, 1000, Outcome::Draw), 1000);
        assert_eq!(rating_after(1000, 1000, Outcome::Disconnect), 984, "a disconnect is a loss");
        assert!(rating_after(1000, 1400, Outcome::Win) - 1000 > rating_after(1400, 1000, Outcome::Win) - 1400, "beating a stronger player pays more");
        assert_eq!(rating_after(1, 3000, Outcome::Loss), 1, "never below 1");
    }

    #[test]
    fn standings_sort_by_the_method_asked_and_skip_the_unplayed() {
        let rows = vec![row("Zed", 10, 0, 1200), row("amy", 3, 1, 1300), row("Bob", 20, 30, 1100), row("Idle", 0, 0, 1000)];
        let by_rating = standings(rows.clone(), SortMethod::Rating);
        assert_eq!(by_rating.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["amy", "Zed", "Bob"]);
        assert_eq!(rank_of(&by_rating, "ZED"), Some(1));
        assert_eq!(rank_of(&by_rating, "Idle"), None, "no games, no rank");
        let by_wins = standings(rows.clone(), SortMethod::Wins);
        assert_eq!(by_wins[0].name, "Bob");
        let by_games = standings(rows, SortMethod::Games);
        assert_eq!(by_games.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Bob", "Zed", "amy"]);
    }

    #[test]
    fn nobody_below_rank_500_is_ranked() {
        let rows: Vec<LadderRow> = (0..600).map(|i| row(&format!("p{i:03}"), 1, 0, 2000 - i)).collect();
        let ladder = standings(rows, SortMethod::Rating);
        assert_eq!(ladder.len(), MAX_RANK as usize);
        assert_eq!(rank_of(&ladder, "p000"), Some(0), "rank 1");
        assert_eq!(rank_of(&ladder, "p499"), Some(499), "rank 500");
        assert_eq!(rank_of(&ladder, "p500"), None, "rank 501 is unranked");
    }
}
