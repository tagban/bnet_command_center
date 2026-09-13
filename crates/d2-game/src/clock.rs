//! An act's time of day.
//!
//! Each act of a game keeps an environment clock (`.\ENVIRONMENT\Env.cpp`): a period (one of
//! six), its light phase, and ticks counting degrees around the day. The server advances it once
//! a frame and tells the act's clients (`0x53`) when something visible changed; the client uses
//! the light phase for lighting and, among other things, to light the Rogue Encampment's
//! bonfire from dusk to dawn.

use d2_data::engine::DayPeriod;

/// The clock of one act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActClock {
    act: u8,
    periods: [DayPeriod; 6],
    /// `env+0x00`.
    period: usize,
    /// `env+0x04`.
    phase: i32,
    /// `env+0x08`.
    ticks: i32,
    /// `env+0x28`.
    ticks_per_degree: i32,
    /// `env+0x30`: an eclipse runs its own period table, not ported.
    eclipse: bool,
    /// `env+0x34`: the degree last reported.
    reported_degree: i32,
}

impl ActClock {
    /// A new act's clock (`0x0061BE40`): period 2 at its angle, the first speed.
    #[must_use]
    pub fn new(act: u8, periods: [DayPeriod; 6], ticks_per_degree: i32) -> Self {
        let period = 2;
        let ticks_per_degree = ticks_per_degree.max(1);
        Self {
            act,
            periods,
            period,
            phase: periods[period].phase,
            ticks: ticks_per_degree * periods[period].angle,
            ticks_per_degree,
            eclipse: false,
            reported_degree: 0,
        }
    }

    /// What `0x53` carries: `(period, ticks, eclipse)` (`0x0061C330`).
    #[must_use]
    pub fn state(&self) -> (u32, u32, bool) {
        (self.period as u32, self.ticks as u32, self.eclipse)
    }

    /// The light phase: 0 day, 1 dusk, 2 night, 3 dawn.
    #[must_use]
    pub fn phase(&self) -> i32 {
        self.phase
    }

    /// One server frame (`0x0061C040` → `0x0061BEE0`). `true` when the act's clients must be
    /// sent the new state: the period or phase changed, or the clock moved more than 16 degrees
    /// since the last report.
    pub fn step(&mut self) -> bool {
        let (period, phase) = (self.period, self.phase);
        self.ticks += 1;
        if self.act == 3 {
            self.ticks += 15; // Act IV's days are short.
        } else if self.periods[self.period].phase == 2 {
            self.ticks += 1; // nights pass twice as fast,
            if self.act == 2 {
                self.ticks += 8; // and ten times as fast in Act III.
            }
        }
        if self.ticks >= self.ticks_per_degree * 360 {
            self.ticks = 0;
        }
        let next = (self.period + 1) % 6;
        if self.ticks > self.periods[next].angle * self.ticks_per_degree {
            self.period = next;
            self.phase = self.periods[next].phase;
            self.ticks = self.periods[next].angle * self.ticks_per_degree;
        }
        let degree = self.ticks / self.ticks_per_degree;
        if (degree - self.reported_degree).abs() > 16 {
            self.reported_degree = degree;
            return true;
        }
        period != self.period || phase != self.phase
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up periods in the table's shape: day at 0°, dusk at 100°, night at 200°, dawn at 300°
    /// and two more.
    fn periods() -> [DayPeriod; 6] {
        let p = |angle, phase| DayPeriod { angle, phase };
        [p(300, 3), p(330, 3), p(0, 0), p(100, 1), p(150, 1), p(200, 2)]
    }

    #[test]
    fn a_day_runs_through_its_periods_and_reports_what_changes() {
        let mut clock = ActClock::new(0, periods(), 4);
        assert_eq!((clock.state(), clock.phase()), ((2, 0, false), 0), "a new act starts at day");
        let mut reports = Vec::new();
        for frame in 1..=20_000u32 {
            if clock.step() {
                reports.push((frame, clock.state().0, clock.phase()));
            }
        }
        let dusk = reports.iter().find(|r| r.2 == 1).expect("dusk comes");
        assert_eq!((dusk.1, dusk.0), (3, 100 * 4 + 1), "past 100°, not at it");
        assert!(reports.iter().any(|r| r.0 == 17 * 4), "a report every 16° of day");
        assert!(reports.iter().any(|r| r.1 == 2 && r.2 == 0 && r.0 > dusk.0), "and day again after dawn");
    }

    #[test]
    fn act_four_runs_sixteen_ticks_a_frame_and_nights_run_double() {
        let mut four = ActClock::new(3, periods(), 4);
        four.step();
        assert_eq!(four.state().1, 16);
        let mut one = ActClock::new(0, periods(), 4);
        one.period = 5;
        one.phase = 2;
        one.ticks = 200 * 4;
        one.step();
        assert_eq!(one.state().1, 200 * 4 + 2);
    }
}
