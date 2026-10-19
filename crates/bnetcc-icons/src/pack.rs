//! Redrawing a server's icons: `export` writes every icon of a BNI as a numbered PNG with a list
//! of what each one is for (`icons.txt`); `pack` reads the PNGs back, redrawn, into a BNI.
//!
//! The rules a client holds a BNI to, which `pack` checks:
//!
//! - **Every icon keeps its size** — 28 × 14 in Blizzard's files. The classic clients draw each
//!   icon in a slot of that size and do not scale it, so a bigger picture is refused.
//! - **No transparency.** A BNI's picture is 24-bit RGB; see-through pixels are laid on black.
//! - **The same icons in the same order, with the same flags and codes.** A client picks an
//!   icon by matching a user's chat flags, or the product code in their statstring, against
//!   `icons.txt`'s columns; `pack` takes them from there, so only the pictures change.
//!
//! `icons.txt`, one line per icon in the file's order (`#` starts a comment):
//!
//! ```text
//! 0  0x00000001  28x14  -          00-blizzard.png
//! 8  0x00000000  28x14  STAR,SSHR  08-STAR+SSHR.png
//! ```

use std::fmt::Write as _;
use std::path::Path;

use bnetcc_proto::bni::{self, BniFile, BniIcon};
use bnetcc_proto::FourCc;

use crate::{png, tga};

/// What a flag-matched icon is for, by the chat flag it answers to.
fn flag_name(flags: u32) -> &'static str {
    match flags {
        0x01 => "blizzard",
        0x02 => "operator",
        0x04 => "speaker",
        0x08 => "admin",
        0x10 => "no-udp",
        0x20 => "squelched",
        0x40 => "guest",
        0x10_0000 => "gf-official",
        0x20_0000 => "gf-player",
        _ => "flags",
    }
}

fn code_text(code: &FourCc) -> String {
    code.to_string().trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string()
}

/// Write every icon in `source` as `<folder>/NN-<what>.png`, and `icons.txt` saying what each is.
///
/// # Errors
/// The BNI unreadable, or the folder unwritable.
pub fn export(source: &Path, folder: &Path) -> Result<String, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if bni::is_mpq(&bytes) {
        return Err(format!("{} is an MPQ archive, not a BNI — WarCraft III's icon files are not this format", source.display()));
    }
    let file = bni::parse(&bytes).map_err(|e| format!("cannot read {} as a BNI: {e:?}", source.display()))?;
    let image = tga::decode(&file.image).ok_or("the embedded image is not a TGA this can read")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    let mut list = String::from(
        "# bnetcc-icons: one line per icon, in the file's order. Redraw the PNGs (keep each one's\n\
         # size, no transparency) and run `bnetcc-icons pack <this folder> <new icons.bni>`.\n\
         # index  flags  size  codes  picture\n",
    );
    let mut top = 0u32;
    for (i, icon) in file.icons.iter().enumerate() {
        let rows = tga::rows(&image, top, icon.height).ok_or_else(|| format!("the image ends before icon {i} — is the BNI truncated?"))?;
        top += icon.height;
        // Each icon's own width out of the image's rows (icons narrower than the widest).
        let stride = image.width as usize * 3;
        let mut pixels = Vec::with_capacity((icon.width * icon.height) as usize * 3);
        for y in 0..icon.height as usize {
            pixels.extend_from_slice(&rows[y * stride..y * stride + icon.width as usize * 3]);
        }
        let codes: Vec<String> = icon.codes.iter().map(code_text).filter(|c| !c.is_empty()).collect();
        let what = if codes.is_empty() { flag_name(icon.flags).to_string() } else { codes.join("+") };
        let name = format!("{i:02}-{what}.png");
        let path = folder.join(&name);
        std::fs::write(&path, png::rgb(icon.width, icon.height, &pixels)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let code_col = if codes.is_empty() { "-".to_string() } else { codes.join(",") };
        let _ = writeln!(list, "{i}  {:#010x}  {}x{}  {code_col}  {name}", icon.flags, icon.width, icon.height);
    }
    let list_path = folder.join("icons.txt");
    std::fs::write(&list_path, list).map_err(|e| format!("cannot write {}: {e}", list_path.display()))?;
    Ok(format!("wrote {} icons and icons.txt to {}", file.icons.len(), folder.display()))
}

