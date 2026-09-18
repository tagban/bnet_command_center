//! What the map files' preset units mean: `LvlPrest.txt` (which DS1 a preset level uses),
//! `MonPreset.txt` with `MonStats.txt` / `SuperUniques.txt` / `MonPlace.txt` (what a preset
//! monster id places), and `objects.txt` names.

use std::collections::HashMap;

use d2_formats::excel::Table;

use crate::Error;

/// One `LvlPrest.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvlPrest {
    /// `Def`.
    pub def: i32,
    /// `Name`, e.g. `Act 1 - DOE Entrance`.
    pub name: String,
    /// `LevelId` (0 for rows used only by mazes and sub-levels).
    pub level_id: i32,
    /// `Populate`: whether rooms spawn their populate objects.
    pub populate: bool,
    /// `SizeX`/`SizeY`, in tiles.
    pub size: (i32, i32),
    /// `Files`: how many map files the preset rotates through.
    pub file_count: i32,
    /// `Scan`: the map is read for warps when the piece is built.
    pub scan: bool,
    /// `Pops`: pop-out regions (roofs) the map marks.
    pub pops: i32,
    /// `Dt1Mask`: which of the level type's tile files (`LvlTypes.txt` columns) its rooms load.
    pub dt1_mask: u32,
    /// `FillBlanks`: an empty floor cell inside a room gets the blank tile.
    pub fill_blanks: bool,
    /// `KillEdge`: rooms leave out the map's far edge.
    pub kill_edge: bool,
    /// `Outdoors`.
    pub outdoors: bool,
    /// `File1`..`File6` as written, blanks included, so a file index points at its column.
    pub file_slots: Vec<String>,
    /// `File1`..`File6`, relative to `data\global\tiles`, blanks and `0` dropped.
    pub files: Vec<String>,
}

/// `LvlPrest.txt`.
#[derive(Debug, Clone, Default)]
pub struct LvlPrests {
    rows: Vec<LvlPrest>,
}

impl LvlPrests {
    /// Parse the table.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if `Def`, `LevelId` or `File1` is missing.
    pub fn from_table(t: &Table) -> Result<Self, Error> {
        for column in ["Def", "LevelId", "File1"] {
            if t.column(column).is_none() {
                return Err(Error::BadTable { table: "lvlprest.txt", problem: format!("no {column} column") });
            }
        }
        let rows = t
            .rows()
            .map(|row| LvlPrest {
                def: row.int("Def").unwrap_or(0) as i32,
                name: row.get("Name").unwrap_or_default().to_string(),
                level_id: row.int("LevelId").unwrap_or(0) as i32,
                populate: row.int("Populate").unwrap_or(0) != 0,
                size: (row.int("SizeX").unwrap_or(0) as i32, row.int("SizeY").unwrap_or(0) as i32),
                file_count: row.int("Files").unwrap_or(0) as i32,
                scan: row.int("Scan").unwrap_or(0) != 0,
                pops: row.int("Pops").unwrap_or(0) as i32,
                dt1_mask: row.int("Dt1Mask").unwrap_or(0) as u32,
                fill_blanks: row.int("FillBlanks").unwrap_or(0) != 0,
                kill_edge: row.int("KillEdge").unwrap_or(0) != 0,
                outdoors: row.int("Outdoors").unwrap_or(0) != 0,
                file_slots: (1..=6).map(|i| row.get(&format!("File{i}")).unwrap_or_default().to_string()).collect(),
                files: (1..=6)
                    .filter_map(|i| row.get(&format!("File{i}")))
                    .filter(|f| *f != "0")
                    .map(str::to_string)
                    .collect(),
            })
            .collect();
        Ok(Self { rows })
    }

    /// The preset row for a level.
    #[must_use]
    pub fn for_level(&self, level_id: i32) -> Option<&LvlPrest> {
        self.rows.iter().find(|r| r.level_id == level_id && level_id != 0)
    }

    /// The row with `Def` = `def` (`TXT_LvlPrest_GetLine`).
    #[must_use]
    pub fn by_def(&self, def: i32) -> Option<&LvlPrest> {
        self.rows.iter().find(|r| r.def == def)
    }
}

impl LvlPrest {
    /// The map file a piece built with file index `pick` reads: that column, or the first
    /// non-empty one when it is empty or `0` (`DRLGPRESET_AllocDrlgFile`'s caller).
    #[must_use]
    pub fn file_for(&self, pick: i32) -> Option<&str> {
        let usable = |f: &&String| !f.is_empty() && f.as_str() != "0";
        usize::try_from(pick)
            .ok()
            .and_then(|i| self.file_slots.get(i))
            .filter(usable)
            .or_else(|| self.file_slots.iter().find(usable))
            .map(String::as_str)
    }
}

