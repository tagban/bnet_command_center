//! What a server needs to know about a monster class before it simulates one: whether the
//! server spawns it at all, and how many variants each of its graphics components has.
//!
//! `MonStats.txt` gives the class (`hcIdx`) and its `MonStatsEx`, which names the
//! `MonStats2.txt` row holding the display side.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::Error;

/// Graphics components in engine order: the `MonStats2.txt` variant columns behind the 16
/// counts at `+0x15` of the compiled record.
pub const COMPONENT_COLUMNS: [&str; 16] =
    ["HDv", "TRv", "LGv", "Rav", "Lav", "RHv", "LHv", "SHv", "S1v", "S2v", "S3v", "S4v", "S5v", "S6v", "S7v", "S8v"];

/// One monster class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonsterClass {
    /// `MonStats.txt` `Id`.
    pub id: String,
    /// `MonStats2.txt` `critter`: the client spawns these itself (from `Levels.txt` `cmon*`), so
    /// the server skips them when it places a map's preset monsters (flag 13, tested by
    /// `0x0054E490`).
    pub critter: bool,
    /// Variants per component, as the engine counts them: the entries in each variant column.
    pub components: [u8; 16],
    /// `MonStats.txt` `interact` (flag bit 9, record `+0xD` bit 1): a player can talk to it
    /// (`0x00572C10` refuses otherwise).
    pub interact: bool,
    /// `MonStats.txt` `npc` (flag bit 8).
    pub npc: bool,
    /// `MonStats.txt` `Align` (record `+0x4C`): 1 for the player's side, 2 neutral, else hostile.
    pub align: u8,
    /// `MonStats2.txt` `SizeX` (record `+8`): the shape a spot is tested with — 1 one subtile, 2
    /// a cross, 3 a 3×3 square (`0x0064D9B0`).
    pub size: u8,
    /// `MonStats2.txt` `spawnCol` (record `+0xA`): which collision bits keep it from standing
    /// somewhere (`0x005B2A00`).
    pub spawn_collision: u8,
    /// `MonStats2.txt` `restore` (record `+0x130`): whether the unit is kept when its room is
    /// released (`0x005431F0`, the last word on it). **0 never**, **2 always**, 1 leaves the
    /// decision to the level's `SaveMonsters` and, for a corpse, its roll. Town NPCs are 2, which
    /// is why they survive a town whose `SaveMonsters` is 0.
    pub restore: u8,
    /// `MonStats.txt` `boss`: an act boss — never stunned, terrified or taunted (`0x0057AAE0`,
    /// `0x00623470`).
    pub boss: bool,
    /// `MonStats.txt` `SwitchAI`: whether a skill may change its mind — Howl's terror and Taunt
    /// need it (`0x00623470`).
    pub switch_ai: bool,
    /// How the class spawns in a level's rooms.
    pub spawn: SpawnRules,
    /// How it fights.
    pub combat: CombatStats,
}

