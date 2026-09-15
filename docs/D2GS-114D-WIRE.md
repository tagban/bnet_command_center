# Diablo II 1.14d game-server wire — read from `Game.exe`

**Status (2026-09-13, overnight):** research notes for `docs/D2GS-RUST.md` milestone 3 (a retail
client standing in the Rogue Encampment on our packets). Everything below was read from tagban's
retail **`Game.exe` 1.14.3.71** (SHA-256 `631066c1…589adaaf`) in Ghidra 12.1.3, and checked
against `jaenster/libd2` @ `ac4d735` (MIT). Addresses are 1.14d virtual addresses.

Nothing here has been sent to a real client yet. "Confirmed" means read in the disassembly (not
just the decompiler) or reproduced byte-exact; "likely" is flagged as such.

## 1. Setup, so the next session can pick this up

- Ghidra project: `~/ghidra-projects/D2_114d` (`Game.exe` imported + auto-analysed, 13,419
  functions). Open with `JAVA_HOME=/opt/homebrew/opt/openjdk@21 ghidraRun ~/ghidra-projects/D2_114d.gpr`;
  the GhidraMCP plugin starts with it and the Claude bridge finds it (`connect_instance D2_114d`).
- Game files: `~/Downloads/Diablo II` (all MPQs + `Game.exe`) — milestone 0's data, not yet moved
  to a permanent `diablo2.data_dir`.
- Scripts (read the tables out of the PE at run time; no Blizzard bytes in the repo):
  - `scripts/d2re/verify_libd2_tables.py <Game.exe> <libd2>` — §2
  - `scripts/d2re/d2gs_codec_from_binary.py <Game.exe>` — §3
  - `scripts/d2re/server_send_builders.py <Game.exe>` — §6

## 2. libd2's packet tables are exact

| Table | Address | Entries | Result |
|---|---|---|---|
| S→C framing sizes (`sc.SC_SIZE`) | `0x730AE8` | 181 | identical |
| C→S framing sizes (`cs.OUTGOING_SIZE`) | `0x730DC0` | 113 | identical |
| S→C handler table (`sc_table.TABLE`) | `0x7114D0`, 12-byte `{handler, size, unit_handler}` | 175 | sizes identical; 0x6E–0x72 are named after the second (unit) callback slot, not the first |
| Wire Huffman code lengths | `0x7076C0` | 256 | identical, a complete prefix code |
| Huffman bit masks | `0x7077C0` | 16 | identical |

The server's own C→S framer `0x52BC20` has the same variable-length opcodes libd2 models (0x14/0x15
chat, 0x66, 0x6C, and 0xFF = 16 bytes), but two of libd2's `cs.sizeOf` rules differ from its
disassembly: chat is `[op][u16][cstr][cstr][i8 n][n bytes]` (libd2: a 4-byte header and no
trailing count), and `0x6C` is `7 + u8@1`, read once 6 bytes are present (libd2: `u16@1`).
`bnetcc-proto::d2gs::client_packet_len` follows the engine. The server command table is `0x6E0D18` (8-byte
`{handler, flag}`, opcodes 0x00–0x66); 0x67–0x70 are connection packets handled separately (§4).
The framer `0x52B100` classifies opcodes into three kinds: <0x67 (game commands), 0x67–0x70
(connection packets), and 0xFF.

## 3. Server→client framing and compression (confirmed)

`SendPacketToClient 0x52B330(mode, client, buf, len)`:

- `len > 0x204` is a fatal error. Output window is 0x408 bytes.
- **mode 0 and `buf[0] == 0xAF`** → sent raw (the greeting).
- **mode 2** → sent raw.
- otherwise the buffer is Huffman-compressed by `0x40B1B0` and prefixed with a length that
  **counts the header itself**: `[n+1]` when `n+1 < 0xF0`, else `[((n+2)>>8)|0xF0, (n+2)&0xFF]`.

Reproduced from the binary, not from libd2: `d2gs_codec_from_binary.py` ports the table builder
`0x40ADB0`, the compressor and the framer, and turns the plaintext `01 00 04 00 10 00 01 00 00`
into `7a 09 a5 f0` — the exact bytes libd2 captured off a live 1.14d server. On the wire:
`05 7a 09 a5 f0`.

**Batching.** Apart from a few control packets sent on the spot (`AF`, `B0`, `B4`, `06`), game
packets do not go to the sender directly: `0x53B280` appends to the client's queued buffer (0x208-byte chunks, ≤0x200 payload each), flushed later. So one
compressed frame carries several packets back to back — the capture above is `0x01 GameFlags` +
`0x00` in one frame. The client decompresses a frame and splits it by the S→C size table.

### The greeting chooses the client's receive mode

Client receive thread `0x52AB00`, packet splitter `0x52A8D0` (both confirmed in disassembly):

- The client starts in **raw** mode: bytes are split directly by the S→C size table.
- In raw mode, a packet `AF xx` with **`xx != 0`** sets the mode flag (`[EBP-0x110] = 1`,
  returned in EAX). From the **next `recv`** on, the client reads length-prefixed compressed
  frames and never goes back.
- `AF 81` is followed by 128 bytes of nibble-packed code lengths (each nibble + 1) and rebuilds
  the Huffman table (`0x40ADB0`) before switching.
- `AF 00` leaves the client in raw mode.

Who sends what: Blizzard's TCP game server (listener on **4000**, registered at `0x52B7A0`) sends
**`AF 01`** from its connect callback `0x52B720`. `AF 00` is sent only by `0x52B780`, the
in-process path used when the local-game flag (`0x882D10` ∈ {1,2}) is set — single player.

**Trap:** bytes that arrive in the *same* `recv` as `AF 01` are still split as raw. The real
server sends nothing after the greeting until the client's `0x68`, and ours must do the same.

### Where libd2 disagrees with itself

- `packages/util/src/frame.zig` + `util/src/huffman.zig` — **correct** (compressed, header
  counts itself, AF raw, mode 2 raw, 0x204 / 1032 limits). Port these.
- `packages/net/src/sc.zig` `writeFrameHeader` / `frameInto` / `nextFrame` — **wrong for any
  retail stream**: uncompressed, length *excludes* the header. Do not port.
- `packages/game/src/gameserver.zig` sends **`AF 00` and a raw stream**. The client code above
  does accept that (it is the single-player path), but it is not what Battle.net's game servers
  did. **Decided (tagban, 2026-09-13): match Blizzard — send `AF 01` and compress.** Raw mode can
  stay behind a flag as a debugging aid, never the default.

## 4. The join, as the 1.14d server runs it

Blizzard left `[JOIN n]` log strings in the server path; they anchor each stage. The game server
calls its host through a function table at `0x883D50` (null = open TCP/IP game). The slots seen on
the join path: `+0x18` validates the join token, `+0x08` fetches the character from the
database (the reply arrives as JOIN 3), `+0x20` unlocks it on failure, `+0x14` reports
"entered game". **In `bnetccd` the realm is the host**: these become in-process calls into
`realm.rs` / `characters.save`, not a network hop.

Client join state lives at `client+4` (set by `0x5386D0`).

