//! `d2-characters.zip`: the character-select screen's animations as layers, for chat bots that
//! draw realm characters themselves.
//!
//! Built from `diablo2.data_dir` at startup ([`d2_data::character::CharacterArt::pack`]) and
//! written beside `d2-equipment.json` in the BNFTP files directory. The bundle holds the palette,
//! the tint maps, one GIF per body-part graphic facing the viewer as the character screen draws
//! it (the game's palette, index 0 transparent), and `manifest.json`: where each part sits against the base point, the draw order
//! of every frame, the loop's timing, and the rules that take a portrait's bytes to the parts.
//! `docs/D2-CHARACTER-PACK.md` is the format for bot authors.

use std::path::Path;

use d2_data::character::{CharacterArt, Pack, TICK_MS};
use d2_data::engine::EngineData;
use d2_data::items::code_str;
use d2_data::GameData;
use d2_formats::zip;
use serde_json::{json, Map, Value};

/// Format name in the manifest.
pub const FORMAT: &str = "bnetcc-d2-characters";
/// Bumped when the bundle's shape changes incompatibly.
pub const FORMAT_VERSION: u32 = 1;

/// How a client turns a portrait into layers, in order.
const RULES: [&str; 8] = [
    "A realm user's statstring is <product><realm>,<character>,<portrait>; the portrait is 33 bytes. class = portrait[13] - 1 (0 Amazon .. 6 Assassin); status = portrait[26]; g[c] = portrait[2 + c] and t[c] = portrait[14 + c] for components c = 0..10 (HD TR LG RA LA RH LH SH S1 S2 S3); other components are 255.",
    "Stance: hardcore (status & 4) and dead (status & 8): not in this pack, draw nothing. Hardcore: mode NU. Otherwise: mode TN.",
    "Hands: rh = g[5], lh = g[6], sh = g[7]; both = rh != 255 and lh != 255. For a hand value v with s = slots[v]: its class is s.two_handed if both; for the right hand also if lh == 255 and sh == 255 and s.two_handed != s.hand; otherwise s.hand. Then if (class is 13 or 14 and the character is not an Assassin) or s.armor, class = s.reserved_hand; then if class is still 13 or 14 and not an Assassin, class = 0. A missing hand is 0.",
    "Weapon class w = hand_pairs[right][left]. If w is 0 the screen draws its fallback figure instead: draw nothing.",
    "Animation: animations[classes[class] + mode + weapon_classes[w]], upper case, e.g. BATNHTH.",
    "Parts: for each [component c, layer weapon class L] in the animation's layers: v = g[c]; code = 'lit' if v is 0 or 255, or c is 0 and not slots[v].helm, or slots[v] has no code; otherwise slots[v].code. The part is parts[classes[class] + components[c] + code + mode + L], upper case; if it is not in the pack, the component is not drawn. (Armour values above 3 in components 1..4 come only from very old saves.)",
    "Tints: x = t[c]; none if x is 0 or 255. s = x - 1; colour = s & 31; transform = s >> 5, and 0 means 8. None if transform is 3 or 4 or colour > 20. Otherwise map every non-zero pixel p to tints[(transform - 1) * 21 + colour][p].",
    "Drawing: play the animation's sequence, [frame f, ticks] pairs of tick_ms each, and loop. For each f draw the components in order[f], back to front: the part's GIF frame min(f, frames - 1) at (left, top) from the base point; index 0 is transparent. Colours come from the palette.",
];

/// Build the bundle from a loaded install.
///
/// # Errors
///
/// What failed to load or draw, as text.
pub fn build_zip(data_dir: &str, data: &GameData, engine: &EngineData) -> Result<Vec<u8>, String> {
    let art = CharacterArt::load(data_dir, data, engine).map_err(|e| e.to_string())?;
    let pack = art.pack().map_err(|e| e.to_string())?;
    Ok(bundle(&pack))
}

/// Write the bundle built from the install in `data_dir` to `path` if it changed. `Ok(true)`
/// when written. Blocking.
///
/// # Errors
///
/// What failed, as text.
pub fn write(data_dir: &str, path: &Path) -> Result<bool, String> {
    let (data, engine) = crate::d2_equipment::load_install(data_dir)?;
    let zip = build_zip(data_dir, &data, &engine)?;
    crate::d2_equipment::write_if_changed(path, &zip).map_err(|e| format!("{}: {e}", path.display()))
}