/// The `MonStats.txt` columns a fight reads, by difficulty (Normal, Nightmare, Hell). Life,
/// defence, attack rating, damage and experience are percentages of `MonLvl.txt` at its level.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CombatStats {
    /// `Level`.
    pub level: [i32; 3],
    /// `minHP`/`maxHP`.
    pub life: [(i32, i32); 3],
    /// `AC`.
    pub defense: [i32; 3],
    /// `Exp`.
    pub experience: [i32; 3],
    /// `A1MinD`/`A1MaxD`.
    pub damage: [(i32, i32); 3],
    /// `A1TH`.
    pub to_hit: [i32; 3],
    /// `aidist`: how far it notices a player, subtiles; 0 for the default.
    pub ai_distance: [i32; 3],
    /// `aidel`: frames between its AI's thoughts.
    pub ai_delay: [i32; 3],
    /// `Velocity` and `Run`: walking and running speed.
    pub speed: (i32, i32),
    /// `MonStats2.txt` `MeleeRng`: how far its melee reaches, subtiles.
    pub melee_range: i32,
    /// `AI`.
    pub ai: String,
    /// `Code`: the animation token, e.g. `FA`.
    pub token: String,
    /// `MonStats2.txt` `BaseW`: the weapon class its animations are drawn with.
    pub weapon_class: String,
    /// `TreasureClass1` by difficulty: what it drops.
    pub treasure: [String; 3],
    /// `TreasureClass2` by difficulty: what it drops as a champion.
    pub treasure_champion: [String; 3],
    /// `TreasureClass3` by difficulty: what it drops as a unique (`0x005A6600`).
    pub treasure_unique: [String; 3],
    /// `MonType`: the type a boss modifier's `exclude` columns are matched against.
    pub mon_type: String,
    /// `isMelee`: a boss of it cannot have multiple shots.
    pub is_melee: bool,
    /// `noMultiShot`.
    pub no_multishot: bool,
    /// `MonStats2.txt` `mA1`, `mWL`: it has an attack mode, a walk mode (the modifiers that need
    /// them, `0x005A03E0`).
    pub modes: (bool, bool),
    /// `ResDm`, `ResMa`, `ResFi`, `ResLi`, `ResCo`, `ResPo` by difficulty: percent resistance to
    /// physical, magic, fire, lightning, cold and poison damage.
    pub resistances: [[i32; 6]; 3],
    /// `coldeffect` by difficulty (record `+0x168`): the percent a chill leaves of its speeds
    /// (negative, −50 half); 0 cannot be chilled, and only a negative one can be frozen
    /// (`0x0057AF80`, `0x0057B230`).
    pub cold_effect: [i32; 3],
    /// `Skill1`–`Skill8`: the skills it uses, by name, with their levels (`Sk1lvl`…).
    pub skills: Vec<(String, i32)>,
    /// `aip1`–`aip8` by difficulty: its AI's own numbers (a trap's reach is `aip4`).
    pub ai_params: [[i32; 3]; 8],
    /// `Drain` by difficulty: the percent of what is struck from it that a life steal takes.
    pub drain: [i32; 3],
    /// `noRatio`: its life, defence, attack rating and damage are its own numbers, not
    /// percentages of `MonLvl.txt` — a player's summons.
    pub no_ratio: bool,
    /// `El1`–`El3`: the elemental damage its attacks of a mode carry (`0x005A4F50`).
    pub elements: Vec<ElementAttack>,
    /// `Crit`: the percent of its hits that do double (`0x005A5560`).
    pub crit: i32,
    /// `inTown`: it may hurt a player in town (`0x0057C6C0`).
    pub in_town: bool,
    /// `MonStats2.txt` `HitClass`: how hard its blows land, which sets how easily they make a
    /// player flinch (`0x0057CB00`).
    pub hit_class: i32,
}

/// One of a class's `El1`–`El3`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementAttack {
    /// `ElnMode`: the mode whose attacks carry it (`A1`, `A2`, `S1`…).
    pub mode: String,
    /// `ElnType`: `fire`, `ltng`, `mag`, `cold`, `pois`, `life`, `mana`, `stam`, `stun`, `rand`,
    /// `burn`.
    pub kind: String,
    /// `ElnPct` by difficulty: the percent of those attacks that carry it.
    pub percent: [i32; 3],
    /// `ElnMinD`, `ElnMaxD` by difficulty, as percentages of the monster level's damage.
    pub damage: [(i32, i32); 3],
    /// `ElnDur` by difficulty, frames.
    pub length: [i32; 3],
}

/// The `MonStats.txt` columns room population reads, class names resolved to class ids (-1 for
/// none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpawnRules {
    /// `isSpawn`: may be picked for a level's roster.
    pub is_spawn: bool,
    /// `Rarity`: its weight in the roster pick.
    pub rarity: i32,
    /// `rangedtype`: counts as ranged for a `rangedspawn` level's first pick.
    pub ranged: bool,
    /// `MinGrp`/`MaxGrp`: how many of it a spawn places.
    pub group: (i32, i32),
    /// `PartyMin`/`PartyMax`: how many minions come with each.
    pub party: (i32, i32),
    /// `minion1`/`minion2`.
    pub minions: [i32; 2],
    /// `spawn`: the class it can be replaced by when placed, with `placespawn`.
    pub spawn: i32,
    /// `placespawn`.
    pub place_spawn: bool,
    /// `sparsePopulate`: percent chance a placement goes ahead.
    pub sparse: i32,
    /// `BaseId`.
    pub base: i32,
}

/// Monster classes by id (`hcIdx`).
#[derive(Debug, Clone, Default)]
pub struct Monsters {
    by_class: HashMap<i32, MonsterClass>,
    by_name: HashMap<String, i32>,
}