| # | Trigger | Server does | S→C (queued unless noted) | State |
|---|---|---|---|---|
| 0 | TCP connect on 4000 | `0x52B720` | `AF 01` **immediately, raw** | — |
| 1 | C→S `0x68` GAMELOGON (37) | connection dispatcher `0x53F100`, `[JOIN 1]` | — | — |
| 1a | open game only | `0x53EFF0`: game id must be 1 and exist, else reason `6`; version must be **`0x0E`**, else `0x10`; game already holding 8 players → `0x0F` | on failure `0xB4 [u32 reason]` **immediately** (`0x53B260`), then disconnect (`CCmd.cpp:0x14B`) | — |
| 1b | | `0x52C690`: class < 7, host `+0x18` token check | — | — |
| 2 | | SrvJoinGame `0x52FA50`: add client to game `0x539A30` | on failure: host `+0x20`, `0xB0` **immediately**, drop | — |
| 2a | | `0x52C260` | `0x01` GameFlags (8, built `0x53B340`), then `0x00` | 1 |
| 2b | realm game | `[JOIN 2]` host `+0x08`: fetch character, wait | — | 1 |
| 2b′ | open game | — | `0x02` | 1 |
| 3 | host delivers the character | SrvRecvDatabaseCharacter `0x5306E0`, `[JOIN 3]` | `0x02` | 1 |
| 4 | C→S `0x6B` ENTERGAME (1) | SrvJoinAct `0x530190` → `ClientAddPlayerToGame` `0x539760`: load the character (new-character path `0x532590` calls `SendUnitToClient`, then the player's quest setup `0x546270`), check expansion/hardcore against the game (failure → `0xB4` with its code) | `0x59` for the player, **unplaced (0, 0)**; `0x5E` quest states, `0x28` type 6 player quest flags, `0x29` game quest flags (§5, *Quests at join*); `0x0B` `[type][guid]` "this unit is yours" (`0x537930`); `0x5F`; every stat as `0x1D`/`0x1E`/`0x1F` `[stat][value]` (stat-list walk with callback `0x548520` → `0x53BE40`); `0x7B` hotkeys; `0x23` selected skill ×2 (right, left); life/mana `0x548760` → `0x95` (first time, unplaced) | 1 |
| 4a | | `0x52C210`: build the act if needed (`0x53AC70`, seed `game+0x7C`, difficulty `game+0x6D`) | `0x03` LoadAct (12), then `0x53` (10) | 2 |
| 4b | | place the player `0x5394A0` | `0x07` `[room tile x u16][room tile y u16][level u8]` for the spawn room; then `0x5381F0` → `0x537B50` enters the player into that room: for each room "near" it (§5, *Town units*) `0x53A8E0` sends `0x07` and every unit already in the room (`SendUnitToClient`); `0x15` ReassignPlayer (11) `[type][guid][x][y][1]`, `0x7E` (5) | 3 |
| 5 | next server frame | sUpdateClients `0x52D440` for state 3, `[JOIN 6] SCMD_STARTACT` | `0x04`; item/equipment pass `0x55DF00`; `0x5B` roster records both ways with every player already in the game (plus `0x8E` per entry of that player's unit`+0x60` list), then the joiner's own (`0x52C410`, confirmed in disassembly); `0x55B620`; host `+0x14`; broadcast `0x5A 02 04 …` "joined our world" (`0x54AA40`) | 4 |

Units the client sees — itself included — arrive through **`SendUnitToClient 0x571F90`**,
called from eight room/visibility paths rather than from the join itself. By unit type:
player → `0x59` (26: `[guid u32][class u8][name 16][x u16][y u16]`, builder `0x53E8F0`) + `0x75`
+ player-state helpers (`0x571620`, `0x571CD0`, `0x570E30`, `0x5484B0`; these reach builders for
`0x0B/0x76/0x7C/0x92`, `0x23`, `0x9E`–`0xA5`, `0xAB`, and item packets); monster → `0xAC`, `0x21`
per skill `MonStats` `SendSkills` selects, `0xAA` states, queued unit events (`0x571CD0`), then its
mode (`0x597E20`: `0x6D` standing, `0x67`/`0x68` walking, skill packets); object → `0x51` (+ `0x60`
for portals, `objects.txt` SubClass bit 4); item → `0x9C`/`0x9D`; warp tile → `0x09`. So
"standing in town" = the join table above **plus** the room-activation stream. §5 *Town units* has
the monster and object packets in full.

`0x7E` detail: `0x53DB70` writes only the opcode; the other 4 bytes are uninitialised stack. The
client therefore cannot depend on them — send zeros.

`0x68` GAMELOGON fields, as the dispatcher reads them (disassembly at `0x53F1D0`–`0x53F270`):
`+1` u32 game hash (to the host token check) · `+5` u16 **game id** — SrvJoinGame indexes the
server's game table `0x882D34[id]` with it · `+7` u8 class · `+8` u32 version (`0x0E`) · `+20`
u8 (passed through, purpose not yet read) · `+21` char name[16]. So the token our realm hands out
in `MCP_JOINGAME` must be the game's slot number on the game server, and the hash must be what
the host check expects.

Other connection packets in `0x53F100`: `0x67` create game (open mode, `0x530BF0`, same
`01`/`00`/`02` opening), `0x69` leave (`0x5303D0`: `0x05`, `0x06`, then `0xB0` immediately),
`0x6A` (`0x52E9B0`), `0x6C` (`0x52DB10`), `0x6D` ping (`0x52C400`), `0x6E` (`0x530270`),
`0x70` (`0x5377A0`).

Packet layouts confirmed at the builder:

- `0x01` GameFlags (`0x53B340`): `[01][difficulty u8 = game+0x6D][flags u32 from 0x53FD40][expansion u8 = game+0x70 != 0][ladder u8 = game+0x74 != 0]`.
  The capture decodes as normal difficulty, flags `0x00100004`, expansion, non-ladder.
- `0x03` LoadAct (`0x53ABE0`): `[03][act u8][map seed u32 = game+0x7C][area u16 = the act's town][u32 = game+0x80]` — matches libd2 `sc.LoadAct`.
- `0x53` (`0x53ABE0` → `0x61C330`): `[53][u32][u32][u8]`, three fields of the act's environment
  record; the client handler `0x45E300` writes them back into its own copy. Likely time of day.
  Unnamed in libd2, and libd2's server does not send it.
- `0x15` ReassignPlayer (`0x53BC10`, from `0x5394A0`): 11 bytes as above.

## 5. What this changes for the Rust port

1. Framing/compression: port `util/frame.zig` + `util/huffman.zig`; send `AF 01`; nothing after
   it until `0x68`; batch queued packets per flush up to 0x200 per compressed frame.
2. Join state machine: the table in §4 is the spec — `01 00` on logon, `02` once the character
   is loaded, `59 0B 23 23 03 53 07 15 7E` on `0x6B` (player before act: the client's `0x53`
   handler dereferences its own player), `04` + rosters on the following frame, other units
   via the room-activation path.
3. Reject logons whose version field is not `0x0E`, as the engine does, with `0xB4`.
4. libd2's `gameserver.zig` skips `0x53` and uses raw mode; treat it as a reference, not the spec.

The handshake test (`crates/bnetccd/src/d2gs.rs`, `diablo2.game_server_probe`) implements the
join table above. Where it had to choose without a confirmed answer, the choice is marked here
so a client test can overturn it:

- `0x03`'s last field is sent as `0` (the client stores it beside the seed; meaning unread).
- `0x53` is `(period 2, ticks 0, no eclipse)` — period 2 starts at angle 0 in the engine's
  period table `0x7443F0`; the client aborts on a period above 5.
- `0x5F` (`[u32]` = player data `+0x2C`, meaning unread) and the `0x7B` hotkeys are not
  sent; both `0x23`s say skill 0 (Attack) with item guid `0xFFFFFFFF`.
- `0x07`'s fields are read as the room's tile rectangle (`+0x10`, `+0x14` of the struct
  `0x619730` fills) — the usual D2 coords layout, not traced further.
- Each game draws a random map seed (the engine's `game+0x7C`); without the install's maps it
  uses `0x12345678`. Players start on the town waypoint (§5, *Where a new player starts*); the
  engine's final free-spot search around it is not ported.
- `0x5B` rosters are not sent. Room units go out with the join, right after each near room's
  `0x07`, as they would for a room another player had already populated; the engine's first
  population of a fresh game may send them a frame later instead.
- The near-room order compares room fields `+0x34`…`+0x40`, the ones the spawn search reads as
  a room's tile x, y, width and height (`0x66B2B0`).

### First client test (2026-09-13)

tagban's retail 1.14d client, LAN, against the handshake test sending `03 53 59 15 7E` / `04`:
create → join → port 4000 → `GAMELOGON` (id, hash, version `0x0E` all right) → decoded our
compressed `01 00` and `02` → pinged → **`ENTERGAME`**. So framing, compression and the logon
half are confirmed against a real client. It then crashed 0.4 s later:
`ACCESS_VIOLATION` at `0x0045E31C` — the `0x53` handler's `CMP ECX,[EAX+0x1C]` with `EAX` =
the client's own player unit (`0x7A6A70`), still null. That pointer is only ever set by the
`0x0B` handler (`0x0045CC50`), which looks up an **existing** unit by guid — hence the engine
order in row 4: `0x59` first, then `0x0B`, and only then `0x03 0x53`. The test now sends that.

Second run, same day, with `59 0B 23 23 03 53 07 15 7E` / `04`: **the client is in the game** — in
the Rogue Encampment, standing where §5's spawn put it, and stays connected (a `0x6D` ping every
5 s, answered with `0x8F`). Clicking the ground sends `0x01` WalkToLocation to subtiles a few
steps from the spawn (`01 ad16 5f11` = 5805, 4447), which confirms map seed, spawn and room.
Missing, as expected with nothing sent after `04`: life/mana/stamina (no stat packets), and no
response to actions (walking needs the server to answer).

### Player stats (step 1, 2026-09-13)

A new character's stats come from `charstats.txt` (`0x5706D0`, class record `+0x30..+0x35`):
str, energy (`int`), dex, vit as is; life = max life = `(vit + hpadd) << 8`; mana = max mana =
`int << 8`; stamina = max stamina = `stamina << 8`; level 1; `nextexp` (30) from
`experience.txt` row `"1"` (`0x611800` indexes `[(level + 1) × 8 + class]` past the `MaxLvl`
row); velocity/attack rate/animation rate (67/68/69) 100. Stat ids are `ItemStatCost.txt` rows.

The wire: `0x1D`/`0x1E`/`0x1F` `[stat u8][value]` in the smallest of byte/word/dword the value
fits below all-ones; the client's handler `0x45D780` sets it on its own player (and asserts that
player exists — another reason `0x0B` comes first). `0x95` (13) is bit-packed LSB-first by Fog's
bit buffer `0x410EB0`: life u15, mana u15, stamina u15 (whole points), x u16, y u16, dx i8, dy
i8; handler `0x45DB20` sets stats 6/8/10 `<< 8` and nudges the position. Its siblings are `0x18`
(15: the same plus two u7 fields after stamina — projected regen percentages, `0x5485B0` /
`0x548640`) and `0x96` (9: stamina, x, y, dx, dy); `0x548760` picks whichever carries what
changed since the last one it sent.

### Town units (step 2, 2026-09-13)

**Which rooms.** A room's "near" list is itself plus every room of its level less than 6 tiles
away on both axes (`DRLGROOM_DefineRoomsNear` `0x66BC20`, ported in libd2 `DrlgRoom.zig`) — the
3×3 neighbourhood for a town's 8×8-tile rooms — ordered by a bubble pass `0x66BBC0` that moves a
room ahead of one it lies wholly left of or above. When a player's room changes, `0x537B50`
sends `0x53A8E0` (`0x07` + units) for near rooms that are new and `0x53A9B0` (unit removals, then
`0x08` `[tile x][tile y][level]`) for those left behind.

**When units exist.** A room is populated the first time it is activated (`0x52D0F0`, room flag
`+0x34` bit 0): `0x5559A0` walks its preset units twice — every non-monster first, then the
monsters — skipping units flagged client-side (`+0x1C` bit 0); then inactive units come back
(`0x542B40`), `Levels.txt` object groups roll (`0x552610`) and monsters spawn (`0x54EC90`). Units
created while a client is in the room are sent to it at creation.

**What the map places.** The DS1 loader `0x665950` gives preset monsters mode 1 (neutral),
objects mode 0, items mode 3. Objects `0x23D` are dropped and larger ids go to a special spawner
(`0x54F490`). Preset monsters spawn through `0x54E490`, which **skips critters** (`MonStats2`
flag 13, `critter`): the client spawns those itself per room from `Levels.txt` `cmon1-4`,
`cpct1-4`, `camt1-4` (`0x46C460`; the Rogue Encampment has `chicken` at 30%) — hence the
chickens moving in the first test with no server units at all.

**Ids and seeds.** Each unit takes the next guid of its type: `game+0x90+type×4`, pre-incremented
from 0, 0 skipped on wrapping (`0x552EE0`). Its seed is `{low of the game seed stepped once,
0x29A}` (`0x552DF0`, `0x650E40`) — and the game seed at `game+0xD0` starts from the clock
(`0x650DE0`), so a real server's rogue bows differ game to game.

**Object modes.** After allocation `0x54F5D0` calls `0x731BC0[InitFn]` (`objects.txt` InitFn,
record `+0x1B1`); the selectable flag is record `+0xC4 + mode`; a `PreOperate` class
(`+0x13D`) can roll into mode 2 afterwards. Ported so far: InitFn 8 (`0x5500C0`, torches) → mode
2; InitFn 17 (`0x547210`, waypoints) → mode 2 in a town (`0x61AB00` → `0x6426A0`: levels 1, 40, 75,
103, 109) unless a pending activation is queued; InitFn 54 (`0x5940E0`) only records Cain's start
for his quest. The Rogue Encampment's bonfire has InitFn 0: mode 0 is its looping fire.

**Monster components.** `0x5739D0` picks each of the 16 components with the unit seed's
`RandomNumberSelector` over the class's variant count (`MonStats2` `HDv`…`S8v` entries; record
`+0x15`), leaving the seed alone for a count of 0.

**`0x51`** (14, builder `0x53BD10`): `[51][type u8 = 2][guid u32][class u16][x u16][y u16][mode
u8][interaction u8 = object data +4]`.

