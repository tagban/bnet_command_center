//! Take the icons out of an `icons.bni` and write them as PNGs.
//!
//! The tracker page shows a game's own Battle.net icon beside each server — the same artwork
//! a player sees next to their name in chat, rather than a picture borrowed from another
//! list site. Those icons live in `icons.bni`, which a server hands its clients over BNFTP.
//!
//! This is a separate tool, run by hand, on purpose. `icons.bni` is an operator's own game
//! data: it is never committed here, and the running server has no business writing image
//! files into a website. Point this at yours, upload what comes out, and the page uses it.
//!
//! ```text
//! bnetcc-icons <icons.bni> <output directory>
//! ```
//!
//! One `CODE.png` per icon, named for the product codes the icon serves (`W3XP.png`), which
//! is what the page looks for.

mod png;
mod tga;

use std::path::{Path, PathBuf};

use bnetcc_proto::bni;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source, out_dir] = args.as_slice() else {
        eprintln!("usage: bnetcc-icons <icons.bni> <output directory>");
        return std::process::ExitCode::from(2);
    };
    match run(Path::new(source), Path::new(out_dir)) {
        Ok(written) => {
            println!("wrote {} icon{} to {}", written.len(), if written.len() == 1 { "" } else { "s" }, out_dir);
            for path in written {
                println!("  {}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(source: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if bni::is_mpq(&bytes) {
        // icons-WAR3.bni and WAR3.bni are MPQ archives wearing a .bni extension; a BNI parser
        // makes nonsense of them. Say so plainly rather than writing garbage images.
        return Err(format!("{} is an MPQ archive, not a BNI — WarCraft III's icon files are not this format", source.display()));
    }
    let file = bni::parse(&bytes).map_err(|e| format!("cannot read {} as a BNI: {e:?}", source.display()))?;
    let image = tga::decode(&file.image).ok_or_else(|| "the embedded image is not a TGA this can read".to_string())?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot make {}: {e}", out_dir.display()))?;

    let mut written = Vec::new();
    let mut top = 0u32;
    for icon in &file.icons {
        // The icons are stacked vertically in one image, in entry order.
        let slice = tga::rows(&image, top, icon.height);
        top += icon.height;
        let Some(pixels) = slice else {
            return Err(format!("the image ends before the icon at row {top} — is the BNI truncated?"));
        };
        if icon.codes.is_empty() {
            continue; // Matched by chat flags, not by a product code; the page wants codes.
        }
        let png = png::rgb(icon.width, icon.height, &pixels);
        for code in &icon.codes {
            let name = code.to_string();
            let name = name.trim_matches(|c: char| c.is_whitespace() || c == '\0');
            if name.is_empty() {
                continue;
            }
            let path = out_dir.join(format!("{name}.png"));
            std::fs::write(&path, &png).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            written.push(path);
        }
    }
    if written.is_empty() {
        return Err("no icon in that file is named by a product code".to_string());
    }
    Ok(written)
}
