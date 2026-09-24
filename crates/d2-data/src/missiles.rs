//! `Missiles.txt`: how a missile flies, what it collides with and how it hits (record stride
//! `0x1A4`). A missile's id is its row.

use d2_formats::excel::Table;

/// One missile row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Missile {
    /// `Missile`.
    pub name: String,
    /// `ReturnFire` (flags bit 9): Chilling Armor answers it.
    pub return_fire: bool,
    /// `Vel` (`+0x178`): speed in sixteenths of a subtile a frame, as a unit's velocity (`<< 8`,
    /// `0x004CD540`).
    pub velocity: i32,
    /// `MaxVel`.
    pub max_velocity: i32,
    /// `Accel`.
    pub acceleration: i32,
    /// `Range` (`+0x96`) and `LevRange` (`+0x98`): frames it lives, plus per skill level.
    pub range: (i32, i32),
    /// `Size`: the collision size it is tested with.
    pub size: i32,
    /// `CollideType`: what stops it (0 nothing, 3 walls and units, …).
    pub collide_type: i32,
    /// `CollideKill`: it ends when it collides.
    pub collide_kill: bool,
    /// `Explosion`: an explosion, dealing its damage where it starts.
    pub explosion: bool,
    /// `pSrvHitFunc`: what hitting does on the server (1 damages every unit in `sHitPar1` subtiles).
    pub server_hit_func: i32,
    /// `sHitPar1`–`sHitPar3`.
    pub server_hit_params: [i32; 3],
    /// `Skill`: the skill whose damage it deals, by name.
    pub skill: String,
    /// `Pierce`: may pass through what it hits.
    pub pierce: bool,
    /// `pSrvDoFunc`: what it does each frame on the server (1 flies, 5 burns where it lies, …).
    pub server_do_func: i32,
    /// `pSrvDmgFunc`: what its damage does besides (3 burns, 4 freezes, …).
    pub server_damage_func: i32,
    /// `Param1`–`Param5`: its do function's own numbers.
    pub params: [i32; 5],
    /// `NextHit` and `NextDelay`: it may hit again, and after how many frames a unit it hit may be
    /// hit by it again.
    pub next_hit: (bool, i32),
    /// `SubMissile1`–`SubMissile3`: what its do function makes as it goes.
    pub sub_missiles: [String; 3],
    /// `HitSubMissile1`–`HitSubMissile4`: what its hit function makes.
    pub hit_sub_missiles: [String; 4],
    /// `EType`: the element of its own damage, for a missile with no `Skill` to take it from.
    pub element: String,
    /// `EMin` and `MinELev1`–`MinELev5`: its own elemental minimum and its level brackets.
    pub element_min: (i32, [i32; 5]),
    /// `Emax` and `MaxELev1`–`MaxELev5`.
    pub element_max: (i32, [i32; 5]),
    /// `EDmgSymPerCalc`: the synergy percent on its own elemental damage.
    pub element_synergy: String,
    /// `HitShift`: its own damage's shift into 256ths.
    pub hit_shift: i32,
    /// `DamageRate`: how much of the target's damage reduction applies, in 1024ths.
    pub damage_rate: i32,
    /// `MinDamage` and `MaxDamage`: its own physical damage, before `HitShift`.
    pub physical: (i32, i32),
    /// `ELen` and `ELevLen1`–`ELevLen3`: how long its own cold chills or its poison lasts.
    pub element_length: (i32, [i32; 3]),
    /// `MinLevDam1`–`5` and `MaxLevDam1`–`5`: its own physical damage's level brackets.
    pub physical_levels: ([i32; 5], [i32; 5]),
    /// `SrcDamage`: 128ths of its shooter's damage it carries (a monster's arrow its whole A1).
    pub source_damage: i32,
    /// `ToHit`: it rolls to hit what it meets, and ends on a miss.
    pub to_hit: bool,
    /// `VelLev`: eighths of a sixteenth of a subtile a frame faster a level.
    pub velocity_per_level: i32,
}

