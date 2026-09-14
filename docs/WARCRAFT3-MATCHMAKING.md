# WarCraft III ladder matchmaking — research and plan

Research, 2026-09-13. Goal: bring back classic Battle.net's **Anonymous Matchmaking** (AMM,
the client's **Play Game** and **Arranged Teams** buttons) and the WarCraft III ladder for the
classic `WAR3`/`W3XP` client (tagban's test client is 1.27b, version byte `0x1B`). Modern
WarCraft III has nothing like it; classic clients still have the whole UI, and it only needs a
server to answer.

Status today: chat and custom games work (`WARCRAFT3.md`, `WARCRAFT3-FIELD-NOTES.md`).
`SID_WARCRAFTGENERAL` (`0x44`) is logged and ignored, so Play Game stops at "problem receiving
required matchmaking data".

**Summary.**

- The chat server answers a handful of `0x44` subcommands (map lists, profile records, icons,
  search start/cancel). Their layouts are on BNETDocs (§3.1).
- When it has a match, the chat server tells each client where the **route server** is (TCP
  6200). That listener is a *coordinator*, not a game host. It swaps each player's addresses,
  levels, loading state and results. **The game itself is peer-to-peer**, like a custom game
  (§3.3). So bnetccd needs no WarCraft III game-hosting code.
- Not documented anywhere permissive: the "match found" message and the route server's packet
  layouts. They can be pinned from a wire capture of a real session or from the client binary
  (§5). Everything else can be built now.
- The classic ladder rules and XP charts are Blizzard's published numbers (§2) and are easy to
  reproduce exactly.

---

## 1. What players saw

- **Play Game** (solo queue): choose race (Human, Orc, Night Elf, Undead, Random), game type
  (1v1, 2v2, 3v3, 4v4, FFA), and thumbs up/down per map in the pool. Press Play Game; the
  client shows a search timer; a match lands the players straight into loading.
- **Arranged Teams**: a party of friends (invited from the friends screen) queues as a unit for
  2v2/3v3/4v4. Each distinct team has its own ladder entry.
- **Profile** (`WID_USERRECORD`): solo, random-team and FFA records (wins, losses, level, XP,
  rank), per-race wins/losses, arranged-team records, last game, partners.
- **Icons** (`WID_ICONLIST`/`WID_SETICON`): race unit portraits unlocked by wins, shown in chat
  (statstring) and the profile.
- **Ladder Info**: in-client standings plus web links (`'\0URL'`/`'LADR'` blocks).

## 2. The classic ladder rules

From Blizzard's classic.battle.net WarCraft III ladder pages (Ladder Rules, Ladder FAQ, Ladder
Charts), archived copies. Paraphrased; the numbers are facts.

### 2.1 Matchmaking

- Match players of **comparable level**; widen the level range when few players are searching.
- Never pair a player with **the opponents of their last game** (they can meet again later).
- **Map choice**: random, weighted by every player's preferences. A map several players liked is
  much more likely. Maps everyone thumbed down are never chosen, unless every map is thumbed
  down, in which case the pick is uniform.
- **No leave grace period**: leaving at the start is a loss.

### 2.2 Ladders

- **Solo** (1v1) and **Random Team** (2v2/3v3/4v4 joined alone) per player; **Arranged Team**
  ladders per unique team for 2v2, 3v3 and 4v4. **FFA** records wins/losses/level/XP in the
  profile (patch 1.03) but had no ranked ladder. "Small Free for All" was never recorded.
- **Rank** = position by XP, top 1000 shown. **No seasons.** Custom games are never recorded.

### 2.3 Levels and XP (Chart 1)

Everyone starts at level 1 with 0 XP and never drops below that.

