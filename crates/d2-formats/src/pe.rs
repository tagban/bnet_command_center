//! Just enough of a PE32 image to read initialised data by virtual address — for the handful
//! of tables the 1.14d engine keeps in `Game.exe` rather than in its MPQs.

/// A parsed image borrowing the file's bytes.
#[derive(Debug)]
pub struct Image<'a> {
    file: &'a [u8],
    base: u32,
    /// `(virtual address, raw size, raw offset)` per section.
    sections: Vec<(u32, u32, u32)>,
}

impl<'a> Image<'a> {
    /// Parse the headers; `None` if this is not a PE image.
    #[must_use]
    pub fn parse(file: &'a [u8]) -> Option<Self> {
        let u16_at = |o: usize| file.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| file.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if file.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(0x3C)? as usize;
        if file.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let count = u16_at(pe + 6)? as usize;
        let optional = u16_at(pe + 20)? as usize;
        let base = u32_at(pe + 24 + 28)?;
        let first = pe + 24 + optional;
        let sections = (0..count)
            .map(|i| {
                let s = first + 40 * i;
                Some((u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { file, base, sections })
    }

    /// `len` bytes of initialised data at virtual address `va`.
    #[must_use]
    pub fn bytes(&self, va: u32, len: usize) -> Option<&'a [u8]> {
        let rva = va.checked_sub(self.base)?;
        self.sections.iter().find_map(|&(start, raw_size, raw_offset)| {
            let within = rva.checked_sub(start)?;
            (within as usize + len <= raw_size as usize)
                .then(|| self.file.get(raw_offset as usize + within as usize..)?.get(..len))
                .flatten()
        })
    }

    /// Little-endian `i32`s at `va`, filling `out`.
    #[must_use]
    pub fn i32s(&self, va: u32, out: &mut [i32]) -> Option<()> {
        let bytes = self.bytes(va, out.len() * 4)?;
        for (v, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
            *v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        Some(())
    }
}

/// `(dwFileVersionMS, dwFileVersionLS)` from the image's `VS_FIXEDFILEINFO`, found by signature.
#[must_use]
pub fn file_version(file: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 4] = 0xFEEF_04BD_u32.to_le_bytes();
    let at = file.windows(4).position(|w| w == SIGNATURE)?;
    let b = file.get(at + 8..at + 16)?;
    Some((u32::from_le_bytes([b[0], b[1], b[2], b[3]]), u32::from_le_bytes([b[4], b[5], b[6], b[7]])))
}
