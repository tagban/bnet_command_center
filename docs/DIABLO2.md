# Diablo II

Status of Diablo II support, how the closed realm is put together, and how to test it with a
real client.

| Button in the client | What it is | Status |
|---|---|---|
| **Open Battle.net** | BNCS login + chat; characters live on the player's PC; games are peer-to-peer | Login and chat work through the same path as StarCraft |
| **Battle.net** (closed realm) | BNCS login, then a *realm* connection (MCP) for server-side characters | **Characters: verified with a real LoD 1.14d client (2026-09-13).** Games: need a game server — not yet |

---

## 1. What works

A closed-realm client can:

- log in (X-SHA-1, the same accounts as StarCraft/Warcraft II — `Tagban`, not `Tagban@bncc`),
- see the realm (`SID_QUERYREALMS2`) and log on to it (`SID_LOGONREALMEX`),
- open the realm connection and **list, create, select, delete and upgrade characters**, stored
  in this server's database (`characters` table, schema v2),
- enter chat as the character: shown as **`Character*Account`** with the character's portrait
  (class, level, hardcore/expansion/ladder bits) in its statstring,
- open the game lobby: the game list is empty, the ladder is empty.

**Creating or joining a game answers "Server Down" / "Game does not exist."** That is
expected: hosting a closed-realm game needs a Diablo II game server (§5).

Rules the realm enforces:

- Character names are **unique across the whole realm** (case-insensitive), 2–15 letters with
  at most one `-` or `_` in the middle — the client's own rules.
- Druid and Assassin require the expansion. A **classic (D2DV) client neither sees nor can
  make expansion characters**; LoD sees both and can upgrade classic characters.
- A character belongs to one account: nobody else can list, select, delete, or chat as it.
- Up to `diablo2.max_characters` per account (default 18).

### Verified against a real client

A retail Diablo II: LoD 1.14d client (Windows, version byte `0x0E`) on 2026-09-13: created an
account, logged on to the realm, created a ladder expansion Barbarian, entered chat as
`TestBan*TestRep`, joined channels and talked, and returned to character select. Observed on the
wire, worth knowing:

- **Creating a character goes straight into the realm** — the client sends `MCP_MOTD` and then
  `SID_ENTERCHAT` for the new character with **no `MCP_CHARLOGON`**. Anything that must happen
  "when a character is selected" cannot hang off `MCP_CHARLOGON` alone.
- After `SID_ENTERCHAT` the client sends `SID_GETCHANNELLIST` (before it), `SID_NEWS_INFO`,
  `SID_CHECKAD` every 15 s, and `SID_JOINCHANNEL "Diablo II"`. Leaving to character select is
  `SID_LEAVECHAT` → `SID_QUERYREALMS2` → `MCP_CHARLIST2` on the still-open realm connection.
- **Account creation needs `tos-unicode_USA.txt`** in the BNFTP files directory (a copy of
  `tos_USA.txt` is fine). Without it the client disconnects when "Create Account" opens.
- Ticking **Ladder** makes the client show its own ladder notice before sending
  `MCP_CHARCREATE` with status `0x60` — not a server error.

## 2. Testing from Windows

Nothing to forward: the realm shares port 6112 with login (§3).

1. Point Diablo II at this server as a gateway, the same way as for WarCraft III (the
   `Diablo II Battle.net gateways` registry value, or a gateway editor). No loader is needed —
   Diablo II has no server-signature check. **1.14d** is the version the reference realm was
   verified against; 1.13c and older should work but are untested.
2. Click **Battle.net** (not Open Battle.net). Log in or create an account.
   *Expect:* an empty character-select screen with "Create New Character".
   *If instead* you get "realm unavailable" or it hangs, the log will show where (below).
3. Create a character — try one expansion and one hardcore. *Expect:* it appears in the list
   with the right class and a "hardcore" / "expansion" title.
4. Select it. *Expect:* the lobby/chat. Other clients see you as `Character*Account`.
5. Try **Create Game**. *Expect:* "Server Down". (Not a bug — §5.)
6. Delete a character, log out, log back in. *Expect:* the rest are still there.

What the server logs for a test session (temporary INFO-level tracing, `bnetccd.out`):

```
D2 frame in    id=0x40 …          realm list requested
realm logon    account=… ip=… port=6112
realm connection started          MCP_STARTUP accepted
MCP frame in   id=0x19 …          character list
character created name=… class=Sorceress status=0x20
character selected character=…
entered chat as a realm character name=Tyrael*Tagban
D2 session ended …
```

`unhandled Diablo II packet` / `unhandled realm request` lines name anything the client sent
that we do not answer yet — that is the first thing to look at if the client stalls.

