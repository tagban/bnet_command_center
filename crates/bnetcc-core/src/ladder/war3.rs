//! The WarCraft III ladder's rules: levels and XP (classic Chart 1), XP per game (Charts 2–4),
//! random-team levels, the weekly inactivity loss, and settling a game from its players'
//! reports. Numbers from Blizzard's classic ladder pages as restated in
//! `docs/WARCRAFT3-MATCHMAKING.md` §2.
//!
//! Ranks run 1 to [`super::MAX_RANK`] (500) here, not classic WarCraft III's top 1,000 (tagban,
//! 2026-09-14).
//!
//! ⚠️ **Readings pending tagban's decision** (§2.4 decision (b)):
//! - Charts 2 and 3 print the same numbers headed "25th level = max" and "35th level = max",
//!   Chart 4 "45th level = max". Here the heading names the realm's top level the chart is
//!   for and caps nothing: [`Chart::for_top_level`] picks the chart and levels are used as they are.
//! - A loss times a loss factor rounds to the nearest XP; a random team's level is its players'
//!   average, rounded to nearest.
//! - A week short of the minimum games costs one loss to a player one level below, however
//!   many games short.

use super::Outcome;

/// Highest level; XP keeps accruing past its start.
pub const MAX_LEVEL: u8 = 50;
/// Level differences past this count as this.
pub const MAX_DIFFERENCE: u8 = 6;

/// XP at which `level` starts (Chart 1).
#[must_use]
pub const fn level_start(level: u8) -> u32 {
    const FIRST_TEN: [u32; 10] = [0, 100, 200, 400, 600, 900, 1_200, 1_600, 2_000, 2_500];
    match level {
        0 | 1 => 0,
        2..=10 => FIRST_TEN[level as usize - 1],
        _ => {
            let level = if level > MAX_LEVEL { MAX_LEVEL } else { level };
            2_500 + 500 * (level as u32 - 10)
        }
    }
}

/// The level a player with `xp` is at.
#[must_use]
pub fn level_for(xp: u32) -> u8 {
    (1..=MAX_LEVEL).rev().find(|&l| level_start(l) <= xp).unwrap_or(1)
}

/// What a loss costs at `level`, as a fraction of the chart's loss (Chart 1).
#[must_use]
pub fn loss_factor(level: u8) -> f64 {
    match level {
        0 | 1 => 0.10,
        2 | 3 => 0.11,
        4 | 5 => 0.25,
        6 | 7 => 0.43,
        8 | 9 => 0.67,
        _ => 1.0,
    }
}

/// Games a week a player at `level` must play to avoid the inactivity loss (Chart 1).
#[must_use]
pub const fn weekly_minimum(level: u8) -> u32 {
    match level {
        0..=10 => 0,
        11..=21 => 1,
        22..=30 => 2,
        31..=40 => 3,
        _ => 4,
    }
}

/// Which XP chart a realm plays under, by the highest level any player has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chart {
    /// Chart 2: top level 25 or lower.
    Two,
    /// Chart 3: top level above 25.
    Three,
    /// Chart 4: top level above 35.
    Four,
}

impl Chart {
    /// The chart for a realm whose highest player level is `top`.
    #[must_use]
    pub const fn for_top_level(top: u8) -> Self {
        match top {
            0..=25 => Self::Two,
            26..=35 => Self::Three,
            _ => Self::Four,
        }
    }

    /// `[higher wins, higher loses, lower wins, lower loses]` for a level difference.
    const fn row(self, difference: u8) -> [u32; 4] {
        const TWO_THREE: [[u32; 4]; 7] =
            [[100, 100, 100, 100], [95, 105, 140, 60], [90, 110, 152, 48], [85, 115, 163, 37], [80, 120, 172, 28], [75, 125, 178, 22], [70, 130, 184, 16]];
        const FOUR: [[u32; 4]; 7] =
            [[100, 100, 100, 100], [85, 115, 115, 85], [70, 130, 130, 70], [55, 145, 145, 55], [45, 155, 155, 45], [35, 165, 165, 35], [25, 175, 175, 25]];
        let d = if difference > MAX_DIFFERENCE { MAX_DIFFERENCE } else { difference } as usize;
        match self {
            Self::Two | Self::Three => TWO_THREE[d],
            Self::Four => FOUR[d],
        }
    }
}

