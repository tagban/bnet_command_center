//! `data\global\animdata.d2`: how long each unit animation runs and on which frame its action
//! lands.
//!
//! An animation is named by its COF: unit token, mode and weapon class, e.g. `FAA1HTH` (a Fallen's
//! first attack, hand to hand). The file is 256 hash buckets, each a record count and that many
//! 160-byte records: the name (8 bytes, NUL-padded), frames per direction (`u32`), speed (`u32`,
//! 256ths of a frame advanced per game frame at 100% rate) and 144 frame trigger bytes (1 = the
//! attack connects on that frame). Written from the published AnimData format notes.

use std::collections::HashMap;
use std::fmt;

/// Why an AnimData file could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file ended early.
    Truncated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AnimData: truncated")
    }
}

impl std::error::Error for Error {}

/// Size of a record.
const RECORD: usize = 160;
/// Hash buckets before the records.
const BUCKETS: usize = 256;

/// One animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anim {
    /// Frames per direction.
    pub frames: u32,
    /// 256ths of a frame the animation advances each game frame at 100% rate.
    pub speed: u32,
    /// Per-frame trigger byte.
    pub triggers: [u8; 144],
}

impl Anim {
    /// Game frames the whole animation takes at `rate` percent (100 for none): the frames times
    /// 256 over the speed scaled by the rate, rounded up.
    #[must_use]
    pub fn game_frames(&self, rate: u32) -> u32 {
        let step = self.speed * rate / 100;
        if step == 0 {
            return self.frames;
        }
        (self.frames * 256).div_ceil(step)
    }

    /// Game frames until the first frame with a trigger (the hit), `None` when none is marked.
    #[must_use]
    pub fn trigger_game_frames(&self, rate: u32) -> Option<u32> {
        let frame = self.triggers.iter().take(self.frames as usize).position(|&t| t != 0)? as u32;
        let step = self.speed * rate / 100;
        Some(if step == 0 { frame } else { (frame * 256).div_ceil(step) })
    }
}

/// Every animation, by upper-case COF name.
#[derive(Debug, Clone, Default)]
pub struct AnimData {
    by_name: HashMap<String, Anim>,
}

impl AnimData {
    /// Parse the file.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] if a bucket's records run past the end.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut by_name = HashMap::new();
        let mut at = 0;
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(Error::Truncated);
        for _ in 0..BUCKETS {
            let count = u32_at(at)? as usize;
            at += 4;
            for _ in 0..count {
                let record = bytes.get(at..at + RECORD).ok_or(Error::Truncated)?;
                let name: String = record[..8].iter().take_while(|&&b| b != 0).map(|&b| char::from(b).to_ascii_uppercase()).collect();
                let mut triggers = [0; 144];
                triggers.copy_from_slice(&record[16..]);
                by_name.insert(name, Anim { frames: u32_at(at + 8)?, speed: u32_at(at + 12)?, triggers });
                at += RECORD;
            }
        }
        Ok(Self { by_name })
    }

    /// The animation of `token`, `mode` and weapon class, e.g. `("FA", "A1", "hth")`.
    #[must_use]
    pub fn get(&self, token: &str, mode: &str, weapon_class: &str) -> Option<&Anim> {
        self.by_name.get(&format!("{token}{mode}{weapon_class}").to_ascii_uppercase())
    }

    /// How many animations the file holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, frames: u32, speed: u32, trigger: Option<usize>) -> Vec<u8> {
        let mut r = vec![0u8; RECORD];
        r[..name.len()].copy_from_slice(name.as_bytes());
        r[8..12].copy_from_slice(&frames.to_le_bytes());
        r[12..16].copy_from_slice(&speed.to_le_bytes());
        if let Some(t) = trigger {
            r[16 + t] = 1;
        }
        r
    }

    #[test]
    fn records_are_found_by_cof_name_in_any_bucket() {
        let mut file = Vec::new();
        for bucket in 0..BUCKETS {
            match bucket {
                7 => {
                    file.extend_from_slice(&2u32.to_le_bytes());
                    file.extend(record("XXA1HTH", 10, 256, Some(7)));
                    file.extend(record("XXDTHTH", 27, 240, None));
                }
                _ => file.extend_from_slice(&0u32.to_le_bytes()),
            }
        }
        let data = AnimData::parse(&file).unwrap();
        assert_eq!(data.len(), 2);
        let attack = data.get("xx", "a1", "hth").unwrap();
        assert_eq!(attack.game_frames(100), 10);
        assert_eq!(attack.trigger_game_frames(100), Some(7));
        assert_eq!(attack.game_frames(200), 5, "twice the rate, half the frames");
        let death = data.get("XX", "DT", "HTH").unwrap();
        assert_eq!(death.game_frames(100), 29, "27 frames at 240/256 a game frame, rounded up");
        assert_eq!(death.trigger_game_frames(100), None);
        assert!(AnimData::parse(&file[..file.len() - 1]).is_err());
    }
}