**`0xAC`** (variable, builder `0x53E2E0`, client `0x45F190`): `[AC][guid u32][class u16][x u16][y
u16][life u8][total length u8]` then Fog bits (LSB first):
- mode, 4 bits — 0, 8, 9, 12 as they are, anything else 1;
- 1 bit "components follow"; if set, 16 values, each in 1 bit below 3 variants, else
  `bitlen(variants − 1)` bits;
- 1 bit "type block" (monster type flags or unit flag `+0xC4` bit 9); if set: 5 flag bits (the
  client maps them to 4, 8, 2, 0x10, 0x40), 16 bits super-unique id when the third is set, name
  bytes to a zero byte, 16 bits, and 1 bit + 32 bits for a minion's leader;
- 1 bit "owner" (`+0xC8` bit 10, no owner unit): 31 bits;
- 1 bit "stats" (stat list with flag 0x40): `[stat 9][param][value]`… up to `0x1FF` in 9.
`life` is `hp × 128 ÷ max hp`, `0x80` at full (`0x5A5650`). A plain town NPC's body is one byte.

**`0xAA`** (variable, `0x570E30`): `[AA][type u8][guid u32][total length u8]`, then per active
state that is not flagged unsent: `[state 8]` + 1 bit + optional stat list; terminated by `0xFF`
in 8 bits. A unit with no states: `AA 01 <guid> 08 FF`.

**`0x6D`** (10, builder `0x53BB70`): `[6D][guid u32][x u16][y u16][life u8]` — `0x597E20` for a
monster in mode 1 not using a skill.

The handshake test sends, for the 3×3 rooms around the spawn, `07` then each room's objects
(`51`) and monsters (`AC AA 6D`), populating a room the first time any player enters its
neighbourhood. For seed `0x12345678` that is 24 objects (lit torches, the bonfire, the stash,
the waypoint) and Warriv, Kashya, Akara, Charsi, Gheed and five rogues; the cows two rooms east
are not sent. `crates/d2-game/examples/town_units.rs` prints the list for any seed and spot.

### Second client test: town units, and a crash leaving town (2026-09-13)

With the town units in: tagban walked around the camp and saw the NPCs, torches, stash and
waypoint; the bonfire looked like smouldering embers. Clicking Warriv sent `02 01000000 05000000`
(walk to monster 5), `59 01000000 05000000 ae16 0000 6211 0000` and `13 01000000 05000000`
(interact); clicking the stash `02`/`13` with object 16 — exactly the guids §5 *Town units*
gives Warriv and the stash, so the unit ids line up with the client's. Nothing answers those
yet. Walking south out of the camp (player at about (5833, 4543), past the town's last row
4520) the client halted: "Unrecoverable internal error … failed at (96)", `0x4B92FE`.

**Quests at join.** That halt is `0x4B92E0` asserting its quest table is loaded: the area
change check `0x4DCAA0` → `0x4CC270` (per level group, `0x72A2C4`) → `0x4A4180` reads it. The
table is filled only by `0x5E` (38, client `0x45E570` → `0x4B92B0`: 37 bytes, then the loaded
flag). The server sends it from `0x546270`, which every new or loaded player goes through inside
`ClientAddPlayerToGame`'s load, right after the player's `0x59` and before `0x0B`:
- `0x5E` `[state × 37]` — each quest object's `+9` (quests allocated with 1 by `0x545D80`);
- `0x28` `[6][u32 0][u8 0][flags 96]` — the player's quest flags for the game's difficulty
  (`0x53D670`; the client `0x4B6DD0` copies type 6, other types are NPC quest dialog updates);
- `0x29` `[flags 96]` — the game's quest flags (`0x544520`, client `0x4B2620`);
- `0x89 00` only when quest 1's object is active past state 3 (`0x590810`), never at a fresh start.
The handshake test now sends `5E` (all 1) `28` `29` (all clear) in that place.

**The bonfire is on a clock.** `objects.txt` 39 (RogueBonfire) is the one object with ClientFn
14 (`0x4BDCC0` → `0x4BC5E0`): every 500 ms the client itself puts it in mode 1, lit, while the
act's light phase is 1–3, and back to mode 0, embers, in phase 0. The phase is the second column
of the period table `0x7443F0` (six rows `angle, phase, colour`: 320/3, 340/3, 0/0, 160/1,
180/1, 200/2), picked by `0x53`'s period (`0x61C240`); the engine advances period and ticks each
frame (`0x61BEE0`, towns faster). We send period 2 and never advance it: day, embers. The
server's own starting period is not yet read.

### Where a new player starts (2026-09-13)

tagban noticed the camp's exit was always in the same place: every test game had used map seed
`0x12345678`, while the camp's layout (TownN1/E1/S1/W1) follows the seed. Seeds are now drawn per
game, which needed the engine's spawn instead of a spot picked for that one seed.
`PlacePlayerInAct` (`0x5394A0`, no warp given) calls `0x61B060` with the act's town:
`0x66B2B0` loads the level and — with `+0x90` of `0x61E470`'s struct clear (meaning unread;
taken as clear on a fresh join) — asks `0x66AD80`
for the first room flagged as holding a waypoint (`+0x28 & 0x30000`) and, in it, the first
preset object with class ≤ `0x23C` whose `objects.txt` SubClass has bit `0x40` (waypoint); the
player's tile is that object's (preset x ÷ 5 + room tile x). Without one it tries `0x66B1F0`,
`0x642630` and `0x66AE70` (not read). The tile becomes
subtiles × 5 + 3, and `0x64E7B0` (`0x64DEA0`, radius `0x32`, collision mask `0x1C09`) moves the
player to the nearest free spot for its size. A waypoint has no collision (`HasCollision*` 0), so
on a fresh game that is the waypoint's own tile. Over 400 seeds every camp builds, all four maps
appear, and each has its waypoint inside the town.

### Walking and loading rooms (2026-09-13)

