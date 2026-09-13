#!/usr/bin/env python3
"""Diff jaenster/libd2's recovered D2GS packet tables against a real 1.14d Game.exe.

    verify_libd2_tables.py <Game.exe> <libd2 checkout>

Reads the tables straight out of the PE image, so nothing from Game.exe is stored here.
Exit status 1 on any mismatch. See docs/D2GS-114D-WIRE.md §2.
"""
import os
import re
import struct
import sys

if len(sys.argv) != 3:
    sys.exit(__doc__)
EXE, LIBD2 = sys.argv[1], sys.argv[2]
NET = os.path.join(LIBD2, "packages/net/src")
UTIL = os.path.join(LIBD2, "packages/util/src")

d = open(EXE, "rb").read()
pe = struct.unpack_from("<I", d, 0x3C)[0]
nsec = struct.unpack_from("<H", d, pe + 6)[0]
base = struct.unpack_from("<I", d, pe + 24 + 28)[0]
off = pe + 24 + struct.unpack_from("<H", d, pe + 20)[0]
sections = [struct.unpack_from("<IIII", d, off + 8 + 40 * i) for i in range(nsec)]

version = d.find("FileVersion".encode("utf-16-le"))
print("Game.exe", d[version:version + 40].decode("utf-16-le", "ignore").replace("\0", " "))


def file_offset(va):
    """VA -> file offset, initialised bytes only."""
    for vsz, sva, rsz, raw in sections:
        if base + sva <= va < base + sva + rsz:
            return raw + va - base - sva
    raise ValueError(f"0x{va:x} is not initialised data")


def i32s(va, n):
    return list(struct.unpack_from("<%di" % n, d, file_offset(va)))


def zig_i16_array(path, name):
    src = open(path).read()
    body = re.search(r"pub const %s = \[[^\]]*\]i16\{(.*?)\n\};" % name, src, re.S).group(1)
    return [int(x) for x in re.findall(r"-?\d+", re.sub(r"//[^\n]*", "", body))]


fails = 0


def report(label, bad):
    global fails
    fails += len(bad)
    print(f"{label}: {'OK' if not bad else 'MISMATCH ' + repr(bad)}")


# S->C framing sizes, NET_D2GS_CLIENT_INCOMING_SIZE
sc = zig_i16_array(os.path.join(NET, "sc.zig"), "SC_SIZE")
real = i32s(0x730AE8, len(sc))
report(f"S->C sizes   @0x730AE8 ({len(sc)})", [(hex(i), sc[i], real[i]) for i in range(len(sc)) if sc[i] != real[i]])

# C->S framing sizes, NET_D2GS_CLIENT_OUTGOING_SIZE
cs = zig_i16_array(os.path.join(NET, "cs.zig"), "OUTGOING_SIZE")
real = i32s(0x730DC0, len(cs))
report(f"C->S sizes   @0x730DC0 ({len(cs)})", [(hex(i), cs[i], real[i]) for i in range(len(cs)) if cs[i] != real[i]])

# S->C handler table: 12-byte {handler, size, unit_handler}
rows = re.findall(r'\.handler = "([^"]+)", \.expected_size = (-?\d+) \}, // 0x([0-9a-f]+)',
                  open(os.path.join(NET, "sc_table.zig")).read())
table = file_offset(0x7114D0)
bad, renamed = [], []
for name, expected, op in rows:
    op = int(op, 16)
    handler, size, unit_handler = struct.unpack_from("<IiI", d, table + 12 * op)
    if int(expected) != size:
        bad.append((hex(op), name, int(expected), size))
    cited = re.search(r"_([0-9a-f]{8})$", name)
    if cited and int(cited.group(1), 16) not in (handler, unit_handler):
        bad.append((hex(op), name, "cites", hex(handler)))
    elif cited and int(cited.group(1), 16) != handler:
        renamed.append(hex(op))
report(f"S->C handlers @0x7114D0 ({len(rows)})", bad)
if renamed:
    print("  named after the second (unit) callback slot:", " ".join(renamed))

# Wire Huffman code lengths + bit masks
src = open(os.path.join(UTIL, "huffman.zig")).read()
lengths = [int(x, 16) for x in re.findall(r"0x([0-9a-f]{2})",
           re.search(r"default_code_lengths = \[256\]u8\{(.*?)\};", src, re.S).group(1))]
report("huffman lengths @0x7076C0", [] if lengths == list(d[file_offset(0x7076C0):][:256]) else ["differs"])
masks = [int(x, 16) for x in re.findall(r"0x([0-9a-f]+)",
         re.search(r"bit_masks = \[16\]u32\{(.*?)\};", src, re.S).group(1))]
report("huffman masks   @0x7077C0", [] if masks == list(struct.unpack_from("<16I", d, file_offset(0x7077C0))) else ["differs"])

sys.exit(1 if fails else 0)
