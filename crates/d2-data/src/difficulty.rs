//! `DifficultyLevels.txt`: what each difficulty changes — the resistance penalty players take, and
//! how much shorter chills, freezes and curses are on its monsters.

use d2_formats::excel::Table;

/// One difficulty's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Difficulty {
    /// `ResistPenalty`: added to a player's resistances in an expansion game.
    pub resist_penalty: i32,
    /// `MonsterColdDivisor` (`+0x18`): a monster's chill length is divided by it.
    pub cold_divisor: i32,
    /// `MonsterFreezeDivisor` (`+0x14`): a monster's freeze length is divided by it.
    pub freeze_divisor: i32,
    /// `AiCurseDivisor` (`+0x1C`): a terror's or a curse's length on a monster is divided by it.
    pub curse_divisor: i32,
    /// `StaticFieldMin` (`+0x40`): in an expansion game Static Field leaves a monster at least
    /// this percent of its life.
    pub static_field_min: i32,
    /// `ChampionDamageBonus` (`+0x34`): percent of the champion and Extra Strong bonuses dealt.
    pub champion_damage_bonus: i32,
    /// `MonsterCEDamagePercent` (`+0x3C`): a Fire Enchanted death blast's share of its life.
    pub ce_damage_percent: i32,
    /// `MonsterSkillBonus` (`+0x10`): levels a monster's skills and missiles gain.
    pub monster_skill_bonus: i32,
}

impl Default for Difficulty {
    fn default() -> Self {
        Self { resist_penalty: 0, cold_divisor: 1, freeze_divisor: 1, curse_divisor: 1, static_field_min: 0, champion_damage_bonus: 100, ce_damage_percent: 0, monster_skill_bonus: 0 }
    }
}

/// The three rows, Normal first.
#[derive(Debug, Clone, Default)]
pub struct Difficulties {
    rows: Vec<Difficulty>,
}

impl Difficulties {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|r| {
                let int = |c: &str, default: i64| r.int(c).filter(|&v| v != 0).unwrap_or(default) as i32;
                Difficulty {
                    resist_penalty: r.int("ResistPenalty").unwrap_or(0) as i32,
                    cold_divisor: int("MonsterColdDivisor", 1),
                    freeze_divisor: int("MonsterFreezeDivisor", 1),
                    curse_divisor: int("AiCurseDivisor", 1),
                    static_field_min: r.int("StaticFieldMin").unwrap_or(0) as i32,
                    champion_damage_bonus: r.int("ChampionDamageBonus").unwrap_or(100) as i32,
                    ce_damage_percent: r.int("MonsterCEDamagePercent").unwrap_or(0) as i32,
                    monster_skill_bonus: r.int("MonsterSkillBonus").unwrap_or(0) as i32,
                }
            })
            .collect();
        Self { rows }
    }

    /// A difficulty's row; the defaults — nothing divided — for one the table lacks.
    #[must_use]
    pub fn get(&self, difficulty: u8) -> Difficulty {
        self.rows.get(usize::from(difficulty)).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_the_difficulties_in_order() {
        let t = Table::parse(b"Name\tResistPenalty\tMonsterColdDivisor\tMonsterFreezeDivisor\tAiCurseDivisor\r\nNormal\t0\t1\t1\t1\r\nNightmare\t-40\t2\t2\t2\r\nHell\t-100\t4\t4\t4\r\n");
        let d = Difficulties::from_table(&t);
        assert_eq!(d.get(1), Difficulty { resist_penalty: -40, cold_divisor: 2, freeze_divisor: 2, curse_divisor: 2, static_field_min: 0, champion_damage_bonus: 100, ce_damage_percent: 0, monster_skill_bonus: 0 });
        assert_eq!(d.get(9), Difficulty::default());
    }
}
