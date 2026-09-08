//! Blizzard's "Broken SHA-1" (X-SHA-1), used by every pre-WarCraft-III Battle.net
//! client for password hashing.
//!
//! # Provenance
//!
//! Reimplemented in Rust from the behaviour of `wjlafrance/broken-sha1`
//! (<https://github.com/wjlafrance/broken-sha1>), itself a C port of MBNCSUtil by
//! Robert Paveza, distributed under a BSD-3-clause-style licence. That licence permits
//! reuse with attribution, which this notice provides. **Deliberately not derived from
//! PvPGN**, whose SRP/bignum sources are AGPL-3.0 — see `docs/LEGAL.md`.
//!
//! # How it differs from real SHA-1
//!
//! 1. **The message expansion has its operands swapped.** Real SHA-1 computes
//!    `w[i] = ROL(w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16], 1)`. This computes
//!    `w[i] = ROL(1, (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]) % 32)` — rotating the
//!    *constant 1* by the xor rather than the xor by 1. This is the actual "break":
//!    the expanded schedule can only ever contain single-bit words.
//! 2. **Little-endian word interpretation** (real SHA-1 is big-endian).
//! 3. **No length padding.** The input is copied into a zeroed buffer with no `0x80`
//!    terminator and no appended length, so **only the first 64 bytes of input affect
//!    the digest**.
//!
//! Round functions and constants are otherwise standard SHA-1.
//!
//! # Security
//!
//! This is not a secure hash and must never be used as one. It exists solely because
//! 1998-era game clients compute it and we have to match them byte for byte. It is not
//! used for anything except verifying `SID_LOGONRESPONSE`/`SID_LOGONRESPONSE2` proofs.

/// Number of input bytes that actually influence the digest.
///
/// The reference implementation copies the input into a zeroed buffer and then
/// overwrites words 16..80 during expansion, so anything past word 15 is discarded.
pub const EFFECTIVE_INPUT_LEN: usize = 64;

const H0: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];

#[inline]
fn rol(v: u32, s: u32) -> u32 {
    v.rotate_left(s & 31)
}

/// Compute X-SHA-1 over `input`, returning the five state words.
///
/// Only the first [`EFFECTIVE_INPUT_LEN`] bytes are read; longer inputs are silently
/// truncated, matching the reference implementation and therefore the game clients.
#[must_use]
pub fn xsha1(input: &[u8]) -> [u32; 5] {
    let mut block = [0u8; EFFECTIVE_INPUT_LEN];
    let n = input.len().min(EFFECTIVE_INPUT_LEN);
    block[..n].copy_from_slice(&input[..n]);

    let mut w = [0u32; 80];
    for (i, word) in w.iter_mut().take(16).enumerate() {
        let b = &block[i * 4..i * 4 + 4];
        *word = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }
    for i in 16..80 {
        // NOTE: operands swapped relative to real SHA-1. This is intentional and is
        // the defining quirk of the algorithm. Do not "fix" it.
        let x = w[i - 16] ^ w[i - 8] ^ w[i - 14] ^ w[i - 3];
        w[i] = rol(1, x % 32);
    }

    let [mut a, mut b, mut c, mut d, mut e] = H0;
    for (i, &wi) in w.iter().enumerate() {
        let fk = match i {
            0..=19 => ((b & c) | (!b & d)).wrapping_add(0x5A82_7999),
            20..=39 => (b ^ c ^ d).wrapping_add(0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d)).wrapping_add(0x8F1B_BCDC),
            _ => (b ^ c ^ d).wrapping_add(0xCA62_C1D6),
        };
        let temp = wi
            .wrapping_add(rol(a, 5))
            .wrapping_add(e)
            .wrapping_add(fk);
        e = d;
        d = c;
        c = rol(b, 30);
        b = a;
        a = temp;
    }

    [
        a.wrapping_add(H0[0]),
        b.wrapping_add(H0[1]),
        c.wrapping_add(H0[2]),
        d.wrapping_add(H0[3]),
        e.wrapping_add(H0[4]),
    ]
}

