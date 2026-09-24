//! `Skills.txt`: a skill's class, the weapon type it needs, its requirements, what it costs and
//! the damage and missiles it deals (the record `0x00613F80` loads, stride `0x23C`). A skill's id
//! is its row.

use d2_formats::excel::Table;

use crate::items::class_index;

/// One skill row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skill {
    /// `skill`.
    pub name: String,
    /// `charclass`, as a class index.
    pub class: Option<u8>,
    /// `itypea1`: the item type the skill needs (`+0x18`), when it needs one.
    pub item_type: Option<String>,
    /// `reqlevel`.
    pub req_level: i32,
    /// `maxlvl`.
    pub max_level: i32,
    /// `cost mult` (`+0x234`): what a point of the skill on an item adds to its price, in 1024ths.
    pub cost_mult: i32,
    /// `cost add` (`+0x238`): gold a point of it adds.
    pub cost_add: i32,
    /// `srvdofunc` (`+0x2C`): what doing the skill does on the server (1 attack, 0 none).
    pub srv_do_func: i32,
    /// `srvmissile`: the missile it shoots, by `Missiles.txt` name.
    pub missile: Option<String>,
    /// `srvmissilea`: the first of the missiles a many-missile skill shoots.
    pub missile_a: Option<String>,
    /// `range`: `h2h`, `rng`, `both` or `none`.
    pub range: String,
    /// `anim`: the player mode it plays (`A1`, `SC`, …).
    pub anim: String,
    /// `passive`.
    pub passive: bool,
    /// `InTown` (`+5` bit 0): usable in a town.
    pub in_town: bool,
    /// `InGame` (`+5` bit 2): a skill players can learn.
    pub in_game: bool,
    /// `reqstr`, `reqdex`, `reqint`, `reqvit` (`+0x176`–`+0x17C`).
    pub req_attributes: [i32; 4],
    /// `reqskill1`–`reqskill3` (`+0x17E`–`+0x182`), by name.
    pub req_skills: Vec<String>,
    /// `skpoints` (`+0x170`): the points a level costs, as a calc; blank for 1.
    pub skill_points: String,
    /// `minmana`, `manashift`, `mana`, `lvlmana` (`+0x186`–`+0x18C`).
    pub mana: (i32, i32, i32, i32),
    /// `ToHit`, `LevToHit`.
    pub to_hit: (i32, i32),
    /// `HitShift` (`+0x1A4`): damage columns are in `2^HitShift`ths of 256ths.
    pub hit_shift: i32,
    /// `SrcDam`: 128ths of the weapon's damage it deals.
    pub source_damage: i32,
    /// `MinDam`, `MinLevDam1`–`5` (`+0x1A8`).
    pub min_damage: (i32, [i32; 5]),
    /// `MaxDam`, `MaxLevDam1`–`5` (`+0x1AC`).
    pub max_damage: (i32, [i32; 5]),
    /// `EType`: `fire`, `ltng`, `cold`, `pois`, `mag`, blank for none.
    pub element: String,
    /// `EMin`, `EMinLev1`–`5`.
    pub element_min: (i32, [i32; 5]),
    /// `EMax`, `EMaxLev1`–`5`.
    pub element_max: (i32, [i32; 5]),
    /// `EDmgSymPerCalc` (`+0x210`): the percent other skills' levels add to its elemental damage,
    /// as a calc.
    pub element_synergy: String,
    /// `Param1`–`Param8`.
    pub params: [i32; 8],
    /// `calc1`–`calc4`.
    pub calcs: [String; 4],
    /// `srvstfunc` (`+0x2C`): what starting the skill does on the server (table `0x00732140`).
    pub srv_start_func: i32,
    /// `ToHitCalc`: the attack rating percent a swing with it adds, as a calc; when blank,
    /// `ToHit` + `LevToHit` × (level − 1).
    pub to_hit_calc: String,
    /// `ELen`, `ELevLen1`–`3`: frames an elemental effect lasts (a stun's, a freeze's).
    pub element_length: (i32, [i32; 3]),
    /// `ELenSymPerCalc`: the percent other skills add to that length.
    pub element_length_synergy: String,
    /// `DmgSymPerCalc`: the percent other skills add to its physical damage.
    pub damage_synergy: String,
    /// `ResultFlags`: what a hit does besides damage (8 knocks back).
    pub result_flags: i32,
    /// `HitClass`: the hit's sound and overlay class.
    pub hit_class: i32,
    /// `AttackNoMana`: short of mana, it swings as Attack instead of refusing.
    pub attack_no_mana: bool,
    /// `weapsel`: which hand swings — 2 both at once (Whirlwind), 3 alternating (Double Swing,
    /// Frenzy, Double Throw), 4 none (Kick).
    pub weapon_select: i32,
    /// `itypeb1`: the item type the other hand needs.
    pub item_type_b: Option<String>,
    /// `srvmissileb`, `srvmissilec`.
    pub missile_b: Option<String>,
    /// `srvmissilec`.
    pub missile_c: Option<String>,
    /// `TargetCorpse`: aimed at a body.
    pub target_corpse: bool,
    /// `passivestate` (`+0x94`), by `States.txt` name.
    pub passive_state: Option<String>,
    /// `passiveitype` (`+0x96`): the weapon type a passive's stats count for.
    pub passive_item_type: Option<String>,
    /// `passivestat1`–`5` with `passivecalc1`–`5`: `ItemStatCost.txt` name and calc.
    pub passive_stats: Vec<(String, String)>,
    /// `aurastate` (`+0x80`): the state on the user.
    pub aura_state: Option<String>,
    /// `pettype` (`+0xBE`): the kind of pet a summon is, by `PetType.txt` name.
    pub pet_type: Option<String>,
    /// `prgcalc1`–`prgcalc3` (`+0x38`…): a charge-up skill's charges, and some skills' counts
    /// (Shock Web's webs), as calcs.
    pub progress_calcs: [String; 3],
    /// `progressive`: a charge-up skill (Tiger Strike, Fists of Fire…).
    pub progressive: bool,
    /// `srvprgfunc1`–`srvprgfunc3`: what each held charge does when a finisher lets it go (by the
    /// `srvdofunc` table).
    pub progress_funcs: [i32; 3],
    /// `prgstack`: a finisher lets every held charge's go, not only the last's.
    pub progress_stack: bool,
    /// `summon` (`+0xBC`): the `MonStats.txt` row a summon makes.
    pub summon: Option<String>,
    /// `petmax` (`+0xC0`): how many of its pets a player may have at once, as a calc.
    pub pet_max: String,
    /// `summode` (`+0xBF`): the mode a summon appears in.
    pub summon_mode: Option<String>,
    /// `sumskill1`–`5` with `sumsk1calc`–`5`: skills a summon is given, by name, and their level
    /// calcs, evaluated on its owner (`0x005C4470`).
    pub summon_skills: Vec<(String, String)>,
    /// `auratargetstate`: the state on whoever it reaches.
    pub aura_target_state: Option<String>,
    /// `auralencalc`: how long those states last, frames, as a calc.
    pub aura_length: String,
    /// `aurarangecalc`: how far it reaches, as a calc.
    pub aura_range: String,
    /// `aurastat1`–`6` with `aurastatcalc1`–`6`: the stats those states carry.
    pub aura_stats: Vec<(String, String)>,
    /// `aurafilter`: which units it reaches.
    pub aura_filter: i32,
    /// `aura`: selected on the right button it runs by itself, ticking every `perdelay` frames.
    pub aura: bool,
    /// `immediate`: an aura that works the moment it is selected.
    pub immediate: bool,
    /// `perdelay`: an aura's tick, frames (a calc).
    pub per_delay: String,
    /// `restrict` (`+0x228`): 0 usable only out of a restricting state (a Druid's wolf or bear
    /// form), 1 always, 2 only in one and in one of [`Self::restrict_states`] (`0x00644060`).
    pub restrict: i32,
    /// `State1`–`State3` (`+0x22A`): the restricting states a restrict-2 skill wants.
    pub restrict_states: Vec<String>,
    /// `delay` (`+0x190`): frames before any skill with a delay may be used again (a calc).
    pub delay: String,
    /// `seqtrans`: the mode a sequence counts as for speed (`SC` makes it a cast, `0x006216E0`).
    pub seq_trans: String,
    /// `UseAttackRate`: a sequence or special mode timed by the attack rate (`0x00621580`).
    pub use_attack_rate: bool,
    /// `skilldesc`: its `SkillDesc.txt` row's name, whose `SkillPage` is the tab an item's
    /// `item_addskill_tab` raises.
    pub description: Option<String>,
    /// Its tab, 1–3 (`SkillDesc.txt` `SkillPage`); 0 for none — set by [`Skills::set_pages`].
    pub page: u8,
    /// Its `EType` as an `ElemTypes.txt` row — the param of `item_elemskill` — 0 for none; set by
    /// [`Skills::set_pages`].
    pub element_type: u8,
}

