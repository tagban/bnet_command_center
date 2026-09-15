//! Fights: players hitting monsters, monsters chasing and hitting players, deaths, experience
//! and levels.
//!
//! A game's [`Battle`] runs in engine frames (25 a second). It knows the hostile monsters of the
//! rooms populated so far and the players in the game, and turns what they do into [`Event`]s —
//! a life bar dropping, a flinch, a death, a walk, a swing, experience — that the game server
//! sends as packets. Nothing here touches the network.
//!
//! What follows the engine, and where from:
//! - Monster life, defence, attack rating, damage and experience are the class's `MonStats.txt`
//!   value times `MonLvl.txt`'s percentage for its level, the `L-` columns in an expansion game
//!   (`0x005A0000` looks the row up, stride `0x78`; `0x005A1990` indexes it by difficulty and
//!   game type). Monster level is `MonStats.txt` `Level` on Normal and the area's `MonLvl2`/`3`
//!   (`Ex` in an expansion game) on Nightmare and Hell.
//! - Chance to hit (`0x0057D9B0`): `200 × AR / (AR + DEF) × alvl / (alvl + dlvl)`, clamped to
//!   5..95. A player's rating is `(dexterity − 7) × 5 + ToHitFactor` (`0x00622560`), its defence
//!   `dexterity / 4` (`0x006223F0`); a monster's rating is its `A1TH` scaled, its defence `AC`.
//! - Whether a hit makes the victim flinch (`0x0057CB00`): never below a sixteenth of its maximum
//!   life for physical damage, always from a quarter.
//! - Animation lengths and hit frames come from `animdata.d2`.
//! - Stamina (`0x0057F240`, `0x00580500`): running outside a town spends `RunDrain × 2` 256ths a
//!   frame; standing gains a 256th of the maximum a frame, walking half that, and swinging or
//!   running nothing. On Battle.net the server does this and tells the client.
//! - Potions (`0x005BF240` by `Misc.txt` `pSpell`): a healing or mana potion (3, `0x005BE3F0`) spreads
//!   its `calc1` points — ×1.5 for an Amazon, Paladin or Assassin, ×2 for a Barbarian's healing and
//!   a Sorceress's, Necromancer's or Druid's mana, and doubled again when `rand(100)` falls under
//!   half of `rand(vitality)` (energy for mana) — over `len` frames, a potion drunk while one works
//!   folding what is left of it into the new one; a rejuvenation potion (5, `0x005BEAC0`) restores
//!   its percentages of life and mana at once.
//! - The packets' shapes and pacing are from a recorded retail fight (bnemu
//!   `docs/d2/re/combat.md`): a kill is `DYING`, then `DEAD` one death animation later.
//! - A walking monster covers `MonStats.txt` `Velocity` sixteenths of a subtile a frame, as a
//!   player covers its `WalkVelocity` (`0x0064FE40`), times its velocity percent (stat 67,
//!   `0x00623F50`: base × percent / 100). The walk packet carries that percent (`0x0053B710` writes
//!   stat 67) and the client sets the stat from it (`0x004AFF60`), so both sides glide at
//!   [`WALK_VELOCITY_PERCENT`] of the class speed (a zombie, `Velocity` 1, crawls where a Fallen,
//!   5, trots). Gliding at the full speed put the server ahead of the client until swings snapped
//!   monsters forward, or left them off screen still hitting (tagban's Dark Hunters, 2026-09-15).
//!
//! Not the engine's: the AI is a plain chase-and-swing loop (the per-class AI routines are not
//! ported), paths come from [`crate::path`], an unarmed player hits for 1–2, and the experience
//! penalty for a level gap is the published table rather than read from `Game.exe`.

use std::collections::BTreeMap;

use d2_data::monlvl::Scale;
use d2_data::{stat, GameData};
use d2_data::treasure::{gold_amount, Drop};
use d2_drlg::rng::Seed;
use d2_drlg::world::RoomId;

use crate::gear::Gear;
use crate::path;
use crate::skills;

/// Engine frames a second.
pub const FRAMES_PER_SECOND: u64 = 25;

/// The event codes reaction packets carry — `0x0D` for a player, `0x69` for a monster — which the
/// client's unit event dispatcher (`0x00461250`) turns into modes.
pub mod reaction {
    /// Get-hit: the victim flinches.
    pub const GET_HIT: u8 = 0x06;
    /// Death throes start (and "You have died" for the client's own player).
    pub const DYING: u8 = 0x08;
    /// The corpse pose.
    pub const DEAD: u8 = 0x09;
    /// A small hit: a sound, no flinch.
    pub const HIT_SOUND: u8 = 0x13;
}

/// A monster's mode once dead, as `0xAC` sends it.
pub const DEAD_MODE: u8 = 12;

/// Gold a character carries per level.
pub const GOLD_PER_LEVEL: u32 = 10_000;

/// How far a monster whose `aidist` is blank notices a player, subtiles (bnemu's reading of the
/// retail fights; the engine keeps it per AI routine, not ported).
const DEFAULT_AI_DISTANCE: i32 = 35;
/// A chase is given up past this multiple of the notice distance.
const LEASH: i32 = 2;
/// Sixteenths of a subtile a walking monster covers per frame per point of `Velocity`.
const VELOCITY_UNIT: f64 = 1.0 / 16.0;
/// A walking monster's velocity percent, as retail walk packets carry it (75; runs carry 125).
pub const WALK_VELOCITY_PERCENT: u16 = 75;
/// How far along its path one walk packet sends a monster.
const WALK_LEAD: usize = 8;
/// How close a player must be to hit a monster, subtiles. The server follows a player only
/// roughly, so this is generous.
pub const PLAYER_REACH: i32 = 6;
/// How many frames one call may catch up; a game nobody drove for longer skips the rest.
const MAX_CATCH_UP: u64 = 5 * FRAMES_PER_SECOND;
/// Unarmed damage.
/// Frames past a player's death animation before a release stands it up: the client's corpse
/// mode follows the animation's end.
const DEATH_SETTLE_MARGIN: u64 = 15;
/// Least frames between two stamina updates to a client.
const VITALS_EVERY: u64 = 5;
/// Player animation tokens by class.
const CLASS_TOKENS: [&str; 7] = ["AM", "SO", "NE", "PA", "BA", "DZ", "AI"];

/// Something a client should be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `0xAB`: a monster's life bar, in 128ths.
    MonsterLife {
        /// The room it spawned in: who can see it.
        room: RoomId,
        /// Its guid.
        guid: u32,
        /// Life in 128ths.
        life: u8,
    },
    /// `0x69`: a monster flinches, starts dying or lies dead.
    MonsterReaction {
        /// Who can see it.
        room: RoomId,
        /// Its guid.
        guid: u32,
        /// A [`reaction`] code.
        event: u8,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
        /// Life in 128ths.
        life: u8,
        /// Still alive (the packet's last byte, `0x03`, else 0).
        alive: bool,
    },
    /// `0x67`: a monster walks to a spot.
    MonsterWalk {
        /// Who can see it.
        room: RoomId,
        /// Its guid.
        guid: u32,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
    },
    /// `0x6D`: a monster stands still.
    MonsterStop {
        /// Who can see it.
        room: RoomId,
        /// Its guid.
        guid: u32,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
        /// Life in 128ths.
        life: u8,
    },
    /// `0x6C`: a monster swings at a player.
    MonsterAttack {
        /// Who can see it.
        room: RoomId,
        /// Its guid.
        guid: u32,
        /// The player swung at.
        target: String,
        /// Where the monster stands.
        x: u16,
        /// Where the monster stands.
        y: u16,
    },
    /// A monster moved, was hurt or died: where the room's population should keep it.
    MonsterState {
        /// Its guid.
        guid: u32,
        /// World subtiles.
        x: u16,
        /// World subtiles.
        y: u16,
        /// Unit mode.
        mode: u8,
        /// Life in 128ths.
        life: u8,
    },
    /// `0x0D` to a player about itself.
    PlayerReaction {
        /// The player.
        player: String,
        /// A [`reaction`] code.
        event: u8,
    },
    /// `0x95`: a player's life, mana and stamina, whole points.
    PlayerVitals {
        /// The player.
        player: String,
        /// Life.
        life: u16,
        /// Mana.
        mana: u16,
        /// Stamina.
        stamina: u16,
    },
    /// `0x1A`–`0x1C`: a player's experience went from `old` to `new`.
    Experience {
        /// The player.
        player: String,
        /// Before.
        old: u32,
        /// After.
        new: u32,
    },
    /// A dying monster dropped a gold pile (`0x9C`), from where it fell.
    GoldDrop {
        /// The room the monster died in.
        room: RoomId,
        /// Where it fell, world subtiles.
        x: u16,
        /// Where it fell, world subtiles.
        y: u16,
        /// Gold.
        amount: u32,
    },
    /// A dying monster dropped an item (`0x9C`), by code, from where it fell.
    ItemDrop {
        /// The room the monster died in.
        room: RoomId,
        /// Where it fell, world subtiles.
        x: u16,
        /// Where it fell, world subtiles.
        y: u16,
        /// The item code the treasure class named (`hp1`, `lax`).
        code: String,
        /// The quality mods of the classes it came through.
        mods: d2_data::treasure::QualityMods,
        /// The item level: the monster's level.
        level: i32,
    },
    /// `0x21`: a player's base level in a skill.
    SkillLevel {
        /// The player.
        player: String,
        /// `Skills.txt` id.
        skill: u16,
        /// Base level.
        level: u8,
    },
    /// `0x1D`–`0x1F`: one of a player's stats.
    PlayerStat {
        /// The player.
        player: String,
        /// `ItemStatCost.txt` id.
        stat: u8,
        /// The value, 256ths for life, mana and stamina.
        value: u32,
    },
}

/// How a monster fights, worked out when it joins the battle.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MonsterSheet {
    level: i32,
    defense: i32,
    to_hit: i32,
    damage: (i32, i32),
    experience: u32,
    notice: i32,
    think: u64,
    reach: i32,
    attack_frames: u64,
    hit_frame: u64,
    get_hit_frames: u64,
    dying_frames: u64,
    /// `Velocity`: sixteenths of a subtile a frame while walking.
    glide: u64,
    /// `TreasureClass1` for the difficulty.
    treasure: String,
    /// `MonStats2.txt` `SizeX`: how wide a target it is for a missile.
    size: i32,
    /// Resistances for the difficulty: physical, magic, fire, lightning, cold, poison.
    resistances: [i32; 6],
}

#[derive(Debug, Clone, PartialEq)]
struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    start: u64,
    frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Doing {
    Idle,
    Chasing(String),
    Attacking { target: String, hit_at: u64, done_at: u64 },
    Recovering { target: String, until: u64 },
    Dying { until: u64 },
    Dead,
}

#[derive(Debug, Clone, PartialEq)]
struct Monster {
    room: RoomId,
    x: f64,
    y: f64,
    life: i32,
    max_life: i32,
    sheet: MonsterSheet,
    doing: Doing,
    next_think: u64,
    glide: Option<Glide>,
}

impl Monster {
    fn at(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    fn life_byte(&self) -> u8 {
        life_byte(self.life, self.max_life)
    }

    fn alive(&self) -> bool {
        !matches!(self.doing, Doing::Dying { .. } | Doing::Dead)
    }
}

/// A life as `0xAB` and `0x69` carry it: 128ths of the maximum, at least 1 while alive.
#[must_use]
pub fn life_byte(life: i32, max: i32) -> u8 {
    if life <= 0 || max <= 0 {
        return 0;
    }
    (i64::from(life) * 128 / i64::from(max)).clamp(1, 128) as u8
}

/// How a player is moving, for stamina.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    /// Standing still.
    #[default]
    Standing,
    /// Walking.
    Walking,
    /// Running.
    Running,
}