/// What a preset monster id places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetMonster {
    /// A `MonStats.txt` class (`hcIdx`) — an NPC or a monster.
    Class {
        /// The class id.
        class: i32,
        /// The `MonPreset.txt` `Place` name.
        name: String,
    },
    /// A `SuperUniques.txt` row.
    SuperUnique {
        /// Its row.
        index: i32,
        /// The `Place` name.
        name: String,
    },
    /// A `MonPlace.txt` placement rule (a spawn group, not a monster).
    Placement {
        /// Its row.
        index: i32,
        /// The `Place` name.
        name: String,
    },
    /// A name no table knows.
    Unknown(String),
}

/// A `SuperUniques.txt` row: one named monster a map places by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperUnique {
    /// `Superunique`: the name a `MonPreset.txt` `Place` matches.
    pub key: String,
    /// `Class`: the `MonStats.txt` row it is made from, as a class id.
    pub class: i32,
}

/// `MonPreset.txt` resolved against `MonStats`, `SuperUniques` and `MonPlace`, as
/// `DATATBLS_LinkerMonsterPreset` (`0x006597E0`) resolves it at load.
#[derive(Debug, Clone, Default)]
pub struct MonPresets {
    /// Per act (0..=4), in `MonPreset.txt` order: the block a DS1 monster id indexes.
    per_act: [Vec<PresetMonster>; 5],
    /// `MonStats.txt` rows, where the engine's class ids for super uniques and placements start.
    monstats_rows: i32,
    /// `SuperUniques.txt` in row order, so a [`PresetMonster::SuperUnique`] index resolves.
    super_uniques: Vec<SuperUnique>,
}

impl MonPresets {
    /// Resolve `MonPreset.txt`.
    ///
    /// # Errors
    ///
    /// [`Error::BadTable`] if a needed column is missing.
    pub fn from_tables(monpreset: &Table, monstats: &Table, superuniques: &Table, monplace: &Table) -> Result<Self, Error> {
        let need = |t: &Table, table: &'static str, column: &str| {
            t.column(column).map(|_| ()).ok_or_else(|| Error::BadTable { table, problem: format!("no {column} column") })
        };
        need(monpreset, "monpreset.txt", "Place")?;
        need(monstats, "monstats.txt", "Id")?;
        need(monstats, "monstats.txt", "hcIdx")?;
        need(superuniques, "superuniques.txt", "Superunique")?;
        need(monplace, "monplace.txt", "code")?;

        let index = |t: &Table, column: &str| -> HashMap<String, i32> {
            let mut m = HashMap::new();
            for (i, row) in t.rows().enumerate() {
                if let Some(name) = row.get(column) {
                    m.entry(name.to_ascii_lowercase()).or_insert(i as i32);
                }
            }
            m
        };
        let classes: HashMap<String, i32> = monstats
            .rows()
            .filter_map(|r| Some((r.get("Id")?.to_ascii_lowercase(), r.int("hcIdx")? as i32)))
            .collect();
        let supers = index(superuniques, "Superunique");
        let places = index(monplace, "code");

        let mut per_act: [Vec<PresetMonster>; 5] = Default::default();
        for row in monpreset.rows() {
            let (Some(act), Some(place)) = (row.int("Act"), row.get("Place")) else { continue };
            let Some(block) = usize::try_from(act - 1).ok().and_then(|a| per_act.get_mut(a)) else { continue };
            let key = place.to_ascii_lowercase();
            let name = place.to_string();
            block.push(if let Some(&index) = supers.get(&key) {
                PresetMonster::SuperUnique { index, name }
            } else if let Some(&class) = classes.get(&key) {
                PresetMonster::Class { class, name }
            } else if let Some(&index) = places.get(&key) {
                PresetMonster::Placement { index, name }
            } else {
                PresetMonster::Unknown(name)
            });
        }
        let super_uniques = superuniques
            .rows()
            .map(|r| SuperUnique {
                key: r.get("Superunique").unwrap_or_default().to_string(),
                class: r.get("Class").and_then(|c| classes.get(&c.to_ascii_lowercase()).copied()).unwrap_or(-1),
            })
            .collect();
        Ok(Self { per_act, monstats_rows: monstats.rows().count() as i32, super_uniques })
    }

    /// What DS1 monster id `ds1_id` places in `act` (0-based).
    #[must_use]
    pub fn get(&self, act: u8, ds1_id: i32) -> Option<&PresetMonster> {
        self.per_act.get(usize::from(act))?.get(usize::try_from(ds1_id).ok()?)
    }

    /// `SuperUniques.txt` rows, where placement rows' class ids start.
    #[must_use]
    pub fn super_unique_rows(&self) -> i32 {
        i32::try_from(self.super_uniques.len()).unwrap_or(0)
    }

    /// What a `MonPlace.txt` row places (`0x0054E600`'s switch on the row). The engine compiles
    /// this in: the table has only a `code` column, and the row's position is all it contributes.
    /// `None` is a row that spawns nothing, or one we have not ported — the rows that roll a class
    /// out of the level's own roster, and the later-act rows that pick by level.
    #[must_use]
    pub fn placement_class(row: i32) -> Option<i32> {
        match row {
            4 => Some(266),
            // Blood Raven, whom the Sisters' Burial Grounds is about (`0x0054E808`).
            5 => Some(267),
            8 => Some(284),
            _ => None,
        }
    }

    /// The class id the engine gives a DS1 monster unit (`ParsePresetsOfDrlgFile`): the
    /// `MonStats` row, then a super unique's row past those, then a placement's row past both.
    #[must_use]
    pub fn engine_class(&self, act: i32, ds1_id: i32) -> i32 {
        let Some(block) = usize::try_from(act).ok().and_then(|a| self.per_act.get(a)) else { return ds1_id };
        match usize::try_from(ds1_id).ok().and_then(|i| block.get(i)) {
            None => ds1_id,
            Some(PresetMonster::Class { class, .. }) => *class,
            Some(PresetMonster::SuperUnique { index, .. }) => index + self.monstats_rows,
            Some(PresetMonster::Placement { index, .. }) => index + self.monstats_rows + self.super_unique_rows(),
            // An unmatched name links as placement row 0, which spawns nothing.
            Some(PresetMonster::Unknown(_)) => self.monstats_rows + self.super_unique_rows(),
        }
    }

    /// The `SuperUniques.txt` row a [`PresetMonster::SuperUnique`] names.
    #[must_use]
    pub fn super_unique(&self, index: i32) -> Option<&SuperUnique> {
        usize::try_from(index).ok().and_then(|i| self.super_uniques.get(i))
    }

    /// `MonStats.txt` rows.
    #[must_use]
    pub fn monstats_rows(&self) -> i32 {
        self.monstats_rows
    }
}

