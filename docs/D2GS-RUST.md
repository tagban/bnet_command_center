# A Rust Diablo II game server — port plan

Goal (tagban, 2026-09-13): convert `jaenster/d2-dedicated-server`'s game server to Rust, so closed
realm games work on bnet.cc. tagban has spoken with jaenster about it. The source is MIT; the port
keeps its copyright notice and credits it file by file (`docs/LEGAL.md` §1).

## 1. What is being converted

jaenster's repo contains **no Diablo II game engine of its own**. Every one of its game servers runs
**Blizzard's own game code** headlessly, and his code is the host around it:

| His variant | Runs | Host needs |
|---|---|---|
| `apps/d2gs` | Windows 1.14d `Game.exe` + an injected DLL | wine |
| `apps/d2host` | pre-1.14 Windows `D2Game`/`D2Common` DLLs (1.06b–1.13c) with his own `Fog.dll`/`D2Net.dll` | wine |
| **`apps/d2gs-native`** | **macOS 1.14d `DiabloII` i386 Mach-O**, mapped and run directly | **32-bit x86 Linux** |

`d2gs-native` is the one worth porting: one process, ~22 MB resident, no wine, measured as fast as
the wine server on real hardware (`docs/native-vs-wine.md` in his repo), and a retail **Windows**
1.14d client plays on it (same game version, same protocol). It is ~14k lines of Zig:

| Part | His package | Lines | What it does |
|---|---|---|---|
| Mach-O loader | `packages/macho` | ~830 | parse, map segments, rebase + bind fixups, protect — what `dyld` would do |
| Darwin runtime | `packages/darwin` | ~5,400 | the `libSystem`/Carbon/C++ imports the image calls: libc, pthreads, mach, files, **sockets**, memory, `setjmp` |
| Engine glue | `packages/d2engine` | ~2,500 | realm callback table (fastcall shims), character load/save, packet hooks, versioning |
| The server | `apps/d2gs-native` | ~2,850 | boot, per-game tick loop, realm bridge, character DB, crash handling, health |
| Realm store | `packages/gs-store`, `gs-seats` | ~1,700 | his Redis contract — **replaced** by a direct link to `bnetccd` |

Address maps (`docs/mac-address-map.md`, `mac-tu-map.md`, `mac-tcpip-host-path.md`) are his
reverse-engineering results for that exact binary; the port depends on them.

## 2. The constraint that shapes everything

**Blizzard's code is 32-bit x86 (i386). This Mac is Apple Silicon, which cannot run 32-bit x86
code natively** — Rosetta 2 translates 64-bit x86 only. The host calls into the game image and the
game calls back into the host in one address space, so the Rust host must itself be an **i386
process** (`i686-unknown-linux-musl`). Converting the language does not remove this; only
replacing Blizzard's code would (ROADMAP Phase 5, a multi-year engine rewrite).

Ways to run the result:

| Where | How | Speed |
|---|---|---|
| Any x86-64 Linux box (VPS, NAS, PC) | natively — i386 binaries run on amd64 kernels | full |
| **This Mac** | Docker Desktop / colima, `linux/386` image under `qemu-i386` — jaenster ran his full stress test this way, 20/20 clean | slower, fine for a handful of players |

Later option — **Mac-native via an embedded x86 emulator** in the Rust host. Unicorn/QEMU are
GPL (unusable here) and a pure-Rust i386 interpreter with the SSE the image uses is a project of
its own; revisit once the ported server works.

## 3. How it meets bnet.cc

`bnetccd` (arm64 macOS) and the game server (i386 Linux) are separate processes, so they talk over
a small TCP control link — the `GameHost` seam in `docs/ROADMAP.md` Phase 4 — instead of
jaenster's Redis + Postgres:

```
client ── :6112 ──▶ bnetccd realm ── control link ──▶ d2gs-rs (i386)
   │                  MCP_CREATEGAME → "create game X for char Y"      │
   │                  ◀─ ok, game token                                  │
   │                  MCP_JOINGAME → client told d2gs address + token     │
   └────────────── :4000 ──────────────────────────────────────────────▶ │
                      ◀─ "load char Y" / "save char Y (.d2s)" / "game ended"
```

The realm keeps characters (`characters.save` already exists for the `.d2s`), seat locks, game
names and tokens. The game server keeps nothing durable.

## 4. Milestones

| # | Milestone | Proves |
|---|---|---|
| 0 | **Prerequisites**: the macOS 1.14d `DiabloII` binary + its MPQs from tagban's own install; an i386 Linux runtime (colima/Docker on the Mac, or an x86 box) | we can run anything |
| 1 | Rust Mach-O loader + `--dry-run` report (parse, map, resolve every import) — runs on any host, including this Mac | loader correctness against the real image |
| 2 | Darwin runtime shims → the image boots headless and `QSERVER` listens on :4000 | the host ABI port works |
| 3 | Control link + `bnetccd` realm: create/join routed to the game server; a fresh character's `.d2s` generated from the format spec | **first playable: a character standing in the Rogue Encampment** |
| 4 | Save-back into `characters.save`, one-game-at-a-time seat lock, several games per server | characters persist |
| 5 | Crash recovery, health, multiple game servers, operator docs | runs unattended |

## 5. Open questions for tagban

1. **Game files**: do you have Diablo II **1.14d for macOS** (the `DiabloII` binary and MPQs)? The
   disc images and 1.13c/1.13d zips on the backup volume are Windows/older builds. Blizzard's
   legacy downloads have offered a Mac installer; jaenster's MIT `blizzard-legacy-dl` fetches the same
   payload. Nothing from Blizzard goes in the repo either way.
2. **Where it runs**: Docker/colima on this Mac (simplest to start), or an x86-64 Linux box?
3. **Scope**: 1.14d only to start (matches your client), older engines later?

Not reused: `jaenster/libd2` has no licence grant (GitHub reports `NOASSERTION`), so the `.d2s`
writer for new characters is written from the published save-format documentation instead.