/// What drinking a potion does (`Misc.txt` `pSpell` and its stats).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Potion {
    /// `pSpell` 3 with `hpregen`: `points` of life over `frames`.
    Healing {
        /// Whole points before the class's share.
        points: i32,
        /// Frames.
        frames: i32,
    },
    /// `pSpell` 3 with `manarecovery`: `points` of mana over `frames`.
    Mana {
        /// Whole points before the class's share.
        points: i32,
        /// Frames.
        frames: i32,
    },
    /// `pSpell` 5: percentages of maximum life and mana, at once.
    Rejuvenation {
        /// Percent of maximum life.
        life: i32,
        /// Percent of maximum mana.
        mana: i32,
    },
}

impl Potion {
    /// The potion an item is, from its `pSpell`, stats and `len`.
    #[must_use]
    pub fn of(item: &d2_data::items::ItemDef) -> Option<Self> {
        let calc = |stat: &str| item.effects.iter().find(|(s, _)| s.eq_ignore_ascii_case(stat)).map(|&(_, v)| v);
        match item.spell {
            3 => {
                let frames = item.duration.max(0);
                calc("hpregen").map(|points| Self::Healing { points, frames }).or_else(|| calc("manarecovery").map(|points| Self::Mana { points, frames }))
            }
            5 => Some(Self::Rejuvenation { life: calc("hitpoints").unwrap_or(0), mana: calc("mana").unwrap_or(0) }),
            _ => None,
        }
    }
}

/// A potion working on life or mana: 256ths a frame, for each frame after it was drunk up to and
/// including `until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Recovery {
    per_frame: i32,
    until: u64,
}

impl Recovery {
    /// Fold a new potion's `total` 256ths over `frames` into what is left of this one
    /// (`0x005BE3F0`: the rate is `(rate × left + total) / (frames + left)`).
    fn add(&mut self, now: u64, total: i32, frames: i32) {
        let left = self.until.saturating_sub(now) as i64;
        let span = i64::from(frames.max(1)) + left;
        self.per_frame = ((i64::from(self.per_frame) * left + i64::from(total)) / span) as i32;
        self.until = now + span as u64;
    }

    /// One frame's worth for `value` up to `max`.
    fn step(&self, now: u64, value: &mut i32, max: i32) {
        if now <= self.until && *value < max {
            *value = (*value + self.per_frame).min(max);
        }
    }
}

/// A player in the fight. Life, mana and stamina are 256ths, as the engine keeps them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hero {
    class: u8,
    level: u32,
    experience: u32,
    attributes: [i32; 4],
    life: i32,
    max_life: i32,
    mana: i32,
    max_mana: i32,
    stamina: i32,
    max_stamina: i32,
    stat_points: u32,
    skill_points: u32,
    gold: u32,
    at: Option<(i32, i32, i32)>,
    view: Vec<RoomId>,
    dead: bool,
    /// When its death throes are over, once dead.
    settled_at: u64,
    swing_until: u64,
    attack_frames: u64,
    hit_frame: u64,
    dying_frames: u64,
    motion: Motion,
    run_drain: i32,
    /// Whole life, mana and stamina the client was last told, and when.
    told: (u16, u16, u16),
    told_at: u64,
    healing: Recovery,
    mana_recovery: Recovery,
    /// What its worn items add.
    gear: Gear,
    /// The life, mana and stamina (256ths) its gear adds to its maximums, kept out of its save.
    gear_vitals: (i32, i32, i32),
    /// Base levels of the skills it has, by id.
    skills: BTreeMap<i32, u8>,
    /// The skills on its left and right mouse buttons.
    hands: [i32; 2],
}

impl Hero {
    /// An attribute with what its items add.
    fn attribute(&self, which: u8) -> i32 {
        self.attributes[usize::from(which)] + self.gear.attributes[usize::from(which)]
    }

    fn attack_rating(&self, data: &GameData) -> i32 {
        let base = (self.attribute(stat::DEXTERITY) - 7) * 5 + data.class(self.class).map_or(0, |c| c.to_hit_factor) + self.gear.to_hit;
        base + base * self.gear.to_hit_percent / 100
    }

    fn defense(&self) -> i32 {
        self.attribute(stat::DEXTERITY) / 4 + self.gear.defense
    }

    /// Its life, mana and stamina for its client, noting what was told when.
    fn vitals(&mut self, name: &str, now: u64) -> Event {
        let whole = self.whole();
        (self.told, self.told_at) = (whole, now);
        Event::PlayerVitals { player: name.to_string(), life: whole.0, mana: whole.1, stamina: whole.2 }
    }

    /// Whole life, mana and stamina.
    fn whole(&self) -> (u16, u16, u16) {
        let whole = |v: i32| (v.max(0) >> 8).min(0x7FFF) as u16;
        (whole(self.life), whole(self.mana), whole(self.stamina))
    }

    /// One frame of stamina (`0x0057F240` drain, `0x00580500` regeneration).
    fn stamina_step(&mut self, now: u64) {
        let in_town = self.at.is_some_and(|(_, _, level)| crate::population::is_town(level));
        let swinging = now < self.swing_until;
        let shift = match self.motion {
            Motion::Running if !in_town => {
                self.stamina = (self.stamina - (self.run_drain * 2).max(1)).max(0);
                return;
            }
            _ if swinging => return,
            Motion::Standing => 8,
            Motion::Running => 9,
            Motion::Walking if in_town || self.stamina >= 0x100 => 9,
            Motion::Walking => return,
        };
        if self.stamina < self.max_stamina {
            self.stamina = (self.stamina + (self.max_stamina >> shift)).min(self.max_stamina);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Due {
    Hit { player: String, guid: u32 },
    Dead { player: String },
    Cast { player: String, skill: i32, aim: Aim },
}

/// Where a player aims a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// At a unit: a monster's guid.
    Unit(u32),
    /// At a spot, world subtiles.
    At(u16, u16),
}

/// What a player's skill packet came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillUse {
    /// A swing: walk up to the monster and [`Battle::player_attack`] it.
    Swing,
    /// A cast has started.
    Cast,
    /// Nothing: busy, short of mana, in town, or a skill this port does not do yet.
    Refused,
}

/// A missile in flight, as the server moves it (`0x005AE1F0`): a straight line at its speed, ending
/// against a wall, when its range runs out, or on the first monster it reaches.
#[derive(Debug, Clone, PartialEq)]
struct Flight {
    player: String,
    skill: i32,
    level: i32,
    at: (f64, f64),
    step: (f64, f64),
    level_id: i32,
    frames_left: i32,
    /// `pSrvHitFunc` 1: what it hits and every monster within this many subtiles.
    splash: Option<i32>,
    /// A `pSrvHitFunc` other than 0 and 1 (Holy Bolt's undead-only hit, and the rest): not ported,
    /// so it hurts nothing.
    inert: bool,
}

/// One game's fighting.
#[derive(Debug, Clone)]
pub struct Battle {
    frame: u64,
    difficulty: u8,
    expansion: bool,
    seed: Seed,
    monsters: BTreeMap<u32, Monster>,
    heroes: BTreeMap<String, Hero>,
    due: BTreeMap<u64, Vec<Due>>,
    flights: Vec<Flight>,
}

/// The chance to hit, percent (`0x0057D9B0`).
#[must_use]
pub fn chance_to_hit(rating: i32, defense: i32, attacker_level: i32, defender_level: i32) -> i32 {
    let (rating, defense) = (i64::from(rating.max(0)), i64::from(defense.max(0)));
    let base = if rating + defense == 0 { 100 } else { rating * 100 / (rating + defense) };
    let (a, d) = (i64::from(attacker_level.max(1)), i64::from(defender_level.max(1)));
    (base * 2 * a / (a + d)).clamp(5, 95) as i32
}

/// Whether a physical hit of `damage` makes a victim with `max_life` flinch (`0x0057CB00`): never
/// below a sixteenth, always from a quarter, and between them on two rolls the engine makes with
/// a width Ghidra loses (taken here as even odds each).
fn flinches(seed: &mut Seed, damage: i32, max_life: i32) -> bool {
    if damage * 16 < max_life {
        return false;
    }
    (damage * 8 >= max_life || seed.pick(2) == 0) && (damage * 4 >= max_life || seed.pick(2) == 0)
}

/// Experience for killing a monster of `monster_level` at `player_level`: the published level-gap
/// table (not yet read from `Game.exe`).
#[must_use]
pub fn experience_for(base: u32, player_level: u32, monster_level: i32) -> u32 {
    let (p, m) = (i64::from(player_level), i64::from(monster_level.max(1)));
    let base = i64::from(base);
    let exp = if p - m > 5 {
        base * [81, 62, 43, 24, 5][((p - m - 6) as usize).min(4)] / 100
    } else if m - p > 5 {
        base * p / m
    } else {
        base
    };
    exp.max(if base > 0 { 1 } else { 0 }) as u32
}

/// Game frames of `token`'s `mode` animation with a weapon class, or `fallback`.
fn anim_frames(data: &GameData, token: &str, mode: &str, weapon_class: &str, fallback: u64) -> u64 {
    data.anim_data().get(token, mode, weapon_class).map_or(fallback, |a| u64::from(a.game_frames(100)).max(1))
}

fn hit_frames(data: &GameData, token: &str, mode: &str, weapon_class: &str, fallback: u64) -> u64 {
    data.anim_data().get(token, mode, weapon_class).and_then(|a| a.trigger_game_frames(100)).map_or(fallback, u64::from)
}

impl Battle {
    /// A battle for a game of `difficulty`, rolling on `seed`.
    #[must_use]
    pub fn new(difficulty: u8, seed: u32) -> Self {
        Self {
            frame: 0,
            difficulty: difficulty.min(2),
            expansion: false,
            seed: Seed::new(seed, 0x29A),
            monsters: BTreeMap::new(),
            heroes: BTreeMap::new(),
            due: BTreeMap::new(),
            flights: Vec::new(),
        }
    }

    /// Scale monsters by the expansion columns from now on.
    pub fn set_expansion(&mut self, expansion: bool) {
        self.expansion = expansion;
    }