| Level | Starting XP | XP to next | Loss factor | Min. games/week |
|---|---|---|---|---|
| 1 | 0 | 100 | 0.10 | 0 |
| 2 | 100 | 100 | 0.11 | 0 |
| 3 | 200 | 200 | 0.11 | 0 |
| 4 | 400 | 200 | 0.25 | 0 |
| 5 | 600 | 300 | 0.25 | 0 |
| 6 | 900 | 300 | 0.43 | 0 |
| 7 | 1,200 | 400 | 0.43 | 0 |
| 8 | 1,600 | 400 | 0.67 | 0 |
| 9 | 2,000 | 500 | 0.67 | 0 |
| 10 | 2,500 | 500 | 1.00 | 0 |
| 11–21 | 2,500 + 500 × (L − 10) | 500 | 1.00 | 1 |
| 22–30 | 〃 | 500 | 1.00 | 2 |
| 31–40 | 〃 | 500 | 1.00 | 3 |
| 41–49 | 〃 | 500 | 1.00 | 4 |
| 50 | 22,500 | — (XP keeps accruing) | 1.00 | 4 |

- **Loss factor** scales what a loss costs (only below level 10 is it under 1).
- **Inactivity** (level 11+): each week (Monday 00:00 PST, UTC−8) short of the minimum counts as a
  loss to an opponent one level below. It costs XP only; it is not added to the record.

### 2.4 XP per game (Charts 2–4)

XP for a game depends on the level difference (capped at 6) and whether the higher or lower
player won. Which chart applies depends on the **highest level anyone on the realm** has
reached: Chart 2 while it is ≤ 25, Chart 3 once it passes 25, Chart 4 once it passes 35.

| Diff | Ch.2/3 higher wins | higher loses | lower wins | lower loses | Ch.4 higher wins | higher loses | lower wins | lower loses |
|---|---|---|---|---|---|---|---|---|
| 0 | 100 | 100 | 100 | 100 | 100 | 100 | 100 | 100 |
| 1 | 95 | 105 | 140 | 60 | 85 | 115 | 115 | 85 |
| 2 | 90 | 110 | 152 | 48 | 70 | 130 | 130 | 70 |
| 3 | 85 | 115 | 163 | 37 | 55 | 145 | 145 | 55 |
| 4 | 80 | 120 | 172 | 28 | 45 | 155 | 155 | 45 |
| 5 | 75 | 125 | 178 | 22 | 35 | 165 | 165 | 35 |
| 6 | 70 | 130 | 184 | 16 | 25 | 175 | 175 | 25 |

Read as: the winner gains the "wins" amount, the loser loses the "loses" amount times their loss
factor. ⚠️ Charts 2 and 3 print the same numbers, headed "25th level = max" and "35th level =
max", and 4 says "45th level = max". Whether "max" caps something else (a level used in the
difference, or a scale) is not explained. Decide once we have a second source (a period
forum post, or a capture of XP moving on a PvPGN server with known levels), or pick a reading
and document it.

**Teams**: a random team's level is its players' average. Each player gains or loses what a
1v1 against a player of the enemy team's level would give. Arranged teams level as a unit.

## 3. Protocol

### 3.1 On the chat connection: `SID_WARCRAFTGENERAL` (0x44)