/// One line of `icons.txt`.
struct Entry {
    flags: u32,
    width: u32,
    height: u32,
    codes: Vec<FourCc>,
    picture: String,
}

fn parse_list(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let bad = |what: &str| format!("icons.txt line {}: {what}", n + 1);
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [index, flags, size, codes, picture] = cols.as_slice() else {
            return Err(bad("wants five columns: index, flags, size, codes, picture"));
        };
        if index.parse::<usize>().ok() != Some(entries.len()) {
            return Err(bad(&format!("index {index}, but this is icon {}: keep the icons in the file's order", entries.len())));
        }
        let flags = u32::from_str_radix(flags.trim_start_matches("0x"), 16).map_err(|_| bad("flags are not hex"))?;
        let (w, h) = size.split_once('x').ok_or_else(|| bad("size is not WxH"))?;
        let (width, height) = (w.parse().map_err(|_| bad("bad width"))?, h.parse().map_err(|_| bad("bad height"))?);
        let codes = if *codes == "-" {
            Vec::new()
        } else {
            codes
                .split(',')
                .map(|c| {
                    let b = c.as_bytes();
                    if b.len() == 4 && b.iter().all(u8::is_ascii_graphic) {
                        Ok(FourCc::from_ascii(&[b[0], b[1], b[2], b[3]]))
                    } else {
                        Err(bad(&format!("code {c:?} is not four characters")))
                    }
                })
                .collect::<Result<_, _>>()?
        };
        entries.push(Entry { flags, width, height, codes, picture: (*picture).to_string() });
    }
    Ok(entries)
}

/// A PNG as RGB, see-through pixels laid on black; and whether any were.
fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>, bool), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut decoder = ::png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(::png::Transformations::EXPAND | ::png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: not a PNG this can read ({e})", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let buf = &buf[..info.buffer_size()];
    let mut see_through = false;
    let rgb: Vec<u8> = match info.color_type {
        ::png::ColorType::Rgb => buf.to_vec(),
        ::png::ColorType::Rgba => buf
            .chunks_exact(4)
            .flat_map(|p| {
                see_through |= p[3] < 255;
                let a = u16::from(p[3]);
                [p[0], p[1], p[2]].map(|c| (u16::from(c) * a / 255) as u8)
            })
            .collect(),
        ::png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g]).collect(),
        ::png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| {
                see_through |= p[1] < 255;
                let g = (u16::from(p[0]) * u16::from(p[1]) / 255) as u8;
                [g, g, g]
            })
            .collect(),
        ::png::ColorType::Indexed => return Err(format!("{}: still palette-indexed after expanding", path.display())),
    };
    Ok((info.width, info.height, rgb, see_through))
}

