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
    /// `LevelId` (0 for rows used only by mazes and sub-levels).
    pub level_id: i32,
    /// `Populate`: whether rooms spawn their populate objects.
    pub populate: bool,
    /// `SizeX`/`SizeY`, in tiles.
    pub size: (i32, i32),
    /// `Files`: how many map files the preset rotates through.
    pub file_count: i32,
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
                level_id: row.int("LevelId").unwrap_or(0) as i32,
                populate: row.int("Populate").unwrap_or(0) != 0,
                size: (row.int("SizeX").unwrap_or(0) as i32, row.int("SizeY").unwrap_or(0) as i32),
                file_count: row.int("Files").unwrap_or(0) as i32,
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

/// `MonPreset.txt` resolved against `MonStats`, `SuperUniques` and `MonPlace`, as
/// `DATATBLS_LinkerMonsterPreset` (`0x006597E0`) resolves it at load.
#[derive(Debug, Clone, Default)]
pub struct MonPresets {
    /// Per act (0..=4), in `MonPreset.txt` order: the block a DS1 monster id indexes.
    per_act: [Vec<PresetMonster>; 5],
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
        Ok(Self { per_act })
    }

    /// What DS1 monster id `ds1_id` places in `act` (0-based).
    #[must_use]
    pub fn get(&self, act: u8, ds1_id: i32) -> Option<&PresetMonster> {
        self.per_act.get(usize::from(act))?.get(usize::try_from(ds1_id).ok()?)
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