From BNETDocs (C>S [packet 393](https://bnetdocs.org/packet/393/sid-warcraftgeneral), S>C
[packet 292](https://bnetdocs.org/packet/292/sid-warcraftgeneral); both marked "still being
researched"). Every message starts with a `u8` subcommand. Replies echo the request's `u32`
cookie.

| Sub | Name | Client sends | Server replies |
|---|---|---|---|
| `0x00` | `WID_GAMESEARCH` | cookie, `u32` ?, `u8` ?, `u8` game type (0 1v1, 1 2v2, 2 3v3, 3 4v4, 4 FFA), `u16` enabled-maps bitmask, `u16` ?, `u8` ?, `u32` tick count, `u32` race (1 Human, 2 Orc, 4 Night Elf, 8 Undead, 0x20 Random) | cookie, `u32` status: 0 search started, 4 banned CD key, 6 too many games for now |
| `0x02` | `WID_MAPLIST` | cookie, `u8` n, n × {`u32` id, `u32` checksum} of the blocks it has cached | cookie, `u8` n, n × {`u32` id, `u32` CRC-32/BZIP2 of the uncompressed block, `u16` uncompressed length, `u16` compressed length, data}, `u8` packets still to come |
| `0x03` | `WID_CANCELSEARCH` | — | cookie |
| `0x04` | `WID_USERRECORD` | cookie, account name, product | cookie, `u32` icon, `u8` n × {`u32` ladder type `SOLO`/`TEAM`/`FFA `, `u16` wins, `u16` losses, `u8` level, `u8` ?, `u16` XP, `u32` rank}, `u8` n × race {`u16` wins, `u16` losses} (5 WAR3, 6 W3XP), `u8` n × team {`u32` type `2VS2`…, `u16` wins, `u16` losses, `u8` level, `u8` ?, `u16` XP, `u32` rank}, `FILETIME` last game, `u8` n partners, n × name |
| `0x07` | `WID_TOURNAMENT` | cookie | cookie, `u8` status (0 none … 4), `FILETIME`, `u16`, `u16`, `u8` wins, `u8` losses, `u8` draws, 4 × `u8` |
| `0x08` | `WID_CLANRECORD` | cookie, clan tag, product | cookie, `u8` n × {`u32` type `CLNS`/`CNL2`/`CLN3`/`CLN4`, `u32` wins, `u32` losses, `u8` level, `u8` ?, `u32` XP, `u32` rank}, `u8` n × race {`u32` wins, `u32` losses} |
| `0x09` | `WID_ICONLIST` | cookie | cookie, `u32` selected icon, `u8` tiers, `u8` count × {`u32` icon, `u32` unit id, `u8` race, `u16` wins required, `u8` enabled} |
| `0x0A` | `WID_SETICON` | `u32` icon | — |

**The map-list blocks** (`WID_MAPLIST`), each a `u32` type tag and then:

| Tag | Contents |
|---|---|
| `'\0URL'` | realm URL, profile URL, tournament URL, clan URL (strings) |
| `'\0MAP'` | `u8` n, n × map path (e.g. a `Maps\FrozenThrone\…` path the client ships with) |
| `'TYPE'` | `u8` n categories × {`u8` index, `u8` m × {5-byte prefix, `u8` k, k × `u8` map index}} (BNETDocs: the first entry with fewer than 12 maps carries 3 extra bytes; unexplained) |
| `'DESC'` | `u8` n × {`u8` index (PG/AT/type), `u8` index in list, short text, long text} |
| `'LADR'` | `u8` n × {`u32` type, category name, URL} |

The client caches blocks by id and checksum, so the server changes a block's checksum when the
pool changes. The compression is not named on the page: zlib is the working assumption, to
confirm from a capture (§5).

**What tagban's 1.27b client actually sent** (live log, 2026-09-11/12, 50 requests, all
unanswered):

| When | Body | Meaning |
|---|---|---|
| Right after logon, before `SID_ENTERCHAT` | `02 01000000 05` + `'\0URL'`, `'\0MAP'`, `'TYPE'`, `'DESC'`, `'LADR'`, each with checksum 0 | `WID_MAPLIST`, cookie 1, all five blocks, nothing cached. On the wire each tag's bytes run backwards (`4C 52 55 00` = `'\0URL'`), i.e. a multi-character constant stored little-endian. |
| On entering chat | `07 01000000` | `WID_TOURNAMENT`, cookie 1 |
| Every ~11 minutes in a channel | `07 1a000000`, `07 1b000000`, … | `WID_TOURNAMENT` again, cookie counting up |
| In a channel (opening Play Game) | `02 02000000 03` + `'\0MAP'`, `'TYPE'`, `'DESC'`, checksum 0 | `WID_MAPLIST` for the three blocks that screen needs |

A `WID_GAMESEARCH` posted as a comment on BNETDocs' C>S page pins the field positions: `00 06000000
00000000 00 00 3f0a 0000 08 2003ff00 08000000` is cookie 6, a 1v1, map mask `0x0A3F`, and Undead.

No `WID_ICONLIST`, `WID_USERRECORD` or `WID_GAMESEARCH` appeared: the client likely waits for
the map list before offering them. So the login-time `WID_MAPLIST` reply is the first thing to
build. **Arranged Teams** uses `0x60 SID_GAMEPLAYERSEARCH` (C>S request, S>C the list of friends
free to team) plus invitations whose packets are not yet identified.

### 3.2 "Match found"

After a search starts, the server later tells each matched client that the game is ready. PvPGN
names this `SERVER_ANONGAME_FOUND` and sends it on the chat connection. From reading PvPGN's
design (not its code; `LEGAL.md`) it carries:

- the route server's address and port (translated for LAN/NAT like `w3routeaddr`),
- an id for this game and the player's number in it,
- the chosen map's path,
- the game type and team layout, and the count/cookie from the search request.

⚠️ Its subcommand and byte layout are **not documented** in any permissive source.

### 3.3 The route server (TCP 6200)

Design, from reading PvPGN's `anongame.cpp` (again: design only, nothing copied).

1. Each client connects to the route address and sends a **route request**: its name and the
   game id from §3.2.
2. The server checks the id belongs to a pending game and that player. It **acks** with the
   player number and its own view of the connection.
3. When **every** player has connected, it sends each one **player info** for every opponent:
   name, player number, **external address/port and internal (LAN) address**. Then it sends
   **level info** (ladder levels for the game's type) and two **start game** messages.
4. The clients load the map and **connect to each other directly** (W3GS peer traffic, the same
   mechanism as custom games). The route server does not relay game traffic.
5. Each client reports **loading done**. The server acks to all and sends **ready** once all
   have loaded.
6. At the end each client sends a **game result** for each player. The server stores the
   reports, decides each player's outcome by majority (other players' "win/loss" reports beat
   a self-reported disconnect), throws away games with no winner, updates the ladders, and
   closes the route connections after a timeout (PvPGN: 5 minutes).

If a player never connects, the other players wait. PvPGN retries the "everyone here?" check.
We should add a deadline that cancels the game and puts the others back in the queue.

⚠️ Message ids and layouts: **not documented** in any permissive source.

**Network consequence**: like custom games, matched players must be reachable (TCP 6112 open or
forwarded). The external/internal address pair lets two players behind the same NAT find each
other. A relay mode for unreachable players would be our own addition, later.

## 4. Proposed design for bnetccd

```
 client ──0x44──▶ session ──▶ Matchmaker (node task)
                               │  queues: (product, type, arranged?) → searchers
                               │  tick 1 s: match by level window (widens with wait),
                               │  skip last opponents, balance random teams,
                               │  pick map by weighted preferences
                               ▼
               "match found" to each session (route addr, game id, slot, map)
 client ──TCP 6200──▶ w3route listener ──▶ PendingGame: route requests, player/level info,
                                           start, loading, ready, results ──▶ Ladder
                                                                               │
                                          storage: ladder rows, games, reports ◀┘
```

- **`bnetcc-core::ladder::war3`**: pure rules. Chart 1 levels, Charts 2–4 XP per game, loss
  factor, team average, inactivity decay, and result arbitration from per-player reports.
  Table-driven tests straight from §2.
- **Storage** (new tables): `war3_ladder` (account × ladder `SOLO`/`TEAM`/`FFA` × product: wins,
  losses, level, XP, last game, games this week), `war3_race_record` (account × race),
  `war3_team` (sorted member set × `2VS2`/`3VS3`/`4VS4`), `war3_game` (id, type, map, players,
  start/end), `war3_report` (game × reporter × subject × outcome). Rank is computed, not stored.
  Icons: selected icon per account (`WID_SETICON`).
- **Session**: answer all `0x44` subcommands. `WID_USERRECORD`/`WID_ICONLIST` read storage;
  `WID_GAMESEARCH`/`WID_CANCELSEARCH` talk to the matchmaker. Leaving chat or disconnecting
  cancels the search.
- **Matchmaker**: one task in the node, holding each searcher's session handle, race, level,
  map mask, arranged-team id, queued-at and last opponents. Level window starts at ±1 and widens
  by 1 every N seconds up to a configured maximum. Game ids are random `u32`s held in a pending
  table until the route server takes them (or a deadline cancels them).
- **w3route listener** (`crate::w3route`): its own TCP listener, off in warnet mode like the
  other WarCraft III listeners, driven entirely by the pending-game table.
- **Config** `[warcraft3.matchmaking]`: `enabled`, `route_address` (public host/IP; empty = the
  address the client reached us on, as the D2 realm does), `route_port = 6200`, per-type map
  pools (paths the client already has; the server ships no maps), `level_window`,
  `widen_every_secs`, `max_level_window`, `found_deadline_secs`.
- **Surfaces**: statstring icon/level for WarCraft III users (bots already read these), a web
  ladder page and admin panel views from the same storage.

## 5. Closing the unknowns

What is missing: the §3.2 "match found" message, every §3.3 route message layout, the
`WID_MAPLIST` compression and the meaning of `TYPE`'s 5-byte prefix, and the unknown fields of
`WID_GAMESEARCH`. Three clean ways to get them:

1. **Wire capture (recommended first).** Run an unmodified PvPGN locally with two classic 1.27b
   clients, play one 1v1 through Play Game, and capture TCP 6112, TCP 6200 and the players' game
   port (Wireshark or `tcpdump -w`). What crosses the wire is fact; we write layouts from the
   capture and never from PvPGN's code. One 1v1 and one 2v2 cover almost everything. Do the
   same with an Arranged Team invite for its packets.
2. **The client binary.** Ghidra on the 1.27b `Game.dll` would pin every field by name. The
   decompilation decision recorded for Diablo II does not extend to WarCraft III, so this needs
   tagban's go-ahead first.
3. **PvPGN, design only.** Already used above to learn the flow. Not a source of layouts
   (`LEGAL.md`: no copying of structs, tables or parsers).

## 6. Legal notes

- BNETDocs and BNETDocs/Atlas (MIT) are primary sources, as elsewhere.
- PvPGN is GPL: read for design only, never copy. `bnetsrp3`/`bigint` stay unopened.
- The ladder charts and rules are facts from Blizzard's published pages, restated in our own
  words here.
- Maps: the server sends map *paths*. Players need the maps in their own install. We ship no
  maps and no Blizzard files.

## 7. Build order

| Phase | What | Needs | Done when |
|---|---|---|---|
| 0 | `0x44` answers from BNETDocs layouts: `WID_TOURNAMENT` (none), `WID_ICONLIST` (nothing), `WID_USERRECORD`/`WID_CLANRECORD` (zeros), `WID_SETICON` (accepted) — **built** (`bnetcc_proto::w3general`, `Session::warcraft_general`), awaiting a client test | nothing | the 11-minute `WID_TOURNAMENT` polls get answers and the login stays clean |
| 1 | `WID_MAPLIST` blocks from config (URLs, map paths, types, descriptions, ladder links), answering the login-time request for all five | compression confirmed (a capture, or try zlib against the client) | "problem receiving required matchmaking data" is gone; Play Game shows types and maps with thumbs |
| 2 | `ladder::war3` rules + storage + profile records from storage + web page | the Charts 2/3 reading (§2.4) | tests from §2's tables; profile shows real records |
| 3 | Matchmaker + `WID_GAMESEARCH`/`WID_CANCELSEARCH` + "match found" | §5 capture | two clients get matched and sent to the route port |
| 4 | w3route listener: route requests, player/level info, start, loading, ready, results → ladder | §5 capture | a 1v1 plays to the end and both profiles change |
| 5 | Random teams and FFA, then Arranged Teams (`0x60`, invites) | 2v2 + AT capture | a 2v2 random and an AT 2v2 record correctly |
| 6 | Inactivity decay, icons by wins, statstring level/icon, admin views | — | — |

**Decisions for tagban**: (a) capture from a local PvPGN, or Ghidra on `Game.dll`, or both;
(b) the reading of Charts 2/3 "max" (or a modern rating instead of the classic charts, which
would not match how "real Battle.net" worked); (c) the map pools per game type for 1.27b.