    /// The frame the battle has reached.
    #[must_use]
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Bring a monster of `class` standing at (`x`, `y`) in `room` into the fight, with its life
    /// and numbers rolled for this game. Monsters that are not hostile, and guids already in, are
    /// left alone; returns whether it joined.
    pub fn add_monster(&mut self, data: &GameData, guid: u32, class: i32, room: RoomId, x: u16, y: u16) -> bool {
        let Some(m) = data.monsters().get(class) else { return false };
        if self.monsters.contains_key(&guid) || m.npc || m.interact || m.alignment() != 0 || m.critter {
            return false;
        }
        let d = usize::from(self.difficulty);
        let c = &m.combat;
        let level = if d == 0 {
            c.level[0]
        } else {
            data.levels().get(room.level).map_or(c.level[d], |l| {
                if self.expansion {
                    l.monsters.area_level_expansion[d]
                } else {
                    l.monsters.area_level[d]
                }
            })
        }
        .max(1);
        let scale = |value: i32, s: Scale| (i64::from(value) * i64::from(data.monster_levels().get(level, s, self.difficulty, self.expansion)) / 100) as i32;
        let (lo, hi) = c.life[d];
        let rolled = if hi > lo { lo + self.seed.pick((hi - lo + 1) as u32) as i32 } else { lo };
        let max_life = scale(rolled, Scale::Life).max(1);
        let weapon = if m.combat.weapon_class.is_empty() { "hth" } else { m.combat.weapon_class.as_str() };
        let attack_frames = anim_frames(data, &m.combat.token, "A1", weapon, 16);
        let sheet = MonsterSheet {
            level,
            defense: scale(c.defense[d], Scale::Defense),
            to_hit: scale(c.to_hit[d], Scale::ToHit),
            damage: (scale(c.damage[d].0, Scale::Damage).max(1), scale(c.damage[d].1, Scale::Damage).max(1)),
            experience: scale(c.experience[d], Scale::Experience).max(0) as u32,
            notice: if c.ai_distance[d] > 0 { c.ai_distance[d] } else { DEFAULT_AI_DISTANCE },
            think: u64::try_from(c.ai_delay[d]).unwrap_or(0).max(1),
            reach: 2 + c.melee_range.max(0),
            glide: u64::try_from(c.speed.0).unwrap_or(0).max(1),
            attack_frames,
            hit_frame: hit_frames(data, &m.combat.token, "A1", weapon, attack_frames / 2),
            get_hit_frames: anim_frames(data, &m.combat.token, "GH", weapon, 8),
            dying_frames: anim_frames(data, &m.combat.token, "DT", weapon, 20),
            treasure: c.treasure[d].clone(),
            size: i32::from(m.size).max(1),
            resistances: c.resistances[d],
        };
        let think = sheet.think;
        self.monsters.insert(
            guid,
            Monster {
                room,
                x: f64::from(x),
                y: f64::from(y),
                life: max_life,
                max_life,
                sheet,
                doing: Doing::Idle,
                next_think: self.frame + self.seed.pick(think as u32) as u64,
                glide: None,
            },
        );
        true
    }

    /// A monster in the fight: where it stands, its mode and its life in 128ths.
    #[must_use]
    pub fn monster(&self, guid: u32) -> Option<(u16, u16, u8, u8)> {
        let m = self.monsters.get(&guid)?;
        let (x, y) = m.at();
        let mode = if m.alive() { 1 } else { DEAD_MODE };
        Some((x as u16, y as u16, mode, m.life_byte()))
    }

    /// A player joins with a new character's stats (`(stat, value)`, as
    /// [`GameData::new_character_stats`] gives them).
    pub fn add_player(&mut self, data: &GameData, name: &str, class: u8, stats: &[(u8, u32)]) {
        let get = |id: u8| stats.iter().find(|&&(s, _)| s == id).map_or(0, |&(_, v)| v);
        let token = CLASS_TOKENS[usize::from(class.min(6))];
        let attack_frames = anim_frames(data, token, "A1", "hth", 12);
        let hero = Hero {
            class,
            level: get(stat::LEVEL).max(1),
            experience: get(stat::EXPERIENCE),
            attributes: [get(stat::STRENGTH) as i32, get(stat::ENERGY) as i32, get(stat::DEXTERITY) as i32, get(stat::VITALITY) as i32],
            life: get(stat::HITPOINTS) as i32,
            max_life: get(stat::MAXHP) as i32,
            mana: get(stat::MANA) as i32,
            max_mana: get(stat::MAXMANA) as i32,
            stamina: get(stat::STAMINA) as i32,
            max_stamina: get(stat::MAXSTAMINA) as i32,
            stat_points: get(stat::STATPTS),
            skill_points: get(stat::NEWSKILLS),
            gold: get(stat::GOLD),
            at: None,
            view: Vec::new(),
            dead: false,
            settled_at: 0,
            swing_until: 0,
            attack_frames,
            hit_frame: hit_frames(data, token, "A1", "hth", attack_frames / 2),
            dying_frames: anim_frames(data, token, "DT", "hth", 28),
            motion: Motion::Standing,
            run_drain: data.class(class).map_or(20, |c| c.run_drain),
            told: ((get(stat::HITPOINTS) >> 8) as u16, (get(stat::MANA) >> 8) as u16, (get(stat::STAMINA) >> 8) as u16),
            told_at: 0,
            healing: Recovery::default(),
            mana_recovery: Recovery::default(),
            gear: Gear::default(),
            gear_vitals: (0, 0, 0),
            skills: skills::COMMON_SKILLS.iter().map(|&id| (id, 1)).collect(),
            hands: [0, 0],
        };
        self.heroes.insert(name.to_string(), hero);
    }

    /// A player's worn items changed: what they add from now on — attributes, maximum life, mana
    /// and stamina (what is left of each kept no higher than its new maximum), defence, attack
    /// rating, damage — and the attack animation its weapon class gives.
    pub fn set_player_gear(&mut self, data: &GameData, name: &str, gear: Gear) {
        let Some(h) = self.heroes.get_mut(name) else { return };
        let per_point = data.class(h.class).map_or((0, 0, 0), |c| c.per_point);
        let vitals = (
            (gear.life << 8) + ((gear.attributes[usize::from(stat::VITALITY)] * per_point.0) << 6),
            (gear.mana << 8) + ((gear.attributes[usize::from(stat::ENERGY)] * per_point.2) << 6),
            (gear.stamina << 8) + ((gear.attributes[usize::from(stat::VITALITY)] * per_point.1) << 6),
        );
        h.max_life += vitals.0 - h.gear_vitals.0;
        h.max_mana += vitals.1 - h.gear_vitals.1;
        h.max_stamina += vitals.2 - h.gear_vitals.2;
        h.life = h.life.min(h.max_life);
        h.mana = h.mana.min(h.max_mana);
        h.stamina = h.stamina.min(h.max_stamina);
        h.gear_vitals = vitals;
        h.gear = gear;
        let token = CLASS_TOKENS[usize::from(h.class.min(6))];
        let class = gear.weapon.map_or_else(|| "hth".to_string(), |w| d2_data::items::code_str(&w.class));
        h.attack_frames = anim_frames(data, token, "A1", &class, 12);
        h.hit_frame = hit_frames(data, token, "A1", &class, h.attack_frames / 2);
    }

    /// A player's saved stats, ids 0–15 as a `.d2s` keeps them (life, mana and stamina in 256ths;
    /// a dead player's life as its maximum, as it comes back).
    #[must_use]
    pub fn player_stats(&self, name: &str) -> Option<Vec<(u8, u32)>> {
        let h = self.heroes.get(name)?;
        let (max_life, max_mana, max_stamina) = (h.max_life - h.gear_vitals.0, h.max_mana - h.gear_vitals.1, h.max_stamina - h.gear_vitals.2);
        let life = if h.dead || h.life <= 0 { max_life } else { h.life };
        let a = |i: u8| h.attributes[usize::from(i)].max(0) as u32;
        Some(vec![
            (stat::STRENGTH, a(stat::STRENGTH)),
            (stat::ENERGY, a(stat::ENERGY)),
            (stat::DEXTERITY, a(stat::DEXTERITY)),
            (stat::VITALITY, a(stat::VITALITY)),
            (stat::STATPTS, h.stat_points),
            (stat::NEWSKILLS, h.skill_points),
            (stat::HITPOINTS, life.max(0) as u32),
            (stat::MAXHP, max_life.max(0) as u32),
            (stat::MANA, h.mana.max(0) as u32),
            (stat::MAXMANA, max_mana.max(0) as u32),
            (stat::STAMINA, h.stamina.max(0) as u32),
            (stat::MAXSTAMINA, max_stamina.max(0) as u32),
            (stat::LEVEL, h.level),
            (stat::EXPERIENCE, h.experience),
            (stat::GOLD, h.gold),
        ])
    }

    /// A player's level.
    #[must_use]
    pub fn player_level(&self, name: &str) -> Option<u32> {
        self.heroes.get(name).map(|h| h.level)
    }

    /// What an item's requirements are measured against: a player's class, level, strength and
    /// dexterity.
    #[must_use]
    pub fn player_requirements(&self, name: &str) -> Option<(u8, u32, i32, i32)> {
        self.heroes.get(name).map(|h| (h.class, h.level, h.attributes[usize::from(stat::STRENGTH)], h.attributes[usize::from(stat::DEXTERITY)]))
    }

    /// A player left.
    pub fn remove_player(&mut self, name: &str) {
        self.heroes.remove(name);
    }

    /// How a player is moving.
    pub fn set_motion(&mut self, name: &str, motion: Motion) {
        if let Some(h) = self.heroes.get_mut(name) {
            h.motion = motion;
        }
    }

    /// Where a player stands — world subtiles and level — and the rooms its client holds.
    pub fn place_player(&mut self, name: &str, at: Option<(i32, i32, i32)>, view: &[RoomId]) {
        if let Some(h) = self.heroes.get_mut(name) {
            h.at = at;
            if h.view != view {
                h.view = view.to_vec();
            }
        }
    }

    /// Whether a dead player's death throes have played out on its client: the release (`0x41`)
    /// can only stand up a corpse (`0x0045DB20` revives a unit in mode 0x11).
    #[must_use]
    pub fn player_death_settled(&self, name: &str) -> bool {
        self.heroes.get(name).is_some_and(|h| h.dead && self.frame >= h.settled_at)
    }

    /// Move a monster to the room it now stands in: who sees it follows.
    pub fn set_monster_room(&mut self, guid: u32, room: RoomId) {
        if let Some(m) = self.monsters.get_mut(&guid) {
            m.room = room;
        }
    }

    /// The room a monster is in.
    #[must_use]
    pub fn monster_room(&self, guid: u32) -> Option<RoomId> {
        self.monsters.get(&guid).map(|m| m.room)
    }

    /// Whether a player is dead.
    #[must_use]
    pub fn player_dead(&self, name: &str) -> bool {
        self.heroes.get(name).is_some_and(|h| h.dead)
    }

    /// A player swings at monster `guid` (`0x06` and the other skill-on-unit packets). It lands on
    /// the swing's hit frame; a swing still under way ignores more. Returns whether it started.
    pub fn player_attack(&mut self, name: &str, guid: u32) -> bool {
        let Some(h) = self.heroes.get_mut(name) else { return false };
        if h.dead || self.frame < h.swing_until || !self.monsters.get(&guid).is_some_and(Monster::alive) {
            return false;
        }
        h.swing_until = self.frame + h.attack_frames;
        self.due.entry(self.frame + h.hit_frame.max(1)).or_default().push(Due::Hit { player: name.to_string(), guid });
        true
    }

    /// A player's skills from its save: base levels by id (the common skills are always there at
    /// least at level 1) and the skills on its left and right mouse buttons.
    pub fn set_player_skills(&mut self, name: &str, learned: &BTreeMap<i32, u8>, hands: [i32; 2]) {
        let Some(h) = self.heroes.get_mut(name) else { return };
        for (&id, &level) in learned {
            let base = h.skills.entry(id).or_insert(0);
            *base = (*base).max(level);
        }
        h.hands = hands.map(|id| if h.skills.get(&id).is_some_and(|&l| l > 0) { id } else { 0 });
    }

    /// A player's skills: base levels by id, and the skills on its left and right mouse buttons.
    #[must_use]
    pub fn player_skills(&self, name: &str) -> Option<(BTreeMap<i32, u8>, [i32; 2])> {
        self.heroes.get(name).map(|h| (h.skills.clone(), h.hands))
    }

