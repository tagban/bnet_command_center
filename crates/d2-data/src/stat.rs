//! Stat ids: rows of `ItemStatCost.txt`, which the engine and the wire use directly.

/// `strength`.
pub const STRENGTH: u8 = 0;
/// `energy`.
pub const ENERGY: u8 = 1;
/// `dexterity`.
pub const DEXTERITY: u8 = 2;
/// `vitality`.
pub const VITALITY: u8 = 3;
/// `hitpoints` — current life, 1/256 fixed-point.
pub const HITPOINTS: u8 = 6;
/// `maxhp`, 1/256 fixed-point.
pub const MAXHP: u8 = 7;
/// `mana`, 1/256 fixed-point.
pub const MANA: u8 = 8;
/// `maxmana`, 1/256 fixed-point.
pub const MAXMANA: u8 = 9;
/// `stamina`, 1/256 fixed-point.
pub const STAMINA: u8 = 10;
/// `maxstamina`, 1/256 fixed-point.
pub const MAXSTAMINA: u8 = 11;
/// `level`.
pub const LEVEL: u8 = 12;
/// `experience`.
pub const EXPERIENCE: u8 = 13;
/// `gold`.
pub const GOLD: u8 = 14;
/// `nextexp` — experience needed to leave the current level.
pub const NEXTEXP: u8 = 30;
/// `velocitypercent`.
pub const VELOCITY_PERCENT: u8 = 67;
/// `attackrate`.
pub const ATTACK_RATE: u8 = 68;
/// `other_animrate`.
pub const OTHER_ANIM_RATE: u8 = 69;
