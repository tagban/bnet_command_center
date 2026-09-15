//! Players' skills: what a level of a skill costs in points and mana, the damage it deals, and the
//! calcs `Skills.txt` writes them in.
//!
//! - **Learning** (`0x3B`, `0x0054BD90`): a skill of the player's class (`0x0056C700`) that players
//!   use (`InGame`), no higher in required level and attributes than the player
//!   (`0x006447D0`, `0x00644920`), whose required skills it has, below its `maxlvl` (20 when blank,
//!   `0x004AA8B0`), for its `skpoints` (1 when blank, `0x00570080`).
//! - **Mana** (`0x0056BFE0`): `max(minmana × 256, (mana + lvlmana × (level − 1)) << manashift)`, in
//!   256ths.
//! - **Damage** (`0x00644D50`, `0x00644E40`, physical `0x00647C?0`/`0x00647D00`): the base column plus
//!   its five-bracket per-level progression (`0x00644B70`: levels 2–8, 9–16, 17–22, 23–28 and 29 up),
//!   shifted by `HitShift` into 256ths; the elemental part gains `EDmgSymPerCalc` percent.
//! - **Calcs** (compiled at load, `0x00611BD0`; evaluated by `0x006C0BC0`): arithmetic, comparisons,
//!   `?:`, `min`/`max`, `lvl`, `blvl`, `par1`–`par8`, `lnXY` (`parX + (lvl − 1) × parY`), `dmXY`
//!   (`parX + 110 × lvl × (parY − parX) / (100 × (lvl + 6))`) and `skill('Name'.blvl)`/`.lvl`. Other
//!   identifiers count as 0.
//!
//! Item bonuses to skill levels are not counted yet: a skill's level is what the player put into it.

use std::collections::BTreeMap;

use d2_data::skills::Skill;
use d2_data::GameData;

/// Skills every player has at level 1, in the order the engine lists them (`0x94` on a retail
/// fresh character, bnemu's capture): Attack, Throw, Kick, the four scroll and tome skills,
/// Left Hand Throw, Left Hand Swing, Unsummon.
pub const COMMON_SKILLS: [i32; 10] = [0, 2, 1, 217, 218, 219, 220, 4, 5, 3];

/// A skill's highest level when `maxlvl` is blank (`0x004AA8B0`).
pub const DEFAULT_MAX_LEVEL: i32 = 20;

/// The 30 skills of a class, in id order: the order a `.d2s` keeps their levels in.
#[must_use]
pub fn class_skills(data: &GameData, class: u8) -> Vec<i32> {
    (0..data.skills().len() as i32).filter(|&id| data.skills().get(id).is_some_and(|s| s.class == Some(class))).take(30).collect()
}

/// What a calc reads besides the skill's own row: the skill's level and the player's other skills.
pub struct CalcContext<'a> {
    /// The game rules.
    pub data: &'a GameData,
    /// The skill the calc is on.
    pub skill: &'a Skill,
    /// Its level.
    pub level: i32,
    /// The player's skills' base levels, by id.
    pub skills: &'a BTreeMap<i32, u8>,
}

impl CalcContext<'_> {
    fn param(&self, n: usize) -> i32 {
        self.skill.params.get(n.wrapping_sub(1)).copied().unwrap_or(0)
    }

    fn other(&self, name: &str) -> Option<(&Skill, i32)> {
        let id = self.data.skills().id(name)?;
        Some((self.data.skills().get(id)?, i32::from(self.skills.get(&id).copied().unwrap_or(0))))
    }
}

/// Evaluate a `Skills.txt` calc; a blank or unreadable one is `None`.
#[must_use]
pub fn eval(calc: &str, ctx: &CalcContext<'_>) -> Option<i32> {
    // A calc with a comma in it is quoted in the table.
    let calc = calc.trim();
    let calc = calc.strip_prefix('"').and_then(|c| c.strip_suffix('"')).unwrap_or(calc);
    let tokens = tokenize(calc)?;
    if tokens.is_empty() {
        return None;
    }
    let mut p = Parser { tokens, at: 0, ctx };
    let value = p.ternary()?;
    (p.at == p.tokens.len()).then_some(value)
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(i32),
    Ident(String),
    Text(String),
    Op(&'static str),
}

fn tokenize(s: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Token::Num(chars[start..i].iter().collect::<String>().parse().ok()?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Token::Ident(chars[start..i].iter().collect::<String>().to_ascii_lowercase()));
        } else if c == '\'' {
            let start = i + 1;
            i = start;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            out.push(Token::Text(chars.get(start..i)?.iter().collect()));
            i += 1;
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            let op = ["<=", ">=", "==", "!="].into_iter().find(|o| *o == two);
            if let Some(op) = op {
                out.push(Token::Op(op));
                i += 2;
            } else {
                let op = ["+", "-", "*", "/", "(", ")", "<", ">", "?", ":", ",", "."].into_iter().find(|o| o.starts_with(c))?;
                out.push(Token::Op(op));
                i += 1;
            }
        }
    }
    Some(out)
}