/// Every missile, by id.
#[derive(Debug, Clone, Default)]
pub struct Missiles {
    rows: Vec<Missile>,
}

impl Missiles {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|row| {
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                Missile {
                    name: row.get("Missile").unwrap_or_default().to_string(),
                    velocity: int("Vel"),
                    max_velocity: int("MaxVel"),
                    acceleration: int("Accel"),
                    range: (int("Range"), int("LevRange")),
                    size: int("Size"),
                    collide_type: int("CollideType"),
                    collide_kill: int("CollideKill") != 0,
                    explosion: int("Explosion") != 0,
                    server_hit_func: int("pSrvHitFunc"),
                    server_hit_params: [int("sHitPar1"), int("sHitPar2"), int("sHitPar3")],
                    skill: row.get("Skill").unwrap_or_default().to_string(),
                    pierce: int("Pierce") != 0,
                    server_do_func: int("pSrvDoFunc"),
                    server_damage_func: int("pSrvDmgFunc"),
                    params: [int("Param1"), int("Param2"), int("Param3"), int("Param4"), int("Param5")],
                    next_hit: (int("NextHit") != 0, int("NextDelay")),
                    sub_missiles: ["SubMissile1", "SubMissile2", "SubMissile3"].map(|c| row.get(c).unwrap_or_default().to_string()),
                    hit_sub_missiles: ["HitSubMissile1", "HitSubMissile2", "HitSubMissile3", "HitSubMissile4"].map(|c| row.get(c).unwrap_or_default().to_string()),
                    element: row.get("EType").unwrap_or_default().to_string(),
                    element_min: (int("EMin"), ["MinELev1", "MinELev2", "MinELev3", "MinELev4", "MinELev5"].map(int)),
                    element_max: (int("Emax"), ["MaxELev1", "MaxELev2", "MaxELev3", "MaxELev4", "MaxELev5"].map(int)),
                    element_synergy: row.get("EDmgSymPerCalc").unwrap_or_default().to_string(),
                    hit_shift: int("HitShift"),
                    damage_rate: int("DamageRate"),
                    physical: (int("MinDamage"), int("MaxDamage")),
                    element_length: (int("ELen"), ["ELevLen1", "ELevLen2", "ELevLen3"].map(int)),
                    physical_levels: (
                        ["MinLevDam1", "MinLevDam2", "MinLevDam3", "MinLevDam4", "MinLevDam5"].map(int),
                        ["MaxLevDam1", "MaxLevDam2", "MaxLevDam3", "MaxLevDam4", "MaxLevDam5"].map(int),
                    ),
                    source_damage: int("SrcDamage"),
                    to_hit: int("ToHit") != 0,
                    velocity_per_level: int("VelLev"),
                    return_fire: int("ReturnFire") != 0,
                }
            })
            .collect();
        Self { rows }
    }

    /// A missile by id.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&Missile> {
        usize::try_from(id).ok().and_then(|i| self.rows.get(i))
    }

    /// A missile's id by name, any case.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<i32> {
        self.rows.iter().position(|m| m.name.eq_ignore_ascii_case(name)).map(|i| i as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missiles_keep_their_rows() {
        let t = Table::parse(b"Missile\tId\tVel\tRange\tLevRange\tCollideType\tCollideKill\tpSrvHitFunc\tsHitPar1\tSkill\r\narrow\t0\t24\t40\t\t3\t1\t\t\t\r\nfireball\t1\t20\t50\t\t3\t1\t1\t4\tFire Ball\r\n");
        let missiles = Missiles::from_table(&t);
        let fireball = missiles.get(missiles.id("FireBall").unwrap()).unwrap();
        assert_eq!((fireball.velocity, fireball.range, fireball.server_hit_func, fireball.server_hit_params[0], fireball.skill.as_str()), (20, (50, 0), 1, 4, "Fire Ball"));
        assert!(missiles.get(0).unwrap().collide_kill);
    }
}
