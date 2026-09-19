//! `Gems.txt`: what a gem or rune in a socket lends the item holding it — one set of mods for a
//! weapon, one for a helm or body armour, one for a shield, chosen by the holder's
//! `gemapplytype` (`0x0055C2C0`).

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::affixes::{mods, Mod};
use crate::items::{code, Code};

/// One gem's or rune's row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gem {
    /// `weaponMod1`–`3`: in a weapon (`gemapplytype` 0).
    pub weapon: Vec<Mod>,
    /// `helmMod1`–`3`: in a helm or body armour (1).
    pub helm: Vec<Mod>,
    /// `shieldMod1`–`3`: in a shield (2).
    pub shield: Vec<Mod>,
}

impl Gem {
    /// The mods it lends an item of `gemapplytype` `apply`; none for another value.
    #[must_use]
    pub fn mods_for(&self, apply: i32) -> &[Mod] {
        match apply {
            0 => &self.weapon,
            1 => &self.helm,
            2 => &self.shield,
            _ => &[],
        }
    }
}

/// `Gems.txt` by item code.
#[derive(Debug, Clone, Default)]
pub struct Gems {
    by_code: HashMap<Code, Gem>,
}

impl Gems {
    /// Read `Gems.txt`. A row's mods stop at its first blank code, as the engine's loop does.
    #[must_use]
    pub fn from_table(table: &Table) -> Self {
        let names = |kind: &str| -> Vec<(String, String, String, String)> {
            (1..=3).map(|i| (format!("{kind}Mod{i}Code"), format!("{kind}Mod{i}Param"), format!("{kind}Mod{i}Min"), format!("{kind}Mod{i}Max"))).collect()
        };
        let (weapon, helm, shield) = (names("weapon"), names("helm"), names("shield"));
        let by_code = table
            .rows()
            .filter_map(|row| {
                let c = row.get("code").filter(|c| !c.is_empty())?;
                Some((code(c), Gem { weapon: mods(&row, &weapon), helm: mods(&row, &helm), shield: mods(&row, &shield) }))
            })
            .collect();
        Self { by_code }
    }

    /// The row for an item code.
    #[must_use]
    pub fn get(&self, code: &Code) -> Option<&Gem> {
        self.by_code.get(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gem_lends_by_where_it_sits() {
        let table = Table::parse(
            b"name\tcode\tweaponMod1Code\tweaponMod1Param\tweaponMod1Min\tweaponMod1Max\tweaponMod2Code\thelmMod1Code\thelmMod1Min\thelmMod1Max\tshieldMod1Code\tshieldMod1Min\tshieldMod1Max\r\n\
              Chipped Ruby\tgcr\tdmg-fire\t\t3\t4\t\thp\t10\t10\tres-fire\t12\t12\r\n",
        );
        let gems = Gems::from_table(&table);
        let ruby = gems.get(&code("gcr")).unwrap();
        assert_eq!(ruby.mods_for(0), [Mod { code: "dmg-fire".into(), param: 0, min: 3, max: 4 }]);
        assert_eq!(ruby.mods_for(1)[0].code, "hp");
        assert_eq!(ruby.mods_for(2)[0].min, 12);
        assert!(ruby.mods_for(3).is_empty());
    }
}