/// `objects.txt`, by class id (row order).
#[derive(Debug, Clone, Default)]
pub struct Objects {
    rows: Vec<ObjectClass>,
}

/// The `objects.txt` columns read so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectClass {
    /// `description - not loaded`, else `Name`: for logs.
    pub name: String,
    /// `InitFn`: the engine's per-class spawn routine (table `0x00731BC0`, record `+0x1B1`).
    pub init_fn: u8,
    /// `OperateFn`: what operating it does (table `0x00732D18`, record `+0x1B3`).
    pub operate_fn: u8,
    /// `PreOperate`: spawn some already operated (record `+0x13D`).
    pub pre_operate: bool,
    /// `SubClass` bits (record `+0x167`), e.g. 0x04 portals (`SendUnitToClient` adds `0x60`),
    /// 0x40 waypoints (the town spawn search looks for one).
    pub sub_class: u8,
    /// `Parm0`: the class's `InitFn` argument (record `+0x178`), e.g. a shrine's kind.
    pub parm0: i32,
    /// `Parm2` (record `+0x180`), e.g. a well's refill.
    pub parm2: i32,
}

impl Objects {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|r| ObjectClass {
                name: r.get("description - not loaded").or_else(|| r.get("Name")).unwrap_or_default().to_string(),
                init_fn: r.int("InitFn").and_then(|v| u8::try_from(v).ok()).unwrap_or(0),
                operate_fn: r.int("OperateFn").and_then(|v| u8::try_from(v).ok()).unwrap_or(0),
                pre_operate: r.int("PreOperate").unwrap_or(0) != 0,
                sub_class: r.int("SubClass").and_then(|v| u8::try_from(v).ok()).unwrap_or(0),
                parm0: r.int("Parm0").unwrap_or(0) as i32,
                parm2: r.int("Parm2").unwrap_or(0) as i32,
            })
            .collect();
        Self { rows }
    }

    /// An object class.
    #[must_use]
    pub fn get(&self, class: i32) -> Option<&ObjectClass> {
        self.rows.get(usize::try_from(class).ok()?)
    }

    /// An object class's description, for logs.
    #[must_use]
    pub fn name(&self, class: i32) -> Option<&str> {
        self.get(class).map(|c| c.name.as_str())
    }
}

/// One `Shrines.txt` row: what picking a shrine type reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shrine {
    /// `effectclass`: the kind the random pick groups shrines by (1 magic, 2 health, 3 mana,
    /// 4 boost).
    pub effect_class: i32,
    /// `LevelMin`: the lowest level id the shrine may appear in.
    pub level_min: i32,
}

/// `Shrines.txt`, by row (the shrine type).
#[derive(Debug, Clone, Default)]
pub struct Shrines {
    rows: Vec<Shrine>,
}

impl Shrines {
    /// Parse the table.
    #[must_use]
    pub fn from_table(t: &Table) -> Self {
        let rows = t
            .rows()
            .map(|r| Shrine { effect_class: r.int("effectclass").unwrap_or(0) as i32, level_min: r.int("LevelMin").unwrap_or(0) as i32 })
            .collect();
        Self { rows }
    }

    /// Shrine types.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A shrine type.
    #[must_use]
    pub fn get(&self, shrine: usize) -> Option<&Shrine> {
        self.rows.get(shrine)
    }

    /// The shrine types of an effect class, in table order (`aShrinesRng`, built when the
    /// object control is made, `0x00546C60`).
    #[must_use]
    pub fn of_class(&self, effect_class: i32) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| self.rows[i].effect_class == effect_class).collect()
    }
}