/// Compute X-SHA-1 and return the 20 wire bytes (each state word little-endian).
#[must_use]
pub fn xsha1_bytes(input: &[u8]) -> [u8; 20] {
    let words = xsha1(input);
    let mut out = [0u8; 20];
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Stage-one password hash: `XSHA1(lowercase(password))`.
///
/// Lowercasing is required for compatibility with Blizzard clients, which do it
/// client-side. This is the value stored server-side for XSHA-1 products, and it is
/// **password-equivalent** — anyone holding it can authenticate as the user. See
/// `docs/FEDERATION.md` section 4 for why it must never leave the hub.
#[must_use]
pub fn password_hash(password: &str) -> [u8; 20] {
    xsha1_bytes(password.to_ascii_lowercase().as_bytes())
}

/// Stage-two proof: `XSHA1(clientToken ‖ serverToken ‖ h1)`, as sent in
/// `SID_LOGONRESPONSE2` (0x3A) and `SID_LOGONRESPONSE` (0x29).
///
/// Tokens are serialised little-endian, matching every other numeric on this wire.
#[must_use]
pub fn logon_proof(client_token: u32, server_token: u32, h1: &[u8; 20]) -> [u8; 20] {
    let mut buf = [0u8; 28];
    buf[0..4].copy_from_slice(&client_token.to_le_bytes());
    buf[4..8].copy_from_slice(&server_token.to_le_bytes());
    buf[8..28].copy_from_slice(h1);
    xsha1_bytes(&buf)
}

/// Constant-time comparison of two 20-byte proofs.
///
/// Login verification must not leak the correct proof through timing. This is a
/// simple fold rather than a dependency on a crypto crate, which is adequate for a
/// fixed-length, non-secret-dependent-branch comparison.
#[must_use]
pub fn proofs_match(a: &[u8; 20], b: &[u8; 20]) -> bool {
    let mut diff = 0u8;
    for i in 0..20 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer vectors generated by compiling and running the BSD-licensed
    /// reference implementation (`wjlafrance/broken-sha1`). The `"1234567890"` vector
    /// is additionally published in that repository's own `main.c`.
    ///
    /// If any of these fail, no real game client will be able to log in — this is the
    /// single highest-value test in the tree.
    const VECTORS: &[(&[u8], [u32; 5])] = &[
        (b"", [0x4d3a_a0ee, 0x9426_1d5a, 0x584a_6f57, 0x6b8d_9960, 0x1546_c680]),
        (b"a", [0xfe44_2493, 0x6dc2_0078, 0xa033_9551, 0x59f8_2303, 0x6e51_3f13]),
        (b"1234567890", [0x99f0_fab8, 0xb5b4_523e, 0x0d58_e5ef, 0xe126_fa5f, 0x1263_3b4b]),
        (b"password", [0x1d0d_c8ec, 0xc058_e776, 0x258c_dab9, 0xff6a_10ff, 0x1629_248e]),
        (
            b"abcdefghijklmnopqrstuvwxyz",
            [0x6dc1_f87b, 0xf61e_f734, 0xf573_a688, 0x2907_9c3f, 0x6abb_0309],
        ),
    ];

    #[test]
    fn known_answer_vectors() {
        for (input, expected) in VECTORS {
            assert_eq!(
                &xsha1(input),
                expected,
                "xsha1({:?}) mismatch",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn sixty_four_byte_block() {
        let input = [b'A'; 64];
        assert_eq!(
            xsha1(&input),
            [0x1936_12de, 0x92e1_b17d, 0x1d66_d2f3, 0x0fbb_6387, 0x177e_cd60]
        );
    }

    #[test]
    fn input_is_truncated_at_64_bytes() {
        // The reference discards everything past word 15, so a 64-byte input and a
        // 200-byte input sharing a prefix must collide. Documenting this as a test
        // because it is a real (if benign) property operators should know about:
        // passwords longer than 64 characters are effectively truncated.
        let short = [b'A'; 64];
        let mut long = [b'A'; 200];
        long[64..].fill(b'Z');
        assert_eq!(xsha1(&short), xsha1(&long));
    }

    #[test]
    fn wire_bytes_are_little_endian_words() {
        let words = xsha1(b"password");
        let bytes = xsha1_bytes(b"password");
        assert_eq!(&bytes[0..4], &words[0].to_le_bytes());
        assert_eq!(&bytes[0..4], &[0xec, 0xc8, 0x0d, 0x1d]);
    }

    #[test]
    fn full_logon_chain() {
        // Verified end-to-end against the reference: h1 = xsha1("password"), then
        // xsha1(ct ‖ st ‖ h1) with ct = 0xDEADBEEF, st = 0x12345678.
        let h1 = password_hash("password");
        assert_eq!(h1, xsha1_bytes(b"password"));

        let proof = logon_proof(0xDEAD_BEEF, 0x1234_5678, &h1);
        let expected: [u32; 5] =
            [0x7488_ad2d, 0x82dc_91a2, 0x8aa4_3a7c, 0x8d59_6822, 0xa292_0091];
        let mut expected_bytes = [0u8; 20];
        for (i, w) in expected.iter().enumerate() {
            expected_bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        assert_eq!(proof, expected_bytes);
        assert!(proofs_match(&proof, &expected_bytes));
    }

    #[test]
    fn password_hashing_is_case_insensitive() {
        assert_eq!(password_hash("PassWord"), password_hash("password"));
    }

    #[test]
    fn proofs_match_rejects_differences() {
        let a = [0u8; 20];
        let mut b = [0u8; 20];
        assert!(proofs_match(&a, &b));
        b[19] = 1;
        assert!(!proofs_match(&a, &b));
        b[19] = 0;
        b[0] = 1;
        assert!(!proofs_match(&a, &b));
    }
}
