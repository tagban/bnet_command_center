# A native Rust Diablo II engine — plan

Goal (tagban, 2026-09-13): closed-realm Diablo II games on bnet.cc, served by an engine that runs
**natively on macOS, Linux and Windows** — a reimplementation, not a host for Blizzard's binary.
Longer term, the same engine underpins a native modern client (§6).

## 1. Why reimplement, and why it is not from zero

Every game server in `jaenster/d2-dedicated-server` runs Blizzard's own **32-bit x86** code (wine,
or the macOS 1.14d i386 image). Apple Silicon cannot execute that, so hosting it can never be
native here. A reimplementation can.

It starts from **[`jaenster/libd2`](https://github.com/jaenster/libd2)**: a Zig reimplementation of
the Diablo II **1.14d** engine core, **MIT** (the source; its Blizzard-derived data blobs and excel
tables are expressly *not* covered — see §4), about 91k lines:

| libd2 package | Lines | What it is | Evidence it matches the game |
|---|---|---|---|
| `drlg` | 31.8k | the seed-driven map generator — rooms, tiles, collision, objects, monster presets | **cell-exact** vs. retail dumps, 11.1M subtiles/seed, all acts, blind holdouts |
| `game` | 18.2k | the runtime: `GameInstance` server loop, units, stats, combat, skills (all 7 classes), monsters + AI, missiles, objects, shrines | rules read from the binary; determinism-tested; **not yet checked against a live server** |
| `formats` | 7.3k | MPQ (incl. protected), ds1, dt1, dc6/dcc/cof, `.d2s` header | parsers |
| `net` | 7.2k | the D2GS wire protocol, both directions, bit-packed packets | recovered layouts; not vs. live |
| `pathfinding` | 5.5k | routing with the server's movement gates | generated maps |
| `item` | 5.3k | treasure classes, quality, affixes | tables + rolls traced through the binary |
| `core`, `world`, `bnet`, `client`, `render`, `save`, `util` | ~15k | RNG, stats base, live world, realm protocol, client world model, tile art, `.d2s` read/write (byte-exact), Huffman codec | varies |

**Why porting matters even with a working original:** the retail client generates the map itself
from the game seed, so the server's world must match it cell for cell or players walk through
walls. libd2's `drlg` already does; porting it faithfully inherits that.

## 2. The reference models we compare against

1. **libd2 itself.** While porting, the Rust crate and the Zig package run on the same inputs
   (seed, difficulty, level; attack inputs; item rolls) and must produce identical output.
   Differential tests need no Blizzard data in the repo.
2. **Blizzard's real engine**, run by jaenster's `d2gs-native` in a `linux/386` container under
   `qemu` on this Mac — **test harness only, never production**. Record the D2GS packet stream for
   scripted sessions against it and against our server, and diff. This is how `game` and `net`,
   which libd2 has not yet checked against a live server, get checked. Needs the macOS 1.14d binary.
3. **Your retail 1.14d Windows client** — the final judge of every milestone.

Where libd2 and the binary disagree, the binary wins. `docs/D2GS-114D-WIRE.md` records what was
read directly from the 1.14d `Game.exe` — framing and compression (we send `AF 01` and compress,
as Blizzard's servers did), the join state machine, and the two libd2 files not to port as-is.

## 3. Shape

```
crates/d2-data      excel tables, read at runtime from the operator's own MPQs (never embedded)
crates/d2-formats   MPQ + ds1/dt1 (+ dc6/dcc/cof for a client later)
crates/d2-core      seed RNG, stats model, unit base
crates/d2-drlg      map generator            ◀─ first port, diffed against libd2
crates/d2-net       D2GS protocol + Huffman codec
crates/d2-item, d2-world, d2-pathfinding, d2-game   the runtime
bnetccd             realm (done) + a D2GS listener on :4000 hosting d2-game in-process
```

All plain Rust, no C, so it builds for macOS/Linux/Windows (and wasm) from one tree. The game
server runs **inside `bnetccd`** behind the `GameHost` seam (`docs/ROADMAP.md` Phase 4) — no
second process, no control link; `characters.save` already holds the `.d2s`.

## 4. Legal posture — decide knowingly

- **Game data**: libd2 commits Blizzard's 1.14d excel tables and derived blobs. We do not. Tables
  and archives are read at runtime from the operator's own install (`diablo2.data_dir`), exactly
  as `docs/ROADMAP.md` Phase 5 already requires. The repo ships no Blizzard bytes.
- **Code provenance**: libd2 calls itself clean-room, but its READMEs describe porting from a
  decompiled `Game.exe` ("every ported function cites its 1.14d address"). Code translated from a
  decompilation is a weaker position than a true two-team clean room — the MIT licence covers
  jaenster's work, it cannot license Blizzard's. Precedents exist on both sides (DevilutionX, a
  decompilation-derived Diablo I, is long-lived and public; bnetd was sued). `docs/LEGAL.md` §2 is
  the relevant section. **Decided 2026-09-13 (tagban): go ahead** — recorded in `docs/LEGAL.md`
  §2, "Decision: the Diablo II game server is built from decompilation".
- **Attribution**: each ported file carries jaenster's MIT notice and names its libd2 source.

## 5. Milestones

| # | Milestone | Proves |
|---|---|---|
| 0 | tagban copies the 1.14d MPQs from his Windows install (`d2data`, `d2exp`, `d2char`, `d2sfx`…, `Patch_D2.mpq`) to a data directory on the Mac | real tables to load |
| 1 | `d2-formats` MPQ + `d2-data` tables load from that directory | the data path, no embedded blobs |
| 2 | `d2-drlg` ported; identical to libd2 over hundreds of seeds × acts × difficulties | the world the client will expect |
| 3 | `d2-net` + a minimal `GameInstance` in `bnetccd`: create/join from the realm, **your character standing in the Rogue Encampment, a second player visible** | ROADMAP Phase 5 step 1 — the client accepts our packets |
| 4 | Movement, warps between levels, save on leave into `characters.save` | a character that persists |
| 5 | Monsters, combat, skills, items, loot — then quests — each diffed against the real-engine harness | the game |

Milestone 3 is the risk gate: until a retail client stands in town on our packets, nothing else
matters.

## 6. A native modern client (later, separate)

The server engine alone does not make the game playable on current macOS — that is the client:
rendering, UI, audio, input. But the shared crates (data, formats including dc6/dcc/dt1 sprites and
tiles, drlg, net, the client world model) are roughly half of one. A client would add an
isometric renderer on `wgpu` (Metal on macOS, Vulkan/DX12 elsewhere), the UI, audio and input,
loading art from the player's own MPQs — the OpenMW/DevilutionX model. Possible because of this
engine; its own project after the server plays.

## 7. Superseded

An earlier version of this file planned a Rust *host* for Blizzard's i386 macOS binary (a port of
`d2gs-native`). Dropped: it can never run natively on Apple Silicon. That binary survives only as
the test oracle in §2. The earlier note that libd2 had no licence was wrong — GitHub's
`NOASSERTION` came from the extra note about Blizzard-derived blobs appended to its MIT licence.

**Also considered: [`tesseract2048/d2gs`](https://github.com/tesseract2048/d2gs)** — the classic
marsgod/onlyer/faster D2GS PvPGN realms ran, as rebuilt for 91D2.cn (C, last updated 2015). Not
chosen: it is **Windows-only** and runs **Blizzard's own 1.13c DLLs** through `d2server.dll`
(its D2GE "based on Diablo II binaries", deployed into a 1.13c game folder), so it has the same
"hosts Blizzard code" limit as above and cannot be native anywhere. It targets **1.13c**, while
1.14d folded those DLLs into `Game.exe` — "updating it to 1.14" means redoing the hooking, which
jaenster's MIT `apps/d2gs` already did. And it carries **no licence** (GitHub reports none; its
headers say only "Copyright (C) 2000, 2001 Onlyer"), plus committed binaries built from Blizzard's
library (`d2server.dll`, `D2GS.exe`, `patch_d2server.exe`), so it cannot be forked and
redistributed as-is.

