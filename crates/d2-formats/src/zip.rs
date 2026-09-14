//! A ZIP writer for bundles of already-compressed files: entries are stored, not deflated, and
//! every date is the format's earliest (1980-01-01), so the same files always give the same
//! bytes.
//!
//! Written from PKWARE's APPNOTE: a local header and the data for each entry, then the central
//! directory and its end record.

/// CRC-32 (IEEE 802.3, reflected), as ZIP uses.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Bundle `(name, bytes)` entries, in the order given.
///
/// # Panics
///
/// If there are more than 65535 entries, or the bundle passes 4 GiB (ZIP64 is not written).
#[must_use]
pub fn store(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    assert!(entries.len() <= 0xFFFF, "too many entries for a plain ZIP");
    const DOS_DATE: u16 = 0x21; // 1980-01-01
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = u32::try_from(out.len()).expect("under 4 GiB");
        let size = u32::try_from(data.len()).expect("under 4 GiB");
        let crc = crc32(data);
        let name_len = u16::try_from(name.len()).expect("short name");
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&20u16.to_le_bytes()); // version needed
            v.extend_from_slice(&0u16.to_le_bytes()); // flags
            v.extend_from_slice(&0u16.to_le_bytes()); // stored
            v.extend_from_slice(&0u16.to_le_bytes()); // time
            v.extend_from_slice(&DOS_DATE.to_le_bytes());
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&size.to_le_bytes());
            v.extend_from_slice(&size.to_le_bytes());
            v.extend_from_slice(&name_len.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes()); // extra length
        };
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        common(&mut central);
        central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = u32::try_from(out.len()).expect("under 4 GiB");
    let central_size = u32::try_from(central.len()).expect("under 4 GiB");
    out.extend_from_slice(&central);
    let count = entries.len() as u16;
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn entries_can_be_found_from_the_end_record() {
        let zip = store(&[("a.txt".into(), b"hello".to_vec()), ("dir/b.bin".into(), vec![1, 2, 3])]);
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], &[0x50, 0x4B, 0x05, 0x06]);
        let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]) as usize;
        let u32_at = |at: usize| u32::from_le_bytes([zip[at], zip[at + 1], zip[at + 2], zip[at + 3]]) as usize;
        assert_eq!(u16_at(end + 10), 2);
        let mut at = u32_at(end + 16);
        let mut found = Vec::new();
        for _ in 0..2 {
            assert_eq!(u32_at(at), 0x0201_4B50);
            let (size, name_len, offset) = (u32_at(at + 24), u16_at(at + 28), u32_at(at + 42));
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            let data_at = offset + 30 + u16_at(offset + 26);
            found.push((name, zip[data_at..data_at + size].to_vec(), u32_at(at + 16) as u32));
            at += 46 + name_len;
        }
        assert_eq!(found[0], ("a.txt".to_string(), b"hello".to_vec(), crc32(b"hello")));
        assert_eq!(found[1].1, [1, 2, 3]);
        assert_eq!(store(&[("x".into(), vec![9])]), store(&[("x".into(), vec![9])]), "deterministic");
    }
}