/// The XP a player at `level` gains (positive) or loses (negative) for `outcome` against an
/// opponent (or enemy team) at `opponent` level. A draw moves nothing; a disconnect is a loss.
#[must_use]
pub fn xp_change(chart: Chart, level: u8, opponent: u8, outcome: Outcome) -> i64 {
    let [higher_wins, higher_loses, lower_wins, lower_loses] = chart.row(level.abs_diff(opponent));
    let higher = level > opponent;
    match outcome {
        Outcome::Draw => 0,
        Outcome::Win => i64::from(if higher { higher_wins } else { lower_wins }),
        Outcome::Loss | Outcome::Disconnect => {
            let loss = f64::from(if higher { higher_loses } else { lower_loses });
            -((loss * loss_factor(level)).round() as i64)
        }
    }
}

/// A player's XP after a change; never below 0.
#[must_use]
pub fn apply(xp: u32, change: i64) -> u32 {
    (i64::from(xp) + change).clamp(0, i64::from(u32::MAX)) as u32
}

/// A random team's level: its players' average, rounded to nearest.
#[must_use]
pub fn team_level(levels: &[u8]) -> u8 {
    if levels.is_empty() {
        return 1;
    }
    let sum: u32 = levels.iter().map(|&l| u32::from(l)).sum();
    ((f64::from(sum) / levels.len() as f64).round() as u8).max(1)
}

/// The weekly inactivity loss for a player at `level` who played `games` this week: a loss to
/// a player one level below if short of the minimum, else 0. XP only; not a recorded game.
#[must_use]
pub fn inactivity_change(chart: Chart, level: u8, games: u32) -> i64 {
    if games >= weekly_minimum(level) {
        return 0;
    }
    xp_change(chart, level, level.saturating_sub(1).max(1), Outcome::Loss)
}

/// One player's report of how the game went for one player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// The player number reporting.
    pub reporter: u8,
    /// The player number reported on.
    pub subject: u8,
    /// What the reporter says happened to the subject.
    pub outcome: Outcome,
}

/// Settle a game from every player's reports: each player's outcome is the one most other
/// players reported for them (their own report counts only when nobody else reported them),
/// a tie between a win and anything else settles against the win, and a game with no winner is
/// thrown away (`None`). Players nobody reported on at all are disconnects.
#[must_use]
pub fn settle(players: &[u8], reports: &[Report]) -> Option<Vec<(u8, Outcome)>> {
    let settled: Vec<(u8, Outcome)> = players
        .iter()
        .map(|&player| {
            let about: Vec<&Report> = reports.iter().filter(|r| r.subject == player).collect();
            let others: Vec<&Report> = about.iter().copied().filter(|r| r.reporter != player).collect();
            let votes = if others.is_empty() { about } else { others };
            let count = |o: Outcome| votes.iter().filter(|r| r.outcome == o).count();
            let order = [Outcome::Loss, Outcome::Disconnect, Outcome::Draw, Outcome::Win];
            let outcome = order.into_iter().max_by(|&a, &b| count(a).cmp(&count(b)).then_with(|| rank(b).cmp(&rank(a)))).filter(|&o| count(o) > 0);
            (player, outcome.unwrap_or(Outcome::Disconnect))
        })
        .collect();
    settled.iter().any(|&(_, o)| o == Outcome::Win).then_some(settled)
}