    /// A player puts a point into skill `id` (`0x3B`, § [`skills`]): its new base level (`0x21`)
    /// and its unspent points; nothing when it may not.
    pub fn learn_skill(&mut self, data: &GameData, name: &str, id: i32) -> Vec<Event> {
        let Some(h) = self.heroes.get_mut(name).filter(|h| !h.dead) else { return Vec::new() };
        let a = |which: u8| h.attribute(which);
        let learner = skills::Learner { class: h.class, level: h.level, attributes: [a(stat::STRENGTH), a(stat::DEXTERITY), a(stat::ENERGY), a(stat::VITALITY)], points: h.skill_points };
        let Some(cost) = skills::learn_cost(data, learner, &h.skills, id) else { return Vec::new() };
        let (Ok(skill), Some(_)) = (u16::try_from(id), data.skills().get(id)) else { return Vec::new() };
        h.skill_points -= cost;
        let level = h.skills.entry(id).or_insert(0);
        *level += 1;
        let player = name.to_string();
        vec![
            Event::SkillLevel { player: player.clone(), skill, level: *level },
            Event::PlayerStat { player, stat: stat::NEWSKILLS, value: h.skill_points },
        ]
    }

    /// A player puts skill `id` on its left or right mouse button (`0x3C`, `0x0054BE70`): a skill it
    /// has; whether it did.
    pub fn select_skill(&mut self, name: &str, id: i32, left: bool) -> bool {
        let Some(h) = self.heroes.get_mut(name) else { return false };
        if !h.skills.get(&id).is_some_and(|&l| l > 0) {
            return false;
        }
        h.hands[usize::from(!left)] = id;
        true
    }

    /// A player uses the skill on one of its mouse buttons at `aim` (`0x05`–`0x11`). Attack and the
    /// melee skills swing (their own effects are not ported: they hit as Attack does); a skill that
    /// shoots missiles starts its cast animation, paid for and loosed on the animation's action
    /// frame ([`Battle::advance`]). Anything else is refused.
    pub fn player_skill(&mut self, data: &GameData, name: &str, left: bool, aim: Aim) -> SkillUse {
        let frame = self.frame;
        let Some(h) = self.heroes.get_mut(name).filter(|h| !h.dead) else { return SkillUse::Refused };
        let id = h.hands[usize::from(!left)];
        let Some(skill) = data.skills().get(id) else { return SkillUse::Refused };
        if skill.missile.is_none() && skill.missile_a.is_none() {
            let melee = id == 0 || matches!(skill.range.as_str(), "h2h" | "both");
            return if melee && matches!(aim, Aim::Unit(_)) { SkillUse::Swing } else { SkillUse::Refused };
        }
        let in_town = h.at.is_some_and(|(_, _, level)| crate::population::is_town(level));
        let level = i32::from(h.skills.get(&id).copied().unwrap_or(0));
        if frame < h.swing_until || (in_town && !skill.in_town) || level < 1 || h.mana < skills::mana_cost(skill, level) {
            return SkillUse::Refused;
        }
        let token = CLASS_TOKENS[usize::from(h.class.min(6))];
        let class = h.gear.weapon.map_or_else(|| "hth".to_string(), |w| d2_data::items::code_str(&w.class));
        let frames = anim_frames(data, token, &skill.anim, &class, 16);
        let action = hit_frames(data, token, &skill.anim, &class, frames / 2);
        h.swing_until = frame + frames;
        self.due.entry(frame + action.max(1)).or_default().push(Due::Cast { player: name.to_string(), skill: id, aim });
        SkillUse::Cast
    }

    /// A cast's action frame (`0x0056F640`): the mana is paid and the skill's missiles loosed from
    /// the player toward its aim — `srvmissile` once, `srvmissilea` as many times as `calc1` gives,
    /// fanned out.
    fn player_cast(&mut self, data: &GameData, player: &str, id: i32, aim: Aim, events: &mut Vec<Event>) {
        let now = self.frame;
        let Some(skill) = data.skills().get(id) else { return };
        let target = match aim {
            Aim::Unit(guid) => self.monsters.get(&guid).map(|m| position(m, now)),
            Aim::At(x, y) => Some((i32::from(x), i32::from(y))),
        };
        let Some(h) = self.heroes.get_mut(player).filter(|h| !h.dead) else { return };
        let (Some((hx, hy, level_id)), Some((tx, ty))) = (h.at, target) else { return };
        let level = i32::from(h.skills.get(&id).copied().unwrap_or(0)).max(1);
        let cost = skills::mana_cost(skill, level);
        if h.mana < cost {
            return;
        }
        h.mana -= cost;
        events.push(h.vitals(player, now));
        let (name, count) = match (&skill.missile, &skill.missile_a) {
            (Some(one), _) => (one, 1),
            (None, Some(many)) => {
                let ctx = skills::CalcContext { data, skill, level, skills: &h.skills };
                (many, skills::eval(&skill.calcs[0], &ctx).unwrap_or(1).clamp(1, 24))
            }
            (None, None) => return,
        };
        let Some(missile) = data.missiles().id(name).and_then(|m| data.missiles().get(m)) else { return };
        let (dx, dy) = (f64::from(tx - hx), f64::from(ty - hy));
        let length = dx.hypot(dy);
        let (ux, uy) = if length < 0.5 { (1.0, 0.0) } else { (dx / length, dy / length) };
        let speed = f64::from(missile.velocity.max(1)) / 16.0;
        let frames = missile.range.0 + missile.range.1 * level;
        let splash = (missile.server_hit_func == 1).then(|| missile.server_hit_params[0].max(1));
        let inert = missile.server_hit_func > 1;
        for i in 0..count {
            let turn = (f64::from(i) - f64::from(count - 1) / 2.0) * 0.15;
            let (sin, cos) = turn.sin_cos();
            let step = ((ux * cos - uy * sin) * speed, (ux * sin + uy * cos) * speed);
            self.flights.push(Flight { player: player.to_string(), skill: id, level, at: (f64::from(hx), f64::from(hy)), step, level_id, frames_left: frames.max(1), splash, inert });
        }
    }

    /// Move every missile a frame (`0x005AE1F0`): one that meets a wall or ground not in play ends;
    /// one that reaches a living monster hits it — and, with a splash, every monster near — and ends;
    /// one out of range ends.
    fn fly(&mut self, data: &GameData, open: &dyn Fn(i32, i32) -> Option<i32>, events: &mut Vec<Event>) {
        let now = self.frame;
        let mut kept = Vec::new();
        for mut f in std::mem::take(&mut self.flights) {
            f.at = (f.at.0 + f.step.0, f.at.1 + f.step.1);
            f.frames_left -= 1;
            let spot = (f.at.0.round() as i32, f.at.1.round() as i32);
            if open(spot.0, spot.1) != Some(f.level_id) {
                continue;
            }
            let near = |m: &Monster, within: i32| m.alive() && m.room.level == f.level_id && path::distance(position(m, now), spot) <= within;
            let struck = self.monsters.iter().find(|(_, m)| near(m, (m.sheet.size + 1) / 2)).map(|(&guid, _)| guid);
            if let Some(guid) = struck {
                let victims: Vec<u32> = match f.splash {
                    Some(radius) => self.monsters.iter().filter(|(_, m)| near(m, radius)).map(|(&g, _)| g).collect(),
                    None => vec![guid],
                };
                for victim in victims.into_iter().filter(|_| !f.inert) {
                    self.skill_hit(data, &f.player, f.skill, f.level, victim, events);
                }
                continue;
            }
            if f.frames_left > 0 {
                kept.push(f);
            }
        }
        self.flights = kept;
    }

    /// A player's skill hits monster `guid`: its elemental and physical damage rolled, each less
    /// the monster's resistance to it (none at 100% or more).
    fn skill_hit(&mut self, data: &GameData, player: &str, id: i32, level: i32, guid: u32, events: &mut Vec<Event>) {
        let (Some(skill), Some(h), Some(m)) = (data.skills().get(id), self.heroes.get(player), self.monsters.get(&guid)) else { return };
        let ctx = skills::CalcContext { data, skill, level, skills: &h.skills };
        let ((elo, ehi), (plo, phi)) = skills::damage(&ctx);
        let element = match skill.element.as_str() {
            "mag" => 1,
            "fire" => 2,
            "ltng" => 3,
            "cold" => 4,
            "pois" => 5,
            _ => 0,
        };
        let resistances = m.sheet.resistances;
        let mut roll = |lo: i32, hi: i32, resistance: i32| {
            if hi <= 0 || resistance >= 100 {
                return 0;
            }
            let rolled = lo + if hi > lo { self.seed.pick((hi - lo) as u32) as i32 } else { 0 };
            rolled * (100 - resistance) / 100
        };
        let total = roll(elo, ehi, resistances[element]) + roll(plo, phi, resistances[0]);
        if total > 0 {
            self.hurt_monster(data, player, guid, (total >> 8).max(1), events);
        }
    }

    /// Spend a stat point (`0x3A`) on strength, energy, dexterity or vitality.
    pub fn spend_stat_point(&mut self, data: &GameData, name: &str, which: u8) -> Vec<Event> {
        let Some(h) = self.heroes.get_mut(name) else { return Vec::new() };
        if which > stat::VITALITY || h.stat_points == 0 {
            return Vec::new();
        }
        h.stat_points -= 1;
        h.attributes[usize::from(which)] += 1;
        let player = name.to_string();
        let mut events = vec![
            Event::PlayerStat { player: player.clone(), stat: which, value: h.attributes[usize::from(which)] as u32 },
            Event::PlayerStat { player: player.clone(), stat: stat::STATPTS, value: h.stat_points },
        ];
        let per = data.class(h.class).map_or((0, 0, 0), |c| c.per_point);
        let geared = h.gear_vitals;
        let mut grow = |value: &mut i32, max: &mut i32, quarters: i32, current: u8, maximum: u8| {
            *max += quarters << 6;
            *value += quarters << 6;
            // The base stat, as the engine sends it; what items add the client works out itself.
            let from_gear = match maximum {
                stat::MAXHP => geared.0,
                stat::MAXMANA => geared.1,
                _ => geared.2,
            };
            events.push(Event::PlayerStat { player: player.clone(), stat: maximum, value: (*max - from_gear).max(0) as u32 });
            events.push(Event::PlayerStat { player: player.clone(), stat: current, value: (*value).max(0) as u32 });
        };
        if which == stat::VITALITY {
            grow(&mut h.life, &mut h.max_life, per.0, stat::HITPOINTS, stat::MAXHP);
            grow(&mut h.stamina, &mut h.max_stamina, per.1, stat::STAMINA, stat::MAXSTAMINA);
        } else if which == stat::ENERGY {
            grow(&mut h.mana, &mut h.max_mana, per.2, stat::MANA, stat::MAXMANA);
        }
        events
    }

    /// Bring a dead player back to full life (after the server has moved it to town).
    pub fn revive(&mut self, name: &str) -> Vec<Event> {
        let Some(h) = self.heroes.get_mut(name) else { return Vec::new() };
        h.dead = false;
        h.life = h.max_life;
        h.mana = h.max_mana;
        h.stamina = h.max_stamina;
        h.swing_until = self.frame;
        vec![h.vitals(name, self.frame)]
    }

    /// Run the battle up to `frame`, walking monsters on `open` ground: the level of a subtile a
    /// monster may walk on (walkable, in a room that has come into play), `None` where it may not.
    /// A monster never walks out of its own level.
    pub fn advance(&mut self, data: &GameData, frame: u64, open: &dyn Fn(i32, i32) -> Option<i32>) -> Vec<Event> {
        let mut events = Vec::new();
        if frame > self.frame + MAX_CATCH_UP {
            self.frame = frame - MAX_CATCH_UP;
        }
        while self.frame < frame {
            self.frame += 1;
            self.step(data, open, &mut events);
        }
        events
    }