/// The manifest for a pack.
#[must_use]
pub fn manifest(pack: &Pack) -> Value {
    let slots: Vec<Value> = pack
        .slots
        .iter()
        .map(|s| {
            json!({
                "value": s.value,
                "code": s.code.map(|c| code_str(&c)),
                "hand": s.hand,
                "two_handed": s.two_handed,
                "reserved_hand": s.reserved_hand,
                "armor": s.armor,
                "helm": s.helm,
            })
        })
        .collect();
    let mut animations = Map::new();
    for a in &pack.animations {
        let layers: Vec<Value> = a.layers.iter().map(|(c, class)| json!([c, class])).collect();
        let sequence: Vec<Value> = a.sequence.iter().map(|&(f, ticks)| json!([f, ticks])).collect();
        animations.insert(
            a.name.clone(),
            json!({
                "class": a.class,
                "mode": a.mode,
                "weapon_class": a.weapon_class,
                "frames": a.frames,
                "speed": a.speed,
                "layers": layers,
                "sequence": sequence,
                "order": a.order,
            }),
        );
    }
    let mut parts = Map::new();
    for p in &pack.parts {
        parts.insert(
            p.name.clone(),
            json!({
                "file": part_file(&p.name),
                "left": p.left,
                "top": p.top,
                "width": p.width,
                "height": p.height,
                "frames": p.frames,
            }),
        );
    }
    json!({
        "format": FORMAT,
        "version": FORMAT_VERSION,
        "game": "Diablo II 1.14d",
        "about": "The character-select screen's animations as layers, facing the viewer as that screen draws them, from the server's own install. See rules.",
        "rules": RULES,
        "direction": 0,
        "tick_ms": TICK_MS,
        "palette": { "file": "palette.bin", "about": "256 RGB triples; GIFs carry the same table." },
        "tints": {
            "file": "tints.bin",
            "transforms": 8,
            "colours": 21,
            "about": "Maps from palette index to palette index: map (transform t, colour c) is the 256 bytes at ((t - 1) * 21 + c) * 256.",
        },
        "classes": pack.classes,
        "modes": pack.modes,
        "components": pack.components,
        "weapon_classes": pack.weapon_classes,
        "hand_pairs": pack.hand_pairs,
        "slots": slots,
        "animations": animations,
        "parts": parts,
    })
}

fn part_file(name: &str) -> String {
    format!("parts/{}/{name}.gif", &name[..2.min(name.len())])
}

/// The ZIP: `manifest.json` first, then the palette, the tint maps and the parts by name.
#[must_use]
pub fn bundle(pack: &Pack) -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(pack.parts.len() + 3);
    let mut manifest = serde_json::to_vec(&manifest(pack)).unwrap_or_default();
    manifest.push(b'\n');
    entries.push(("manifest.json".into(), manifest));
    entries.push(("palette.bin".into(), pack.palette.iter().flatten().copied().collect()));
    entries.push(("tints.bin".into(), pack.tints.iter().flatten().copied().collect()));
    let mut parts: Vec<_> = pack.parts.iter().collect();
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    entries.extend(parts.into_iter().map(|p| (part_file(&p.name), p.gif.clone())));
    zip::store(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the operator's install (`BNETCC_D2_DATA_DIR`, `BNETCC_D2_GAME_EXE`): the bundle
    /// unpacks with the system's `unzip`, and the manifest names files that are in it.
    #[test]
    fn with_a_real_install_the_bundle_is_a_zip_with_its_manifest() {
        let (Ok(dir), Ok(_)) = (std::env::var("BNETCC_D2_DATA_DIR"), std::env::var("BNETCC_D2_GAME_EXE")) else {
            return;
        };
        let (data, engine) = crate::d2_equipment::load_install(&dir).unwrap();
        let art = CharacterArt::load(&dir, &data, &engine).unwrap();
        let pack = art.pack().unwrap();
        let m = manifest(&pack);
        assert_eq!((m["format"].as_str(), m["version"].as_u64()), (Some(FORMAT), Some(1)));
        let bat = &m["animations"]["BATNHTH"];
        assert_eq!(bat["frames"], 16);
        assert_eq!(bat["order"].as_array().unwrap().len(), 16);
        let head = &m["parts"]["BAHDLITTNHTH"];
        assert_eq!(head["file"], "parts/BA/BAHDLITTNHTH.gif");
        assert_eq!(m["slots"][0x39]["code"], "cap");
        assert!(m["slots"][0x39]["helm"].as_bool().unwrap());
        assert_eq!(m["weapon_classes"][1], "hth");

        let zip = bundle(&pack);
        let path = std::env::temp_dir().join(format!("bnetccd-characters-{}.zip", std::process::id()));
        std::fs::write(&path, &zip).unwrap();
        let listing = std::process::Command::new("unzip").arg("-l").arg(&path).output();
        if let Ok(out) = listing {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{text}");
            assert!(text.contains("manifest.json") && text.contains("parts/BA/BAHDLITTNHTH.gif"));
            let test = std::process::Command::new("unzip").arg("-tq").arg(&path).output().unwrap();
            assert!(test.status.success(), "{}", String::from_utf8_lossy(&test.stdout));
        }
        let _ = std::fs::remove_file(path);
    }
}
