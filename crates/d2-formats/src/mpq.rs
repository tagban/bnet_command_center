//! MPQ archives, as Diablo II ships them (format 0).
//!
//! An archive is a header, an encrypted hash table (name → block index) and an encrypted block
//! table (where each member is, how big, and how it is stored). Members are read from disk on
//! demand, one sector at a time, so opening a 500 MB install costs only its tables.
//!
//! Diablo II's archives need two storage modes: PKWARE implode (`IMPLODE`, or `COMPRESS` with
//! mask `0x08`) and the member cipher, including `FIX_KEY`. Other compressors (zlib, bzip2,
//! Huffman/ADPCM for sound) are reported as unsupported rather than guessed at.
//!
//! Ported from `jaenster/libd2` `packages/formats/src/mpq.zig` (MIT, © 2026 jaenster).

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::pkware;

/// Why a member could not be read.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Io(std::io::Error),
    /// No MPQ header.
    NotAnArchive,
    /// A table or member runs past the end of the file.
    Truncated,
    /// A member's sector offsets are inconsistent.
    BadSectorTable,
    /// A sector uses a compressor this reader does not implement.
    UnsupportedCompression(u8),
    /// A PKWARE stream was malformed.
    Pkware(pkware::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "MPQ: {e}"),
            Self::NotAnArchive => f.write_str("MPQ: no archive header"),
            Self::Truncated => f.write_str("MPQ: truncated"),
            Self::BadSectorTable => f.write_str("MPQ: bad sector table"),
            Self::UnsupportedCompression(m) => write!(f, "MPQ: unsupported compression {m:#04x}"),
            Self::Pkware(e) => write!(f, "MPQ: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Block flags.
mod flags {
    pub const IMPLODE: u32 = 0x0000_0100;
    pub const COMPRESS: u32 = 0x0000_0200;
    pub const ENCRYPTED: u32 = 0x0001_0000;
    pub const FIX_KEY: u32 = 0x0002_0000;
    pub const SINGLE_UNIT: u32 = 0x0100_0000;
    pub const DELETE_MARKER: u32 = 0x0200_0000;
    pub const SECTOR_CRC: u32 = 0x0400_0000;
    pub const EXISTS: u32 = 0x8000_0000;
}

/// `COMPRESS` sector mask for PKWARE implode.
const MASK_PKWARE: u8 = 0x08;

// --- the cipher ------------------------------------------------------------------------

const fn crypt_table() -> [u32; 0x500] {
    let mut table = [0u32; 0x500];
    let mut seed: u32 = 0x0010_0001;
    let mut i = 0;
    while i < 0x100 {
        let mut index = i;
        let mut j = 0;
        while j < 5 {
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            let high = (seed & 0xFFFF) << 16;
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            table[index] = high | (seed & 0xFFFF);
            index += 0x100;
            j += 1;
        }
        i += 1;
    }
    table
}

static CRYPT: [u32; 0x500] = crypt_table();

#[derive(Clone, Copy)]
enum HashKind {
    TableOffset = 0,
    NameA = 1,
    NameB = 2,
    FileKey = 3,
}

/// Storm's string hash. Case-insensitive, and `/` is `\`.
fn hash(name: &str, kind: HashKind) -> u32 {
    let (mut s1, mut s2) = (0x7FED_7FEDu32, 0xEEEE_EEEEu32);
    for b in name.bytes() {
        let ch = match b.to_ascii_uppercase() {
            b'/' => b'\\',
            c => c,
        };
        s1 = CRYPT[((kind as usize) << 8) + usize::from(ch)] ^ s1.wrapping_add(s2);
        s2 = u32::from(ch).wrapping_add(s1).wrapping_add(s2).wrapping_add(s2 << 5).wrapping_add(3);
    }
    s1
}

/// Decrypt whole little-endian `u32`s in place; a trailing partial word is left as is.
fn decrypt(data: &mut [u8], mut key: u32) {
    let mut seed = 0xEEEE_EEEEu32;
    for word in data.chunks_exact_mut(4) {
        seed = seed.wrapping_add(CRYPT[0x400 + (key & 0xFF) as usize]);
        let plain = u32::from_le_bytes([word[0], word[1], word[2], word[3]]) ^ key.wrapping_add(seed);
        word.copy_from_slice(&plain.to_le_bytes());
        key = ((!key << 21).wrapping_add(0x1111_1111)) | (key >> 11);
        seed = plain.wrapping_add(seed).wrapping_add(seed << 5).wrapping_add(3);
    }
}

// --- one archive -------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct HashEntry {
    name_a: u32,
    name_b: u32,
    block: u32,
}