**Movement in.** The in-game C→S handlers sit in the table at `0x6E0D18` (8-byte entries
`{handler, flag}` by opcode). `0x01` walk / `0x03` run to a spot `[x u16][y u16]` go through
`0x5496F0` (length 5, `0x548EF0` validates the spot; a refused spot, if the last correction is
more than 25 frames old, gets `0x15` with the server's position) and `0x5809D0`, which starts
mode 2 (walk) or 3 (run) along a path. `0x02`/`0x04` do the same toward a unit
`[type u32][guid u32]`. `0x5F` `[x u16][y u16]` is the client's own position (`0x54CD50`): when
the server's unit is more than 4 subtiles off, it corrects the client or walks the unit there.
Nothing is sent back to the moving client itself.

**Speed.** `CharStats.txt` `WalkVelocity`/`RunVelocity` (6 and 9) as the path's `dwVelocity`
(`<< 8`) move `velocity / 16` of a subtile a frame (libd2 `world/src/motion.zig`, from
`0x64FE40`): 9.4 subtiles a second walking, 14 running, at 25 frames.

**Rooms out.** When the player's room changes (`0x5380D0`), `0x537B50` compares the old and new
near lists: every new room gets `0x53A8E0` (`0x07` and its units' packets), then every room left
behind gets `0x53A9B0`: `0x0A` `[type u8][guid u32]` for each unit in it (`0x571600`, not for
missiles), the client is taken off the room, and `0x08` `[tile x u16][tile y u16][level u8]`
(`0x53BC90`). A level change also runs the quests' level callbacks (`0x543B90`) and town
arrival/departure hooks (`0x537340`).

**What the client needs to load a room.** The `0x07` handler (`0x45CAB0` → `0x61A070` →
`0x61B640`) generates the level if needed (`0x642BB0`), finds the room *containing* the point
(`0x642C30` → `0x642630`) and activates it when its user count goes from 0; `0x08`
(`0x61B690`) counts down. A point in no room would dereference null. So a room origin is enough,
and a wilderness level loads from its grid: `DRLGOUTDOOR_CreateOutdoorRoomExGrid` (`0x6750F0`, in
libd2 `outdoors/Outdoors.zig`) makes each 8×8-tile cell a room, and a preset piece placed on a
cell is cut into 8×8 rooms from it (`DRLGPRESET_BuildArea`), so every cell holds a room.

**Near rooms across levels.** Within a level: gap under 6 tiles (above). Across levels the engine
links rooms through their visibility slots (`DRLGROOMEX_LinkNearRoomsByVis`, `0x66C220`);
`Levels.txt` `Vis*` lists only warp links (Blood Moor's are the Den of Evil), while walking
between wilderness levels follows the act's placement chains.

The handshake test (`d2_drlg::world`) takes the act's placed levels (and preset/wilderness levels
depending on them), cuts preset levels and wilderness levels into their rooms, and treats rooms
of different levels as near by the same gap — so two levels placed edge to edge without a passage
also count as near, and the client loads terrain it cannot reach.

**Voids.** Not every wilderness cell is a room. The border placement blanks cells
(`DRLGOUTDOOR_SetBlankGridCell`, outdoor flag `0x100` without a preset, skipped by the room grid):
the corners outside a level's border (`SetBlankBorderGridCells`, `0x675670`) and the empty tiles of
the cliff and corner shapes stamped from `LvlSub.txt` maps (`DRLGOUTDOOR_ApplySubTileToGrid`,
`0x66F520`) — 2–13 cells a level, on the edge and, in Black Marsh and Tamoe Highland, inside.
A `0x07` into one would crash the client, so the test generates them: `d2_drlg::outdoor` ports
the Act I outdoor placement (libd2 `outdoors/ActInit.zig`, `Border.zig`, `OutPlace.zig`,
`OutRoom.zig`, `OutSub.zig`, `TileSub.zig`, `DrlgVer.zig`): borders, substitution borders, the
exits and their roads (a depth-first search bounded by a growing cost, `0x6817D0`), the waypoint,
shrines and set pieces. Everything that blanks a cell happens by the last substitution border;
what comes after only places pieces on free cells (`TestOutdoorLevelPreset` refuses blank ones)
or ORs flags. The outline comes from the level's rectangle split where a neighbour touches it: the placement lists wire each
node to its predecessor as an open edge (`DRLGACT_SetWarpConnection`, warp -1), and
`DRLGLEVEL_AllocDrlgLevelFromLevelIdToLevelId` (`0x677680`) turns those `Vis` slots into orths;
a preset neighbour (the camp, the Monastery Gate) gets no border pieces along its edge. The road
flags come from the placement's own directions (`0x677180` reads `aCurrentDir[n]` and `[n + 1]`),
and the lookup tables from `Game.exe` (`d2_data::engine::OutdoorTables`, addresses there). Against libd2's recordings of the engine the rooms match exactly — 21
Normal levels (`deep_seed_*.jsonl`) and 35 Hell levels (`coll_seed*_all.jsonl.gz`) room by room,
and the room count of all seven Act I wilderness levels for 200 seeds on Normal and Hell
(`coll_crc_masked_200_*.jsonl.gz`, 2,800 levels); every piece id and every plain room's link
flags (neighbours, shrine styles, waypoint — drawn after the roads) match in the 21 Normal levels.
The rooms' own seeds and the pieces' file draws at room creation (`DRLGPRESET_BuildArea`) are not
ported yet.

The test moves the player in a straight line at the engine's speeds (no collision, no path), re-syncs on `0x5F`, and on a room
change sends exactly the packets above — except that a room is dropped only once it is two rooms
away (gap under 14), because the straight-line player can run ahead of the client's. Blood Moor's
monsters, objects and warps are not generated.

### Wilderness rooms, waypoints and shrines (2026-09-13)

**Room seeds.** `DRLGOUTDOOR_CreateOutdoorRoomExGrid` (`0x6750F0`) walks the cells row by row;
each room takes the level seed stepped once, and its own state is `{that low, 0x29A}` stepped once
(`DRLGROOM_AllocRoomEx`, `0x66B3F0`), `nSeed` being its low word. A plain room then rolls its
terrain picks on that state (`DRLGROOMEX_RollLevelSubstitutionMask`: a percent roll per row of the
level's `SubType` group against `Prob[SubTheme]`). A piece's anchor cell first draws a map file
from the level seed (`DRLGPRESET_AllocDrlgMap`; the cell's file index replaces it), reads the map's
units if `LvlPrest.txt` `Scan` or `Pops` is set and rolls for some of them
(`DRLGPRESET_AddPresetUnitToDrlgMap`: monster classes `0xCC`/`0xCD`/`0x173`/`0x174` keep on `%3 == 0`,
placements `0x21..0x23` past the `MonStats` rows on `&3`/`&1`/`&3`, objects `0xC4`/`0x105` on even,
`0x245` on `&3 != 0`; units walked newest first), then cuts the piece into 8×8 rooms, one level-seed
step each. `d2_drlg::outdoor` reproduces all 1,739 rooms of libd2's 21 recorded Normal Act I
wilderness levels — seed, terrain picks, piece and link flags.

**What a room's init places** (`DRLGOUTROOM_InitGridCells`, `0x67D2D0`). The room's seed restarts
at `{nSeed, 0x29A}`. A 9×9 floor grid gets `0x40002` on its 8×8; in Act I the level's roads —
`pAdjacentVertices`, the jittered road vertices of `0x681240` (the target's outer point, the snapped
target, each path cell nudged 2–3 tiles in a turning direction, the snapped start, the exit) — are
drawn two tiles wide into an edge grid one tile larger on each side (`0x680A70`) and every edge
cell's floor becomes `(orientation << 8) | 0x82` by its 8-neighbour mask (`0x680B10`, table
`0x6F2700`). Then `SubTypeWpShrine` (`0x6707A0`) runs for the waypoint (`Levels.txt` `SubWaypoint`,
rows by the room flags `>> 16 & 3`), the shrine (`SubShrine`, `>> 12 & 0xF`) and the terrain: each
picked row with `CheckAll` 0 goes to `DoNotCheckAll` (`0x670170`) — `Max` times, a random group of
the row's map, then (for `Trials` -1) every position `1..=8 - size` in a shuffle, placed at the
first where `CheckSubTileOverlap` (`0x66FCF0`) finds plain floor (`& 2`, nothing in `0x3F0FF00`) and
no wall under the group's floor or wall tiles. `ApplyLvlSubTileData` (`0x66FAD0`) stamps the floor
(`| 0x80`) and walls, spawns shadow tiles (tile-library rolls on the room seed), and last copies the
map's units strictly inside the group's box (`0x66FA10`) into the room at `base * 5 + (unit - box
* 5)` subtiles (`0x66BF30`: unit `+0` type, `+4` class, `+8` x, `+0x14` mode, `+0x18` y). The
test runs the waypoint and shrine passes: each Act I waypoint room gets a waypoint (objects row 119;
the large pad's two torches, row 37), and Cold Plains' small pad lines up with the preset marks
(`0x10`) in libd2's engine collision recording. The terrain pass (decoration, and its rolls) is not
ported.

**Shrines and wells.** An object's init routine gets a context `{game, unit, room, object
control, objects.txt row, …}` (`0x54F5D0`); after it, the unit's selectable flag (bit 1) follows
`Selectable[mode]`. `InitFn` 1 (`0x54F9D0`) rolls a shrine type on the object control's seed
(`game+0x10F0`): with `Parm0` 0, `pick(types - 1) + 1`; `Parm0` 1 the health class, 2 mana,
anything else a seed step and the boost class unless the low word is a multiple of ten, then the
magic class; a class pick (`0x54F770`) is `pick(count)` in the types with that `effectclass`.
Either way up to eight tries while the level id is below the type's `LevelMin`; 5, 4 and 16 become
3, 2 and 18. The type is the object data's `+4`, the byte `0x51` carries. `InitFn` 16 (wells,
`0x552B30`) puts twice `Parm2`'s low byte there. The test spawns both (a random game seed, as the
engine's is); operating them is not answered.

**Turning a waypoint on.** `OperateFn` 23 (`0x584E30`) on a mode-0 waypoint sets mode 1
(`0x624690`), which flags the unit for update; the update pass (`0x581AD0` → `0x581A20`) sends
**`0x0E`** (12) `[2][guid u32][3][selectable u8 = unit flags bit 1][mode u32]` (`0x53B470`). The
player learns the waypoint either way; only an active one (mode 1 or 2) answers with the menu. The
test does the same.

### Monsters in the wilderness (2026-09-13)

**Rosters.** At game start `AllocMonsterRegion` (`0x5479C0`) gives each level a region: on one seed
stream `{game seed, 0x29A}`, level by level in id order, `MONREGION_PopulateMonsterTypes`
(`0x5475E0`) draws up to `NumMon` (at most 13) distinct classes from `Levels.txt` `mon1..10`
(`nmon1..10` past Normal), rerolling the first up to 20 times for a `rangedtype` class on a
`rangedspawn` level, and keeps the `isSpawn` ones with their `Rarity`; `SEED_RollChampionPack`
(`0x5BDB20`) then rolls champion looks on the same stream (not ported).

**A room's monsters.** When a room is first populated, `MONSTER_SpawnRoomMonsters` (`0x54EC90`)
takes `MonDen` (clamped to 10000) and walks `(height / 3) × (width / 3)` subtile slots. Each
steps the game seed; a slot with low word mod 100000 ≤ `MonDen` rolls a class on the room's seed
(`0x5BDE80`: a pick in the summed rarities, swapped for its `spawn` class when `placespawn` and the
next roll mod 100 is over 20), then `MONSTERREGION_CheckSpawnDensity` (`0x5BE020`) decides a unique
pack: short of `MonUMin`, a roll under the share of the level's rooms seen so far; short of
`MonUMax`, a 6% roll. A plain spawn skips on `sparsePopulate` (a game-seed roll over it), then
`SPAWN_SpawnMonsterWithMinions` (`0x54DF80`) places the group before the next slot rolls; fallen and
scarabs (base classes 19 and 91, `0x54EC40`) spawn one. The test does this with no unique names,
champions or wandering monsters, and sends each monster as the NPCs are sent (`0xAC`, `0xAA`,
`0x6D`), standing (mode 1) at full life. Act I's wilderness gets 80–230 a level (seeds 1,
`0x12345678`, `0xBEEF`), from its roster, their minions and replacements only.

**Where they stand** (2026-09-14, read from the disassembly). `0x54DC40` rolls up to 20 spots on
the room's seed inside the room inset by a subtile (`0x54DAC0`: `x + 1 + pick(w - 1)`, then y). A
spot within `sqrt(Levels.txt WarpDist)` subtiles of where players arrive is skipped (`0x54DB50`:
the level's waypoint tile, `0x66AD80`, and a list at `drlg+0x1E0` not identified yet); the rest
get a dry-run placement. The placement core `0x5B2A00` searches around a point: `rings` −1 tests
the point itself, otherwise rings of 3, 6, … `rings × 3` subtiles. Each ring steps the room seed
once — an even low word enters on the top or bottom edge at `pick(c)` along it, an odd one on a
side — then two more steps flip the offsets' signs, and it walks `8c` cells turning at the corners.
A cell must be inside the room (`PtInRect`), pass a few classes' own checks (`0x5FD350`, not
ported) and be clear in the collision map (`0x64D9B0`: MonStats2 `SizeX` 1 one subtile, 2 a cross,
3 a 3×3 square; mask by `spawnCol` — blank `0x3C01`, 1 `0x1C0`, 2 `0x3F11`, 3 none). The group's
first member is placed at the spot (another `rings` −1 probe), then `pick(MaxGrp - MinGrp + 1) +
MinGrp - 1` more on its seed in rings of 3 around it (`0x5B2F70`). Every member's creation brings
`rand(PartyMin..=PartyMax)` minions on its own seed (`0x4CC790`), `minion1` and `minion2` in turn,
in rings of 4 around it (`0x5B2830`, `0x5B23C0`), before the next member. Not ported: the flying
classes' placement (`spawnCol` 1, `0x5B2700`), objects' and monsters' exact footprints, and the coord
lists `0x54EC90` really walks (from `0x61AD50`: sub-rects, each with an id at `+0x28` and a flag at
`+0x20` that skips it; the test uses the whole room). Rooms of a `LvlPrest.txt` piece with `Populate` 0 get no monsters (their rooms carry the
no-spawn flag, `DRLGROOMEX_AllocRoomExTypePreset`; where the engine tests it is not confirmed).

### Talking, the stash and the waypoint (2026-09-13)

`0x13` `[type u32][guid u32]` (handler `0x54AA90`, type ≤ 5) goes to `0x548B00`:

- **NPC** (type 1): within range (`0x641530` < `0x33`), an NPC with `MonStats` `npc` and
  `interact` (record `+0xD` bits 0 and 1) stops its path; at a distance of 6 or less, unless the
  player is busy, `0x573020` → `0x572C10`. That checks the NPC is alive and `interact` (flag 9),
  adds the player to the NPC's list, sends Kashya-style hireling lists for classes 150/198/515/
  252 (`0x576770`), marks the player as interacting (`0x554120`, type 1), runs the quests' NPC
  callbacks into a message list (`0x543D10`), then sends **`0x27`** (40) `[1][npc guid][count
  u8][u8][8 × (message u16, flag u8, u8)]` (`0x661480`), **`0x29`** game quest flags, and
  **`0x28`** `[1][npc guid][0][player quest flags 96]`. The client's type-1 `0x28` handler
  (`0x4B6DD0`) finds the NPC and opens its menu (or plays a queued quest message). Closing sends
  C→S `0x30` `[type][guid]` (`0x54B9F0` → `0x572F20`), answered with nothing. The menu's actions
  come as C→S `0x38` `[action u32][npc guid u32][u32]` (`0x579D60`): 1 trade (for the shop NPC
  classes), 2 gamble/repair, 3 hire, others quest-specific — shops need item generation.
- **Object** (type 2): mode ≤ 7, in range and not blocked (`0x623660`, `0x622B50` mask `0x804`)
  → `0x584540` → `0x584420` → `OperateFn` table `0x732D18`.
  - Stash, `OperateFn` 32 (`0x564CD0`): class 267 with both units in town → interacting type 2
    and **`0x77 10`**, then items are re-sorted (`0x55FA40`). Closing is C→S `0x4F`
    `[button u16 0x12][u16][u16]` (`0x54C7C0` → `0x568060` → `0x564D50`): the interaction ends,
    nothing sent. Buttons 0x13/0x14 move gold.
  - Waypoint, `OperateFn` 23 (`0x584E30`): its level's `Levels.txt` `Waypoint` bit is set in the
    player's flags (`0x660E00`, `0x660EC0`: word `1 + n/16`, bit `n % 16`, table `0x746424`); an
    inactive waypoint (mode 0) turns to mode 1; an active one (mode 1 or 2) with the player not
    busy sends **`0x63`** (21) `[waypoint guid][u16 0x0102][flags 14]` (`0x6610B0`) and marks the
    player interacting. Choosing a destination is C→S `0x49` `[guid][level]`.

The handshake test answers the NPC case (no quest messages, clear flags), the stash and the
waypoint (a new character's flags, the camp's bit set on use) without the range, busy and
collision checks; it does not answer `0x38`.

**Waypoint travel** (2026-09-13). C→S `0x49` (9) `[waypoint guid u32][level u16][u16]` (handler
`0x54C5D0`) is refused within ten seconds of a tick `0x55B6C0` keeps and when `0x549570` rejects
the level; otherwise `0x584F60` checks the unit is a waypoint (`OperateFn` 23), ends the
interaction (`0x554190`), and for another level the player has learned (`0x660E00`/`0x660E50`)
warps (`0x53AEC0`, warp type `0xD` for towns and a few special levels, else 0). Another act goes
through the act change (`0x537340`, `0x53ACC0`); the same act finds the level's waypoint tile at
subtile (3, 3) and the nearest free spot (`0x61B060`) and moves the unit there (`0x554EA0`): `0x07`
for the room landed in, the unit flagged `0x10000`, the room stream (`0x554670` for the others'
views); the next update pass (`0x580860`) sends `0x15` with flag 1 for a unit so flagged (0 for a
`0x800` move seen by another client). The test does the same trip within Act I, without the
ten-second guard and the free-spot search.

### The act clock (2026-09-13)

A new act's environment (`0x61BE40`, `.\ENVIRONMENT\Env.cpp`) starts in period 2 at tick 0 with
128 ticks a degree (`0x7443E4` speeds 128, 4, 8) — exactly what the handshake test's first `0x53`
said. Every frame (`0x52D870` → `0x52D7B0`) each act's clock steps (`0x61C040` → `0x61BEE0`): one
tick, plus 15 in Act IV, or one more at night (phase 2) and eight more on top in Act III; wrap at
360°; move to the next period once past its angle, snapping the ticks to it. `0x61C040` reports a
change when the period or phase changed or the degree moved more than 16 from the last report,
and `0x52D7B0` then sends `0x53` `[period u32][ticks u32][eclipse u8]` (`0x61C330`) to every
client in that act in join state 4. Act I: day for 160°, dusk at about 13.6 minutes; the bonfire
lights then (§5, *The bonfire is on a clock*). Period 1's successor, period 2 at 0°, is always
behind the ticks, so period 1 lasts a single frame and the day restarts at 340°.

The handshake test keeps one clock per game (Act I), steps it by elapsed server frames, and
sends `0x53` on each report; eclipses are not ported.

### Collision (2026-09-14)

**Where it comes from.** Each room gets a map of `WorldSize × 5` subtiles when it comes into play
(`DRLGROOM_AllocRoomCollisionGrid`, `0x0064C900`): every floor, wall and roof tile of the room, and
of the rooms around it whose corner lies inside it, is stamped in (`TileLibrary_AddCollision`,
`0x0064C4C0`) — its DT1 tile's 25 subtile flag bytes, rows read bottom first, plus bits its draw
flags carry over the whole tile (`0x02` → `0x10`, `0x40` → `0x01`, `0x80` → `0x04`). A plain
wilderness cell no floor covers gets `0x05` (solid rock). Bits: `0x01` wall (blocks walking), `0x02`
blocks sight, `0x04` missile barrier, `0x08` blocks players only, `0x10` preset tile.

**A room's tiles.** A room loads the DT1 files of its level type (`LvlTypes.txt`) that its DT1 mask
picks by column, then `Blank.dt1`, `InvisWal.dt1` and `Warp.dt1` (`0x0066F240`, read in Ghidra). A
preset piece's mask is `LvlPrest.txt` `Dt1Mask`; a plain Act I wilderness room's is `0x44103` ORed
with each terrain row it rolled (`DRLGROOMEX_RollLevelSubstitutionMask`). Its grids are a window
into the piece's DS1, or, for a plain room, the grass floor with the road edges cut in, then the
waypoint, shrine and terrain passes (`SubTypeWpShrine` three times) — each stamps the piece's floor,
first wall layer with its tile types, and a shadow tile per shadow cell, which is a roll. Then
`DRLGROOMTILE_ProcessTile` (`0x0066E9B0`) makes each cell's tiles: `GetTileLibraryEntry`
(`0x0066D820`) collects the matching tiles of every file in load order, newest record first within
a file, and picks by rarity on the room's seed `{nSeed, 0x29A}`. Border cells go through
`UpdateOrAddTile` (`0x0066E940`): a room built earlier that owns the tile keeps it, and a blank
floor it owns is re-typed on the owner's seed (`0x0066E740`, tables at `0x006EF574`/`0x006EF620`).
Lit warps (cave mouths) add a second wall tile and four floor tiles (`0x0066E260`, `0x0066E360`).

**A DS1 quirk.** `Act1\Outdoors\Trees.ds1` counts 14 substitution groups and holds 13; the engine
reads the missing one as zeros, and without that group the terrain pass rolls differently in every
room that picks trees.

**Checked.** `d2_drlg::room_tiles` and `d2_drlg::collision` (from libd2 `materialize.zig`,
`tilegen.zig` and `lib.zig`) reproduce libd2's engine recordings for every Act I wilderness level
(Blood Moor to Tamoe Highland and the Burial Grounds): all rooms of seven per-subtile captures
(seeds 1, 2, 17, 18, 777 and two blind holdouts, about 4,100 rooms and 6.5 million subtiles) and
the per-level checksums of 200 seeds on Normal and on Hell, with no difference in the terrain bits.
The same room build now gives the world the units its pieces carry — including the objects in the
terrain maps (`Object.ds1`, `Swamp2.ds1`), which were not spawned before.

Not yet: the towns' and other preset levels' maps, cross-level seams where a neighbouring level's
preset room reaches into a room (`DRLGROOMEX_LinkNearRoomsByVis`), and using the maps — monster
spots, walk checks and paths.

### Fighting (2026-09-14)

`d2_game::battle` runs a game's fight in engine frames; the wire shapes and pacing are bnemu's
recorded retail Blood Moor fight (`docs/d2/re/combat.md`, MIT, permission in `docs/LEGAL.md`), the
builders confirmed here:

- C→S `0x06`/`0x07`/`0x09`/`0x0A` (left skill) and `0x0D`/`0x0E`/`0x10`/`0x11` (right) carry
  `[unit type u32][guid u32]`; handlers `0x549D80`, `0x549E00`, `0x549EE0`, `0x549F40` (`0x09`
  and `0x0A` call the first two) all start the skill. A new character's skills are Attack, so
  each is a swing; one under way ignores more. The hit lands on the swing's `animdata.d2`
  trigger frame (Barbarian `BAA1HTH` 12 frames, hit on 6).
- Monster numbers: `MonStats.txt` value × `MonLvl.txt` percentage for the monster's level
  (`0x5A0000` row lookup, stride `0x78`, 30 values: AC, TH, HP, DM, XP each classic then `L-`,
  per difficulty; `0x5A1990` indexes `difficulty + (expansion + group) × 3`). Level: `Level` on
  Normal, the area's `MonLvl2`/`3` (`Ex`) otherwise.
- To-hit `0x57D9B0`: `200·AR/(AR+DEF)·alvl/(alvl+dlvl)`, 5..95; player AR `(dex−7)·5 +
  ToHitFactor` (`0x622560`), defence `dex/4` (`0x6223F0`). Flinch gate `0x57CB00` (physical:
  never under max/16, always from max/4).
- Monster hit: `0xAB` `[type][guid][life/128]` (`0x53C150`) and, when it flinches, `0x69`
  `[guid][event 06][x][y][life][03]` (`0x53BA40`). Kill: `0x69` event `08` flag 3, then `09` flag
  0 one `DT` animation later (Fallen 800 ms, as recorded). Experience: `0x1A` byte / `0x1B` word
  gain, `0x1C` total (`0x53BDD0`); level-ups as `0x1D`–`0x1F` stats 12, 4, 5, 6–11, 29, 30.
- Monsters: notice within `aidist` (35 when blank), walk `0x67` `[guid][01][x][y][01][00][0D]
  [75 u16][05]` (handler `0x45CDE0`; the client paths there and glides ≈5.77 subtiles/s whatever the
  class), attack `0x6C` `[guid][10][00][target][00][x][y]` (`0x53BAA0`; client `0x45CFB0` asserts
  the attacker's position), `0x6D` to stand.
- A player hit: `0x95` with life/mana/stamina and **position zero** — the client re-seats its
  player only for non-zero x and y (`0x45DB20` → `0x4804E0`) — then `0x0D` `[0][guid][event]
  [0][0][03][60]` (`0x53B4B0`; client `0x45CCC0` → `0x461250`): `13` a small hit's sound, `06`
  get-hit, `08` dying (+ "You have died"), `09` corpse. `0x41` (1 byte) is the release
  (`0x54C0E0`, only for a player in mode 0x11): life, mana and stamina set to their maximums and
  sent as stats (`0x548520` → `0x53BE40`), state 0x36 on and off, the move to the camp as waypoint
  travel (`0x53AEC0`), then mode TN/NU. A life stat or `0x95` with life for a player in mode 0x11
  stands it up (`0x45D4B0`, `0x45DB20`: mode 5, dead flag `0x10000` cleared).
- **The client runs some packets late.** Its handler table (`0x7114D0`, 12-byte rows: handler,
  size, deferred handler) gives `0x0C`–`0x10`, `0x17`, `0x4C`, `0x4D` and `0x67`–`0x72` an empty
  first handler and a second one: `0x45F7B0` queues the packet on its unit (`0x45F730`) and the
  unit's next update runs the queue (`0x480810` → `0x45FA40`), after everything else that arrived
  in the frame. `0x0D` event 8 (mode 0, life 0, dead flag) and 9 (mode 0x11, the same) are among
  them. A corpse event sent in the same frame as the stand-up therefore lands after it and kills
  the player again — which is what kept "You have died" up in town (2026-09-15). The release now
  waits until the corpse event went out in an earlier frame, and sends none of its own.
- Not the engine's yet: the per-class AI routines, the path finder (`d2_game::path`, bounded A*),
  unarmed 1–2 damage, the experience level-gap table.

**Stamina.** On Battle.net the client only displays stat 10 (bnemu's gdb trace). The server steps
it every frame: running outside a town spends `RunDrain × 2` 256ths (`0x57F240`, CharStats
`+0x42`), standing gains `max >> 8`, walking `max >> 9` (walking out of town only above one
point), anything else nothing (`0x580500`). Sent as `0x95` on whole-point changes.

### Maze levels and warps (2026-09-14)

`d2_drlg::maze` ports libd2's `DRLGMAZE_GenerateLevel` for Act I's caves (level type 3): the
Den of Evil (`LvlMaze` Rooms 1, grown by the cave tables to three cells), Cave, Underground
Passage, Hole and Pit. Matches libd2's engine recordings room for room (place, seed, preset,
flags) and cell for cell in collision, and by checksum for 200 seeds on Normal and Hell.

Warps, from the engine:
- A room's warp nodes (`RoomEx+0x4C`: `[0]` destination RoomEx, `[4]` next, `+0xC` its tiles,
  `+0x10` `LvlWarp.txt` row) are set up as its tiles are (`0x66E260`, `0x66E360`).
- Warp tiles reach the client as units of type 5: `0x09` (11) `[5][guid][class u8][x u16][y u16]`
  from `SendUnitToClient`'s default case (`0x53BCD0`); the client makes one at that spot
  (`0x45CB90` → `0x4661C0` → `0x465FD0` case 5). `class` is the `LvlWarp` `Id`.
- C→S `0x13` with type 5 (`0x54AA90` → `0x548B00` case 5): within 5 subtiles `0x5550B0`, else the
  player walks there. `0x5550B0` → `0x6195A0` → `0x66AB00` finds the node whose `LvlWarp` `Id` is
  the unit's class, the destination room's node back, initializes that room (`0x61B730`) and
  returns its tile unit; the player goes to a free spot by it (`0x64E7B0`, mask `0x1C09`) through
  `0x554EA0` — as waypoint travel — then walks by the destination row's `ExitWalkX`/`Y`
  (`+0x14`/`+0x18`) with `0x0D` event 1.
- **Not found:** where the server allocates the tile units (their guid, and exactly where they
  stand). The allocator `0x555230`'s 44 callers pass no literal 5, and the D2Common room alloc
  `0x619890` calls a callback at `act+0x4C` whose setter was not found. The test server puts the
  unit on the warp cell's corner, gives it the next type-5 guid, and lands the player on the
  destination cell plus `ExitWalk`.

### Gold drops and pickup (2026-09-14)

A dying monster rolls its `MonStats.txt` `TreasureClass1` (the difficulty's column) in
`d2_data::treasure`, following the resolver `0x55A6D0`:
- The class is upgraded within its `group` to the highest `level` the monster has reached.
- `Picks > 0`: each pick draws `rand(NoDrop + ΣProb)` from the unit's seed (`0x6AC690C5` LCG);
  inside `NoDrop` nothing drops, else the first entry whose running `Prob` passes the draw. A
  class entry is pushed and resolved in place. `Picks < 0`: entry *i* drops `Prob_i` times.
  At most 6 drops (the default limit when no output array is passed).
- `NoDrop` for *n* players (1–8; the game's count averaged with a second count, `0x535790`, not
  yet identified — solo it is 1): `f = NoDrop/(NoDrop+ΣProb)`, new `NoDrop = ΣProb·fⁿ/(1−fⁿ)`.
- A gold pick makes a `gld` item whose stat 14 is `ilvl + rand(5·ilvl)`, at least 1 (`0x557AB0`;
  `ilvl` is the monster's stat 12, set in `0x55A550`), then `× mul >> 8` when the entry carries
  one (`0x55A6D0` at `0x55AF2F`, the entry's `+0x0A` word).

On the wire:
- **S→C `0x9C` action 0** (client `0x45EB10` → `0x4C25B0`), `[0x9C][action][size u8][category]
  [guid u32]`, then the item bits from `+8` read by `0x62A970`: flags 32 (`0x10` identified,
  `0x2000` just dropped → fall animation and sound, `0x200000` simple, `0x800000` on every item),
  version 10 (101), mode 3 (3 = ground), x 16, y 16, code 32 (`gld `), then — for an item type
  with the gold property — a 1-bit width flag and the amount in 12 or 32 bits. The client does
  not read the category for action 0; we send 0.
- **C→S `0x16`** (13): `[unit type u32][item guid u32][to cursor u32]`. For gold we remove the pile
  for everyone holding its room (`0x0A` type 4), tell the picker its gold as the engine does
  (`0x53E9B0`: a gain of 1–254 is `0x19 [gain]`, else stat 14 by `0x1D`–`0x1F`) and save.
- Piles stay in the game: a room coming near sends its piles without the drop flag, a room left
  behind sends their `0x0A`s.

Where a drop lands (`0x555DA0`): the search starts two subtiles right and three down from the
unit when a room is there, and `0x64E810` → `0x64DEA0` takes that spot or the free one with the
least `|dx|+|dy|` ring by ring out to 50 (each ring's side edges row by row, then its top and
bottom, first found winning a tie). Free means clear of the spawn mask `0x3E01` (wall, item,
object, door, no-path, pet) and seen from the unit past no wall or door (`0x801`, `0x66A670`). We
track walls and piles only, and look along a plain straight line.

Pickup: `0x16` goes through `0x54AAD0` to `0x548B00` with the unit type, as `0x13` does. Within 5
subtiles and unblocked the engine picks the item up (`0x563560` auto-place, `0x55CF50` to cursor),
else walks the player there; we do not check range. Gold (`0x55C850`) takes what the purse holds
(level × 10,000, `0x622E70`) and drops the rest as a new pile from the player (`0x55B030` →
`0x55A090`, placed the same way).

Not done: the second player count in `NoDrop`. `TreasureClassEx.txt` quotes an entry holding a
comma (`"gld,mul=1280"`); the quotes are stripped before the multiplier is read (until 2026-09-15
such entries were taken for unknown items and dropped nothing).

### Simple items: drops, belt, inventory, potions (2026-09-15)

A treasure pick naming an item code makes the item when it is simple: `Misc.txt` `compactsave`,
not gold, a quest item or a stack — potions, scrolls, gems, skulls, runes. Type picks (`weap3`,
`armo6`) and magic items (rings, amulets, charms, jewels) are skipped.

**Bits.** A simple item (flag `0x200000`) is written by `0x62AF80` and read by `0x62A970`:
flags 32, version 10, mode 3, then on the ground (mode 3 or 5) x 16 and y 16, otherwise body
location 4, column 4, row 4 and page + 1 3 (`+0x45` holds the page, −1 for none), the code 32,
the gold amount for type 4, and the quest-difficulty bits for quest items. A `.d2s` item starts
with `JM` 16 and ends with a realm-data bit (0, or 1 and 32 + 32 + 32). Version is the game's
`+0x78`: 101 in an expansion game, 2 in a classic one (`0x530930`). The flags written are the
item's with `0x80000` cleared and `0x800000` set (`0x6312B0`). A belt item's column is its slot
and its row 0: `0x63AFD0` places belt items at x = slot, y = 0 (bnemu's `slot % 4`, `slot / 4`
reading does not match it). Checked against a retail pickup capture byte for byte:
`9c 04 14 10 2b0b3efa 1000a0006500728206270302` is hp2 at column 9, row 3 of the inventory.

**Packets.** `0x9C` (`0x53EAE0`) is `[0x9C][action][size][category][guid]` and the bits; `0x9D`
(`0x53CEF0`) adds `[owner type u8][owner guid u32]` before the bits. The category is ItemsTxt
`+0x115`, `component` (16 for these items). The client's action sets are disjoint (`0x45EB10`,
`0x45EC70`); what the server sends is picked per frame from the item's action flags `+0x14`
(`0x5973F0`): `2`/`0x80` → `0x9C` 4, `4` → `0x9D` 5, `8`/`0x200` → `0x9D` 6, `0x400`/`0x2000` →
`0x9C` `0x0E`, `0x800` → `0x9C` `0x0F`, `0x1000` → `0x9C` `0x10`.

**Pickup** (`0x563560`, after gold): tomes take scrolls and stacks merge (`0x560020`); an item
for an empty body location is equipped (`0x55D710`); a beltable item (ItemTypes `Beltable`)
with `autobelt` (`+0x131`) goes to the belt (`0x63C790` → `0x63C600`: first a column already
holding a matching item, then the first free bottom slot) — mode 2, action flag `0x2000`; else
the inventory (`0x5600A0` → `0x63B950`) — mode 0, action flag `0x80`; with no room `0x55C9A0`
event `0x17` and the item stays. The inventory spot (`0x63B850`): for a one-row item in a
player's grid (`0x63B490`) every column from the right, each row from the bottom, keeping the
free spot with the highest contact score (`0x63B340`: occupied cells and grid edges along its
four sides; a tie keeps the first, a spot closed on every side wins at once); other shapes row by
row from the top (`0x63B620`, 2 × 2 and three-row items have their own passes, not read). Empty
10 × 4: (9, 3), (9, 2), (9, 1), (9, 0), (8, 3)… We send `0x0A` to everyone near, then the item
`0x9C` action `0x0E` (belt) or 4 (inventory) to the picker, flags `0x00A00010`. No belt worn is
taken as four slots.

**Use.** `0x26` (13, `0x54B560` → `0x562390`) uses a belt item (mode 2), `0x20` (13, `0x54B1E0`
→ `0x55E170`) a stored one (mode 0); both need the item useable and run `0x5BF240`, which calls
the `pSpell` routine from the table at `0x741790`. Taken out: from the belt `0x9C` action `0x0F`
(`0x561E70`), from the inventory `0x9D` action 5 (`0x55E000`), both with flag `0x20` added. The
belt then slides its column down (`0x55EDC0`, `0x9D` action `0x15` per moved item) — never
needed with one row. `pSpell` 3 (`0x5BE3F0`): each stat's calc `<< 8`; `hpregen` ×1.5 for
Amazon, Paladin, Assassin and ×2 for Barbarian (`0x62A5D0`), `manarecovery` ×1.5 for Amazon,
Paladin, Assassin and ×2 for Sorceress, Necromancer, Druid (`0x62A620`); doubled when
`rand(100) < rand(vitality) >> 1` (energy for mana, from the unit's seed); with `len` it becomes a
state (`healthpot`/`manapot`) whose per-frame rate is `(rate × frames left + total) / (len +
frames left)`. `pSpell` 5 (`0x5BEAC0`): each stat's percent of its maximum, at once. We model the
rate in 256ths a frame and tell the client by `0x95`; the state packets and `ValShift` precision
are not ported.

**Join.** `SendUnitToClient` for a player (`0x571F90`) sends its items through `0x534F80`:
stored `0x9C` 4, belt `0x9C` `0x0E` (own player only), equipped `0x9D` 6. The client's action 4
and `0x0E` handlers put the item on `0x7A6A70`, the client's own player, which `0x0B` sets
(`0x45CC50`); we send the items right after the stats that follow `0x0B`.

### Full items: making, moving, wearing, identifying (2026-09-15)

**Bits.** A full item (writer `0x62FFF0`, reader `0x62CBE0`) goes on after the code: socketed
items 3, the seed 32 (saves), item level 7, quality 4, a picture 3 behind a bit when the type has
`VarInvGfx`, an automagic id 11 behind a bit, the quality's ids (magic prefix 11 + suffix 11,
set/unique 12, rare names 8 + 8 then three prefix/suffix pairs of bit + 11 — the names and single
ids only when identified in a packet), runeword, personal name, the save's realm bit, armour's
defence (stat 31's width), maximum and current durability for armour and weapons, a stack's
quantity 9, sockets, then — in a save or for an identified item — the stat lists ended by
`0x1FF` (id 9, parameter, value in `ItemStatCost.txt` widths; 17/18, 48/49, 50/51, 52/53,
54/55/56, 57/58/59 carry their partners). A `.d2s` list is `JM`, a count leaving out socketed
items, the items. Five retail bodies read and write back byte for byte.

**Making** (`0x55A6D0` per treasure pick). Quality (`0x558640`): the `ItemRatio.txt` row by uber
and class-specific, `levels = monster level − item level`; each of unique, set, rare (type
`Rare`), magic: `chance = (ratio − levels / divisor) × 128`, with magic find `× 100 / (100 +
MF')` (MF' diminishing at 250/500/600 for unique/set/rare), no lower than `Min`, cut by the
treasure class's mod `× mod / 1024`; it is that quality when nothing is left or `rand(left) <
128`. Else superior when `(HiQuality − levels / div) × 128 ≤ 0` or its roll is under 128; else
normal when `(Normal − levels / div) × 128 < 1` or its roll is under 128; else low quality. Type
rules (`0x557450`): `Normal` types normal; `unique` items unique; `Magic` types at least magic
(unique for quest items); no rare for types without `Rare`. Base values (`0x557AB0`): armour
defence `minac + rand(maxac − minac + 1)`, durability max `durability`, current `rand(dur/2) +
dur/2`; stacks `minstack + rand(maxstack − minstack)`; the picture `rand(VarInvGfx)`. Then the
quality, falling back: low quality (`0x5C2D40`, a `LowQualityItems` row, a third of the
durability, three quarters of the defence) → normal; superior (`0x5C2970`, a random
`QualityItems` row that suits the type, its mods) → normal; magic (`0x5565E0`: a prefix half the
time, a suffix half the time or always without a prefix; unidentified) → superior → normal; rare
(`0x5C1BF0`: two names uniform among those whose types suit, then `[3,4,4,5,5,5,6,6][rand & 7]`
affixes, each side up to three, a coin choosing the side; unidentified) → magic; set (`0x5C25C0`,
rows for the code by `rarity`, Cow King's only on request) → magic with durability ×2 in LoD;
unique (`0x5566B0`, rows for the code by `rarity`, ladder rows only in ladder games, once a game
unless `nolimit`) → rare with durability ×3 in LoD. An affix (`0x5C1560`) is picked by
`frequency` (× `level` for items with `magic lvl`) among spawnable rows of the game's version whose
`level..maxlevel` holds the affix level (`magic lvl` added, or `ilvl − qlvl/2`, or `2·ilvl − 99`
near the top), whose `itypes`/`etypes` suit, whose class suits, `rare` for rare/normal/superior,
and whose group the item does not have. Staff mods (`0x5C0F90`) give wands, staves and scepters up
to three class skills by item level. After the quality: ethereal 1 in 20 in LoD (`0x556CA0`),
sockets for normal and superior items one in three up to the type's `MaxSock` by item level and 3/4/6
by difficulty, never body armour in classic (`0x556B60`, count `seed % max + 1`), and LoD's
automagic affix. Properties (`0x65FD70`) run each `Properties.txt` function (table `0x7462F8`),
the first's value handed to the rest: 1/2 roll, 3/4/8 previous or roll, 5/6 minimum/maximum damage
onto the stats the weapon uses (21/22 one-handed, 23/24 two-handed, 159/160 thrown; every one off
weapons), 7 enhanced damage (+1 maximum instead when the percentage adds nothing), 10 skill tab,
11 skill on event, 13 durability percent, 14 sockets, 15–17 fixed values, 18 by time, 19 charges,
20 indestructible, 21/22/24 parameters, 23 ethereal.

**Pickup and moves.** A picked-up item is worn when it is identified, its requirements are met,
it is not a throwing potion and a body location of its type is free and suits it (`0x55D710`);
else belt or inventory as simple items. `0x16` with `to cursor` lifts it (`0x9C` 1). Client moves
(sizes from the table): `0x17` drop [item] (5), `0x18` into a grid [item][x][y][grid] (17), `0x19`
lift [item] (5), `0x1A` wear [item][body] (9), `0x1C` take off [body u16] (3), `0x1D` swap worn
[item][body] (9), `0x1F` swap in grid [cursor][item][x][y] (17), `0x23` into belt [item][slot] (9),
`0x24` out of belt [item] (5), `0x25` swap in belt [cursor][item] (9). Each move sets an action
flag the per-frame pass `0x5973F0` turns into a packet: lifted `0x9D` 5 (the old column, row and
page kept in its bits, mode 4), put `0x9C` 4, worn `0x9D` 6 (flag 1: rebuild), taken off `0x9D` 8
(the old body location kept), body swap `0x9D` 9 twice (the lifted one flagged `0x80` first, the
worn one `0x40`), grid swap `0x9C` `0x0D` twice, belt `0x9C` `0x0E`/`0x0F`/`0x10`, dropped `0x9C` 2
(`0x563C00`, spot as for drops). The client's handlers (`0x45EB10`, `0x45EC70`) read those
flags and fields.

**Wearing counts** (`0x57B420`, a player's physical damage): `min/max` from stats 21/22 (23/24
when two-handed) — the weapon's base with its own enhanced damage (op 13) and flat bonuses —
plus stat 111, then `× (100 + stat 18/17 + strength × StrBonus/100 + dexterity × DexBonus/100) /
100` (strength alone for the fist), no lower than −90%, a roll between; `× mul/128`. Defence adds
each armour's base with its own `ac%` and every flat bonus; attributes, life, mana, stamina and
attack rating add to the player; an unidentified item's stats count for nothing. Saves and
`0x1D`–`0x1F` stats keep the base values; the client works out what items add.

**Identify.** Using a Scroll of Identify (`pSpell` 1 from `Books.txt` row 1) readies it: `0x3F
[SpellIcon 0][scroll guid][book skill u16]` (8, `0x53D220`). The client sends `0x27 [item][scroll]`
(9, `0x561ED0`): the scroll is used up (`0x9D` 5 used, from the belt `0x9C` `0x0F`), the item
flagged identified and sent again with flag 1 (`0x9D` `0x15`, `0x4C4C70` rebuilds it). The book
skill's charge count (`0x22`, `0x55E0D0`) is not sent.

**Starting items** (`0x534F10`): `CharStats.txt` `item1`…`item10` with `loc` and `count`, each
made normal at item level 1 (`0x534C70`), flagged `0x20000`, whole durability, a full stack, the
class's `StartSkill` a point on the first; beltable ones into the belt, those with a location worn,
the rest into the inventory as pickups place them.

### Vendors: stock, prices, buying and selling (2026-09-15)

**Opening.** After talking (`0x2F`), the menu's Trade sends `0x38 [action u32 1][npc guid u32]
[u32]` (13, `0x54BCA0` → `0x579D60`). Classes that trade: 147 Gheed, 148 Akara, 154 Charsi, 177
Drognan, 178 Fara, 199 Elzix, 202 Lysander, 252 Asheara, 253 Hratli, 254 Alkor, 255 Ormus, 257
Halbu, 405 Jamella, 511 Larzuk, 512 Anya, 513 Malah (gamble, action 2: 147, 199, 254, 405, 512,
514). `0x579430` → `0x578B30` makes the stock once per NPC (`0x576980`), again when the store was
flagged stale and the opener is the only one talking to the NPC (`0x537230` flags it after
240,000 ms), then marks every stock item with unit flag 4, which the per-frame pass sends as `0x9C`
action `0x0B` (`0x53EF30`; removal is flag `0x10`, action `0x0C`). The client (`0x4C3C00`) puts
each into the NPC it is interacting with (`0x7C0D25`), at the column, row and page in its bits
(page + 1 in the three-bit field). Closing sends `0x30 [unit type][npc]` (9).

**Stock tables.** The store record (`0x44` bytes per NPC, `game+0x1D24`) copies its vendor's lists
built by `0x536D50`: every `spawnable` item whose vendor `Max` (`+0x157+v`) or `MagicMax`
(`+0x179+v`) is set, `PermStoreItem` (`+0x1A4`) ones into a code list, the rest into 12-byte
entries `[Min][Max][MagicMin][MagicMax][code u32][MagicLvl]`. The vendor index `v` is not the
column order: `Min` columns sit at `+0x146` Akara, Gheed, Charsi, Fara, Lysander, Drognan, Hralti,
Alkor, Ormus, Elzix, Asheara, Cain, Halbu, Jamella, Malah, Larzuk, Drehya. NPC → index and act:
table `0x731188` and the switch in `0x536070`.

**Stock** (`0x576980`). Store level `L` = player level + 5, capped in normal at 12/20/28/36/45 by
act (`0x576890`). Per entry with item level (`+0xFD`) ≤ `L`: `n = rand[Min, Max+1)` when `L < 25`
(else 0) — then skipped if the item is LoD-only in a classic game — `n` items of quality
(`0x576900`) `L<5`: roll%100 > 90 low; `L<10`: > 85 superior; else > 74 superior; normal
otherwise. If `bitfield1 & 1` and `MagicLvl ≤ L`: `rand[MagicMin, MagicMax + e)` magic items, `e`
1, or `rand[1,3)+1` when `L ≥ 25`. More than 32 failures stop the normal loop. Then one normal
item of every always-stocked code, arrows and bolts at their full stack. Each item (`0x576330`):
in nightmare/hell with player level > 25, `r = rand(100000)`: NM `r < L·64+4000` → `ubercode`, else
`NightmareUpgrade`; hell `r < L·16+1000` (LoD) → `ultracode`, else `r < L·128+5000` → `ubercode`,
then `HellUpgrade` over either. Made by `0x559CE0` with create flag 2 (never ethereal); a low
quality `Cracked` is made again (five tries). Page from the type's `StorePage` (`0x62E880`), placed
as a pickup is in the NPC's 10 × 10 grid, a full page 1 spilling to 2; stackable repairable items
full, durability full (`0x5761C0`); flagged identified.

**Prices** (`0x62FDC0` → `0x62EFB0(player, item, difficulty, quest flags, npc class, mode)`, mode
0 buy, 1 sell, 2 gamble, 3 repair). Starter items 1. Base: ear `level × cost`; body part `cost +
monster level × 8`; tome `CostPerCharge × quantity + cost`; quiver type `cost × quantity / 1024`
(repair `maxstack × cost / 1024`); else `cost`, with the divisor `maxstack` for stackables. Armour
`cost × defence / maxac`. Not magic: staff skills (`0x62EDD0`, stat 107 by skill: `(cost mult ×
price / 1024 + cost add) × (2·level − 1)`, sell `/4096`). Identified: the automagic affix's
`multiply`/`add` term, then by quality — low: `−price/2`; superior/tempered: stat costs
(`0x628E70`); magic: prefix and suffix terms then stat costs; set/unique: their `cost mult`/`cost
add`; rare/crafted: all six affixes then stat costs — each term over the divisor, then staff skills
for magic and better. Stat costs walk the item's full stat list (base stats as `0x557AB0` sets
them: armour 31, 20, 67, 72, 73; weapons 70, 72, 73, 21–24, 159/160, 68): `Multiply × price ×
value / 1024 + Add`, skill stats (encode 1) with the skill's `cost mult`/`cost add`, encode 2/3 by
the packed level, sell at `/4096` for skills. Sockets add half each filling's cost. Selling,
ethereal `/4`, class items `/4`, a broken ethereal item 0. Then `Npc.txt` (`0x656900`, record
`0x4C`): `sell mult` (`+4`) on the buy price, `buy mult` (`+8`) on the sell price, `rep mult`
(`+0xC`), quest-flag multipliers, `× quantity` (a repairable stack sells `maxstack × p − (maxstack −
qty) × repair`), sell capped at `max buy` by difficulty (`+0x40`), less the player's
reduced-prices percentage.

**Buying** `0x32 [npc][item][u32: bit 31 fill, low word 0 trade / 2 gamble][cost u32]` (17,
`0x54BAC0` → `0x577F30` → `0x577830`). Gold is stat 14 + stat 15. The store item is copied
(`0x55A2A0`); gold taken (`0x576D90`, inventory first); the copy into the belt when `autobelt` or a
slot is free (`0x628BA0`, `0x55E9B0`), else the inventory (`0x560200`); no room refunds and
answers 10. Not always stocked (`0x576ED0`, which counts hp4/hp5/mp4/mp5 in NM/hell): removed from
the store. Fill repeats for always-stocked items until the belt is full (that one goes to the
inventory). **Selling** `0x33 [npc][item][mode u16][u16][cost]` (17, `0x579510`): the item's mode
must match; not `0x1000`-flagged; price; unless `Cracked`, broken, an ear, personalised, ethereal
or always stocked, a mended copy joins the stock and the price is the lower of the two; removed
(`0x9D` 5 used from a grid); paid through `0x55B060` (what the purse cannot hold is dropped as gold).

**`0x2A`** (15, `0x53D740`): `[0x2A][kind][result][4 bytes left from the stack][item u32][gold
u32]`, gold after the trade. Bought `4, 0, copy`; sold `3, 1, item`; refusals kind 0 with result 7
(no such item or cursor busy), 9 (cannot), 10 (no room), 11 (store not open), 12 (not enough
gold). The client (`0x4B6390`) stores it for the trade screen.

### Skills: learning, choosing and missile casts (2026-09-15)

**Join.** The player's skills go out after its items: `0x94 [count u8][guid u32]` then `[skill
u16][base level u8]` each (builder `0x53C5D0`; retail lists the common skills 0, 2, 1, 217–220, 4,
5, 3 first), then `0x23` for the left and right buttons. A `.d2s` keeps the class's 30 skill levels
(`if`, class skills in id order: Amazon 6, Sorceress 36, Necromancer 66, Paladin 96, Barbarian
126, Druid 221, Assassin 251) and the button skills at header `0x78` (left) and `0x7C` (right).

**Learning** `0x3B [skill u16]` (3, `0x54BD90`): refused unless the skill is the player's class's
(`0x56C700`, record `+0xC`), `InGame`, the player's level and attributes meet `reqlevel` and
`reqstr`/`reqdex`/`reqint`/`reqvit` (`0x6447D0`, `0x644920`, `+0x176`–`+0x17C`), it has each
`reqskill` (`+0x17E`–`+0x182`) at base level ≥ 1, the base level is under `maxlvl` (`+300`, 20 when
blank, `0x4AA8B0`) and stat 5 holds `skpoints` (calc `+0x170`, 1 when blank, `0x570080`). Then stat 5
drops and `0x21 [unit type][0][guid][skill u16][base u8][item bonus u8][unset]` (12, `0x53C4A0`).

**Choosing** `0x3C [skill u16][u16: bit 15 left][item guid u32]` (9, `0x54BE70`): bit 31 of the
first dword is the left button — it picks the setter `0x622F10` whose getter `0x620190` the left
packets `0x05`/`0x06` read — and the skill must be one the unit has at level ≥ 1. The reply `0x23
[unit type][guid][left u8][skill u16][item guid]` (13, `0x53C590`) carries the same bit; the client
(`0x45DE10`) sets the left skill for a nonzero byte.

**Casting.** `0x05`/`0x08` left and `0x0C`/`0x0F` right at a spot `[x u16][y u16]` (5), `0x06`… on a
unit (9); no skill id is sent. The skill runs its `anim` mode; on the action frame
`SkillsSrvDoSkill` (`0x56F640`) pays `max(minmana << 8, (mana + lvlmana·(lvl−1)) << manashift)` from
stat 8 (`0x56BFE0`) and its do-function shoots `srvmissile`. A missile (`Missiles.txt`, stride
`0x1A4`) moves `Vel` sixteenths of a subtile a frame (`Vel << 8` as a unit velocity, `0x4CD540`) for
`Range + LevRange·lvl` frames, ending at walls and on units (`0x5AE1F0`); `pSrvHitFunc` 1 hits every
unit within `sHitPar1` (Fire Ball: 4). Damage (`0x644D50`/`0x644E40`, physical `0x647D00`):
`(E + bracket) << HitShift` 256ths, the bracket (`0x644B70`) adding each level's column — levels
2–8 the first, 9–16 the second, 17–22 the third, 23–28 the fourth, 29 up the fifth — and
`EDmgSymPerCalc` percent on the elemental part; each part less the target's resistance. Calcs are
compiled at load (`0x611BD0`) into bytecode run by `0x6C0BC0`: arithmetic, comparisons, `?:`,
`min`/`max`, `lvl`, `par1`–`8`, `lnXY` = `parX + (lvl−1)·parY`, `dmXY`, `skill('Name'.blvl)`.

## 6. Server packet builders (opcode → function)

`scripts/d2re/server_send_builders.py <Game.exe>` finds every call to the queue function
`0x53B280` (130 sites) and recovers the opcode from the byte written into the buffer or, for
builders that take it as an argument, from the caller's `MOV DL, imm8`. A row is accepted only
if the queued length equals the S→C size table (§2). Result: **93 opcodes** mapped to their
server builders, and every builder traced by hand in §4 is among them (`0x00`–`0x04` via
`0x53B320`/`0x53B340`/`0x53B390`, `0x15` `0x53BC10`, `0x59` `0x53E8F0`, `0x5B` `0x53C940`,
`0x7E` `0x53DB70`). The unresolved rows are builders fed a prebuilt struct or a caller register the
scan does not follow; they are printed as unresolved, never guessed.

The Ghidra project carries names for the functions in §3–§4 (`SendPacketToClient`,
`CompressPacket`, `DispatchConnectionPacket`, `HandleSrvJoinGame`, `HandleSrvJoinAct`,
`UpdateClientsJoinState`, `SendUnitToClient`, …) and plate comments on the key ones.

## 7. Next

- Walk the player-state helpers under `SendUnitToClient` to the exact packet list for one's own
  player (stats, skills, items, states).
- Shops (§5 *Vendors*): gambling, repair (`0x35`), Cain's identify (`0x34`), hirelings (`0x36`),
  scrolls into tomes and arrows onto a worn quiver when bought; NPC quest messages.
- Waypoint travel to other acts (`0x53ACC0`), which needs their maps.
- Collision for the town and other preset levels (the wilderness and Act I's maze caves are done),
  then walk checks (`0x548EF0`) and paths (`0x64DEA0`, `path.zig`) in place of straight lines;
  cross-level near rooms by visibility slots (`0x66C220`).
- Where the server allocates warp tile units (§5 *Maze levels and warps*); the preset levels
  behind the caves (Cave Level 2 and the other treasure levels, `DrlgType` 2) and the other acts'
  mazes.
- Combat (§5 *Fighting*): the per-class AI routines; skills beyond missiles (§5 *Skills*): melee
  skills' own effects, auras, curses, summons, novas and other do-functions, cold and poison
  effects, item bonuses to skill levels, faster cast rate, and `0x73`/`0x4C`/`0x4D` for other
  players to see casts.
- Loot (§5 *Full items*): stacks merging, tomes and Town Portal, belts worn (more rows), the
  stash and cube, socketing, the book skills' charges (`0x22`), other players seeing what is
  worn, requirements from affixes and from what other items add, elemental damage and
  resistances, Barbarian one-or-two-handed swords, repair.
- bnemu (MIT, permission recorded in `docs/LEGAL.md`) has worked items and vendors to port from;
  its wilderness collision is approximate, so collision stays on libd2.
- Unique packs and champions; wandering monsters; NPCs walking their DS1 paths (`0x666120`); the
  set pieces' map units (read at room init).
- The byte at `0x68`+20 and the `0x6A`/`0x6C`/`0x6E` handlers.
- A packet capture from the real engine (`docs/D2GS-RUST.md` §2 oracle) would confirm the dump
  faster than reading it; §4 and §6 say where to look in that capture.

Legal: this is the same decompilation-derived position recorded for libd2 in `docs/D2GS-RUST.md`
§4 — the notes describe behaviour and cite addresses; no Blizzard code or data is copied.
