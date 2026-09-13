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
| 4 | C→S `0x6B` ENTERGAME (1) | SrvJoinAct `0x530190` → `ClientAddPlayerToGame` `0x539760`: load the character (new-character path `0x532590` calls `SendUnitToClient`), check expansion/hardcore against the game (failure → `0xB4` with its code) | `0x59` for the player, **unplaced (0, 0)**; `0x0B` `[type][guid]` "this unit is yours" (`0x537930`); `0x5F`; `0x7B` hotkeys; `0x23` selected skill ×2 (right, left) | 1 |
| 4a | | `0x52C210`: build the act if needed (`0x53AC70`, seed `game+0x7C`, difficulty `game+0x6D`) | `0x03` LoadAct (12), then `0x53` (10) | 2 |
| 4b | | place the player `0x5394A0` | `0x07` `[room tile x u16][room tile y u16][level u8]` for the spawn room, `0x15` ReassignPlayer (11) `[type][guid][x][y][1]`, `0x7E` (5) | 3 |
| 5 | next server frame | sUpdateClients `0x52D440` for state 3, `[JOIN 6] SCMD_STARTACT` | `0x04`; item/equipment pass `0x55DF00`; `0x5B` roster records both ways with every player already in the game (plus `0x8E` per entry of that player's unit`+0x60` list), then the joiner's own (`0x52C410`, confirmed in disassembly); `0x55B620`; host `+0x14`; broadcast `0x5A 02 04 …` "joined our world" (`0x54AA40`) | 4 |

Units the client sees — itself included — arrive through **`SendUnitToClient 0x571F90`**,
called from eight room/visibility paths rather than from the join itself. By unit type:
player → `0x59` (26: `[guid u32][class u8][name 16][x u16][y u16]`, builder `0x53E8F0`) + `0x75`
+ player-state helpers (`0x571620`, `0x571CD0`, `0x570E30`, `0x5484B0`; these reach builders for
`0x0B/0x76/0x7C/0x92`, `0x23`, `0x9E`–`0xA5`, `0xAB`, and item packets); monster → `0xAC` (+ `0x21`
per skill); object → `0x51` (+ `0x60`, `0x82` for town portals); item → `0x9C`/`0x9D`; warp tile →
`0x09`. So "standing in town" = the join table above **plus** the room-activation stream.

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
- Map seed `0x12345678` with the spawn beside the Rogue Encampment campfire, from libd2's
  object dump for that seed; walkability of that exact subtile is not checked.
- `0x5B` rosters and the post-`04` room stream are not sent.

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
  player (stats, skills, items, states), and the room-activation callers that decide when units
  are sent.
- The byte at `0x68`+20 and the `0x6A`/`0x6C`/`0x6E` handlers.
- A packet capture from the real engine (`docs/D2GS-RUST.md` §2 oracle) would confirm the dump
  faster than reading it; §4 and §6 say where to look in that capture.

Legal: this is the same decompilation-derived position recorded for libd2 in `docs/D2GS-RUST.md`
§4 — the notes describe behaviour and cite addresses; no Blizzard code or data is copied.