struct Parser<'a, 'b> {
    tokens: Vec<Token>,
    at: usize,
    ctx: &'a CalcContext<'b>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.peek() == Some(&Token::Op(match op {
            "(" => "(",
            ")" => ")",
            "?" => "?",
            ":" => ":",
            "," => ",",
            "." => ".",
            _ => return false,
        })) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn ternary(&mut self) -> Option<i32> {
        let cond = self.comparison()?;
        if self.eat("?") {
            let yes = self.ternary()?;
            if !self.eat(":") {
                return None;
            }
            let no = self.ternary()?;
            return Some(if cond != 0 { yes } else { no });
        }
        Some(cond)
    }

    fn comparison(&mut self) -> Option<i32> {
        let mut left = self.additive()?;
        while let Some(Token::Op(op @ ("<" | ">" | "<=" | ">=" | "==" | "!="))) = self.peek().cloned() {
            self.at += 1;
            let right = self.additive()?;
            left = i32::from(match op {
                "<" => left < right,
                ">" => left > right,
                "<=" => left <= right,
                ">=" => left >= right,
                "==" => left == right,
                _ => left != right,
            });
        }
        Some(left)
    }

    fn additive(&mut self) -> Option<i32> {
        let mut left = self.multiplicative()?;
        while let Some(Token::Op(op @ ("+" | "-"))) = self.peek().cloned() {
            self.at += 1;
            let right = self.multiplicative()?;
            left = if op == "+" { left.wrapping_add(right) } else { left.wrapping_sub(right) };
        }
        Some(left)
    }

