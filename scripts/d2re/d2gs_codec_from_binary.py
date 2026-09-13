#!/usr/bin/env python3
"""The D2GS server->client codec, re-derived from Game.exe 1.14d rather than from libd2.

    d2gs_codec_from_binary.py <Game.exe>

Ports three engine routines line for line from their decompilation:
  0x0040ADB0  builds the canonical code table from the 256 code lengths at 0x7076C0
  0x0040B1B0  compresses a packet buffer (MSB-first bit packing)
  0x0052B330  frames it: [len+1] below 0xF0, else [((len+2)>>8)|0xF0, (len+2)&0xFF]

then checks the result against the one frame libd2 captured off a live 1.14d server
(packages/util/src/huffman_test.zig, MIT, (c) 2026 jaenster). See docs/D2GS-114D-WIRE.md §3.
"""
import struct
import sys

if len(sys.argv) != 2:
    sys.exit(__doc__)
d = open(sys.argv[1], "rb").read()
pe = struct.unpack_from("<I", d, 0x3C)[0]
nsec = struct.unpack_from("<H", d, pe + 6)[0]
base = struct.unpack_from("<I", d, pe + 24 + 28)[0]
off = pe + 24 + struct.unpack_from("<H", d, pe + 20)[0]
for i in range(nsec):
    vsz, va, rsz, raw = struct.unpack_from("<IIII", d, off + 8 + 40 * i)
    if base + va <= 0x7076C0 < base + va + rsz:
        LENGTHS = list(d[raw + 0x7076C0 - base - va:][:256])


def build_codes(lengths):
    """0x0040ADB0, encoder half: sort symbols by descending length, then
    code[next] = (code[prev] + 1) >> (len[prev] - len[next]), stored in a byte."""
    count = [0] * 16
    for n in lengths:
        count[n] += 1
    for i in range(15, 0, -1):
        count[i], count[0] = count[0], count[0] + count[i]
    work = [0] * 512  # lengths in [0, 256), symbols in [256, 512) — one array, as the engine has it
    for sym in range(256):
        work[0x100 + count[lengths[sym]]] = sym
        count[lengths[sym]] += 1
    for k in range(256):
        work[k] = lengths[work[0x100 + k]]
    codes = [0] * 256
    for i in range(255):
        codes[work[0x101 + i]] = ((codes[work[0x100 + i]] + 1) >> ((work[i] - work[i + 1]) & 0x1F)) & 0xFF
    return codes


def compress(src, lengths, codes):
    """0x0040B1B0."""
    out, acc, free = bytearray(), 0, 8
    for b in src:
        n, code = lengths[b], codes[b]
        if free <= n:
            while True:
                n -= free
                out.append(((code >> n) | acc) & 0xFF)
                acc, free = 0, 8
                if n <= 7:
                    break
        if n:
            free -= n
            acc = (acc | (code << free)) & 0xFF
    if free < 8:
        out.append(acc)
    return bytes(out)


def frame(body):
    """0x0052B330, mode 0, first byte not 0xAF. The length counts the header itself."""
    if len(body) + 1 < 0xF0:
        return bytes([len(body) + 1]) + body
    total = len(body) + 2
    return bytes([(total >> 8) | 0xF0, total & 0xFF]) + body


CODES = build_codes(LENGTHS)
plain = bytes.fromhex("010004001000010000")  # 0x01 GameFlags (8) + 0x00 (1), one flush
wire = compress(plain, LENGTHS, CODES)
print("compressed", wire.hex(), "== live capture 7a09a5f0:", wire == bytes.fromhex("7a09a5f0"))
print("on the wire", frame(wire).hex())
sys.exit(0 if wire == bytes.fromhex("7a09a5f0") else 1)
