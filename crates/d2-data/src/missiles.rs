//! `Missiles.txt`: how a missile flies, what it collides with and how it hits (record stride
//! `0x1A4`). A missile's id is its row.

use d2_formats::excel::Table;

/// One missile row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Missile {
    /// `Missile`.
    pub name: String,
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