/// Read `<folder>/icons.txt` and its PNGs into a BNI at `out`.
///
/// # Errors
/// The list unreadable or wrong, a picture missing or the wrong size, `out` unwritable.
pub fn pack(folder: &Path, out: &Path) -> Result<String, String> {
    let list_path = folder.join("icons.txt");
    let text = std::fs::read_to_string(&list_path).map_err(|e| format!("cannot read {}: {e} — make the folder with `bnetcc-icons export` first", list_path.display()))?;
    let entries = parse_list(&text)?;
    if entries.is_empty() {
        return Err("icons.txt lists no icons".into());
    }
    let width = entries.iter().map(|e| e.width).max().unwrap_or(0);
    let height: u32 = entries.iter().map(|e| e.height).sum();
    let mut image = vec![0u8; (width * height) as usize * 3];
    let mut notes = Vec::new();
    let mut top = 0u32;
    for (i, e) in entries.iter().enumerate() {
        let path = folder.join(&e.picture);
        let (w, h, rgb, see_through) = read_png(&path)?;
        if (w, h) != (e.width, e.height) {
            return Err(format!("{} is {w}x{h}, but icon {i} must be {}x{}: the classic clients draw it in a slot of that size", e.picture, e.width, e.height));
        }
        if see_through {
            notes.push(format!("{}: see-through pixels laid on black (a BNI has no transparency)", e.picture));
        }
        for y in 0..h {
            let from = (y * w) as usize * 3;
            let to = ((top + y) * width) as usize * 3;
            image[to..to + w as usize * 3].copy_from_slice(&rgb[from..from + w as usize * 3]);
        }
        top += h;
    }
    let file = BniFile {
        version: bni::VERSION,
        icons: entries.iter().map(|e| BniIcon { flags: e.flags, width: e.width, height: e.height, codes: e.codes.clone() }).collect(),
        image: tga::encode(width, height, &image),
    };
    let bytes = bni::build(&file);
    // Read back what is written, so a client will parse it.
    let check = bni::parse(&bytes).map_err(|e| format!("the packed file does not read back: {e:?}"))?;
    for w in bni::validate(&check) {
        notes.push(format!("warning: {w:?}"));
    }
    std::fs::write(out, &bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let mut said = format!("packed {} icons into {} ({} bytes)", entries.len(), out.display(), bytes.len());
    for n in notes {
        said.push_str("\n  ");
        said.push_str(&n);
    }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bnetcc-icons-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A BNI of two 28 x 14 icons: one for the Blizzard flag, one for two product codes.
    fn sample() -> Vec<u8> {
        let mut rgb = Vec::new();
        for y in 0..28u32 {
            for x in 0..28u32 {
                rgb.extend_from_slice(&[(x * 9) as u8, (y * 9) as u8, if y < 14 { 200 } else { 40 }]);
            }
        }
        bni::build(&BniFile {
            version: 1,
            icons: vec![
                BniIcon { flags: 1, width: 28, height: 14, codes: vec![] },
                BniIcon { flags: 0, width: 28, height: 14, codes: vec![FourCc::from_ascii(b"STAR"), FourCc::from_ascii(b"SSHR")] },
            ],
            image: tga::encode(28, 28, &rgb),
        })
    }

    #[test]
    fn exported_icons_pack_back_into_the_same_file() {
        let dir = temp("roundtrip");
        let src = dir.join("icons.bni");
        std::fs::write(&src, sample()).unwrap();
        let folder = dir.join("icons");
        export(&src, &folder).unwrap();
        let list = std::fs::read_to_string(folder.join("icons.txt")).unwrap();
        assert!(list.contains("0  0x00000001  28x14  -  00-blizzard.png"), "{list}");
        assert!(list.contains("1  0x00000000  28x14  STAR,SSHR  01-STAR+SSHR.png"), "{list}");
        let out = dir.join("packed.bni");
        pack(&folder, &out).unwrap();
        let (a, b) = (bni::parse(&sample()).unwrap(), bni::parse(&std::fs::read(&out).unwrap()).unwrap());
        assert_eq!(a.icons, b.icons, "the same entries, flags and codes");
        assert_eq!(tga::decode(&a.image).unwrap().pixels, tga::decode(&b.image).unwrap().pixels, "the same pictures");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_refused() {
        let dir = temp("size");
        let src = dir.join("icons.bni");
        std::fs::write(&src, sample()).unwrap();
        let folder = dir.join("icons");
        export(&src, &folder).unwrap();
        std::fs::write(folder.join("00-blizzard.png"), png::rgb(56, 28, &vec![0; 56 * 28 * 3])).unwrap();
        let err = pack(&folder, &dir.join("packed.bni")).unwrap_err();
        assert!(err.contains("is 56x28, but icon 0 must be 28x14"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
