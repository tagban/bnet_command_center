//! `d2-equipment.json`: what the equipment bytes of a Diablo II character's chat statstring
//! mean, for chat bots that draw or describe realm characters.
//!
//! A realm character's statstring ends in a 33-byte portrait whose bytes 2..13 are eleven
//! graphics values and 14..25 their tints (`bnetcc_proto::d2`). A value names a slot in a table
//! the game builds from its item tables, so the mapping is the install's, not a constant; the
//! server builds it from `diablo2.data_dir` at startup ([`d2_data::appearance`]) and writes it
//! to the BNFTP files directory, where a bot fetches it like `icons.bni` — `SID_GETFILETIME`
//! first, then the file when its copy is older. The file is only rewritten when its contents
//! change, so its time stays put across restarts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use d2_data::appearance::{self, component, Graphics, Look, BODY_ARMOR_PARTS, COMPONENTS};
use d2_data::engine::EngineData;
use d2_data::items::{code_str, ItemDef};
use d2_data::GameData;
use d2_formats::excel::Table;
use serde_json::{json, Value};
use tracing::{info, warn};

/// Format name in the file, so a bot can tell it from anything else.
pub const FORMAT: &str = "bnetcc-d2-equipment";
/// Bumped when the file's shape changes incompatibly.
pub const FORMAT_VERSION: u32 = 1;

/// What each of the portrait's eleven equipment bytes is.
const SLOT_NAMES: [(&str, &str); 11] = [
    ("head", "helm"),
    ("torso", "body armour weight"),
    ("legs", "body armour weight"),
    ("right_arm", "body armour weight"),
    ("left_arm", "body armour weight"),
    ("right_hand", "weapon"),
    ("left_hand", "bow, crossbow, or a second one-handed weapon"),
    ("shield", "shield"),
    ("right_shoulder", "body armour weight"),
    ("left_shoulder", "body armour weight"),
    ("special", "Necromancer shrunken head"),
];
/// Body armour weights by value 1..=3.
const WEIGHTS: [&str; 3] = ["light", "medium", "heavy"];
/// The portrait's layout: equipment, class, tints, level.
const EQUIPMENT_OFFSET: usize = 2;
const TINT_OFFSET: usize = 14;