#[derive(Clone, Copy)]
struct Block {
    offset: u32,
    packed: u32,
    unpacked: u32,
    flags: u32,
}

/// An open MPQ archive.
pub struct Archive {
    path: PathBuf,
    file: Mutex<File>,
    /// Where the archive starts in the file.
    base: u64,
    sector_size: usize,
    hashes: Vec<HashEntry>,
    blocks: Vec<Block>,
}

impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Archive").field("path", &self.path).field("blocks", &self.blocks.len()).finish()
    }
}

const HASH_EMPTY: u32 = 0xFFFF_FFFF;
const HASH_DELETED: u32 = 0xFFFF_FFFE;

impl Archive {
    /// Open an archive and read its tables.
    ///
    /// # Errors
    ///
    /// [`Error`] if the file cannot be read or holds no MPQ.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let len = file.metadata()?.len();

        // The header sits at a 512-byte boundary; Diablo II's are at the start.
        let mut header = [0u8; 32];
        let mut base = None;
        let mut at = 0u64;
        while at + 32 <= len && at <= 1 << 20 {
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut header)?;
            if &header[..4] == b"MPQ\x1A" {
                base = Some(at);
                break;
            }
            at += 512;
        }
        let base = base.ok_or(Error::NotAnArchive)?;
        let u16_at = |o: usize| u16::from_le_bytes([header[o], header[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
        let sector_size = 512usize << u16_at(14);
        let (hash_pos, block_pos, hash_count, block_count) = (u32_at(16), u32_at(20), u32_at(24), u32_at(28));

        let mut read_table = |pos: u32, count: u32, key: &str| -> Result<Vec<u8>, Error> {
            let bytes = count as u64 * 16;
            if base + u64::from(pos) + bytes > len {
                return Err(Error::Truncated);
            }
            let mut raw = vec![0u8; bytes as usize];
            file.seek(SeekFrom::Start(base + u64::from(pos)))?;
            file.read_exact(&mut raw)?;
            decrypt(&mut raw, hash(key, HashKind::FileKey));
            Ok(raw)
        };
        let word = |raw: &[u8], o: usize| u32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
        let raw = read_table(hash_pos, hash_count, "(hash table)")?;
        let hashes = raw
            .chunks_exact(16)
            .map(|e| HashEntry { name_a: word(e, 0), name_b: word(e, 4), block: word(e, 12) })
            .collect();
        let raw = read_table(block_pos, block_count, "(block table)")?;
        let blocks = raw
            .chunks_exact(16)
            .map(|e| Block { offset: word(e, 0), packed: word(e, 4), unpacked: word(e, 8), flags: word(e, 12) })
            .collect();
        Ok(Self { path, file: Mutex::new(file), base, sector_size, hashes, blocks })
    }

    /// The file this archive was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lookup(&self, name: &str) -> Option<Block> {
        if self.hashes.is_empty() {
            return None;
        }
        let (a, b) = (hash(name, HashKind::NameA), hash(name, HashKind::NameB));
        let start = hash(name, HashKind::TableOffset) as usize % self.hashes.len();
        for i in 0..self.hashes.len() {
            let e = self.hashes[(start + i) % self.hashes.len()];
            match e.block {
                HASH_EMPTY => return None,
                HASH_DELETED => continue,
                index if e.name_a == a && e.name_b == b => {
                    return self.blocks.get(index as usize).copied().filter(|blk| {
                        blk.flags & flags::EXISTS != 0 && blk.flags & flags::DELETE_MARKER == 0
                    });
                }
                _ => {}
            }
        }
        None
    }

    /// Whether the archive has a member by this name.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    /// Read a member, or `None` if there is none by that name.
    ///
    /// # Errors
    ///
    /// [`Error`] if the member is there but cannot be read.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some(blk) = self.lookup(name) else {
            return Ok(None);
        };
        let key = (blk.flags & flags::ENCRYPTED != 0).then(|| {
            // Only the base name is keyed.
            let base_name = name.rsplit(['\\', '/']).next().unwrap_or(name);
            let key = hash(base_name, HashKind::FileKey);
            if blk.flags & flags::FIX_KEY != 0 {
                key.wrapping_add(blk.offset) ^ blk.unpacked
            } else {
                key
            }
        });

        let mut raw = vec![0u8; blk.packed as usize];
        {
            let mut file = self.file.lock().expect("archive file lock");
            let end = self.base + u64::from(blk.offset) + u64::from(blk.packed);
            if end > file.metadata()?.len() {
                return Err(Error::Truncated);
            }
            file.seek(SeekFrom::Start(self.base + u64::from(blk.offset)))?;
            file.read_exact(&mut raw)?;
        }
        let unpacked = blk.unpacked as usize;
        let mut out = vec![0u8; unpacked];
        if unpacked == 0 {
            return Ok(Some(out));
        }
        let compressed = blk.flags & (flags::IMPLODE | flags::COMPRESS) != 0;

        if blk.flags & flags::SINGLE_UNIT != 0 {
            if let Some(k) = key {
                decrypt(&mut raw, k);
            }
            expand(&raw, &mut out, blk.flags)?;
            return Ok(Some(out));
        }

        let count = unpacked.div_ceil(self.sector_size);
        let offsets: Vec<usize> = if compressed {
            let entries = count + 1 + usize::from(blk.flags & flags::SECTOR_CRC != 0);
            let mut table = raw.get(..entries * 4).ok_or(Error::BadSectorTable)?.to_vec();
            if let Some(k) = key {
                decrypt(&mut table, k.wrapping_sub(1));
            }
            let offsets: Vec<usize> = table
                .chunks_exact(4)
                .take(count + 1)
                .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as usize)
                .collect();
            if offsets[0] != entries * 4 {
                return Err(Error::BadSectorTable);
            }
            offsets
        } else {
            (0..=count).map(|i| (i * self.sector_size).min(raw.len())).collect()
        };

        for i in 0..count {
            let (from, to) = (offsets[i], offsets[i + 1]);
            if from > to || to > raw.len() {
                return Err(Error::BadSectorTable);
            }
            let at = i * self.sector_size;
            let want = self.sector_size.min(unpacked - at);
            let sector = &mut raw[from..to];
            if let Some(k) = key {
                decrypt(sector, k.wrapping_add(i as u32));
            }
            expand(sector, &mut out[at..at + want], blk.flags)?;
        }
        Ok(Some(out))
    }
}