impl Monsters {
    /// Join `MonStats.txt` to `MonStats2.txt`.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if a needed column is missing.
    pub fn from_tables(monstats: &Table, monstats2: &Table) -> Result<Self, Error> {
        for (t, table, column) in [
            (monstats, "monstats.txt", "Id"),
            (monstats, "monstats.txt", "hcIdx"),
            (monstats, "monstats.txt", "MonStatsEx"),
            (monstats2, "monstats2.txt", "Id"),
        ] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table, problem: format!("no {column} column") });
            }
        }
        let mode_flags: HashMap<String, (bool, bool)> = monstats2
            .rows()
            .filter_map(|row| Some((row.get("Id")?.to_ascii_lowercase(), (row.int("mA1").unwrap_or(0) != 0, row.int("mWL").unwrap_or(0) != 0))))
            .collect();
        let weapon_classes: HashMap<String, String> = monstats2
            .rows()
            .filter_map(|row| Some((row.get("Id")?.to_ascii_lowercase(), row.get("BaseW").unwrap_or("hth").to_string())))
            .collect();
        /// What `MonStats2.txt` contributes to a class: critter, component variants, size,
        /// spawn collision, melee range, and whether a released room keeps the unit.
        type Display = (bool, [u8; 16], u8, u8, i32, u8, i32);
        let display: HashMap<String, Display> = monstats2
            .rows()
            .filter_map(|row| {
                let mut components = [0u8; 16];
                for (count, column) in components.iter_mut().zip(COMPONENT_COLUMNS) {
                    let variants = row.get(column).map_or(0, |v| v.split(',').filter(|s| !s.trim().is_empty()).count());
                    *count = u8::try_from(variants).unwrap_or(u8::MAX);
                }
                let byte = |c: &str| u8::try_from(row.int(c).unwrap_or(0)).unwrap_or(0);
                let melee = row.int("MeleeRng").unwrap_or(0) as i32;
                Some((
                    row.get("Id")?.to_ascii_lowercase(),
                    (
                        row.int("critter").unwrap_or(0) != 0,
                        components,
                        byte("SizeX"),
                        byte("spawnCol"),
                        melee,
                        // A table without the column leaves the decision to the level; only a
                        // table that says 0 means "never keep this".
                        row.int("restore").map_or(1, |v| u8::try_from(v).unwrap_or(1)),
                        row.int("HitClass").unwrap_or(0) as i32,
                    ),
                ))
            })
            .collect();
        let by_name: HashMap<String, i32> = monstats
            .rows()
            .filter_map(|row| Some((row.get("Id")?.to_ascii_lowercase(), i32::try_from(row.int("hcIdx")?).ok()?)))
            .collect();
        let class_of = |name: Option<&str>| name.and_then(|n| by_name.get(&n.to_ascii_lowercase()).copied()).unwrap_or(-1);
        let by_class = monstats
            .rows()
            .filter_map(|row| {
                let class = i32::try_from(row.int("hcIdx")?).ok()?;
                let id = row.get("Id")?.to_string();
                let ex = row.get("MonStatsEx")?.to_ascii_lowercase();
                let &(critter, components, size, spawn_collision, melee_range, restore, hit_class) = display.get(&ex)?;
                let flag = |c: &str| row.int(c).unwrap_or(0) != 0;
                let int = |c: &str| row.int(c).unwrap_or(0) as i32;
                let spawn = SpawnRules {
                    is_spawn: flag("isSpawn"),
                    rarity: int("Rarity"),
                    ranged: flag("rangedtype"),
                    group: (int("MinGrp"), int("MaxGrp")),
                    party: (int("PartyMin"), int("PartyMax")),
                    minions: [class_of(row.get("minion1")), class_of(row.get("minion2"))],
                    spawn: class_of(row.get("spawn")),
                    place_spawn: flag("placespawn"),
                    sparse: int("sparsePopulate"),
                    base: class_of(row.get("BaseId")),
                };
                let per = |a: &str, b: &str, c: &str| [int(a), int(b), int(c)];
                let range = |lo: [&str; 3], hi: [&str; 3]| [(int(lo[0]), int(hi[0])), (int(lo[1]), int(hi[1])), (int(lo[2]), int(hi[2]))];
                let combat = CombatStats {
                    level: per("Level", "Level(N)", "Level(H)"),
                    life: range(["minHP", "MinHP(N)", "MinHP(H)"], ["maxHP", "MaxHP(N)", "MaxHP(H)"]),
                    defense: per("AC", "AC(N)", "AC(H)"),
                    experience: per("Exp", "Exp(N)", "Exp(H)"),
                    damage: range(["A1MinD", "A1MinD(N)", "A1MinD(H)"], ["A1MaxD", "A1MaxD(N)", "A1MaxD(H)"]),
                    to_hit: per("A1TH", "A1TH(N)", "A1TH(H)"),
                    ai_distance: per("aidist", "aidist(N)", "aidist(H)"),
                    ai_delay: per("aidel", "aidel(N)", "aidel(H)"),
                    speed: (int("Velocity"), int("Run")),
                    melee_range,
                    ai: row.get("AI").unwrap_or_default().to_string(),
                    token: row.get("Code").unwrap_or_default().to_string(),
                    weapon_class: weapon_classes.get(&ex).cloned().unwrap_or_else(|| "hth".into()),
                    treasure: ["TreasureClass1", "TreasureClass1(N)", "TreasureClass1(H)"].map(|c| row.get(c).unwrap_or_default().to_string()),
                    treasure_champion: ["TreasureClass2", "TreasureClass2(N)", "TreasureClass2(H)"].map(|c| row.get(c).unwrap_or_default().to_string()),
                    treasure_unique: ["TreasureClass3", "TreasureClass3(N)", "TreasureClass3(H)"].map(|c| row.get(c).unwrap_or_default().to_string()),
                    mon_type: row.get("MonType").unwrap_or_default().to_string(),
                    is_melee: flag("isMelee"),
                    no_multishot: flag("noMultiShot"),
                    modes: mode_flags.get(&ex).copied().unwrap_or((true, true)),
                    resistances: ["", "(N)", "(H)"].map(|d| ["ResDm", "ResMa", "ResFi", "ResLi", "ResCo", "ResPo"].map(|r| int(&format!("{r}{d}")))),
                    cold_effect: ["coldeffect", "coldeffect(N)", "coldeffect(H)"].map(int),
                    no_ratio: int("noRatio") != 0,
                    drain: ["Drain", "Drain(N)", "Drain(H)"].map(int),
                    skills: (1..=8).filter_map(|n| Some((row.get(&format!("Skill{n}")).filter(|s| !s.is_empty())?.to_string(), int(&format!("Sk{n}lvl"))))).collect(),
                    ai_params: std::array::from_fn(|n| [format!("aip{}", n + 1), format!("aip{}(N)", n + 1), format!("aip{}(H)", n + 1)].map(|c| int(&c))),
                    elements: (1..=3)
                        .filter_map(|n| {
                            let mode = row.get(&format!("El{n}Mode")).filter(|s| !s.is_empty())?.to_string();
                            let each = |c: &str| ["", "(N)", "(H)"].map(|d| int(&format!("El{n}{c}{d}")));
                            let (lo, hi) = (each("MinD"), each("MaxD"));
                            Some(ElementAttack {
                                mode,
                                kind: row.get(&format!("El{n}Type")).unwrap_or_default().to_string(),
                                percent: each("Pct"),
                                damage: [(lo[0], hi[0]), (lo[1], hi[1]), (lo[2], hi[2])],
                                length: each("Dur"),
                            })
                        })
                        .collect(),
                    crit: int("Crit"),
                    in_town: flag("inTown"),
                    hit_class,
                };
                Some((class, MonsterClass {
                    id,
                    critter,
                    components,
                    restore,
                    interact: flag("interact"),
                    npc: flag("npc"),
                    align: int("Align") as u8,
                    size,
                    spawn_collision,
                    boss: flag("boss"),
                    switch_ai: flag("SwitchAI"),
                    spawn,
                    combat,
                }))
            })
            .collect();
        Ok(Self { by_class, by_name })
    }

    /// A class id by `MonStats.txt` `Id`, any case.
    #[must_use]
    pub fn class_named(&self, name: &str) -> Option<i32> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// A class by id.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&MonsterClass> {
        self.by_class.get(&class)
    }
}

