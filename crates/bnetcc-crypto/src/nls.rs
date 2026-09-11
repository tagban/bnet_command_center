//! Blizzard's "New Logon System" (NLS), the SRP variant WarCraft III uses for
//! `SID_AUTH_ACCOUNTLOGON` (0x53) / `SID_AUTH_ACCOUNTLOGONPROOF` (0x54).
//!
//! # Provenance
//!
//! Implemented from the public specification — BNETDocs' "NLS/SRP Protocol" document,
//! the javaop write-up it quotes, and RFC 2945 for baseline SRP — and cross-checked
//! against an independent OpenSSL-based implementation written by this project's author.
//! **Deliberately not derived from PvPGN's `bnetsrp3.cpp`**, which is AGPL-3.0; that file
//! was never opened. See `docs/LEGAL.md` §1 and `docs/WARCRAFT3.md` §3.
//!
//! # The algorithm
//!
//! ```text
//! x  = SHA1( s ‖ SHA1( upper(C) ‖ ":" ‖ upper(P) ) )   read little-endian
//! v  = g^x mod N                                        (stored with s; the password never is)
//! A  = g^a mod N                                        client, fresh a per logon
//! B  = (v + g^b) mod N                                  server, fresh b per logon
//! u  = first 4 bytes of SHA1(B)                         read BIG-endian; never sent
//! S  = client: ((N + B − v) mod N)^(a + u·x) mod N
//!      server: (A · v^u mod N)^b mod N
//! K  = interleave( SHA1(S[even]), SHA1(S[odd]) )        40 bytes
//! I  = SHA1(g) xor SHA1(N)                              20-byte constant
//! M1 = SHA1( I ‖ SHA1(upper(C)) ‖ s ‖ A ‖ B ‖ K )
//! M2 = SHA1( A ‖ M1 ‖ K )
//! ```
//!
//! # Byte order — every one of these matters
//!
//! - Every big integer on the wire (`s`, `v`, `A`, `B`) is **exactly 32 bytes,
//!   little-endian, zero padded**, and that same encoding is what goes into SHA-1.
//! - `x` is little-endian from its digest; `u` is big-endian from its four bytes.
//! - `I` hashes `N` in its little-endian form. Hashing it big-endian gives a wrong `I`
//!   that looks plausible and fails every proof.
//! - NLS version 1 (logon type `0x01`, pre-1.13 clients) reverses `N`'s byte order. This
//!   implements version 2 (logon type `0x02`), which every classic client since 2003 uses.
//!
//! # What the server holds
//!
//! `(s, v)`. The verifier permits *server* impersonation but not *client* impersonation —
//! recovering `x` from `v` is a discrete log — which is why it can be cached at a
//! semi-trusted federation node (`docs/FEDERATION.md` §4) while an XSHA-1 digest cannot.
//!
//! Everything here is a pure function; the caller supplies randomness (`a`, `b`, `s`) as
//! 32-byte arrays, which keeps the crate free of an RNG dependency and the tests exact.

use num_bigint::BigUint;
use num_traits::Zero;
use sha1::{Digest, Sha1};

pub use crate::xsha1::proofs_match;

/// Blizzard's SRP generator, `g = 47`.
pub const G: u32 = 47;

/// Blizzard's 256-bit SRP modulus, written big-endian (the order a hex dump reads in).
///
/// NLS version 1 reverses this byte order relative to version 2.
pub const N_BE: [u8; 32] = [
    0xF8, 0xFF, 0x1A, 0x8B, 0x61, 0x99, 0x18, 0x03, 0x21, 0x86, 0xB6, 0x8C, 0xA0, 0x92, 0xB5,
    0x55, 0x7E, 0x97, 0x6C, 0x78, 0xC7, 0x32, 0x12, 0xD9, 0x12, 0x16, 0xF6, 0x65, 0x85, 0x23,
    0xC7, 0x87,
];