/// Undo one sector's storage into `dst`, which is exactly the sector's unpacked size. A sector
/// that did not shrink was stored as is.
fn expand(src: &[u8], dst: &mut [u8], block_flags: u32) -> Result<(), Error> {
    if src.len() == dst.len() || block_flags & (flags::IMPLODE | flags::COMPRESS) == 0 {
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
        return Ok(());
    }
    let stream = if block_flags & flags::IMPLODE != 0 {
        src
    } else {
        match src.split_first() {
            Some((&MASK_PKWARE, rest)) => rest,
            Some((&mask, _)) => return Err(Error::UnsupportedCompression(mask)),
            None => return Err(Error::Truncated),
        }
    };
    pkware::explode(stream, dst).map_err(Error::Pkware)?;
    Ok(())
}

// --- an install ----------------------------------------------------------------------------

/// The archives whose members shadow one another in a Diablo II install, most specific first:
/// the patch over the expansion over the classic game. Reading them in another order gives an
/// unpatched game that mostly works.
pub const DATA_ARCHIVES: [&str; 3] = ["Patch_D2.mpq", "d2exp.mpq", "d2data.mpq"];

/// Several archives searched as one, first match wins.
#[derive(Debug)]
pub struct ArchiveSet {
    archives: Vec<Archive>,
}