    fn step(&mut self, data: &GameData, open: &dyn Fn(i32, i32) -> Option<i32>, events: &mut Vec<Event>) {
        let now = self.frame;
        // Everything due by now, including frames a catch-up skipped.
        let later = self.due.split_off(&(now + 1));
        let ready = std::mem::replace(&mut self.due, later);
        for due in ready.into_values() {
            for d in due {
                match d {
                    Due::Hit { player, guid } => self.player_hit(data, &player, guid, events),
                    Due::Dead { player } => {
                        if self.heroes.get(&player).is_some_and(|h| h.dead) {
                            events.push(Event::PlayerReaction { player, event: reaction::DEAD });
                        }
                    }
                    Due::Cast { player, skill, aim } => self.player_cast(data, &player, skill, aim, events),
                }
            }
        }
        self.fly(data, open, events);
        let guids: Vec<u32> = self.monsters.keys().copied().collect();
        for guid in guids {
            self.monster_step(guid, open, events);
        }
        for (name, h) in &mut self.heroes {
            if h.dead {
                continue;
            }
            h.stamina_step(now);
            let (healing, mana_recovery) = (h.healing, h.mana_recovery);
            healing.step(now, &mut h.life, h.max_life);
            mana_recovery.step(now, &mut h.mana, h.max_mana);
            let whole = h.whole();
            let at_end = whole.2 == 0
                || h.stamina == h.max_stamina
                || now == healing.until
                || now == mana_recovery.until
                || h.life == h.max_life && whole.0 != h.told.0
                || h.mana == h.max_mana && whole.1 != h.told.1;
            if whole != h.told && (now >= h.told_at + VITALS_EVERY || at_end) {
                events.push(h.vitals(name, now));
            }
        }
    }

    fn player_hit(&mut self, data: &GameData, player: &str, guid: u32, events: &mut Vec<Event>) {
        let Some(h) = self.heroes.get(player) else { return };
        let Some(m) = self.monsters.get(&guid) else { return };
        let Some((hx, hy, level)) = h.at else { return };
        if h.dead || !m.alive() || level != m.room.level || path::distance((hx, hy), m.at()) > PLAYER_REACH + m.sheet.reach {
            return;
        }
        let chance = chance_to_hit(h.attack_rating(data), m.sheet.defense, h.level as i32, m.sheet.level);
        if self.seed.pick(100) as i32 >= chance {
            return;
        }
        // `0x0057B420`: the range in 256ths, a roll between its ends, whole points dealt.
        let (lo, hi) = h.gear.damage_range(h.attribute(stat::STRENGTH), h.attribute(stat::DEXTERITY));
        let damage = ((lo + if hi > lo { self.seed.pick((hi - lo) as u32) as i32 } else { 0 }) >> 8).max(1);
        self.hurt_monster(data, player, guid, damage, events);
    }

    /// Monster `guid` loses `damage` life to `player`: its life bar, a flinch, or its death with
    /// the experience and treasure it pays.
    fn hurt_monster(&mut self, data: &GameData, player: &str, guid: u32, damage: i32, events: &mut Vec<Event>) {
        let Some(h) = self.heroes.get(player) else { return };
        let Some(m) = self.monsters.get(&guid).filter(|m| m.alive()) else { return };
        let now = self.frame;
        let experience_gain = experience_for(m.sheet.experience, h.level, m.sheet.level);
        let flinch = flinches(&mut self.seed, damage, m.max_life);
        let m = self.monsters.get_mut(&guid).expect("checked");
        if let Some(glide) = m.glide.take() {
            let (x, y) = glide_at(&glide, now);
            (m.x, m.y) = (x, y);
        }
        m.life -= damage;
        let (x, y) = m.at();
        let (room, ux, uy) = (m.room, x as u16, y as u16);
        if m.life > 0 {
            let life = m.life_byte();
            events.push(Event::MonsterLife { room, guid, life });
            if flinch {
                events.push(Event::MonsterReaction { room, guid, event: reaction::GET_HIT, x: ux, y: uy, life, alive: true });
                m.doing = Doing::Recovering { target: player.to_string(), until: now + m.sheet.get_hit_frames };
            } else if !matches!(m.doing, Doing::Attacking { .. }) {
                m.doing = Doing::Chasing(player.to_string());
            }
            events.push(Event::MonsterState { guid, x: ux, y: uy, mode: 1, life });
            return;
        }
        m.life = 0;
        m.doing = Doing::Dying { until: now + m.sheet.dying_frames };
        let (treasure, level) = (m.sheet.treasure.clone(), m.sheet.level);
        events.push(Event::MonsterReaction { room, guid, event: reaction::DYING, x: ux, y: uy, life: 0, alive: true });
        events.push(Event::MonsterState { guid, x: ux, y: uy, mode: DEAD_MODE, life: 0 });
        self.gain_experience(data, player, experience_gain, events);
        self.drop_treasure(data, room, (x, y), &treasure, level, events);
    }

    /// Roll a dead monster's treasure class (upgraded to its level) for the players in the game.
    /// Gold piles and items are dropped from where it fell; the caller finds each its spot and
    /// makes the items it can.
    fn drop_treasure(&mut self, data: &GameData, room: RoomId, (x, y): (i32, i32), treasure: &str, level: i32, events: &mut Vec<Event>) {
        let Some(class) = data.treasure().upgraded(treasure, level) else { return };
        let players = (self.heroes.len() as u32).max(1);
        let seed = &mut self.seed;
        let drops = data.treasure().roll_for(class, players, self.expansion, &mut |n| seed.pick(n));
        for drop in drops {
            match drop {
                Drop::Gold { mul } => {
                    let amount = gold_amount(level, mul, &mut |n| self.seed.pick(n));
                    events.push(Event::GoldDrop { room, x: x as u16, y: y as u16, amount });
                }
                Drop::Item(code, mods) => events.push(Event::ItemDrop { room, x: x as u16, y: y as u16, code, mods, level }),
            }
        }
    }

    /// A healer restores a living player's life, mana and stamina (`0x00578D30`): the stats it
    /// filled, each as the engine sends it.
    pub fn heal(&mut self, name: &str) -> Vec<Event> {
        let Some(h) = self.heroes.get_mut(name).filter(|h| !h.dead) else { return Vec::new() };
        let mut events = Vec::new();
        for (value, max, id) in [(&mut h.life, h.max_life, stat::HITPOINTS), (&mut h.mana, h.max_mana, stat::MANA), (&mut h.stamina, h.max_stamina, stat::STAMINA)] {
            if *value < max {
                *value = max;
                events.push(Event::PlayerStat { player: name.to_string(), stat: id, value: max.max(0) as u32 });
            }
        }
        h.told = h.whole();
        events
    }

    /// A player drinks `potion`: what its client is told at once (a rejuvenation's life and mana);
    /// `None` when the player is not in the fight or is dead, and nothing is used up.
    pub fn drink(&mut self, name: &str, potion: Potion) -> Option<Vec<Event>> {
        let now = self.frame;
        let h = self.heroes.get_mut(name)?;
        if h.dead {
            return None;
        }
        // `0x0062A5D0` and `0x0062A620`, by class: Amazon 0, Sorceress 1, Necromancer 2, Paladin 3,
        // Barbarian 4, Druid 5, Assassin 6.
        let share = |v: i32, doubled: &[u8]| match h.class {
            0 | 3 | 6 => v + (v >> 1),
            c if doubled.contains(&c) => v * 2,
            _ => v,
        };
        let seed = &mut self.seed;
        // A lucky draw doubles it (`0x005BE520`): rand(100) under half of rand(the attribute).
        let mut lucky = |attribute: i32, v: i32| {
            if attribute > 0 {
                let half = seed.pick(attribute as u32) >> 1;
                if seed.pick(100) < half {
                    return v * 2;
                }
            }
            v
        };
        match potion {
            Potion::Healing { points, frames } => {
                let total = lucky(h.attributes[usize::from(stat::VITALITY)], share(points << 8, &[4]));
                h.healing.add(now, total, frames);
                Some(Vec::new())
            }
            Potion::Mana { points, frames } => {
                let total = lucky(h.attributes[usize::from(stat::ENERGY)], share(points << 8, &[1, 2, 5]));
                h.mana_recovery.add(now, total, frames);
                Some(Vec::new())
            }
            Potion::Rejuvenation { life, mana } => {
                let part = |max: i32, percent: i32| if percent >= 100 { max } else { (i64::from(max) * i64::from(percent) / 100) as i32 };
                h.life = (h.life + part(h.max_life, life)).min(h.max_life);
                h.mana = (h.mana + part(h.max_mana, mana)).min(h.max_mana);
                Some(vec![h.vitals(name, now)])
            }
        }
    }

    /// A player picks up a gold pile of `amount`: as much as its purse holds (10,000 a level).
    /// What it took and its new total, `None` for a player not in the fight.
    pub fn pick_up_gold(&mut self, name: &str, amount: u32) -> Option<(u32, u32)> {
        let h = self.heroes.get_mut(name)?;
        let room = (h.level.saturating_mul(GOLD_PER_LEVEL)).saturating_sub(h.gold);
        let taken = amount.min(room);
        h.gold += taken;
        Some((taken, h.gold))
    }

    /// A player's gold.
    #[must_use]
    pub fn player_gold(&self, name: &str) -> Option<u32> {
        self.heroes.get(name).map(|h| h.gold)
    }

    /// A player pays `amount` of its gold: its new total, `None` when it has less or is not in the
    /// fight (`0x00576D90`, which also takes from the stash; the stash is not modelled).
    pub fn pay_gold(&mut self, name: &str, amount: u32) -> Option<u32> {
        let h = self.heroes.get_mut(name)?;
        h.gold = h.gold.checked_sub(amount)?;
        Some(h.gold)
    }

    fn gain_experience(&mut self, data: &GameData, name: &str, gain: u32, events: &mut Vec<Event>) {
        let Some(h) = self.heroes.get_mut(name) else { return };
        if gain == 0 {
            return;
        }
        let old = h.experience;
        let cap = data.next_level_experience(h.class, 98).unwrap_or(u32::MAX);
        h.experience = old.saturating_add(gain).min(cap);
        let player = name.to_string();
        events.push(Event::Experience { player: player.clone(), old, new: h.experience });
        let class = data.class(h.class).copied();
        while h.level < 99 && data.next_level_experience(h.class, h.level as usize).is_some_and(|next| h.experience >= next) {
            h.level += 1;
            let (life, stamina, mana) = class.map_or((0, 0, 0), |c| c.per_level);
            h.max_life += life << 6;
            h.life += life << 6;
            h.max_stamina += stamina << 6;
            h.stamina += stamina << 6;
            h.max_mana += mana << 6;
            h.mana += mana << 6;
            h.stat_points += class.map_or(5, |c| c.stat_per_level.max(0) as u32);
            h.skill_points += 1;
            // A new level fills life, mana and stamina (`0x00570880`).
            if h.life > 0 {
                h.life = h.max_life;
            }
            h.mana = h.max_mana;
            h.stamina = h.max_stamina;
            h.told = h.whole();
            for (stat, value) in [
                (stat::LEVEL, h.level),
                (stat::STATPTS, h.stat_points),
                (stat::NEWSKILLS, h.skill_points),
                (stat::MAXHP, (h.max_life - h.gear_vitals.0).max(0) as u32),
                (stat::HITPOINTS, h.life.max(0) as u32),
                (stat::MAXMANA, (h.max_mana - h.gear_vitals.1).max(0) as u32),
                (stat::MANA, h.mana.max(0) as u32),
                (stat::MAXSTAMINA, (h.max_stamina - h.gear_vitals.2).max(0) as u32),
                (stat::STAMINA, h.stamina.max(0) as u32),
                (stat::LASTEXP, data.next_level_experience(h.class, h.level as usize - 1).unwrap_or(0)),
                (stat::NEXTEXP, data.next_level_experience(h.class, h.level as usize).unwrap_or(0)),
            ] {
                events.push(Event::PlayerStat { player: player.clone(), stat, value });
            }
        }
    }

