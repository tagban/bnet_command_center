//! The engine's seeded RNG: a 64-bit LCG kept as two 32-bit halves.
//!
//! Each step computes `low * 0x6AC690C5 + high` as 64 bits and splits it back into
//! `{low, high}` (`D2_SEED_NEXT`). Ported from libd2 `packages/core/src/rng.zig` (MIT).

const MULTIPLIER: u64 = 0x6AC6_90C5;

/// `D2SeedStrc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seed {
    /// `nSeedLow`.
    pub low: u32,
    /// `nSeedHigh`.
    pub high: u32,
}

impl Seed {
    /// A seed from its two halves.
    #[must_use]
    pub const fn new(low: u32, high: u32) -> Self {
        Self { low, high }
    }

    /// Advance one step, returning the full 64-bit state.
    pub fn next(&mut self) -> u64 {
        let state = u64::from(self.low).wrapping_mul(MULTIPLIER).wrapping_add(u64::from(self.high));
        self.low = state as u32;
        self.high = (state >> 32) as u32;
        state
    }

    /// `RollRandomSeed` (`0x0045C370`): step and return the new low word.
    pub fn roll(&mut self) -> u32 {
        self.next();
        self.low
    }

    /// `RANDOM_RandomNumberSelector` (`0x0045C3E0`): uniform in `[0, modulo)` from the new low
    /// word (masked for a power of two). A modulo with its top bit set reads as `< 1`: 0, no step.
    pub fn pick(&mut self, modulo: u32) -> u32 {
        if (modulo as i32) < 1 {
            return 0;
        }
        self.next();
        if modulo & (modulo - 1) != 0 {
            self.low % modulo
        } else {
            self.low & (modulo - 1)
        }
    }
}

/// The act "start seed" level seeds derive from: the game seed stepped once
/// (`DRLG_AllocDrlgActMisc`, `0x006424xx`).
#[must_use]
pub fn act_start_seed(game_seed: u32) -> u32 {
    Seed::new(game_seed, 0x29A).roll()
}

/// A level's seed: `{start + level id, 0x29A}` (`DRLGACTMISC_AllocDrlgLevel`).
#[must_use]
pub fn level_seed(start_seed: u32, level_id: i32) -> Seed {
    Seed::new(start_seed.wrapping_add(level_id as u32), 0x29A)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_splits_the_64_bit_state() {
        let mut s = Seed::new(1, 0x29A);
        assert_eq!(s.next(), 0x6AC6_90C5 + 0x29A);
        assert_eq!((s.low, s.high), (0x6AC6_935F, 0));
    }

    #[test]
    fn pick_masks_powers_of_two_and_mods_the_rest() {
        assert_eq!(Seed::new(1, 0x29A).pick(16), 0x6AC6_935F & 15);
        assert_eq!(Seed::new(1, 0x29A).pick(100), 0x6AC6_935F % 100);
        let mut s = Seed::new(123, 456);
        assert_eq!((s.pick(0), s.pick(0x8000_0000)), (0, 0));
        assert_eq!(s, Seed::new(123, 456), "no step for a modulo below 1");
    }
}