**Internet players** need `diablo2.address` (Settings → Diablo II realm) set to the public
address or hostname. A client on the LAN is always handed the LAN address it connected to; a
client from outside is handed the configured address, or the LAN address if none is set —
which it cannot reach.

### Game-server handshake test (experimental, off by default)

This checks the riskiest part of the future game server — Blizzard's compression and join
sequence (`docs/D2GS-114D-WIRE.md`) — against a real client, with no world behind it.

```toml
[diablo2]
data_dir = "/path/to/Diablo II"   # the folder holding a 1.14d Game.exe
game_server_probe = true
```

Restart. The log says `Diablo II game server HANDSHAKE TEST is on`, or why it is not (wrong
`Game.exe`, port 4000 taken). Internet players also need TCP 4000 forwarded; LAN clients don't.

1. Select a character and **Create Game** (Normal). *Expect:* the client loads into the Rogue
   Encampment, standing on the waypoint. *Before:* "Server Down". (Verified 2026-09-13.)
   Each game has its own map seed, so the camp's layout — and the side its exit is on —
   changes from game to game (with the MPQs; without them every game is the same camp).
   (Verified 2026-09-13.)
2. *Expect, around the waypoint* (the 3×3 rooms around it, when `data_dir` holds the MPQs):
   the NPCs and objects nearby standing still — in the camp tagban tested, Warriv, Kashya, Akara,
   Charsi, Gheed and five rogue guards — the torches lit, the bonfire, the stash and the
   waypoint (active, blue). What is near depends on the layout. Chickens wander as before —
   the client spawns those itself. (Verified 2026-09-13.)
   *Expect to talk and use things:* clicking Akara, Kashya, Charsi, Gheed or Warriv opens their
   menu (Talk has nothing to say yet; Trade, Hire and the rest do nothing), clicking the stash
   opens it (empty), clicking the waypoint opens its menu with the Rogue Encampment ticked
   (see step 3 for travel). *Expect the day to pass:* the bonfire burns low by day and lights up
   at dusk, about 14 minutes after the game was created.
3. *Expect walking to load the world:* the server follows your character, so NPCs and
   objects further from the waypoint appear as you approach, and walking out of the camp
   shows Blood Moor's ground as you go (`player entered a level level=2` in the log), on to
   Cold Plains, Stony Field, Dark Wood, Black Marsh and Tamoe Highland, cliffs and borders
   included up to the black beyond them. Cold Plains, Stony Field, Dark Wood and Black Marsh
   have their waypoint standing on its pad (the big pads with two lit torches), dark: clicking
   it turns it on, and clicking again opens the menu with that area ticked. Choosing another
   area you have ticked takes you to its waypoint (`waypoint travel level=…` in the log).
   Otherwise the areas are empty — no monsters, shrines or other objects, and cave entrances
   do not lead anywhere. Walking through a fence can make the server think you are somewhere
   you are not (it has no collision).
4. *Expect nothing else to work:* life/mana/stamina show (level-1 values from
   `charstats.txt`), NPCs never walk, there is no combat, no shop and no travel to other
   acts. Esc → Save and Exit returns to chat (verified).
5. Send the log from the moment you clicked Create Game.

What it shows, in order:

```
game created game=… id=…                       realm accepted MCP_CREATEGAME
test game created … map_seed=… map=… spawn=…   the camp this game got
sending client to the game server id=… ip=…    MCP_JOINGAME
game connection; sending AF 01                 client reached port 4000
D2GS packet in op=0x68 …  /  GAMELOGON …       the client's logon, every field
D2GS packets out packets=01… 00                GameFlags + loading
D2GS packets out packets=02                    load success
D2GS packet in op=0x6b                         ENTERGAME: the client accepted our compression
D2GS packets out packets=59… 5e… 28… 29… 0b… 23… 23… 03… 53… 07… 07… 51… ac… aa… 6d… 15… 7e…
                                               player, act, rooms and their units, placement
D2GS packets out packets=04                    load complete
D2GS packet in op=…                            whatever the client asks for next
```

The last lines are the point: how far down this list the client gets, and what it sends after
`04`. No `0x6b` means it did not accept the first frames; a disconnect right after a
`packets out` line names the packet it rejected. `Rogue Encampment built` at start-up and
`room_packets=…` on the join line say the town was sent; without them the MPQs did not load.

## 3. How it fits together