    /// The nearest living player a monster in `room` at `at` can see and that can see it,
    /// within `range`.
    fn nearest_player(&self, room: RoomId, at: (i32, i32), range: i32) -> Option<String> {
        self.heroes
            .iter()
            .filter(|(_, h)| !h.dead && h.view.contains(&room))
            .filter_map(|(name, h)| {
                let (x, y, level) = h.at?;
                let d = path::distance(at, (x, y));
                (level == room.level && d <= range).then_some((d, name))
            })
            .min()
            .map(|(_, name)| name.clone())
    }

    fn monster_step(&mut self, guid: u32, open: &dyn Fn(i32, i32) -> Option<i32>, events: &mut Vec<Event>) {
        let now = self.frame;
        let Some(m) = self.monsters.get_mut(&guid) else { return };
        // A walk that has run its course ends where it was going.
        if let Some(glide) = &m.glide {
            if now >= glide.start + glide.frames {
                (m.x, m.y) = glide.to;
                m.glide = None;
                let (x, y) = m.at();
                events.push(Event::MonsterState { guid, x: x as u16, y: y as u16, mode: 1, life: m.life_byte() });
            }
        }
        match m.doing.clone() {
            Doing::Dead => return,
            Doing::Dying { until } => {
                if now >= until {
                    m.doing = Doing::Dead;
                    let (x, y) = m.at();
                    events.push(Event::MonsterReaction { room: m.room, guid, event: reaction::DEAD, x: x as u16, y: y as u16, life: 0, alive: false });
                }
                return;
            }
            Doing::Recovering { target, until } => {
                if now >= until {
                    m.doing = Doing::Chasing(target);
                    m.next_think = now;
                } else {
                    return;
                }
            }
            Doing::Attacking { target, hit_at, done_at } => {
                if now == hit_at {
                    self.monster_hit(guid, &target, events);
                }
                let Some(m) = self.monsters.get_mut(&guid) else { return };
                if now >= done_at {
                    if matches!(m.doing, Doing::Attacking { .. }) {
                        m.doing = Doing::Chasing(target);
                    }
                    m.next_think = now + m.sheet.think;
                }
                return;
            }
            Doing::Idle | Doing::Chasing(_) => {}
        }
        let Some(m) = self.monsters.get(&guid) else { return };
        if now < m.next_think {
            return;
        }
        let at = match &m.glide {
            Some(glide) => {
                let (x, y) = glide_at(glide, now);
                (x.round() as i32, y.round() as i32)
            }
            None => m.at(),
        };
        let (room, notice, reach, think) = (m.room, m.sheet.notice, m.sheet.reach, m.sheet.think);
        let target = match &m.doing {
            Doing::Chasing(name) => {
                let keep = self.heroes.get(name).is_some_and(|h| {
                    !h.dead && h.view.contains(&room) && h.at.is_some_and(|(x, y, level)| level == room.level && path::distance(at, (x, y)) <= notice * LEASH)
                });
                keep.then(|| name.clone()).or_else(|| self.nearest_player(room, at, notice))
            }
            _ => self.nearest_player(room, at, notice),
        };
        let m = self.monsters.get_mut(&guid).expect("present");
        m.next_think = now + think;
        let Some(target) = target else {
            if m.glide.is_some() || matches!(m.doing, Doing::Chasing(_)) {
                stop(m, guid, now, events);
            }
            m.doing = Doing::Idle;
            return;
        };
        let Some((hx, hy, _)) = self.heroes.get(&target).and_then(|h| h.at) else { return };
        if path::distance(at, (hx, hy)) <= reach {
            // The swing asserts where the monster stands (0x0045CFB0), so no stop is sent first:
            // a 0x6D would place it outright.
            if let Some(glide) = m.glide.take() {
                (m.x, m.y) = glide_at(&glide, now);
            }
            let (x, y) = m.at();
            events.push(Event::MonsterAttack { room, guid, target: target.clone(), x: x as u16, y: y as u16 });
            m.doing = Doing::Attacking { target, hit_at: now + m.sheet.hit_frame.max(1), done_at: now + m.sheet.attack_frames.max(1) };
            return;
        }
        m.doing = Doing::Chasing(target);
        // A walk under way is left to finish while it still brings the monster closer: a client
        // sent a new walk mid-stride restarts the animation (bnemu's recorded fights).
        if let Some(glide) = &m.glide {
            let end = (glide.to.0.round() as i32, glide.to.1.round() as i32);
            if path::distance(end, (hx, hy)) < path::distance(at, (hx, hy)) {
                m.next_think = (glide.start + glide.frames).min(now + think);
                return;
            }
        }
        // Other monsters' spots and walk ends are taken, so a pack spreads around its prey.
        let taken: std::collections::HashSet<(i32, i32)> = self
            .monsters
            .iter()
            .filter(|&(&g, other)| g != guid && other.alive() && other.room.level == room.level && path::distance(other.at(), (hx, hy)) <= notice)
            .flat_map(|(_, other)| [Some(other.at()), other.glide.as_ref().map(|g| (g.to.0.round() as i32, g.to.1.round() as i32))])
            .flatten()
            .collect();
        let free = |x: i32, y: i32| open(x, y) == Some(room.level) && !taken.contains(&(x, y));
        let m = self.monsters.get_mut(&guid).expect("present");
        let step = path::find(at, (hx, hy), reach, 12, 3000, &free).and_then(|p| {
            if p.is_empty() {
                return None;
            }
            // The farthest point within the lead that a straight walk reaches.
            let limit = p.len().min(WALK_LEAD);
            (1..=limit).rev().map(|i| p[i - 1]).find(|&q| path::clear_line(at, q, &free))
        });
        let Some((tx, ty)) = step else {
            if m.glide.is_some() {
                stop(m, guid, now, events);
            }
            return;
        };
        let from = match &m.glide {
            Some(glide) => glide_at(glide, now),
            None => (m.x, m.y),
        };
        (m.x, m.y) = from;
        let length = (f64::from(tx) - from.0).hypot(f64::from(ty) - from.1);
        let frames = glide_frames(m.sheet.glide, length);
        m.glide = Some(Glide { from, to: (f64::from(tx), f64::from(ty)), start: now, frames });
        m.next_think = now + frames.min(think);
        events.push(Event::MonsterWalk { room, guid, x: tx as u16, y: ty as u16 });
    }

    fn monster_hit(&mut self, guid: u32, target: &str, events: &mut Vec<Event>) {
        let Some(m) = self.monsters.get(&guid) else { return };
        let Some(h) = self.heroes.get(target) else { return };
        let Some((hx, hy, level)) = h.at else { return };
        if h.dead || level != m.room.level || path::distance(m.at(), (hx, hy)) > m.sheet.reach + 2 {
            return;
        }
        let chance = chance_to_hit(m.sheet.to_hit, h.defense(), m.sheet.level, h.level as i32);
        if self.seed.pick(100) as i32 >= chance {
            return;
        }
        let (lo, hi) = m.sheet.damage;
        let damage = lo + self.seed.pick((hi - lo).max(0) as u32 + 1) as i32;
        let max_life = h.max_life >> 8;
        let flinch = flinches(&mut self.seed, damage, max_life);
        let dying_frames = h.dying_frames;
        let now = self.frame;
        let h = self.heroes.get_mut(target).expect("checked");
        h.life -= damage << 8;
        let player = target.to_string();
        if h.life > 0 {
            events.push(h.vitals(target, now));
            events.push(Event::PlayerReaction { player, event: if flinch { reaction::GET_HIT } else { reaction::HIT_SOUND } });
            return;
        }
        h.life = 0;
        h.dead = true;
        // Its client plays the death throes, then settles into the corpse a moment later.
        h.settled_at = now + dying_frames + DEATH_SETTLE_MARGIN;
        events.push(h.vitals(target, now));
        events.push(Event::PlayerReaction { player: player.clone(), event: reaction::DYING });
        self.due.entry(self.frame + dying_frames.max(1)).or_default().push(Due::Dead { player });
    }
}

/// Frames a monster of `velocity` takes to walk `length` subtiles, at the pace the client glides it.
fn glide_frames(velocity: u64, length: f64) -> u64 {
    let per_frame = velocity.max(1) as f64 * VELOCITY_UNIT * f64::from(WALK_VELOCITY_PERCENT) / 100.0;
    (length / per_frame).ceil().max(1.0) as u64
}

fn glide_at(glide: &Glide, now: u64) -> (f64, f64) {
    let t = if glide.frames == 0 { 1.0 } else { (now.saturating_sub(glide.start) as f64 / glide.frames as f64).min(1.0) };
    (glide.from.0 + (glide.to.0 - glide.from.0) * t, glide.from.1 + (glide.to.1 - glide.from.1) * t)
}

/// Where a monster is at frame `now`, part way along a walk.
fn position(m: &Monster, now: u64) -> (i32, i32) {
    match &m.glide {
        Some(glide) => {
            let (x, y) = glide_at(glide, now);
            (x.round() as i32, y.round() as i32)
        }
        None => m.at(),
    }
}

/// End a monster's walk where it has got to and say so.
fn stop(m: &mut Monster, guid: u32, now: u64, events: &mut Vec<Event>) {
    if let Some(glide) = m.glide.take() {
        (m.x, m.y) = glide_at(&glide, now);
    }
    let (x, y) = m.at();
    let life = m.life_byte();
    events.push(Event::MonsterStop { room: m.room, guid, x: x as u16, y: y as u16, life });
    events.push(Event::MonsterState { guid, x: x as u16, y: y as u16, mode: 1, life });
}

/// Which players an event is for: `None` for a monster event, which goes to every player whose
/// client holds its room.
impl Event {
    /// The player a player event is for.
    #[must_use]
    pub fn player(&self) -> Option<&str> {
        match self {
            Self::PlayerReaction { player, .. }
            | Self::PlayerVitals { player, .. }
            | Self::Experience { player, .. }
            | Self::PlayerStat { player, .. }
            | Self::SkillLevel { player, .. } => Some(player),
            _ => None,
        }
    }

    /// The room a monster event is seen from.
    #[must_use]
    pub fn room(&self) -> Option<RoomId> {
        match self {
            Self::MonsterLife { room, .. }
            | Self::MonsterReaction { room, .. }
            | Self::MonsterWalk { room, .. }
            | Self::MonsterStop { room, .. }
            | Self::MonsterAttack { room, .. } => Some(*room),
            _ => None,
        }
    }
}

/// The players of a battle, for callers that fan events out.
impl Battle {
    /// Names of the players in the fight.
    pub fn players(&self) -> impl Iterator<Item = &str> {
        self.heroes.keys().map(String::as_str)
    }