/// `I = SHA1(g) xor SHA1(N)`, the constant folded into every `M1`.
///
/// `g` is hashed as the single byte `0x2F` and `N` as its 32 little-endian bytes. Read as
/// a big-endian number this is the `F8018CF0…6C` value quoted in the javaop write-up.
pub const I: [u8; 20] = [
    0x6C, 0x0E, 0x97, 0xED, 0x0A, 0xF9, 0x6B, 0xAB, 0xB1, 0x58, 0x89, 0xEB, 0x8B, 0xBA, 0x25,
    0xA4, 0xF0, 0x8C, 0x01, 0xF8,
];

/// Size of every big-integer field on the wire.
pub const FIELD_LEN: usize = 32;
/// Size of the session key `K`.
pub const KEY_LEN: usize = 40;
/// Size of a proof (`M1`, `M2`) and of every SHA-1 digest here.
pub const PROOF_LEN: usize = 20;

fn modulus() -> BigUint {
    BigUint::from_bytes_be(&N_BE)
}

fn generator() -> BigUint {
    BigUint::from(G)
}

fn sha1(parts: &[&[u8]]) -> [u8; PROOF_LEN] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Encode as exactly 32 little-endian bytes. Every value here is reduced mod `N` (256 bits)
/// before encoding, so it always fits; the `debug_assert` documents that invariant.
fn to_le32(v: &BigUint) -> [u8; FIELD_LEN] {
    let bytes = v.to_bytes_le();
    debug_assert!(bytes.len() <= FIELD_LEN, "value wider than 256 bits");
    let mut out = [0u8; FIELD_LEN];
    let n = bytes.len().min(FIELD_LEN);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

fn from_le(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_le(bytes)
}

/// `upper(C)` — the client folds the account name to upper case before hashing. Names are
/// ASCII by our validator; the client's own fold is ASCII too.
fn upper(s: &str) -> Vec<u8> {
    s.as_bytes().to_ascii_uppercase()
}

/// The private key `x = SHA1(s ‖ SHA1(upper(C) ‖ ":" ‖ upper(P)))`, read little-endian.
#[must_use]
pub fn private_key(username: &str, password: &str, salt: &[u8; FIELD_LEN]) -> BigUint {
    let mut inner_input = upper(username);
    inner_input.push(b':');
    inner_input.extend_from_slice(&upper(password));
    let inner = sha1(&[&inner_input]);
    from_le(&sha1(&[salt, &inner]))
}

/// The verifier `v = g^x mod N` the server stores beside the salt.
///
/// This is what a client computes for `SID_AUTH_ACCOUNTCREATE` (0x52), and what the
/// server computes itself when an operator resets a WarCraft III password from plaintext.
#[must_use]
pub fn verifier(username: &str, password: &str, salt: &[u8; FIELD_LEN]) -> [u8; FIELD_LEN] {
    let x = private_key(username, password, salt);
    to_le32(&generator().modpow(&x, &modulus()))
}

/// Reduce a 32-byte random value to a usable private exponent in `[1, N)`.
///
/// Reducing mod `N` introduces a bias of about `2^-256`, which is irrelevant; a result of
/// zero (probability `2^-256`) is bumped to one so `g^a` is never `1`.
fn exponent(random: &[u8; FIELD_LEN]) -> BigUint {
    let e = from_le(random) % modulus();
    if e.is_zero() {
        BigUint::from(1u32)
    } else {
        e
    }
}

/// The client's public key `A = g^a mod N`, sent in `SID_AUTH_ACCOUNTLOGON`.
#[must_use]
pub fn client_public(a: &[u8; FIELD_LEN]) -> [u8; FIELD_LEN] {
    to_le32(&generator().modpow(&exponent(a), &modulus()))
}

/// The server's public key `B = (v + g^b) mod N`, sent back in `SID_AUTH_ACCOUNTLOGON`.
#[must_use]
pub fn server_public(verifier: &[u8; FIELD_LEN], b: &[u8; FIELD_LEN]) -> [u8; FIELD_LEN] {
    let n = modulus();
    let gb = generator().modpow(&exponent(b), &n);
    to_le32(&((from_le(verifier) + gb) % n))
}

/// The scrambler `u`: the first four bytes of `SHA1(B)` read big-endian.
#[must_use]
pub fn scrambler(server_public: &[u8; FIELD_LEN]) -> u32 {
    let h = sha1(&[server_public]);
    u32::from_be_bytes([h[0], h[1], h[2], h[3]])
}

/// The session key `K`: `S` split into even- and odd-index bytes, each SHA-1'd, the
/// digests interleaved back (`K[2i] = even[i]`, `K[2i+1] = odd[i]`).
#[must_use]
pub fn session_key(shared_secret: &[u8; FIELD_LEN]) -> [u8; KEY_LEN] {
    let mut even_in = [0u8; FIELD_LEN / 2];
    let mut odd_in = [0u8; FIELD_LEN / 2];
    for i in 0..FIELD_LEN / 2 {
        even_in[i] = shared_secret[2 * i];
        odd_in[i] = shared_secret[2 * i + 1];
    }
    let even = sha1(&[&even_in]);
    let odd = sha1(&[&odd_in]);
    let mut k = [0u8; KEY_LEN];
    for i in 0..PROOF_LEN {
        k[2 * i] = even[i];
        k[2 * i + 1] = odd[i];
    }
    k
}

/// `M1 = SHA1(I ‖ SHA1(upper(C)) ‖ s ‖ A ‖ B ‖ K)`.
#[must_use]
pub fn client_proof_from_key(
    username: &str,
    salt: &[u8; FIELD_LEN],
    client_public: &[u8; FIELD_LEN],
    server_public: &[u8; FIELD_LEN],
    key: &[u8; KEY_LEN],
) -> [u8; PROOF_LEN] {
    let name_hash = sha1(&[&upper(username)]);
    sha1(&[&I, &name_hash, salt, client_public, server_public, key])
}

/// `M2 = SHA1(A ‖ M1 ‖ K)`, the server's proof to the client.
#[must_use]
pub fn server_proof_from_key(
    client_public: &[u8; FIELD_LEN],
    client_proof: &[u8; PROOF_LEN],
    key: &[u8; KEY_LEN],
) -> [u8; PROOF_LEN] {
    sha1(&[client_public, client_proof, key])
}

/// Client side: compute `K` and `M1` for `SID_AUTH_ACCOUNTLOGONPROOF` from the server's
/// `(s, B)` challenge. Returns `None` if `B mod N == 0` (RFC 2945 §3: abort).
#[must_use]
pub fn client_proof(
    username: &str,
    password: &str,
    salt: &[u8; FIELD_LEN],
    a: &[u8; FIELD_LEN],
    server_public: &[u8; FIELD_LEN],
) -> Option<([u8; PROOF_LEN], [u8; KEY_LEN])> {
    let n = modulus();
    let big_b = from_le(server_public) % &n;
    if big_b.is_zero() {
        return None;
    }
    let x = private_key(username, password, salt);
    let a = exponent(a);
    let big_a = to_le32(&generator().modpow(&a, &n));
    let u = BigUint::from(scrambler(server_public));
    let v = generator().modpow(&x, &n);
    // ((N + B − v) mod N) ^ (a + u·x) mod N
    let base = (&n + big_b - v) % &n;
    let s = base.modpow(&(a + u * x), &n);
    let key = session_key(&to_le32(&s));
    Some((client_proof_from_key(username, salt, &big_a, server_public, &key), key))
}

/// Server side: given the stored `(s, v)`, this logon's `b`/`B`, and the client's `A` and
/// `M1`, verify the client and return `M2` on success.
///
/// Returns `None` when `A mod N == 0` (RFC 2945 §3: abort, the client is trying to force
/// a trivial shared secret) or when the proofs disagree (wrong password). The comparison
/// is constant-time.
#[must_use]
pub fn server_verify(
    username: &str,
    salt: &[u8; FIELD_LEN],
    verifier: &[u8; FIELD_LEN],
    b: &[u8; FIELD_LEN],
    client_public: &[u8; FIELD_LEN],
    server_public: &[u8; FIELD_LEN],
    client_proof: &[u8; PROOF_LEN],
) -> Option<[u8; PROOF_LEN]> {
    let n = modulus();
    let big_a = from_le(client_public) % &n;
    if big_a.is_zero() {
        return None;
    }
    let u = BigUint::from(scrambler(server_public));
    let v = from_le(verifier);
    // (A · v^u mod N) ^ b mod N
    let base = (big_a * v.modpow(&u, &n)) % &n;
    let s = base.modpow(&exponent(b), &n);
    let key = session_key(&to_le32(&s));
    let expected = client_proof_from_key(username, salt, client_public, server_public, &key);
    if proofs_match(&expected, client_proof) {
        Some(server_proof_from_key(client_public, client_proof, &key))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn arr32(s: &str) -> [u8; 32] {
        hex(s).try_into().expect("32 bytes")
    }

    fn arr20(s: &str) -> [u8; 20] {
        hex(s).try_into().expect("20 bytes")
    }

    /// Deterministic bytes for tests that want variety without an RNG dependency.
    fn pseudo_random(seed: u32, counter: u32) -> [u8; 32] {
        let a = sha1(&[&seed.to_le_bytes(), &counter.to_le_bytes(), b"a"]);
        let b = sha1(&[&seed.to_le_bytes(), &counter.to_le_bytes(), b"b"]);
        let mut out = [0u8; 32];
        out[..20].copy_from_slice(&a);
        out[20..].copy_from_slice(&b[..12]);
        out
    }

    #[test]
    fn the_i_constant_is_sha1_g_xor_sha1_n_little_endian() {
        let hg = sha1(&[&[0x2F]]);
        let mut n_le = N_BE;
        n_le.reverse();
        let hn = sha1(&[&n_le]);
        let mut i = [0u8; 20];
        for k in 0..20 {
            i[k] = hg[k] ^ hn[k];
        }
        assert_eq!(i, I);
        // Read as a big-endian number it is the value the javaop write-up quotes.
        assert_eq!(
            BigUint::from_bytes_le(&I),
            BigUint::from_bytes_be(&hex("F8018CF0A425BA8BEB8958B1AB6BF90AED970E6C"))
        );
    }

    /// Vectors from `scripts/nls_reference.py`, an independent implementation of the
    /// same specification. They pin every byte-order rule at once.
    #[test]
    fn known_answer_vectors() {
        let username = "Tagban";
        let password = "hunter2";
        let salt: [u8; 32] = core::array::from_fn(|i| i as u8);
        let a: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
        let b: [u8; 32] = core::array::from_fn(|i| i as u8 + 2);

        assert_eq!(
            private_key(username, password, &salt),
            BigUint::from_bytes_be(&hex("395e620c90a3919c75ede2fdd52aaf8cd6b28d39"))
        );
        let v = verifier(username, password, &salt);
        assert_eq!(v, arr32("7f2bf7dcd443e0f945a5a7bcbfec2f5e3fee8242d09b62aac46c194963dcafef"));
        let big_a = client_public(&a);
        assert_eq!(big_a, arr32("f085e0261e42a247db6e0bb57ae40d56746b0c16783606718a2c500d3fed2594"));
        let big_b = server_public(&v, &b);
        assert_eq!(big_b, arr32("423ac3a9427c556639f31804d59e3e2c409e182add1648440ed88622abb94580"));
        assert_eq!(scrambler(&big_b), 0x672c_b5c0);

        let (m1, key) = client_proof(username, password, &salt, &a, &big_b).expect("B != 0");
        assert_eq!(
            key.to_vec(),
            hex("2eeb577075dfcbdd4ec5c17e13c8df4efc8bc573ce35f665c6a913bf0c6e2febf0b0c23a00428941")
        );
        assert_eq!(m1, arr20("a4d99ac492f45207ce0ed8e1c9729f38a425d151"));

        let m2 = server_verify(username, &salt, &v, &b, &big_a, &big_b, &m1).expect("accepted");
        assert_eq!(m2, arr20("ba079a3bfcf36d706f1df1dd2934c3252aad086f"));
    }

    #[test]
    fn client_and_server_agree_for_arbitrary_inputs() {
        for i in 0..40u32 {
            let salt = pseudo_random(1, i);
            let a = pseudo_random(2, i);
            let b = pseudo_random(3, i);
            let v = verifier("Zealot", "correct horse", &salt);
            let big_a = client_public(&a);
            let big_b = server_public(&v, &b);
            let (m1, key) = client_proof("Zealot", "correct horse", &salt, &a, &big_b).unwrap();
            let m2 = server_verify("Zealot", &salt, &v, &b, &big_a, &big_b, &m1)
                .expect("the right password is accepted");
            assert_eq!(m2, server_proof_from_key(&big_a, &m1, &key));
        }
    }

    #[test]
    fn a_wrong_password_is_refused() {
        let salt = pseudo_random(4, 0);
        let (a, b) = (pseudo_random(5, 0), pseudo_random(6, 0));
        let v = verifier("Zealot", "right", &salt);
        let big_a = client_public(&a);
        let big_b = server_public(&v, &b);
        let (m1, _) = client_proof("Zealot", "wrong", &salt, &a, &big_b).unwrap();
        assert!(server_verify("Zealot", &salt, &v, &b, &big_a, &big_b, &m1).is_none());
    }

    #[test]
    fn name_and_password_are_folded_to_upper_case() {
        let salt = pseudo_random(7, 0);
        assert_eq!(verifier("Tagban", "hunter2", &salt), verifier("TAGBAN", "HUNTER2", &salt));
        assert_eq!(verifier("tagban", "Hunter2", &salt), verifier("TaGbAn", "hUnTeR2", &salt));
        // And the name inside M1 too: a client logging on as "tagban" must match the
        // verifier created as "Tagban".
        let (a, b) = (pseudo_random(8, 0), pseudo_random(9, 0));
        let v = verifier("Tagban", "hunter2", &salt);
        let big_a = client_public(&a);
        let big_b = server_public(&v, &b);
        let (m1, _) = client_proof("tagban", "hunter2", &salt, &a, &big_b).unwrap();
        assert!(server_verify("TAGBAN", &salt, &v, &b, &big_a, &big_b, &m1).is_some());
    }

    #[test]
    fn a_salt_with_a_zero_top_byte_still_works() {
        // A minimal-length encoding would drop the leading zero and break M1; the fixed
        // 32-byte encoding must not. Same for A/B/K, covered statistically above.
        let mut salt = [0u8; 32];
        salt[0] = 1;
        let (a, b) = (pseudo_random(10, 0), pseudo_random(11, 0));
        let v = verifier("Zealot", "pw", &salt);
        let big_a = client_public(&a);
        let big_b = server_public(&v, &b);
        let (m1, _) = client_proof("Zealot", "pw", &salt, &a, &big_b).unwrap();
        assert!(server_verify("Zealot", &salt, &v, &b, &big_a, &big_b, &m1).is_some());
    }

    #[test]
    fn a_zero_client_key_is_refused_before_any_arithmetic() {
        // A ≡ 0 (mod N) forces S = 0 regardless of the password. Both the literal zero and
        // N itself (which reduces to zero) must be rejected.
        let salt = pseudo_random(12, 0);
        let b = pseudo_random(13, 0);
        let v = verifier("Zealot", "pw", &salt);
        let big_b = server_public(&v, &b);
        let m1 = [0u8; 20];
        assert!(server_verify("Zealot", &salt, &v, &b, &[0u8; 32], &big_b, &m1).is_none());
        let mut n_le = N_BE;
        n_le.reverse();
        assert!(server_verify("Zealot", &salt, &v, &b, &n_le, &big_b, &m1).is_none());
        // Symmetrically the client refuses B ≡ 0.
        let a = pseudo_random(14, 0);
        assert!(client_proof("Zealot", "pw", &salt, &a, &[0u8; 32]).is_none());
    }

    #[test]
    fn public_keys_are_always_32_bytes_and_below_n() {
        let n = modulus();
        for i in 0..20u32 {
            let big_a = client_public(&pseudo_random(15, i));
            assert!(BigUint::from_bytes_le(&big_a) < n);
            let v = verifier("x", "y", &pseudo_random(16, i));
            let big_b = server_public(&v, &pseudo_random(17, i));
            assert!(BigUint::from_bytes_le(&big_b) < n);
        }
    }
}