/// Tie-break order: a lower rank wins a tied vote.
const fn rank(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Loss => 0,
        Outcome::Disconnect => 1,
        Outcome::Draw => 2,
        Outcome::Win => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_one_levels_and_factors() {
        let starts: Vec<u32> = (1..=12).map(level_start).collect();
        assert_eq!(starts, [0, 100, 200, 400, 600, 900, 1_200, 1_600, 2_000, 2_500, 3_000, 3_500]);
        assert_eq!(level_start(50), 22_500);
        assert_eq!(level_start(60), 22_500);
        assert_eq!(level_for(0), 1);
        assert_eq!(level_for(99), 1);
        assert_eq!(level_for(100), 2);
        assert_eq!(level_for(2_499), 9);
        assert_eq!(level_for(3_000), 11);
        assert_eq!(level_for(90_000), 50, "XP keeps accruing at 50");
        assert_eq!([1, 2, 3, 4, 6, 8, 10, 30].map(loss_factor), [0.10, 0.11, 0.11, 0.25, 0.43, 0.67, 1.0, 1.0]);
        assert_eq!([10, 11, 21, 22, 30, 31, 40, 41, 50].map(weekly_minimum), [0, 1, 1, 2, 2, 3, 3, 4, 4]);
    }

    #[test]
    fn the_chart_follows_the_realms_top_level() {
        assert_eq!([1, 25, 26, 35, 36, 50].map(Chart::for_top_level), [Chart::Two, Chart::Two, Chart::Three, Chart::Three, Chart::Four, Chart::Four]);
    }

    #[test]
    fn xp_per_game_reads_the_charts() {
        // Even levels: 100 either way (a level-10 loser pays it all).
        assert_eq!(xp_change(Chart::Two, 10, 10, Outcome::Win), 100);
        assert_eq!(xp_change(Chart::Two, 10, 10, Outcome::Loss), -100);
        // Level 14 beats level 12: higher wins 90, lower loses 48.
        assert_eq!(xp_change(Chart::Two, 14, 12, Outcome::Win), 90);
        assert_eq!(xp_change(Chart::Two, 12, 14, Outcome::Loss), -48);
        // The upset: lower wins 152, higher loses 110.
        assert_eq!(xp_change(Chart::Three, 12, 14, Outcome::Win), 152);
        assert_eq!(xp_change(Chart::Three, 14, 12, Outcome::Disconnect), -110);
        // Differences past 6 read row 6; Chart 4 is symmetric.
        assert_eq!(xp_change(Chart::Two, 30, 11, Outcome::Win), 70);
        assert_eq!(xp_change(Chart::Four, 20, 23, Outcome::Win), 145);
        assert_eq!(xp_change(Chart::Four, 23, 20, Outcome::Loss), -145);
        // Low levels pay a fraction: level 4 losing evenly pays 25, level 1 pays 10.
        assert_eq!(xp_change(Chart::Two, 4, 4, Outcome::Loss), -25);
        assert_eq!(xp_change(Chart::Two, 1, 1, Outcome::Loss), -10);
        assert_eq!(xp_change(Chart::Two, 5, 5, Outcome::Draw), 0);
        assert_eq!(apply(5, -10), 0, "never below 0 XP");
    }

    #[test]
    fn teams_and_inactivity() {
        assert_eq!(team_level(&[10, 13]), 12, "11.5 rounds up");
        assert_eq!(team_level(&[3, 4, 4]), 4);
        assert_eq!(team_level(&[]), 1);
        assert_eq!(inactivity_change(Chart::Two, 10, 0), 0, "no minimum through level 10");
        assert_eq!(inactivity_change(Chart::Two, 25, 1), -105, "short of 2: a loss to level 24");
        assert_eq!(inactivity_change(Chart::Two, 25, 2), 0);
    }

    #[test]
    fn reports_settle_by_the_other_players() {
        let r = |reporter, subject, outcome| Report { reporter, subject, outcome };
        // Both agree.
        let agreed = [r(1, 1, Outcome::Win), r(1, 2, Outcome::Loss), r(2, 1, Outcome::Win), r(2, 2, Outcome::Loss)];
        assert_eq!(settle(&[1, 2], &agreed), Some(vec![(1, Outcome::Win), (2, Outcome::Loss)]));
        // Player 2 claims a disconnect for itself; player 1 saw a loss.
        let dodged = [r(1, 1, Outcome::Win), r(1, 2, Outcome::Loss), r(2, 2, Outcome::Disconnect)];
        assert_eq!(settle(&[1, 2], &dodged), Some(vec![(1, Outcome::Win), (2, Outcome::Loss)]));
        // Both claim the win: each is settled by the other's word, so both lose; no winner, no game.
        let disputed = [r(1, 1, Outcome::Win), r(1, 2, Outcome::Loss), r(2, 2, Outcome::Win), r(2, 1, Outcome::Loss)];
        assert_eq!(settle(&[1, 2], &disputed), None);
        // 2v2: player 3 left without reporting; the others' reports stand.
        let team = [r(1, 1, Outcome::Win), r(1, 2, Outcome::Win), r(1, 3, Outcome::Disconnect), r(1, 4, Outcome::Loss), r(2, 3, Outcome::Loss), r(4, 3, Outcome::Loss)];
        let settled = settle(&[1, 2, 3, 4], &team).unwrap();
        assert_eq!(settled, [(1, Outcome::Win), (2, Outcome::Win), (3, Outcome::Loss), (4, Outcome::Loss)], "a 1–2 vote settles as the majority");
        // A tied vote goes against the win; an unreported player disconnected.
        let tied = [r(2, 1, Outcome::Win), r(3, 1, Outcome::Loss), r(1, 2, Outcome::Win)];
        assert_eq!(settle(&[1, 2, 3], &tied), Some(vec![(1, Outcome::Loss), (2, Outcome::Win), (3, Outcome::Disconnect)]));
    }
}