    /// The rooms a player's client holds, as last placed.
    #[must_use]
    pub fn view_of(&self, name: &str) -> &[RoomId] {
        self.heroes.get(name).map_or(&[], |h| h.view.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_data::monsters::Monsters;
    use d2_data::presets::{MonPresets, Objects};
    use d2_formats::excel::Table;

    /// Made-up tables in the real shapes: one hostile class with 10 life, one friendly.
    fn data() -> GameData {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\ttot\tstamina\thpadd\tToHitFactor\tLifePerLevel\tStaminaPerLevel\tManaPerLevel\tLifePerVitality\tStaminaPerVitality\tManaPerMagic\tStatPerLevel\tRunDrain\r\n");
        for name in ["Amazon", "Sorceress", "Necromancer", "Paladin", "Barbarian"] {
            cs.push_str(&format!("{name}\t30\t27\t10\t25\t0\t92\t30\t20\t8\t4\t4\t16\t4\t4\t5\t20\r\n"));
        }
        cs.push_str("Expansion\r\n");
        for name in ["Druid", "Assassin"] {
            cs.push_str(&format!("{name}\t30\t27\t10\t25\t0\t92\t30\t20\t8\t4\t4\t16\t4\t4\t5\t20\r\n"));
        }
        let mut exp = String::from("Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\tExpRatio\r\nMaxLvl\t99\t99\t99\t99\t99\t99\t99\t10\r\n");
        for (level, v) in [(0, 0), (1, 20), (2, 60), (3, 1000)] {
            exp.push_str(&format!("{level}\t{v}\t{v}\t{v}\t{v}\t{v}\t{v}\t{v}\t1024\r\n"));
        }
        let mut data = GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(exp.as_bytes())).unwrap();
        let monstats = Table::parse(
            b"Id\thcIdx\tMonStatsEx\tCode\tAlign\tnpc\tinteract\tLevel\tminHP\tmaxHP\tAC\tExp\tA1MinD\tA1MaxD\tA1TH\taidist\taidel\r\n\
              brute\t0\tbrute\tXX\t0\t0\t0\t1\t10\t10\t0\t100\t100\t100\t100\t\t5\r\n\
              friend\t1\tbrute\tYY\t1\t1\t1\t1\t10\t10\t0\t0\t0\t0\t0\t\t5\r\n",
        );
        let monstats2 = Table::parse(b"Id\tSizeX\tBaseW\tMeleeRng\r\nbrute\t1\thth\t0\r\n");
        data.set_map_tables(MonPresets::default(), Monsters::from_tables(&monstats, &monstats2).unwrap(), Objects::default());
        let monlvl = Table::parse(
            b"Level\tAC\tAC(N)\tAC(H)\tTH\tHP\tDM\tXP\r\n0\t100\t100\t100\t100\t100\t100\t100\r\n1\t100\t100\t100\t100\t100\t2\t100\r\n",
        );
        data.set_combat_tables(d2_data::monlvl::MonLvls::from_table(&monlvl).unwrap(), Default::default());
        data
    }

    const ROOM: RoomId = RoomId { level: 2, index: 0 };

    fn battle(data: &GameData) -> Battle {
        let mut b = Battle::new(0, 1234);
        assert!(b.add_monster(data, 7, 0, ROOM, 100, 100));
        assert!(!b.add_monster(data, 8, 1, ROOM, 100, 100), "a friendly NPC does not fight");
        b.add_player(data, "hero", 4, &data.new_character_stats(4).unwrap());
        b
    }

    /// A sorceress learns Fire Bolt with her one point, puts it on her right button and casts it at
    /// a brute ten subtiles off: the mana goes on the action frame, the bolt flies at 1.25 subtiles
    /// a frame and lands its fire damage, less the brute's resistance. A wall stops a bolt; a
    /// fire-immune monster takes nothing; Attack swings.
    #[test]
    fn a_learned_fire_bolt_flies_and_burns() {
        let mut data = data();
        // Attack, 35 rows standing in for the common and Amazon skills, then Fire Bolt at its id, 36.
        let mut skills = String::from("skill\tcharclass\treqlevel\tmaxlvl\tInGame\tsrvmissile\trange\tanim\tminmana\tmanashift\tmana\tHitShift\tEType\tEMin\tEMax\r\nAttack\t\t1\t\t1\t\tboth\tA1\t\t8\t\t8\t\t\t\r\n");
        for i in 1..36 {
            skills += &format!("skill {i}\t\t1\t\t\t\t\t\t\t\t\t\t\t\t\r\n");
        }
        skills += "Fire Bolt\tsor\t1\t20\t1\tfirebolt\tnone\tSC\t1\t7\t5\t7\tfire\t6\t12\r\n";
        data.set_skills(d2_data::skills::Skills::from_table(&Table::parse(skills.as_bytes())));
        data.set_missiles(d2_data::missiles::Missiles::from_table(&Table::parse(b"Missile\tVel\tRange\tCollideType\tCollideKill\tSkill\r\nfirebolt\t20\t50\t3\t1\tFire Bolt\r\n")));
        let monstats = Table::parse(
            b"Id\thcIdx\tMonStatsEx\tCode\tLevel\tminHP\tmaxHP\tExp\tA1MinD\tA1MaxD\tA1TH\taidel\tResFi\r\n\
              brute\t0\tbrute\tXX\t1\t500\t500\t100\t1\t1\t1\t5\t50\r\n\
              salamander\t1\tbrute\tXX\t1\t500\t500\t100\t1\t1\t1\t5\t100\r\n",
        );
        let monstats2 = Table::parse(b"Id\tSizeX\tBaseW\tMeleeRng\r\nbrute\t1\thth\t0\r\n");
        data.set_map_tables(MonPresets::default(), Monsters::from_tables(&monstats, &monstats2).unwrap(), Objects::default());
        let mut b = Battle::new(0, 99);
        assert!(b.add_monster(&data, 7, 0, ROOM, 110, 100));
        assert!(b.add_monster(&data, 8, 1, ROOM, 100, 110));
        let mut stats = data.new_character_stats(1).unwrap();
        stats.push((stat::NEWSKILLS, 1));
        b.add_player(&data, "sorc", 1, &stats);
        b.place_player("sorc", Some((100, 100, ROOM.level)), &[ROOM]);
        let fire_bolt = data.skills().id("Fire Bolt").unwrap();
        assert!(!b.select_skill("sorc", fire_bolt, false), "not learned");
        assert_eq!(b.player_skill(&data, "sorc", false, Aim::Unit(7)), SkillUse::Swing, "Attack is on both buttons");
        let learned = b.learn_skill(&data, "sorc", fire_bolt);
        assert_eq!(learned, [Event::SkillLevel { player: "sorc".into(), skill: fire_bolt as u16, level: 1 }, Event::PlayerStat { player: "sorc".into(), stat: stat::NEWSKILLS, value: 0 }]);
        assert!(b.learn_skill(&data, "sorc", fire_bolt).is_empty(), "no points left");
        assert!(b.select_skill("sorc", fire_bolt, false));
        let open = |x: i32, _: i32| (x < 120).then_some(ROOM.level);
        let mana = |b: &Battle| b.heroes["sorc"].mana;
        let before = mana(&b);
        assert_eq!(b.player_skill(&data, "sorc", false, Aim::Unit(7)), SkillUse::Cast);
        assert_eq!(b.player_skill(&data, "sorc", false, Aim::Unit(7)), SkillUse::Refused, "still casting");
        let mut events = b.advance(&data, b.frame() + 8, &open);
        assert_eq!(mana(&b), before - (5 << 7), "2.5 mana on the action frame");
        events.extend(b.advance(&data, b.frame() + 20, &open));
        let life = |b: &Battle, guid| b.monsters[&guid].life;
        let burnt = 500 - life(&b, 7);
        assert!((1..=3).contains(&burnt), "3–6 fire, half resisted: {burnt}");
        assert!(events.iter().any(|e| matches!(e, Event::MonsterLife { guid: 7, .. })));
        assert!(b.flights.is_empty(), "the bolt ended on the brute");

        b.advance(&data, b.frame() + 25, &open);
        assert_eq!(b.player_skill(&data, "sorc", false, Aim::Unit(8)), SkillUse::Cast);
        b.advance(&data, b.frame() + 40, &open);
        assert_eq!(life(&b, 8), 500, "immune to fire");

        b.advance(&data, b.frame() + 25, &open);
        assert_eq!(b.player_skill(&data, "sorc", false, Aim::At(140, 100)), SkillUse::Cast);
        b.advance(&data, b.frame() + 60, &open);
        let (hp7, flights) = (life(&b, 7), b.flights.len());
        assert!(hp7 < 500 - burnt && flights == 0, "aimed past it, the bolt meets the brute on the way and ends");
        let mut dry = b.clone();
        dry.heroes.get_mut("sorc").unwrap().mana = 0;
        assert_eq!(dry.player_skill(&data, "sorc", false, Aim::Unit(7)), SkillUse::Refused, "no mana");
    }

    #[test]
    fn monsters_glide_at_the_speed_their_walk_packet_gives_the_client() {
        // A Dark Hunter (Velocity 5) at 75%: 5 × 0.75 / 16 of a subtile a frame, 5.86 a second.
        assert_eq!(glide_frames(5, 8.0), 35);
        assert_eq!(glide_frames(1, 3.0), 64, "a zombie crawls");
        assert_eq!(glide_frames(5, 0.1), 1);
    }

    #[test]
    fn numbers_follow_the_formulas() {
        assert_eq!(chance_to_hit(85, 5, 1, 1), 94);
        assert_eq!(chance_to_hit(0, 0, 1, 1), 95, "clamped");
        assert_eq!(chance_to_hit(1, 1000, 1, 30), 5, "clamped");
        assert_eq!(life_byte(5, 10), 64);
        assert_eq!(life_byte(1, 1000), 1, "a sliver while alive");
        assert_eq!(life_byte(0, 10), 0);
        assert_eq!(experience_for(100, 1, 1), 100);
        assert_eq!(experience_for(100, 8, 2), 81);
        assert_eq!(experience_for(100, 20, 2), 5);
        assert_eq!(experience_for(100, 2, 10), 20);
    }

    #[test]
    fn hitting_a_monster_to_death_pays_experience_and_levels_up() {
        let data = data();
        let mut b = battle(&data);
        b.place_player("hero", Some((102, 100, ROOM.level)), &[ROOM]);
        let open = |_: i32, _: i32| Some(ROOM.level);
        let mut all = Vec::new();
        for _ in 0..60 {
            b.player_attack("hero", 7);
            all.extend(b.advance(&data, b.frame() + 25, &open));
            if b.monster(7).is_some_and(|m| m.2 == DEAD_MODE) {
                break;
            }
        }
        all.extend(b.advance(&data, b.frame() + 50, &open));
        assert!(all.iter().any(|e| matches!(e, Event::MonsterLife { guid: 7, .. })), "the bar drops first: {all:?}");
        let dying = all.iter().position(|e| matches!(e, Event::MonsterReaction { event: reaction::DYING, .. })).expect("it dies");
        let dead = all.iter().position(|e| matches!(e, Event::MonsterReaction { event: reaction::DEAD, alive: false, .. })).expect("and lies dead");
        assert!(dying < dead);
        assert!(all.contains(&Event::Experience { player: "hero".into(), old: 0, new: 100 }));
        assert!(all.contains(&Event::PlayerStat { player: "hero".into(), stat: stat::LEVEL, value: 3 }), "100 exp passes 20 and 60");
        assert!(all.contains(&Event::PlayerStat { player: "hero".into(), stat: stat::STATPTS, value: 10 }));
        assert!(!b.player_attack("hero", 7), "a corpse cannot be hit");
        let stats = b.player_stats("hero").unwrap();
        let of = |id: u8| stats.iter().find(|s| s.0 == id).unwrap().1;
        assert_eq!((of(stat::HITPOINTS), of(stat::MANA), of(stat::STAMINA)), (of(stat::MAXHP), of(stat::MAXMANA), of(stat::MAXSTAMINA)), "a new level fills them");
        let points = b.spend_stat_point(&data, "hero", stat::VITALITY);
        assert!(points.contains(&Event::PlayerStat { player: "hero".into(), stat: stat::STATPTS, value: 9 }));
        assert!(points.iter().any(|e| matches!(e, Event::PlayerStat { stat: stat::MAXHP, .. })));
    }

    #[test]
    fn a_monster_chases_swings_and_can_kill() {
        let data = data();
        let mut b = battle(&data);
        let open = |_: i32, _: i32| Some(ROOM.level);
        b.place_player("hero", Some((120, 100, ROOM.level)), &[]);
        assert!(b.advance(&data, 50, &open).is_empty(), "a player whose client does not hold the room is not noticed");
        b.place_player("hero", Some((120, 100, ROOM.level)), &[ROOM]);
        let mut all = Vec::new();
        for _ in 0..200 {
            all.extend(b.advance(&data, b.frame() + 5, &open));
            if b.player_dead("hero") {
                break;
            }
        }
        all.extend(b.advance(&data, b.frame() + 60, &open));
        let walk = all.iter().position(|e| matches!(e, Event::MonsterWalk { guid: 7, .. })).expect("it walks over");
        let swing = all.iter().position(|e| matches!(e, Event::MonsterAttack { guid: 7, .. })).expect("then swings");
        assert!(walk < swing);
        assert!(all.iter().any(|e| matches!(e, Event::PlayerVitals { .. })), "the player's life drops");
        let dying = all.iter().position(|e| *e == Event::PlayerReaction { player: "hero".into(), event: reaction::DYING }).expect("and dies");
        let dead = all.iter().position(|e| *e == Event::PlayerReaction { player: "hero".into(), event: reaction::DEAD }).expect("then lies dead");
        assert!(dying < dead);
        let after = b.advance(&data, b.frame() + 100, &open);
        assert!(!after.iter().any(|e| matches!(e, Event::MonsterAttack { .. })), "nobody hits a corpse");
        let revived = b.revive("hero");
        assert!(matches!(revived[..], [Event::PlayerVitals { life, .. }] if life > 0));
        assert!(!b.player_dead("hero"));
    }

    /// A game nobody drove for a while skips frames to catch up; what was due on them still
    /// happens — a dead player is still laid out as a corpse, which its release waits for.
    #[test]
    fn what_falls_due_in_skipped_frames_still_happens() {
        let data = data();
        let mut b = battle(&data);
        let open = |_: i32, _: i32| Some(ROOM.level);
        b.place_player("hero", Some((120, 100, ROOM.level)), &[ROOM]);
        let mut all = Vec::new();
        for _ in 0..2000 {
            all.extend(b.advance(&data, b.frame() + 1, &open));
            if b.player_dead("hero") {
                break;
            }
        }
        assert!(all.iter().any(|e| *e == Event::PlayerReaction { player: "hero".into(), event: reaction::DYING }), "killed");
        assert!(!all.iter().any(|e| *e == Event::PlayerReaction { player: "hero".into(), event: reaction::DEAD }), "not yet laid out");
        let later = b.advance(&data, b.frame() + 10 * FRAMES_PER_SECOND, &open);
        assert!(later.contains(&Event::PlayerReaction { player: "hero".into(), event: reaction::DEAD }), "the corpse, though its frame was skipped");
    }

    #[test]
    fn running_out_of_town_spends_stamina_and_standing_gets_it_back() {
        let data = data();
        let mut b = battle(&data);
        let open = |_: i32, _: i32| Some(ROOM.level);
        // Far from the monster, in Blood Moor: 92 stamina, RunDrain 20 → 40/256 a frame.
        b.place_player("hero", Some((1000, 1000, ROOM.level)), &[]);
        b.set_motion("hero", Motion::Running);
        let mut events = b.advance(&data, 125, &open);
        events.extend(b.advance(&data, 250, &open));
        let stamina: Vec<u16> = events.iter().filter_map(|e| if let Event::PlayerVitals { stamina, .. } = e { Some(*stamina) } else { None }).collect();
        assert!(stamina.windows(2).all(|w| w[1] < w[0]), "falling: {stamina:?}");
        assert_eq!(*stamina.last().unwrap(), (92 * 256 - 250 * 40) / 256, "ten seconds of running");
        assert!(stamina.len() >= 30 && stamina.len() <= 50, "told every few frames, not every frame: {}", stamina.len());
        b.set_motion("hero", Motion::Standing);
        let mut events = b.advance(&data, 350, &open);
        events.extend(b.advance(&data, 400, &open));
        assert!(matches!(events.last(), Some(Event::PlayerVitals { stamina: 92, .. })), "full again within six seconds");
        b.place_player("hero", Some((1000, 1000, 1)), &[]);
        b.set_motion("hero", Motion::Running);
        assert!(b.advance(&data, 500, &open).is_empty(), "the camp costs nothing");
    }

    /// A hero of `class` with 1 life, 1 mana and no vitality or energy (so no lucky doubling).
    fn weakened(data: &GameData, class: u8) -> Battle {
        let mut stats = data.new_character_stats(class).unwrap();
        for (id, value) in &mut stats {
            match *id {
                stat::HITPOINTS | stat::MANA => *value = 1 << 8,
                stat::VITALITY | stat::ENERGY => *value = 0,
                _ => {}
            }
        }
        let mut b = Battle::new(0, 99);
        b.add_player(data, "hero", class, &stats);
        b.place_player("hero", Some((1000, 1000, 1)), &[]);
        b
    }

    fn vitals(events: &[Event]) -> Vec<(u16, u16)> {
        events.iter().filter_map(|e| if let Event::PlayerVitals { life, mana, .. } = e { Some((*life, *mana)) } else { None }).collect()
    }

    #[test]
    fn a_healing_potion_spreads_its_share_over_its_frames() {
        let data = data();
        let open = |_: i32, _: i32| Some(1);
        // A Barbarian drinks for twice the points: 20 over 192 frames, 26/256 a frame.
        let mut b = weakened(&data, 4);
        assert_eq!(b.drink("hero", Potion::Healing { points: 10, frames: 192 }), Some(Vec::new()));
        let mut events = b.advance(&data, 100, &open);
        events.extend(b.advance(&data, 192, &open));
        let told = vitals(&events);
        assert!(told.windows(2).all(|w| w[1].0 > w[0].0), "rising: {told:?}");
        assert_eq!(told.last().unwrap().0, ((256 + 26 * 192) >> 8) as u16, "the last frame is told");
        assert!(b.advance(&data, 300, &open).is_empty(), "then it is spent");
        let life = |b: &Battle| b.player_stats("hero").unwrap().iter().find(|s| s.0 == stat::HITPOINTS).unwrap().1;
        assert_eq!(life(&b), 256 + 26 * 192);

        // A second potion halfway folds the rest of the first into it.
        let mut b = weakened(&data, 1);
        b.drink("hero", Potion::Healing { points: 10, frames: 192 });
        b.advance(&data, 96, &open);
        b.drink("hero", Potion::Healing { points: 10, frames: 192 });
        b.advance(&data, 96 + 100, &open);
        b.advance(&data, 96 + 200, &open);
        b.advance(&data, 96 + 288, &open);
        // 13 a frame for 96 frames, then (13 × 96 + 2560) / 288 = 13 for 288 more.
        assert_eq!(life(&b), 256 + 13 * 96 + 13 * 288);
    }

    #[test]
    fn mana_and_rejuvenation_follow_the_class_and_the_maximum() {
        let data = data();
        let open = |_: i32, _: i32| Some(1);
        // An Amazon gets half again: 30 mana over 128 frames (60/256 a frame).
        let mut b = weakened(&data, 0);
        b.drink("hero", Potion::Mana { points: 20, frames: 128 });
        b.advance(&data, 100, &open);
        b.advance(&data, 128, &open);
        let stat_of = |b: &Battle, id: u8| b.player_stats("hero").unwrap().iter().find(|s| s.0 == id).unwrap().1 as i32;
        assert_eq!(stat_of(&b, stat::MANA), (256 + 60 * 128).min(stat_of(&b, stat::MAXMANA)));
        // A rejuvenation potion: 35% of each at once, told straight away.
        let mut b = weakened(&data, 4);
        let (max_life, max_mana) = (stat_of(&b, stat::MAXHP), stat_of(&b, stat::MAXMANA));
        let told = b.drink("hero", Potion::Rejuvenation { life: 35, mana: 35 }).unwrap();
        assert_eq!(stat_of(&b, stat::HITPOINTS), 256 + max_life * 35 / 100);
        assert_eq!(vitals(&told), [(((256 + max_life * 35 / 100) >> 8) as u16, ((256 + max_mana * 35 / 100) >> 8) as u16)]);
        b.drink("hero", Potion::Rejuvenation { life: 100, mana: 100 });
        assert_eq!((stat_of(&b, stat::HITPOINTS), stat_of(&b, stat::MANA)), (max_life, max_mana), "a full one fills");
        assert_eq!(b.drink("nobody", Potion::Rejuvenation { life: 100, mana: 100 }), None);
    }

    #[test]
    fn a_healer_fills_what_is_missing() {
        let data = data();
        let mut b = weakened(&data, 4);
        let max = |b: &Battle, id: u8| b.player_stats("hero").unwrap().iter().find(|s| s.0 == id).unwrap().1;
        let healed = b.heal("hero");
        assert_eq!(
            healed,
            [
                Event::PlayerStat { player: "hero".into(), stat: stat::HITPOINTS, value: max(&b, stat::MAXHP) },
                Event::PlayerStat { player: "hero".into(), stat: stat::MANA, value: max(&b, stat::MAXMANA) },
            ],
            "stamina was full"
        );
        assert!(b.heal("hero").is_empty(), "nothing left to fill");
        assert!(b.heal("nobody").is_empty());
    }

    #[test]
    fn potions_read_from_their_misc_row() {
        let itemtypes = Table::parse(b"ItemType\tCode\tEquiv1\r\nPotion\tpoti\t\r\n");
        let empty = Table::parse(b"name\tcode\ttype\r\n");
        let misc = Table::parse(
            b"name\tcode\ttype\tpSpell\tlen\tstat1\tcalc1\tstat2\tcalc2\r\n\
              Minor Healing Potion\thp1\tpoti\t3\t192\thpregen\t30\t\t\r\n\
              Minor Mana Potion\tmp1\tpoti\t3\t128\tmanarecovery\t20\t\t\r\n\
              Rejuv Potion\trvs\tpoti\t5\t\thitpoints\t35\tmana\t35\r\n\
              Stamina Potion\tvps\tpoti\t9\t750\tstaminarecoverybonus\t5000\t\t\r\n",
        );
        let items = d2_data::items::Items::from_tables(&itemtypes, &empty, &empty, &misc).unwrap();
        let potion = |i: i32| Potion::of(items.get(i).unwrap());
        assert_eq!(potion(0), Some(Potion::Healing { points: 30, frames: 192 }));
        assert_eq!(potion(1), Some(Potion::Mana { points: 20, frames: 128 }));
        assert_eq!(potion(2), Some(Potion::Rejuvenation { life: 35, mana: 35 }));
        assert_eq!(potion(3), None, "stamina is not ported");
    }

    #[test]
    fn a_walled_off_player_is_not_reached() {
        let data = data();
        let mut b = battle(&data);
        let open = |x: i32, _: i32| (x != 110).then_some(ROOM.level);
        b.place_player("hero", Some((120, 100, ROOM.level)), &[ROOM]);
        let all = b.advance(&data, 200, &open);
        assert!(!all.iter().any(|e| matches!(e, Event::MonsterAttack { .. })));
        assert!(!all.iter().any(|e| matches!(e, Event::MonsterWalk { x, .. } if *x >= 110)));
    }
}