/// A column's text, `None` when blank.
fn text(row: &d2_formats::excel::Row<'_>, column: &str) -> Option<String> {
    row.get(column).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A five-column per-level progression: `<prefix>1`…`<prefix>5`.
fn levels(row: &d2_formats::excel::Row, prefix: &str) -> [i32; 5] {
    [1, 2, 3, 4, 5].map(|i| row.int(&format!("{prefix}{i}")).unwrap_or(0) as i32)
}

/// Every skill, by id.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    rows: Vec<Skill>,
}

impl Skills {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|row| Skill {
                name: row.get("skill").unwrap_or_default().to_string(),
                class: row.get("charclass").and_then(class_index),
                item_type: row.get("itypea1").filter(|s| !s.is_empty()).map(str::to_string),
                req_level: row.int("reqlevel").unwrap_or(0) as i32,
                max_level: row.int("maxlvl").unwrap_or(0) as i32,
                cost_mult: row.int("cost mult").unwrap_or(0) as i32,
                cost_add: row.int("cost add").unwrap_or(0) as i32,
                srv_do_func: row.int("srvdofunc").unwrap_or(0) as i32,
                missile: row.get("srvmissile").filter(|s| !s.is_empty()).map(str::to_string),
                missile_a: row.get("srvmissilea").filter(|s| !s.is_empty()).map(str::to_string),
                range: row.get("range").unwrap_or_default().to_string(),
                anim: row.get("anim").unwrap_or_default().to_string(),
                passive: row.int("passive").unwrap_or(0) != 0,
                in_town: row.int("InTown").unwrap_or(0) != 0,
                in_game: row.int("InGame").unwrap_or(0) != 0,
                req_attributes: ["reqstr", "reqdex", "reqint", "reqvit"].map(|c| row.int(c).unwrap_or(0) as i32),
                req_skills: ["reqskill1", "reqskill2", "reqskill3"].iter().filter_map(|c| row.get(c).filter(|s| !s.is_empty()).map(str::to_string)).collect(),
                skill_points: row.get("skpoints").unwrap_or_default().to_string(),
                mana: (row.int("minmana").unwrap_or(0) as i32, row.int("manashift").unwrap_or(0) as i32, row.int("mana").unwrap_or(0) as i32, row.int("lvlmana").unwrap_or(0) as i32),
                to_hit: (row.int("ToHit").unwrap_or(0) as i32, row.int("LevToHit").unwrap_or(0) as i32),
                hit_shift: row.int("HitShift").unwrap_or(0) as i32,
                source_damage: row.int("SrcDam").unwrap_or(0) as i32,
                min_damage: (row.int("MinDam").unwrap_or(0) as i32, levels(&row, "MinLevDam")),
                max_damage: (row.int("MaxDam").unwrap_or(0) as i32, levels(&row, "MaxLevDam")),
                element: row.get("EType").unwrap_or_default().to_string(),
                element_min: (row.int("EMin").unwrap_or(0) as i32, levels(&row, "EMinLev")),
                element_max: (row.int("EMax").unwrap_or(0) as i32, levels(&row, "EMaxLev")),
                element_synergy: row.get("EDmgSymPerCalc").unwrap_or_default().to_string(),
                params: [1, 2, 3, 4, 5, 6, 7, 8].map(|i| row.int(&format!("Param{i}")).unwrap_or(0) as i32),
                calcs: [1, 2, 3, 4].map(|i| row.get(&format!("calc{i}")).unwrap_or_default().to_string()),
                srv_start_func: row.int("srvstfunc").unwrap_or(0) as i32,
                to_hit_calc: row.get("ToHitCalc").unwrap_or_default().to_string(),
                element_length: (row.int("ELen").unwrap_or(0) as i32, [1, 2, 3].map(|i| row.int(&format!("ELevLen{i}")).unwrap_or(0) as i32)),
                element_length_synergy: row.get("ELenSymPerCalc").unwrap_or_default().to_string(),
                damage_synergy: row.get("DmgSymPerCalc").unwrap_or_default().to_string(),
                result_flags: row.int("ResultFlags").unwrap_or(0) as i32,
                hit_class: row.int("HitClass").unwrap_or(0) as i32,
                attack_no_mana: row.int("AttackNoMana").unwrap_or(0) != 0,
                weapon_select: row.int("weapsel").unwrap_or(0) as i32,
                item_type_b: text(&row, "itypeb1"),
                missile_b: text(&row, "srvmissileb"),
                missile_c: text(&row, "srvmissilec"),
                target_corpse: row.int("TargetCorpse").unwrap_or(0) != 0,
                passive_state: text(&row, "passivestate"),
                passive_item_type: text(&row, "passiveitype"),
                passive_stats: (1..=5).filter_map(|i| Some((text(&row, &format!("passivestat{i}"))?, row.get(&format!("passivecalc{i}")).unwrap_or_default().to_string()))).collect(),
                aura_state: text(&row, "aurastate"),
                pet_type: text(&row, "pettype"),
                progress_calcs: ["prgcalc1", "prgcalc2", "prgcalc3"].map(|c| row.get(c).unwrap_or_default().to_string()),
                progressive: row.int("progressive").unwrap_or(0) != 0,
                progress_funcs: ["srvprgfunc1", "srvprgfunc2", "srvprgfunc3"].map(|c| row.int(c).unwrap_or(0) as i32),
                progress_stack: row.int("prgstack").unwrap_or(0) != 0,
                summon: text(&row, "summon"),
                pet_max: row.get("petmax").unwrap_or_default().to_string(),
                summon_mode: text(&row, "summode"),
                summon_skills: (1..=5)
                    .filter_map(|n| Some((text(&row, &format!("sumskill{n}"))?, row.get(&format!("sumsk{n}calc")).unwrap_or_default().to_string())))
                    .collect(),
                aura_target_state: text(&row, "auratargetstate"),
                aura_length: row.get("auralencalc").unwrap_or_default().to_string(),
                aura_range: row.get("aurarangecalc").unwrap_or_default().to_string(),
                aura_stats: (1..=6).filter_map(|i| Some((text(&row, &format!("aurastat{i}"))?, row.get(&format!("aurastatcalc{i}")).unwrap_or_default().to_string()))).collect(),
                aura_filter: row.int("aurafilter").unwrap_or(0) as i32,
                aura: row.int("aura").unwrap_or(0) != 0,
                immediate: row.int("immediate").unwrap_or(0) != 0,
                per_delay: row.get("perdelay").unwrap_or_default().to_string(),
                restrict: row.int("restrict").unwrap_or(0) as i32,
                restrict_states: (1..=3).filter_map(|i| text(&row, &format!("State{i}"))).collect(),
                delay: row.get("delay").unwrap_or_default().to_string(),
                description: text(&row, "skilldesc"),
                seq_trans: row.get("seqtrans").unwrap_or_default().to_string(),
                use_attack_rate: row.int("UseAttackRate").unwrap_or(0) != 0,
                page: 0,
                element_type: 0,
            })
            .collect();
        Self { rows }
    }

    /// Each skill's tab from `SkillDesc.txt` (`SkillPage` of its `skilldesc` row) and its
    /// `EType`'s row in `ElemTypes.txt` (the params of `item_addskill_tab` and `item_elemskill`,
    /// `0x00644180`).
    pub fn set_pages(&mut self, skilldesc: &Table, elemtypes: &Table) {
        let pages: std::collections::HashMap<String, u8> = skilldesc
            .rows()
            .filter_map(|row| Some((row.get("skilldesc")?.to_ascii_lowercase(), row.int("SkillPage").unwrap_or(0).clamp(0, 255) as u8)))
            .collect();
        let elements: Vec<String> = elemtypes.rows().map(|row| row.get("Code").unwrap_or_default().to_ascii_lowercase()).collect();
        for skill in &mut self.rows {
            skill.page = skill.description.as_deref().and_then(|d| pages.get(&d.to_ascii_lowercase())).copied().unwrap_or(0);
            skill.element_type = (!skill.element.is_empty()).then(|| elements.iter().position(|e| e.eq_ignore_ascii_case(&skill.element))).flatten().map_or(0, |i| i as u8);
        }
    }

    /// A skill by id.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&Skill> {
        usize::try_from(id).ok().and_then(|i| self.rows.get(i))
    }

    /// How many skills there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A skill's id by name, any case.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<i32> {
        self.rows.iter().position(|s| s.name.eq_ignore_ascii_case(name)).map(|i| i as i32)
    }

    /// A class's first skill: the one its item skills count from (`0x006460F0` with index 0).
    #[must_use]
    pub fn first_of(&self, class: u8) -> Option<i32> {
        self.rows.iter().position(|s| s.class == Some(class)).map(|i| i as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_keep_row_ids_and_class_order() {
        let t = Table::parse(
            b"skill\tId\tcharclass\titypea1\treqlevel\tmaxlvl\r\nAttack\t0\t\t\t\t\r\nMagic Arrow\t1\tama\tbow\t1\t20\r\nJab\t2\tama\tspea\t1\t20\r\nFire Bolt\t3\tsor\t\t1\t20\r\n",
        );
        let skills = Skills::from_table(&t);
        assert_eq!((skills.first_of(0), skills.first_of(1), skills.first_of(2)), (Some(1), Some(3), None));
        assert_eq!(skills.get(2).and_then(|s| s.item_type.as_deref()), Some("spea"));
        assert_eq!(skills.get(0).map(|s| s.class), Some(None));
    }
}