```
client ── 0x01 FF … ──────────────▶ :6112  BNCS login (bnetccd session.rs)
          SID_QUERYREALMS2  ◀─────  1 realm: server.realm ("bncc")
          SID_LOGONREALMEX  ◀─────  cookie, ticket id, realm IP, port 6112
client ── 0x01 <len> 01 … ────────▶ :6112  realm / MCP (bnetccd realm.rs)
          MCP_STARTUP (ticket) → MCP_CHARLIST2 → MCP_CHARCREATE → MCP_CHARLOGON
client ── (login connection) SID_ENTERCHAT "Tyrael", "bncc,Tyrael"
          ◀── "Tyrael*Tagban", "PX2Dbncc,Tyrael,<portrait>", "Tagban"
```

- **One port.** Both connections open with protocol selector `0x01`. The next byte tells them
  apart: every BNCS frame starts `0xFF`; an MCP frame starts with its length, whose low byte
  is never `0xFF` for a startup packet. Real Battle.net did the same thing with separate hosts.
  The check happens only when the realm is enabled, and waits at most 2 s for a client that
  stays silent after the selector (it is then treated as a login client).
- **No realm cryptography.** `SID_LOGONREALMEX`'s sixteen `u32`s are opaque to the client,
  which forwards them to the realm. Because login server and realm are one process, they carry
  a random 64-bit handle into an in-memory ticket table (`Node::mint_realm_ticket`). A ticket
  lives as long as the login connection that minted it — the client reconnects to the realm
  with the same data after each game.
- **The portrait** is 33 bytes: see `bnetcc_proto::d2` for the byte table.

Wire layouts that matter and are easy to get wrong:

| Packet | Detail |
|---|---|
| `SID_LOGONREALMEX` reply | Port is a **network-order `u16` followed by `u16 0`**, not a `u32`. A reply of ≤ 8 bytes is a failure (`cookie, status`). |
| `MCP_CHARLIST2` | `u16 requested, u32 total, u16 returned`, then `u32 expiry, cstr name, cstr portrait` each. Expiry `0xFFFFFFFF` = never. |
| `MCP_CHARCREATE` request | `u32 class, u16 status, cstr name` — the `u16` is easy to miss. |
| `MCP_CHARLOGON` refusal | `0x46` returns to character select *keeping* the realm connection. |
| `MCP_GAMELIST` | One packet per game, then a terminator whose token is `0xFFFFFFFE`. |
| `MCP_JOINGAME` failure | All six fields are still sent; the client reads them before the result. |
| `MCP_MOTD` | A pad byte, then the string. |

## 4. Configuration

```toml
[diablo2]
realm = true                          # offer the closed realm (never in warnet mode)
description = "Diablo II closed realm"
address = ""                          # public host/IP for internet players
max_characters = 18
```

The realm's *name* is `server.realm`.

## 5. Games: the game server question

A closed-realm game runs on a **D2GS**, a separate server the client connects to on port
4000 after `MCP_JOINGAME`. Until recently the only one was Blizzard's closed-source, Windows-only
binary.

[`jaenster/d2-dedicated-server`](https://github.com/jaenster/d2-dedicated-server) (**MIT**) is
an open realm + game server that runs the game's **own** server engine headlessly — 1.14d
under wine, or a native Linux port of the macOS 1.14d build — and has a retail client
creating, joining and playing games. Its realm (`realmd`) and game servers meet in Redis rather
than over a socket; characters are `.d2s` saves the game server reads and writes.

**Decided 2026-09-13: a native Rust engine instead** — port `jaenster/libd2` (MIT, a Zig
reimplementation of the 1.14d engine) and host the game server inside `bnetccd`, so it runs on
macOS, Linux and Windows. Plan in [`docs/D2GS-RUST.md`](D2GS-RUST.md). The earlier sketch of
using jaenster's game server as-is, kept for reference (it survives only as a test oracle):

1. Run its `d2gs` (Linux containers; on this Mac that means a Linux VM — Apple Silicon adds
   x86-64 emulation).
2. Teach `bnetccd`'s realm its Redis contract (`docs/redis.md` in that repo): publish create/join
   requests to a game server's queue, stage the character's `.d2s` for it, read its game events
   back, and hand the client a join token and the game server address.
3. Store the `.d2s` it writes in `characters.save` (the column exists for this).

That is a project of its own; the character work above is the prerequisite for it and is
useful without it.

## 6. Sources

- [BNETDocs](https://bnetdocs.org/) — packet formats, the portrait byte table ("Chat
  Statstrings"), `SID_ENTERCHAT` semantics.
- [`jaenster/d2-dedicated-server`](https://github.com/jaenster/d2-dedicated-server), MIT,
  © 2026 jaenster — its `apps/realmd` realm, which a retail 1.14d client renders, was read to
  confirm field layouts and result codes (the `u16` in `MCP_CHARCREATE`, the `LOGONREALMEX`
  port encoding, the game-list terminator, which refusal codes keep the realm connection).
  No code was copied; see `docs/LEGAL.md` §1.