/// Build the file's JSON from the loaded rules and `Game.exe`'s tables.
#[must_use]
pub fn build(data: &GameData, engine: &EngineData) -> Value {
    let items = data.items();
    let graphics = Graphics::build(items, &engine.reserved_graphics);
    let strings = data.strings("eng").unwrap_or_default();
    let item_json = |item: &ItemDef| {
        let name = strings.get(&item.name_key).filter(|s| !s.is_empty()).unwrap_or(&item.name);
        json!({ "code": code_str(&item.code), "name": name })
    };

    // Per slot: value -> items drawn with it there.
    let mut slots: Vec<BTreeMap<u8, Vec<Value>>> = vec![BTreeMap::new(); SLOT_NAMES.len()];
    let mut body_armor: BTreeMap<[u8; 6], Vec<Value>> = BTreeMap::new();
    let mut not_drawn = Vec::new();
    for item in items.iter() {
        let mut put = |slot: usize, value: u8| {
            let list = slots[slot].entry(value).or_default();
            let entry = item_json(item);
            if !list.contains(&entry) {
                list.push(entry);
            }
        };
        match appearance::look(items, &graphics, data.armor_types(), item) {
            None => {}
            Some(Look::Nothing) => not_drawn.push(item_json(item)),
            Some(Look::BodyArmor(parts)) => {
                for (&(slot, _), value) in BODY_ARMOR_PARTS.iter().zip(parts) {
                    slots[slot].entry(value).or_default();
                }
                body_armor.entry(parts).or_default().push(item_json(item));
            }
            Some(Look::Part { component: slot, value, either_hand, crossbow }) => {
                if slot < SLOT_NAMES.len() {
                    put(slot, value);
                }
                if (either_hand && slot == component::RIGHT_HAND) || crossbow {
                    put(component::LEFT_HAND, value);
                }
            }
        }
    }

    let slots: Vec<Value> = slots
        .into_iter()
        .enumerate()
        .map(|(i, values)| {
            let (name, holds) = SLOT_NAMES[i];
            let values: Vec<Value> = values
                .into_iter()
                .map(|(value, items)| {
                    let code = graphics.code(value).map(|c| code_str(&c)).unwrap_or_default();
                    match usize::from(value) {
                        v @ 1..=3 if items.is_empty() => json!({ "value": value, "code": code, "weight": WEIGHTS[v - 1] }),
                        _ => json!({ "value": value, "code": code, "items": items }),
                    }
                })
                .collect();
            json!({
                "index": i,
                "name": name,
                "holds": holds,
                "component": COMPONENTS[i],
                "offset": EQUIPMENT_OFFSET + i,
                "tint_offset": TINT_OFFSET + i,
                "values": values,
            })
        })
        .collect();
    let body_armor: Vec<Value> = body_armor.into_iter().map(|(parts, items)| json!({ "parts": parts, "items": items })).collect();
    let table: Vec<Value> = graphics.iter().map(|(value, code)| json!({ "value": value, "code": code_str(&code) })).collect();
    let colors: Vec<String> = data
        .read_file("data\\global\\excel\\colors.txt")
        .ok()
        .flatten()
        .map(|bytes| Table::parse(&bytes).rows().filter_map(|r| r.get("Transform Color").map(str::to_string)).collect())
        .unwrap_or_default();

    json!({
        "format": FORMAT,
        "version": FORMAT_VERSION,
        "game": "Diablo II 1.14d",
        "portrait": {
            "about": "A realm character's chat statstring is <product><realm>,<character>,<portrait>. \
                      The portrait is 33 bytes; byte <offset> of each slot below is its graphics value \
                      and byte <tint_offset> its tint. 255 means nothing.",
            "length": bnetcc_proto::d2::PORTRAIT_LEN,
            "class_offset": 13,
            "level_offset": 25,
            "none": 255,
        },
        "slots": slots,
        "body_armor": {
            "about": "Body armour sets six slots at once: parts are the values of torso, legs, right_arm, \
                      left_arm, right_shoulder and left_shoulder, in that order (1 light, 2 medium, 3 heavy). \
                      Several armours share a set.",
            "sets": body_armor,
        },
        "not_drawn": {
            "about": "Worn, but the character is drawn without them: the slot stays 255.",
            "items": not_drawn,
        },
        "tints": {
            "about": "A tint byte is transform * 32 + colour + 1, kept to a byte; 255 is untinted. \
                      colour indexes colors.",
            "colors": colors,
        },
        "graphics": table,
    })
}

/// Write `bytes` to `path` unless it already holds them. `Ok(true)` when written.
///
/// # Errors
///
/// The file system's.
pub fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let partial = path.with_file_name(format!("{name}.partial"));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)?;
    Ok(true)
}

/// Build the file from the install in `data_dir` and write it to `path` if it changed.
/// `Ok(true)` when written. Blocking: it reads the MPQs.
///
/// # Errors
///
/// What failed to load or write, as text.
pub fn write(data_dir: &str, path: &Path) -> Result<bool, String> {
    let (data, engine) = load_install(data_dir)?;
    write_if_changed(path, &encode(&data, &engine)).map_err(|e| format!("{}: {e}", path.display()))
}

/// The file's bytes: [`build`], pretty-printed.
#[must_use]
pub fn encode(data: &GameData, engine: &EngineData) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&build(data, engine)).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Load the rules and `Game.exe`'s tables from the install in `data_dir`. Blocking.
///
/// # Errors
///
/// What failed to load, as text.
pub fn load_install(data_dir: &str) -> Result<(GameData, EngineData), String> {
    if data_dir.trim().is_empty() {
        return Err("diablo2.data_dir is not set".into());
    }
    let exe = std::fs::read(Path::new(data_dir).join("Game.exe")).map_err(|e| format!("Game.exe: {e}"))?;
    let engine = EngineData::from_game_exe(&exe).map_err(|e| e.to_string())?;
    let data = GameData::load(data_dir).map_err(|e| e.to_string())?;
    Ok((data, engine))
}