impl MonsterClass {
    /// The alignment the engine gives a monster of this class (`0x005B2A00`): `Align` 1 is good
    /// (2), 2 neutral (1), anything else evil (0).
    #[must_use]
    pub fn alignment(&self) -> u8 {
        match self.align {
            1 => 2,
            2 => 1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_join_their_display_row_and_count_variants() {
        let monstats = Table::parse(b"Id\thcIdx\tMonStatsEx\tnpc\tinteract\r\nguard\t7\tguardex\t1\t1\r\nhen\t8\thenex\t\t\r\nlost\t9\tnowhere\t\t\r\n");
        let mut ms2 = String::from("Id\tcritter");
        for c in COMPONENT_COLUMNS {
            ms2.push('\t');
            ms2.push_str(c);
        }
        ms2.push_str("\r\nGUARDEX\t\t\tlit\t\t\t\t\tsbw,lbw\r\nhenex\t1\t\tlit\r\n");
        let m = Monsters::from_tables(&monstats, &Table::parse(ms2.as_bytes())).unwrap();
        let guard = m.get(7).unwrap();
        assert_eq!((guard.id.as_str(), guard.critter, guard.interact, guard.npc), ("guard", false, true, true));
        assert!(!m.get(8).unwrap().interact);
        assert_eq!(guard.components[..8], [0, 1, 0, 0, 0, 0, 2, 0], "TR one variant, LH two");
        assert!(m.get(8).unwrap().critter);
        assert!(m.get(9).is_none(), "no display row, no class");
        assert_eq!(m.class_named("HEN"), Some(8));
    }
}
