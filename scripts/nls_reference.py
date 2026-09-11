#!/usr/bin/env python3
"""Reference implementation of Blizzard's NLS (WarCraft III SRP variant).

Written from the public specification (BNETDocs "NLS/SRP Protocol", the javaop write-up,
RFC 2945 for baseline SRP) and cross-checked against tagban's own XSRP3a.cpp.
It is NOT derived from PvPGN's bnetsrp3.cpp (AGPL) -- that file was never opened.

Purpose: pin down every byte-order rule in one runnable place, prove the client and
server sides agree, and print deterministic known-answer vectors for the Rust tests in
crates/bnetcc-crypto.

Conventions (all confirmed below by the I-constant check and the round trip):
  * Every big integer on the wire is 32 bytes, LITTLE-endian, zero padded.
  * Every big integer fed to SHA-1 is that same 32-byte LE encoding (K is 40 bytes).
  * x is the 20-byte SHA-1 digest read as a LITTLE-endian integer.
  * u is the first 4 bytes of SHA-1(B) read as a BIG-endian integer.
  * Username and password are upper-cased before hashing (client side only; the server
    never sees a password, it stores (salt, verifier) as sent by SID_AUTH_ACCOUNTCREATE).
"""
import hashlib
import secrets

N_HEX = "F8FF1A8B619918032186B68CA092B5557E976C78C73212D91216F6658523C787"
N = int(N_HEX, 16)
G = 47


def sha1(*parts: bytes) -> bytes:
    h = hashlib.sha1()
    for p in parts:
        h.update(p)
    return h.digest()


def to_le32(n: int) -> bytes:
    return n.to_bytes(32, "little")


def from_le(b: bytes) -> int:
    return int.from_bytes(b, "little")


# I = SHA1(g) xor SHA1(N), with g as the single byte 0x2F and N as 32 LE bytes.
I_BYTES = bytes(a ^ b for a, b in zip(sha1(bytes([G])), sha1(to_le32(N))))
assert I_BYTES.hex().upper() == "6C0E97ED0AF96BABB15889EB8BBA25A4F08C01F8"
# The same value printed as a big-endian number is the constant quoted by javaop and by
# XSRP3a.cpp: F8018CF0A425BA8BEB8958B1AB6BF90AED970E6C.
assert int.from_bytes(I_BYTES, "little") == int("F8018CF0A425BA8BEB8958B1AB6BF90AED970E6C", 16)


def private_x(username: str, password: str, salt: bytes) -> int:
    inner = sha1((username.upper() + ":" + password.upper()).encode("ascii"))
    return from_le(sha1(salt, inner))


def verifier(username: str, password: str, salt: bytes) -> bytes:
    return to_le32(pow(G, private_x(username, password, salt), N))


def scrambler(B: bytes) -> int:
    return int.from_bytes(sha1(B)[:4], "big")


def session_key(S: int) -> bytes:
    s = to_le32(S)
    even = sha1(s[0::2])  # bytes at even indexes
    odd = sha1(s[1::2])   # bytes at odd indexes
    K = bytearray(40)
    K[0::2] = even
    K[1::2] = odd
    return bytes(K)


def m1(username: str, salt: bytes, A: bytes, B: bytes, K: bytes) -> bytes:
    return sha1(I_BYTES, sha1(username.upper().encode("ascii")), salt, A, B, K)


def m2(A: bytes, M1: bytes, K: bytes) -> bytes:
    return sha1(A, M1, K)


# ---- client side ---------------------------------------------------------------
def client_public(a: int) -> bytes:
    return to_le32(pow(G, a, N))


def client_proof(username: str, password: str, salt: bytes, a: int, A: bytes, B: bytes):
    x = private_x(username, password, salt)
    u = scrambler(B)
    v = pow(G, x, N)
    S = pow((N + from_le(B) - v) % N, a + u * x, N)
    K = session_key(S)
    return m1(username, salt, A, B, K), K


# ---- server side ---------------------------------------------------------------
def server_public(v: bytes, b: int) -> bytes:
    return to_le32((from_le(v) + pow(G, b, N)) % N)


def server_proof(username: str, salt: bytes, v: bytes, b: int, A: bytes, B: bytes):
    u = scrambler(B)
    S = pow((from_le(A) * pow(from_le(v), u, N)) % N, b, N)
    K = session_key(S)
    return m1(username, salt, A, B, K), K


def round_trip(username, password, salt, a, b):
    v = verifier(username, password, salt)          # SID_AUTH_ACCOUNTCREATE carries (salt, v)
    A = client_public(a)                             # C>S 0x53: A, username
    B = server_public(v, b)                          # S>C 0x53: status, salt, B
    M1_c, K_c = client_proof(username, password, salt, a, A, B)  # C>S 0x54: M1
    M1_s, K_s = server_proof(username, salt, v, b, A, B)
    assert K_c == K_s, "session keys differ"
    assert M1_c == M1_s, "M1 differs"
    M2 = m2(A, M1_s, K_s)                            # S>C 0x54: status, M2
    return v, A, B, M1_s, K_s, M2


if __name__ == "__main__":
    # Random round trips prove the two sides agree for arbitrary inputs.
    for _ in range(50):
        round_trip("Tagban", "hunter2", secrets.token_bytes(32),
                   secrets.randbelow(N), secrets.randbelow(N))
    # Edge: values whose top byte is zero (where a minimal-length encoding would break).
    salt = bytes(31) + b"\x01"
    round_trip("tagban", "HUNTER2", salt, 3, 5)

    # Deterministic known-answer vectors for the Rust tests.
    username, password = "Tagban", "hunter2"
    salt = bytes(range(32))
    a = from_le(bytes(range(1, 33)))
    b = from_le(bytes(range(2, 34)))
    v, A, B, M1, K, M2 = round_trip(username, password, salt, a, b)
    print("username:", username, " password:", password)
    print("salt    :", salt.hex())
    print("x (int) :", hex(private_x(username, password, salt)))
    print("v       :", v.hex())
    print("a       :", to_le32(a).hex())
    print("A       :", A.hex())
    print("b       :", to_le32(b).hex())
    print("B       :", B.hex())
    print("u       :", hex(scrambler(B)))
    print("K       :", K.hex())
    print("M1      :", M1.hex())
    print("M2      :", M2.hex())
    print("I       :", I_BYTES.hex())
    print("all round trips OK")