/// Build the bot files from `data_dir` and write them into `files_dir` — the equipment map as
/// `equipment` and the character pack (`crate::d2_characters`) as `characters`; an empty name
/// skips that file. Off the async runtime; failures are logged, never fatal.
pub async fn publish(data_dir: String, files_dir: PathBuf, equipment: String, characters: String) {
    let plain = |name: &str, setting: &str| -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let ok = bnetcc_proto::bnftp::sanitize_filename(name.as_bytes()).map(str::to_string);
        if ok.is_none() {
            warn!(name = %name, setting, "not a plain file name; not written");
        }
        ok
    };
    let equipment = plain(&equipment, "diablo2.equipment_file");
    let characters = plain(&characters, "diablo2.character_pack");
    if equipment.is_none() && characters.is_none() {
        return;
    }
    /// A file written: where, what, and whether it changed.
    type Written = (PathBuf, &'static str, Result<bool, String>);
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<Written>, String> {
        let (data, engine) = load_install(&data_dir)?;
        let mut done = Vec::new();
        if let Some(name) = equipment {
            let path = files_dir.join(name);
            let written = write_if_changed(&path, &encode(&data, &engine)).map_err(|e| e.to_string());
            done.push((path, "Diablo II equipment map", written));
        }
        if let Some(name) = characters {
            let path = files_dir.join(name);
            let written = crate::d2_characters::build_zip(&data_dir, &data, &engine)
                .and_then(|zip| write_if_changed(&path, &zip).map_err(|e| e.to_string()));
            done.push((path, "Diablo II character pack", written));
        }
        Ok(done)
    })
    .await;
    match result {
        Ok(Ok(done)) => {
            for (path, what, written) in done {
                match written {
                    Ok(true) => info!(path = %path.display(), "wrote the {what} for bots"),
                    Ok(false) => info!(path = %path.display(), "{what} is up to date"),
                    Err(e) => warn!(path = %path.display(), error = %e, "{what} not written"),
                }
            }
        }
        Ok(Err(e)) => warn!(error = %e, "Diablo II bot files not written"),
        Err(e) => warn!(error = %e, "Diablo II bot files task failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_file_is_not_rewritten() {
        let dir = std::env::temp_dir().join(format!("bnetccd-equipment-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d2-equipment.json");
        assert!(write_if_changed(&path, b"{}\n").unwrap());
        assert!(!write_if_changed(&path, b"{}\n").unwrap());
        assert!(write_if_changed(&path, b"{\"a\":1}\n").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With the operator's install (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`): the map names
    /// the values a real character's portrait carries.
    #[test]
    fn with_a_real_install_the_map_names_worn_items() {
        let (Ok(dir), Ok(exe)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let data = GameData::load(dir).unwrap();
        let engine = EngineData::from_game_exe(&std::fs::read(exe).unwrap()).unwrap();
        let map = build(&data, &engine);
        assert_eq!(map["format"], FORMAT);
        let slots = map["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 11);
        let value = |slot: usize, v: u64| slots[slot]["values"].as_array().unwrap().iter().find(|e| e["value"] == v).cloned();
        let names = |slot: usize, v: u64| -> Vec<String> {
            value(slot, v).map_or_else(Vec::new, |e| {
                e["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_string()).collect()
            })
        };
        assert!(names(component::HEAD, 0x39).contains(&"Shako".to_string()), "{:?}", names(0, 0x39));
        assert!(names(component::SHIELD, 0x51).contains(&"Monarch".to_string()));
        assert!(names(component::RIGHT_HAND, 0x33).contains(&"Eldritch Orb".to_string()));
        assert!(names(component::LEFT_HAND, 0x33).contains(&"Eldritch Orb".to_string()), "a second one-hander");
        assert!(names(component::LEFT_HAND, 0x29).contains(&"Short Bow".to_string()));
        assert_eq!(value(component::TORSO, 1).unwrap()["weight"], "light");
        let sets = map["body_armor"]["sets"].as_array().unwrap();
        assert!(sets.iter().any(|s| s["parts"] == json!([1, 1, 1, 1, 2, 2]) && s["items"].to_string().contains("Dusk Shroud")));
        assert!(map["not_drawn"]["items"].to_string().contains("Diadem"));
        assert_eq!(map["tints"]["colors"].as_array().unwrap().len(), 21);
    }
}