impl ArchiveSet {
    /// Open the named archives in `dir`, in priority order. A missing archive is skipped (a
    /// classic-only install has no `d2exp.mpq`); an unreadable one is an error.
    ///
    /// # Errors
    ///
    /// [`Error`] if an archive that exists cannot be opened, or none exist.
    pub fn open(dir: impl AsRef<Path>, names: &[&str]) -> Result<Self, Error> {
        let mut archives = Vec::new();
        for name in names {
            if let Some(path) = find_case_insensitive(dir.as_ref(), name) {
                archives.push(Archive::open(path)?);
            }
        }
        if archives.is_empty() {
            return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no MPQ archives found")));
        }
        Ok(Self { archives })
    }

    /// The archives found, in priority order.
    pub fn archives(&self) -> impl Iterator<Item = &Archive> {
        self.archives.iter()
    }

    /// Read a member from the first archive that has it.
    ///
    /// # Errors
    ///
    /// [`Error`] if the archive that has it cannot read it.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        for archive in &self.archives {
            if archive.contains(name) {
                return archive.read(name);
            }
        }
        Ok(None)
    }
}

/// `dir/name`, matching the file name case-insensitively (installs differ: `Patch_D2.mpq` vs
/// `patch_d2.mpq`).
fn find_case_insensitive(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_file() && p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_keys_are_the_well_known_constants() {
        assert_eq!(hash("(hash table)", HashKind::FileKey), 0xC3AF_3770);
        assert_eq!(hash("(block table)", HashKind::FileKey), 0xEC83_B3A3);
    }

    #[test]
    fn names_hash_case_insensitively_with_either_slash() {
        let a = hash("data\\global\\excel\\CharStats.txt", HashKind::NameA);
        assert_eq!(a, hash("DATA/GLOBAL/EXCEL/charstats.TXT", HashKind::NameA));
    }

    #[test]
    fn decrypt_leaves_a_partial_word_alone() {
        let mut data = [1, 2, 3, 4, 5];
        decrypt(&mut data, 0x1234);
        assert_eq!(data[4], 5);
    }

    /// With the operator's install (set `BNETCC_D2_DATA_DIR`), the excel tables come out as text,
    /// including one stored encrypted with `FIX_KEY` in `d2exp.mpq`.
    #[test]
    fn with_a_real_install_members_read_back() {
        let Ok(dir) = std::env::var("BNETCC_D2_DATA_DIR") else {
            return;
        };
        let set = ArchiveSet::open(&dir, &DATA_ARCHIVES).expect("open install");
        let charstats = set.read("data\\global\\excel\\charstats.txt").unwrap().expect("charstats");
        assert!(charstats.starts_with(b"class\t"), "{:?}", String::from_utf8_lossy(&charstats[..40]));
        let exp = Archive::open(Path::new(&dir).join("d2exp.mpq")).unwrap();
        // d2exp.mpq's copy predates the patch's and leads with `Class`.
        let monstats = exp.read("data\\global\\excel\\monstats.txt").unwrap().expect("encrypted member");
        assert!(monstats.starts_with(b"Class\t"), "{:?}", String::from_utf8_lossy(&monstats[..20]));
    }
}
