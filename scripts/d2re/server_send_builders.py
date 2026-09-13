#!/usr/bin/env python3
"""Map the 1.14d game server's S->C packet builders: opcode -> builder address.

    server_send_builders.py <Game.exe> [--tsv out.tsv]

Every normal S->C packet is queued through 0x0053B280 (append to the client's outbound
buffer; see docs/D2GS-114D-WIRE.md §3). Most builders have the same shape:

    PUSH len                  6A nn | 68 nn nn nn nn
    LEA  r32, [EBP + d]       8D 45|4D|55|5D|75|7D d8   (or the disp32 forms)
    ...
    MOV  byte ptr [EBP + d], opcode     C6 45 d8 imm8   (or C6 85 d32 imm8)
    CALL 0x0053B280

The opcode comes from the byte written at the same stack slot the LEA points to. A match is
only accepted if `len` agrees with the verified S->C size table at 0x730AE8 (or that entry is
variable, -1), so a wrong guess shows up as unmatched rather than as a wrong row.
Builders whose opcode is passed in by the caller (DL, or a prebuilt struct) are listed as such.
No external disassembler needed; nothing from Game.exe is written unless --tsv is given.
"""
import struct
import sys

args = sys.argv[1:]
if not args:
    sys.exit(__doc__)
exe = args[0]
tsv = args[args.index("--tsv") + 1] if "--tsv" in args else None

d = open(exe, "rb").read()
pe = struct.unpack_from("<I", d, 0x3C)[0]
nsec = struct.unpack_from("<H", d, pe + 6)[0]
base = struct.unpack_from("<I", d, pe + 24 + 28)[0]
off = pe + 24 + struct.unpack_from("<H", d, pe + 20)[0]
sections = []
for i in range(nsec):
    name = d[off + 40 * i:off + 40 * i + 8].rstrip(b"\0").decode()
    vsz, va, rsz, raw = struct.unpack_from("<IIII", d, off + 8 + 40 * i)
    sections.append((name, base + va, rsz, raw))
text = next(s for s in sections if s[0] == ".text")


def va_to_off(va):
    for _, sva, rsz, raw in sections:
        if sva <= va < sva + rsz:
            return raw + va - sva
    raise ValueError(hex(va))


def off_to_va(o):
    for _, sva, rsz, raw in sections:
        if raw <= o < raw + rsz:
            return sva + o - raw
    raise ValueError(hex(o))


QUEUE = 0x0053B280
SC_SIZE = list(struct.unpack_from("<181i", d, va_to_off(0x730AE8)))

t0, t1 = text[3], text[3] + text[2]
calls = []
for o in range(t0, t1 - 5):
    if d[o] == 0xE8 and (off_to_va(o) + 5 + struct.unpack_from("<i", d, o + 1)[0]) & 0xFFFFFFFF == QUEUE:
        calls.append(o)

LEA_MODRM8 = {0x45, 0x4D, 0x55, 0x5D, 0x75, 0x7D}
LEA_MODRM32 = {0x85, 0x8D, 0x95, 0x9D, 0xB5, 0xBD}


def function_start(o):
    """Walk back to the nearest `push ebp; mov ebp, esp` prologue."""
    for p in range(o, max(t0, o - 0x800), -1):
        if d[p:p + 3] == b"\x55\x8b\xec":
            return p
    return None


def analyse(call):
    start = function_start(call) or call - 0x80
    window = range(start, call)
    # stack slot the buffer pointer is loaded from: last LEA r32,[EBP+d] before the call
    slot = None
    for p in window:
        if d[p] == 0x8D and d[p + 1] in LEA_MODRM8:
            slot = struct.unpack_from("<b", d, p + 2)[0]
        elif d[p] == 0x8D and d[p + 1] in LEA_MODRM32:
            slot = struct.unpack_from("<i", d, p + 2)[0]
    # length: last PUSH imm before the call
    length = None
    for p in window:
        if d[p] == 0x6A:
            length = d[p + 1]
        elif d[p] == 0x68:
            length = struct.unpack_from("<I", d, p + 1)[0]
    if slot is None:
        return start, None, length, "buffer passed in"
    opcode = None
    for p in window:
        if d[p] == 0xC6 and d[p + 1] == 0x45 and struct.unpack_from("<b", d, p + 2)[0] == slot:
            opcode = d[p + 3]
        elif d[p] == 0xC6 and d[p + 1] == 0x85 and struct.unpack_from("<i", d, p + 2)[0] == slot:
            opcode = d[p + 6]
    if opcode is None:
        return start, None, length, "opcode set by caller"
    if opcode > 0xB4:
        return start, opcode, length, "not an S->C opcode"
    want = SC_SIZE[opcode]
    if want == -1 or length == want:
        return start, opcode, length, "ok" if want != -1 else "ok (variable)"
    return start, opcode, length, f"size {length} != table {want}"


def callers_passing_dl(fn_off):
    """Call sites of a builder whose opcode arrives in DL: the last `MOV DL, imm8` (B2 nn)
    within 0x30 bytes before each `CALL fn`."""
    target = off_to_va(fn_off)
    found = []
    for o in range(t0, t1 - 5):
        if d[o] != 0xE8 or (off_to_va(o) + 5 + struct.unpack_from("<i", d, o + 1)[0]) & 0xFFFFFFFF != target:
            continue
        dl = None
        for p in range(o - 0x30, o):
            if d[p] == 0xB2:
                dl = d[p + 1]
            elif d[p] == 0x32 and d[p + 1] == 0xD2:  # XOR DL, DL
                dl = 0
        found.append((off_to_va(o), dl))
    return found


rows = []
for c in calls:
    start, op, length, verdict = analyse(c)
    if verdict == "opcode set by caller" and start is not None and length is not None and length < 0x200:
        for site, dl in callers_passing_dl(start):
            if dl is None:
                rows.append((None, off_to_va(start), site, length, "caller: no MOV DL"))
            elif dl <= 0xB4 and SC_SIZE[dl] == length:
                rows.append((dl, off_to_va(start), site, length, "ok (opcode from caller DL)"))
            else:
                rows.append((dl, off_to_va(start), site, length,
                             f"caller DL 0x{dl:02x}: size {length} != table {SC_SIZE[dl] if dl <= 0xB4 else '-'}"))
        continue
    rows.append((op, off_to_va(start) if start else 0, off_to_va(c), length, verdict))

ok = [r for r in rows if r[4].startswith("ok")]
print(f"{len(calls)} queue call sites; {len(rows)} rows after expanding caller-supplied opcodes; "
      f"{len(ok)} size-checked; opcodes covered: {len({r[0] for r in ok})}")
grouped = {}
for op, fn, site, length, verdict in rows:
    key = (op if verdict.startswith("ok") else None, fn, verdict if not verdict.startswith("ok") else "ok")
    grouped.setdefault(key, []).append(site)
for (op, fn, verdict), sites in sorted(grouped.items(), key=lambda kv: (kv[0][0] is None, kv[0][0] or 0, kv[0][1])):
    ops = f"0x{op:02X}" if op is not None else "  ? "
    print(f"{ops}  builder 0x{fn:08X}  {len(sites):2d} site(s)  {verdict}")
if tsv:
    with open(tsv, "w") as f:
        for op, fn, site, length, verdict in rows:
            f.write(f"{'' if op is None else f'0x{op:02x}'}\t0x{fn:08x}\t0x{site:08x}\t{length}\t{verdict}\n")
