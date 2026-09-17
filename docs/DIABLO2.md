# Diablo II

Status of Diablo II support, how the closed realm is put together, and how to test it with a
real client.

The Diablo II server is here mainly for older computers — Mac OS 9, Mac OS X 10.4 and earlier —
that can no longer play on Battle.net because their patches are no longer made. It is
educational and supports that older architecture; on a modern computer, buy *Diablo II:
Resurrected* for the true Battle.net experience (see the README's *Purpose*).

| Button in the client | What it is | Status |
|---|---|---|
| **Open Battle.net** | BNCS login + chat; characters live on the player's PC; games are peer-to-peer | Login and chat work through the same path as StarCraft |
| **Battle.net** (closed realm) | BNCS login, then a *realm* connection (MCP) for server-side characters | **Characters: verified with a real LoD 1.14d client (2026-09-13).** Games: on the separate game server (§2, *Games*) |

---

## 1. What works

A closed-realm client can:

- log in (X-SHA-1, the same accounts as StarCraft/Warcraft II — `Tagban`, not `Tagban@bncc`),
- see the realm (`SID_QUERYREALMS2`) and log on to it (`SID_LOGONREALMEX`),
- open the realm connection and **list, create, select, delete and upgrade characters**, stored
  in this server's database (`characters` table, schema v2),
- enter chat as the character: shown as **`Character*Account`** with the character's portrait
  (class, level, hardcore/expansion/ladder bits) in its statstring,
- open the game lobby and the **ladder**: ladder characters ranked by experience, softcore and
  hardcore, classic and expansion, overall and by class, down to rank 500.

**Creating or joining a game answers "Server Down" / "Game does not exist."** That is
expected: hosting a closed-realm game needs a Diablo II game server (§5).

Rules the realm enforces:

- Character names are **unique across the whole realm** (case-insensitive), 2–15 letters with
  at most one `-` or `_` in the middle — the client's own rules.
- Druid and Assassin require the expansion. A **classic (D2DV) client neither sees nor can
  make expansion characters**; LoD sees both and can upgrade classic characters.
- A character belongs to one account: nobody else can list, select, delete, or chat as it.
- Up to `diablo2.max_characters` per account (default 18).

### Ladder seasons

The ladder runs in **seasons**, ended by hand from the admin panel's **D2 ladder** page
(`/d2/season`), since how long a season should last depends on how many people play (tagban,
2026-09-14). The page shows the season, when it began and how many softcore and hardcore
characters are on the ladder. **End season** turns every ladder character, softcore and hardcore
alike, into a normal character that keeps its level, items and progress, in the realm and in its
`.d2s`. The ladder is then empty, and new ladder characters belong to the next season. A player
still in a game when the season ends plays that game as a ladder character and is saved without
the ladder bit.

The season number and start are kept in `bnetccd-d2-season.json` beside the account database (in
memory with in-memory storage). Unlike StarCraft and Warcraft II, the Diablo II ladder needs no
wins: a character is on it from creation.

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

### Games: the game server

Games run on the **Diablo II game server**, a separate program kept in its own private repository
(it is built from decompilation of the 1.14d `Game.exe`; see `docs/LEGAL.md` §2). This server's
realm hands it create and join requests over a small local link, and the client then plays on
port 4000:

```toml
[diablo2]
game_server_link = "127.0.0.1:6119"   # where the game server dials in
game_server_token = "..."             # the same secret as link_token in its d2gs.toml
```

- **Game server not running:** chat, the realm and characters work as normal; *Create Game*
  answers "Server Down" and joins answer "game does not exist" (the client's own messages).
  The game server links back by itself when it starts.
- **Restarting either one leaves the other up:** a game server update does not drop chat, and a
  realm restart does not end games in progress. Both processes open the same database, so the
  game server saves characters while the realm is down.
- Internet players need TCP 4000 forwarded to the game server; LAN clients don't.

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
- **The portrait** is 33 bytes: see `bnetcc_proto::d2` for the byte table. Its equipment bytes
  are graphics values, not items; the server writes a map for chat bots,
  [`d2-equipment.json`](D2-EQUIPMENT-FILE.md), into the BNFTP files directory at startup, and a
  pack of the character-select animations, [`d2-characters.zip`](D2-CHARACTER-PACK.md), so a bot
  can draw the character.

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
data_dir = ""                         # your 1.14d install: Game.exe and the MPQs
equipment_file = "d2-equipment.json"  # written into [files] dir for bots; "" disables
character_pack = "d2-characters.zip"  # the character animations as layers, for bots; "" disables
game_server_link = ""                 # e.g. "127.0.0.1:6119": where the game server links in
game_server_token = ""                # its link_token
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

**Decided 2026-09-13: a native Rust engine instead** — a port of `jaenster/libd2` (MIT, a Zig
reimplementation of the 1.14d engine) that runs on macOS, Linux and Windows. **Since 2026-09-17
it is its own program in a private repository**, linked to this realm (§2, *Games*), so its
frequent updates restart only the game server. Jaenster's game server survives only as a test
oracle for it.

## 6. Sources

- [BNETDocs](https://bnetdocs.org/) — packet formats, the portrait byte table ("Chat
  Statstrings"), `SID_ENTERCHAT` semantics.
- [`jaenster/d2-dedicated-server`](https://github.com/jaenster/d2-dedicated-server), MIT,
  © 2026 jaenster — its `apps/realmd` realm, which a retail 1.14d client renders, was read to
  confirm field layouts and result codes (the `u16` in `MCP_CHARCREATE`, the `LOGONREALMEX`
  port encoding, the game-list terminator, which refusal codes keep the realm connection).
  No code was copied; see `docs/LEGAL.md` §1.