    fn multiplicative(&mut self) -> Option<i32> {
        let mut left = self.unary()?;
        while let Some(Token::Op(op @ ("*" | "/"))) = self.peek().cloned() {
            self.at += 1;
            let right = self.unary()?;
            left = if op == "*" { left.wrapping_mul(right) } else if right == 0 { 0 } else { left.wrapping_div(right) };
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<i32> {
        if self.peek() == Some(&Token::Op("-")) {
            self.at += 1;
            return Some(self.unary()?.wrapping_neg());
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<i32> {
        let token = self.peek()?.clone();
        self.at += 1;
        match token {
            Token::Num(n) => Some(n),
            Token::Op("(") => {
                let v = self.ternary()?;
                self.close().then_some(v)
            }
            Token::Ident(name) => self.identifier(&name),
            _ => None,
        }
    }

    fn arguments(&mut self) -> Option<Vec<i32>> {
        if !self.eat("(") {
            return None;
        }
        let mut args = vec![self.ternary()?];
        while self.eat(",") {
            args.push(self.ternary()?);
        }
        self.close().then_some(args)
    }

    /// A closing parenthesis, or the end of the calc: the table has calcs missing their last one
    /// (Fire Wall's `EDmgSymPerCalc`), which the engine's compiler takes as closed.
    fn close(&mut self) -> bool {
        self.eat(")") || self.peek().is_none()
    }

    fn identifier(&mut self, name: &str) -> Option<i32> {
        let ctx = self.ctx;
        let pair = |prefix: &str| -> Option<(usize, usize)> {
            let digits = name.strip_prefix(prefix)?.as_bytes();
            (digits.len() == 2 && digits.iter().all(u8::is_ascii_digit)).then(|| (usize::from(digits[0] - b'0'), usize::from(digits[1] - b'0')))
        };
        if let Some((x, y)) = pair("ln") {
            return Some(ctx.param(x) + (ctx.level - 1) * ctx.param(y));
        }
        if let Some((x, y)) = pair("dm") {
            let (lo, hi, lvl) = (ctx.param(x), ctx.param(y), ctx.level);
            return Some(lo + (110 * lvl * (hi - lo)) / (100 * (lvl + 6)));
        }
        if let Some(n) = name.strip_prefix("par").and_then(|d| d.parse::<usize>().ok()) {
            return Some(ctx.param(n));
        }
        match name {
            "lvl" | "blvl" | "sklvl" => Some(ctx.level),
            "min" | "max" => {
                let args = self.arguments()?;
                let pick = if name == "min" { args.iter().min() } else { args.iter().max() };
                pick.copied()
            }
            "skill" => {
                if !self.eat("(") {
                    return None;
                }
                let Some(Token::Text(other)) = self.peek().cloned() else { return None };
                self.at += 1;
                if !self.eat(".") {
                    return None;
                }
                let Some(Token::Ident(field)) = self.peek().cloned() else { return None };
                self.at += 1;
                if !self.close() {
                    return None;
                }
                let (row, level) = ctx.other(&other).unwrap_or((ctx.skill, 0));
                Some(match field.as_str() {
                    "lvl" | "blvl" => level,
                    f => f.strip_prefix("par").and_then(|d| d.parse::<usize>().ok()).and_then(|n| row.params.get(n.wrapping_sub(1))).copied().unwrap_or(0),
                })
            }
            _ => {
                // An identifier this port does not read (a stat, a missile's column): 0, and a call's
                // arguments skipped.
                if self.peek() == Some(&Token::Op("(")) {
                    let mut depth = 0;
                    while let Some(token) = self.peek().cloned() {
                        self.at += 1;
                        match token {
                            Token::Op("(") => depth += 1,
                            Token::Op(")") => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Some(0)
            }
        }
    }
}

/// A five-bracket level progression (`0x00644B70`): nothing at level 1, then each level adds its
/// bracket's column — levels 2–8 the first, 9–16 the second, 17–22 the third, 23–28 the fourth,
/// 29 up the fifth.
#[must_use]
pub fn bracket(levels: &[i32; 5], level: i32) -> i32 {
    let l = levels;
    match level {
        ..=1 => 0,
        2..=8 => (level - 1) * l[0],
        9..=16 => (level - 8) * l[1] + l[0] * 7,
        17..=22 => (level - 16) * l[2] + l[1] * 8 + l[0] * 7,
        23..=28 => (level - 22) * l[3] + l[2] * 6 + l[1] * 8 + l[0] * 7,
        _ => (level - 28) * l[4] + (l[3] + l[2]) * 6 + l[1] * 8 + l[0] * 7,
    }
}

/// The mana a skill costs at `level`, 256ths (`0x0056BFE0`).
#[must_use]
pub fn mana_cost(skill: &Skill, level: i32) -> i32 {
    let (min, shift, base, per_level) = skill.mana;
    let cost = (base + per_level * (level - 1)).max(0) << shift.clamp(0, 16);
    cost.max(min << 8)
}

/// What a skill deals at `level`: its elemental range with synergies and its physical range, each
/// in 256ths.
#[must_use]
pub fn damage(ctx: &CalcContext<'_>) -> ((i32, i32), (i32, i32)) {
    let s = ctx.skill;
    let shift = s.hit_shift.clamp(0, 16);
    let range = |min: &(i32, [i32; 5]), max: &(i32, [i32; 5])| ((min.0 + bracket(&min.1, ctx.level)) << shift, (max.0 + bracket(&max.1, ctx.level)) << shift);
    let (mut elo, mut ehi) = range(&s.element_min, &s.element_max);
    if let Some(bonus) = eval(&s.element_synergy, ctx).filter(|&b| b != 0) {
        elo += elo * bonus / 100;
        ehi += ehi * bonus / 100;
    }
    ((elo, ehi), range(&s.min_damage, &s.max_damage))
}

/// The attributes and level a player learning a skill has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Learner {
    /// Class.
    pub class: u8,
    /// Level.
    pub level: u32,
    /// Strength, dexterity, energy, vitality — `reqstr`, `reqdex`, `reqint`, `reqvit` order.
    pub attributes: [i32; 4],
    /// Unspent skill points.
    pub points: u32,
}

/// What putting a point into skill `id` costs (`0x0054BD90` → `0x00570080`): the points, or `None`
/// when the player may not.
#[must_use]
pub fn learn_cost(data: &GameData, learner: Learner, skills: &BTreeMap<i32, u8>, id: i32) -> Option<u32> {
    let skill = data.skills().get(id)?;
    if skill.class != Some(learner.class) || !skill.in_game || learner.level < u32::try_from(skill.req_level).unwrap_or(0) {
        return None;
    }
    if skill.req_attributes.iter().zip(learner.attributes).any(|(need, has)| has < *need) {
        return None;
    }
    let has = |name: &str| data.skills().id(name).is_some_and(|other| skills.get(&other).is_some_and(|&l| l > 0));
    if !skill.req_skills.iter().all(|name| has(name)) {
        return None;
    }
    let level = i32::from(skills.get(&id).copied().unwrap_or(0));
    let max = if skill.max_level > 0 { skill.max_level } else { DEFAULT_MAX_LEVEL };
    if level >= max {
        return None;
    }
    let ctx = CalcContext { data, skill, level, skills };
    let cost = eval(&skill.skill_points, &ctx).unwrap_or(1).max(0) as u32;
    (cost <= learner.points).then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use d2_formats::excel::Table;

    fn data() -> GameData {
        let mut cs = String::from("class\tstr\tdex\tint\tvit\tstamina\thpadd\r\n");
        for name in d2_data::CLASSES {
            cs.push_str(&format!("{name}\t10\t25\t35\t10\t74\t30\r\n"));
        }
        let mut data = GameData::from_tables(&Table::parse(cs.as_bytes()), &Table::parse(b"Level\tAmazon\tSorceress\tNecromancer\tPaladin\tBarbarian\tDruid\tAssassin\r\n0\t0\t0\t0\t0\t0\t0\t0\r\n1\t500\t500\t500\t500\t500\t500\t500\r\n")).unwrap();
        let skills = Table::parse(
            b"skill\tcharclass\treqlevel\tmaxlvl\treqskill1\treqint\tInGame\tminmana\tmanashift\tmana\tlvlmana\tHitShift\tEType\tEMin\tEMinLev1\tEMinLev2\tEMinLev3\tEMinLev4\tEMinLev5\tEMax\tEMaxLev1\tEMaxLev2\tEMaxLev3\tEMaxLev4\tEMaxLev5\tEDmgSymPerCalc\tParam1\tParam2\tParam8\tcalc1\tskpoints\r\n\
              Attack\t\t1\t\t\t\t1\t\t8\t\t\t8\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\r\n\
              Fire Bolt\tsor\t1\t20\t\t\t1\t1\t7\t5\t\t7\tfire\t6\t3\t4\t8\t18\t54\t12\t3\t6\t10\t20\t56\t(skill('Fire Ball'.blvl)+skill('Meteor'.blvl))*par8\t\t\t16\t\t\r\n\
              Charged Bolt\tsor\t1\t20\t\t\t1\t1\t5\t24\t4\t7\tltng\t4\t1\t1\t2\t3\t4\t8\t1\t1\t2\t3\t4\t\t3\t1\t\tmin(24,ln12)\t\r\n\
              Fire Ball\tsor\t12\t20\tFire Bolt\t\t1\t1\t7\t10\t1\t7\tfire\t12\t13\t23\t28\t33\t38\t28\t15\t25\t30\t35\t40\t\t\t\t14\t\t\r\n\
              Warmth\tsor\t1\t20\t\t40\t1\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t2\r\n",
        );
        data.set_skills(d2_data::skills::Skills::from_table(&skills));
        data
    }

    fn ctx<'a>(data: &'a GameData, name: &str, level: i32, skills: &'a BTreeMap<i32, u8>) -> CalcContext<'a> {
        CalcContext { data, skill: data.skills().get(data.skills().id(name).unwrap()).unwrap(), level, skills }
    }

    #[test]
    fn calcs_read_levels_params_and_other_skills() {
        let data = data();
        let none = BTreeMap::new();
        let bolt = ctx(&data, "Charged Bolt", 5, &none);
        assert_eq!(eval("min(24,ln12)", &bolt), Some(7), "3 + 4 × 1");
        assert_eq!(eval("(lvl < 4) ? 0 : ((lvl-3)*par2)", &bolt), Some(2));
        assert_eq!(eval("dm12", &bolt), Some(3 + (110 * 5 * (1 - 3)) / (100 * 11)));
        assert_eq!(eval("", &bolt), None);
        assert_eq!(eval("stat('item_fastercastrate'.accr) + 2", &bolt), Some(2), "unread identifiers are 0");
        let mut learned = BTreeMap::new();
        learned.insert(data.skills().id("Fire Ball").unwrap(), 3);
        let fire = ctx(&data, "Fire Bolt", 1, &learned);
        assert_eq!(eval(&fire.skill.element_synergy, &fire), Some(3 * 16));
    }

    #[test]
    fn fire_bolt_deals_and_costs_what_the_game_shows() {
        let data = data();
        let none = BTreeMap::new();
        let at = |level| damage(&ctx(&data, "Fire Bolt", level, &none)).0;
        assert_eq!(at(1), (6 << 7, 12 << 7), "3–6");
        assert_eq!(at(2), ((6 + 3) << 7, (12 + 3) << 7));
        assert_eq!(at(10), ((6 + 7 * 3 + 2 * 4) << 7, (12 + 7 * 3 + 2 * 6) << 7));
        let skill = ctx(&data, "Fire Bolt", 1, &none).skill;
        assert_eq!(mana_cost(skill, 1), 5 << 7, "2.5 mana");
        let fire_ball = ctx(&data, "Fire Ball", 20, &none).skill;
        assert_eq!(mana_cost(fire_ball, 20), 29 << 7, "14.5 mana at level 20");
        let mut learned = BTreeMap::new();
        learned.insert(data.skills().id("Fire Ball").unwrap(), 2);
        assert_eq!(damage(&ctx(&data, "Fire Bolt", 1, &learned)).0, ((6 << 7) * 132 / 100, (12 << 7) * 132 / 100), "+16% a Fire Ball level");
        assert_eq!(bracket(&[1, 2, 3, 4, 5], 30), 7 + 16 + 18 + 24 + 10);
    }

    /// With the install's tables: every class has its 30 skills, every calc the port reads parses,
    /// every missile a skill names is in `Missiles.txt`, and the skills shown on screen deal and
    /// cost what the game shows.
    #[test]
    fn with_a_real_install_skills_read_and_add_up() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else { return };
        let data = GameData::load(dir).unwrap();
        let none = BTreeMap::new();
        for class in 0..7 {
            assert_eq!(class_skills(&data, class).len(), 30, "class {class}");
        }
        for id in 0..data.skills().len() as i32 {
            let skill = data.skills().get(id).unwrap();
            let ctx = CalcContext { data: &data, skill, level: 5, skills: &none };
            for calc in skill.calcs.iter().chain([&skill.element_synergy, &skill.skill_points]).filter(|c| !c.is_empty()) {
                assert!(eval(calc, &ctx).is_some(), "{}: {calc}", skill.name);
            }
            for missile in skill.missile.iter().chain(&skill.missile_a) {
                assert!(data.missiles().id(missile).is_some(), "{}: {missile}", skill.name);
            }
        }
        let at = |name: &str, level: i32| {
            let skill = data.skills().get(data.skills().id(name).unwrap()).unwrap();
            let ctx = CalcContext { data: &data, skill, level, skills: &none };
            let ((lo, hi), _) = damage(&ctx);
            (lo >> 8, hi >> 8, mana_cost(skill, level))
        };
        assert_eq!(at("Fire Bolt", 1), (3, 6, 640), "3–6, 2.5 mana");
        assert_eq!(at("Ice Bolt", 1), (3, 5, 768), "3–5, 3 mana");
        assert_eq!(at("Charged Bolt", 1), (2, 4, 768), "2–4, 3 mana");
        assert_eq!(at("Fire Ball", 1), (6, 14, 1280), "6–14, 5 mana");
        let bolts = data.skills().get(data.skills().id("Charged Bolt").unwrap()).unwrap();
        assert_eq!(eval(&bolts.calcs[0], &CalcContext { data: &data, skill: bolts, level: 1, skills: &none }), Some(3), "three bolts at level 1");
    }

    #[test]
    fn only_a_classs_skills_with_their_requirements_are_learned() {
        let data = data();
        let sorceress = Learner { class: 1, level: 1, attributes: [10, 25, 35, 10], points: 1 };
        let mut skills = BTreeMap::new();
        let id = |n: &str| data.skills().id(n).unwrap();
        assert_eq!(learn_cost(&data, sorceress, &skills, id("Fire Bolt")), Some(1));
        assert_eq!(learn_cost(&data, Learner { class: 4, ..sorceress }, &skills, id("Fire Bolt")), None, "a Barbarian's");
        assert_eq!(learn_cost(&data, Learner { points: 0, ..sorceress }, &skills, id("Fire Bolt")), None, "no points");
        assert_eq!(learn_cost(&data, Learner { level: 12, ..sorceress }, &skills, id("Fire Ball")), None, "needs Fire Bolt");
        skills.insert(id("Fire Bolt"), 1);
        assert_eq!(learn_cost(&data, sorceress, &skills, id("Fire Ball")), None, "needs level 12");
        assert_eq!(learn_cost(&data, Learner { level: 12, ..sorceress }, &skills, id("Fire Ball")), Some(1));
        assert_eq!(learn_cost(&data, sorceress, &skills, id("Warmth")), None, "40 energy, and two points");
        assert_eq!(learn_cost(&data, Learner { attributes: [10, 25, 40, 10], points: 2, ..sorceress }, &skills, id("Warmth")), Some(2));
        skills.insert(id("Fire Bolt"), 20);
        assert_eq!(learn_cost(&data, sorceress, &skills, id("Fire Bolt")), None, "at its highest");
        assert_eq!(class_skills(&data, 1), [id("Fire Bolt"), id("Charged Bolt"), id("Fire Ball"), id("Warmth")]);
    }
}
