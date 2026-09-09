# Roadmap

The governing rule: **get one real StarCraft 1.16.1 client into a channel before writing a
single line of realm, ladder, or clan code.** Every prior project in this space that started
with breadth ended at a hundred thousand lines. PvPGN has fourteen connection classes and
Westwood Online support; it also has a dormant repository and an open issue titled "A new
team for this project?".

---

## Phase 0 — Foundations ✅ done

- **`bnetcc-crypto`** — X-SHA-1 ported and verified against known-answer vectors, including
  the full `SID_LOGONRESPONSE2` double-hash chain.
- **`bnetcc-proto`** — BNCS framing, **MCP framing** (length-first, no magic — the
  classic D2 realm bug, with a test that names it), chat-gateway line framing, checked
  wire readers/writers, chat events and limits, product/auth-family classification,
  **statstring parsing** including WarCraft III icon codes, **the BNI icon format**
  (parse, build, validate, icon selection) and **BNFTP v1** with a hardened filename
  sanitiser. Zero dependencies.
- **`bnetcc-core`** — policy engine (gaming/warnet/both), per-client-type connection limits
  with two-stage product classification, channel operator rules, CD-key session
  uniqueness, flood control, **advertisement rotation**, **bridged identities** (name
  derivation, reserved namespace, stable mapping, loop prevention), session state machine.
  Zero dependencies.
- **`bnetcc-storage`** — `Storage` trait, per-key attribute ACLs, write-behind batching with
  a tested durability contract, in-memory reference backend, and a conformance suite every
  backend must pass. Zero dependencies.
- **`bnetcc-storage-sqlite`** — SQLite backend with migrations. Built and tested as of
  2026-09-08; two real bugs found by that first compile (ban precedence, a stale write
  blocking the rest of a flush batch) and fixed — see `docs/HANDOFF.md` §2.
- **`bnetccd`** — node daemon. Built and tested as of 2026-09-08; part of the workspace.
- **`crates/smoke`** — real handshake over real sockets, thread-per-connection (client and
  server side), sized to whatever the host's thread ceiling can sustain.
- Full workspace builds, tests, and lints (`clippy -D warnings`) clean.

## Phase 1 — A single-node gaming server a real client can use

The milestone is a screenshot of Brood War sitting in a channel. Nothing else counts.

- [x] **Resolve dependencies and build `bnetccd` and `bnetcc-storage-sqlite`.** Done
      2026-09-08 — both compiled clean on the first real attempt.
- [ ] **Test against a real client.** Expect surprises; `docs/PROTOCOL-NOTES.md` marks each
      with ⚠️ or 🛑. The likeliest: the zero-game `SID_GETADVLISTEX` shape, four-character
      code byte order, the BNI code-list-when-flags-are-set question, and the ad extension
      tag's wire bytes.
- [x] **Wire account storage into `bnetccd`** — done 2026-09-08. A dedicated thread
      (`bnetccd::storage`) owns the `Storage` backend and is reached over a channel;
      `SID_CREATEACCOUNT2`/`SID_LOGONRESPONSE2` go through it, so accounts persist and get
      real name validation for the first time. **Still open:** everything about attribute
      storage — `WriteBehind` isn't wired to anything client-facing yet, so there is no
      `AttrSchema::filter_readable` read path to guard. That's the rest of this item.
- [ ] **BNFTP serving** (protocol byte `0x02`). The codec exists; it needs the file server
      behind it, serving **only operator-supplied files** from a configured directory —
      placeholders in the repo, never Blizzard assets (`docs/LEGAL.md` §2).
- [ ] **Icon serving**: answer `SID_GETICONDATA` per product before `SID_ENTERCHAT` (a
      client that does not get this **terminates the connection**), plus `SID_GETFILETIME`
      for revalidation. Ship a `bnetcc icons` subcommand wrapping the existing
      parse/build/validate so an operator can check an icon pack before deploying it.
- [ ] **Advertisement serving**: `SID_CHECKAD`/`SID_CLICKAD`/`SID_DISPLAYAD`, plus
      `SID_QUERYADURL` for WarCraft III. Rotation logic is done and stateless; this is the
      packet handlers, the config, and the BNFTP delivery path.
- [x] **Wire `KeyRegistry` into `SID_AUTH_CHECK`** — done 2026-09-08: one live session per
      CD key, result `0x201` with the holder named once known, `0x202` for a banned key.
      **Caveat:** the request-side wire layout it parses is this project's best-confidence
      reconstruction, never confirmed against a real client capture — see
      `docs/PROTOCOL-NOTES.md` §3. A parse failure fails open (accepts, skips the key
      check) rather than disconnecting, specifically because of that. Version checking
      itself is still unenforced by design (`auth_info`'s comment) — only key uniqueness
      is real.
- [ ] **Real randomness for server tokens.** Currently a time-and-counter mix with a TODO.
- [ ] **`rlimit` crate**: read and raise `RLIMIT_NOFILE` on all three platforms.
- [ ] **Metrics and `tracing`**: Prometheus on the admin listener; connections by class and
      state, per-packet decode/error counters, outbound queue depth histogram, login
      latency split by edge-verified and hub-proxied. Plus `/debug/slow`.
- [ ] **Read-only admin API** (JSON over HTTP on the admin listener): who is online,
      channels and rosters, game list, server status — filtered through the same
      `AttrSchema` as every other read path, so it cannot return what a client could not
      see. This is the supported integration surface for a website or monitoring, and it
      exists so that nobody points a dashboard at the database: that bypasses the
      attribute ACLs and welds a site to a schema that will change. See
      `docs/OPERATIONS.md` §3.
- [ ] **`cargo fuzz`** targets for `decode_frame`, `decode_line`, `bni::parse` and
      `bnftp::decode_request`. The in-tree pseudo-random tests are a stand-in.
- [ ] **Packaging**: systemd unit, launchd plist, Windows service wrapper.

## Phase 2 — Federation, and Diablo II Open

- [ ] `bnetcc-hub`: directory, identity, ladder, ban authority.
- [ ] `bnetcc-fed`: mTLS transport (`rustls`), Ed25519 node identities, one-time enrolment
      tokens, CBOR message framing, reconnect with jittered backoff.
- [ ] **Hub-proxied X-SHA-1 verification** and **edge SRP verification** — the asymmetry in
      `docs/FEDERATION.md` §4, which is the load-bearing part of the identity design.
- [ ] Federated channels with hub sequencing; netsplit synthesises `EID_LEAVE`; reconnect
      sends a roster snapshot with a fresh sequence base.
- [ ] Federated game list with hub-side reachability probing, so dead ads are
      de-prioritised. "The game list is full of dead games" is a perennial complaint nobody
      in this ecosystem has fixed.
- [ ] Ladder submission with session attestation, plausibility checks, rate limits and
      per-node reputation.
- [ ] `bnetcc`: node enrolment, moderation, policy push, icon-pack validation.
- [ ] **Diablo II *Open*.** Open games are peer-to-peer, exactly like StarCraft and
      Warcraft II — the client dials the host directly and characters live client-side. So
      D2 players get a working server here with **no game server of any kind**: it is
      `SID_STARTADVEX3` and `SID_GETADVLISTEX` with D2's statstring passed through
      verbatim. This is the cheap 80% of Diablo II support and it belongs early.

## Phase 3 — Warnet hardening, WarCraft III, and bridges

- [ ] `HubSerialized` ordering end to end, with the optional `arrival_jitter_window_ms`
      fairness window (default off) from `docs/WARNET.md` §4.
- [ ] Full operator command set: `/designate`, `/kick`, `/ban`, `/squelch`, `/rejoin`,
      moderated channels, `EID_USERFLAGS` propagation.
- [ ] Registered-bot accounts as a first-class concept.
- [ ] Key-registry admin surface: live keys and holders, ban/unban, and a report of how
      many distinct keys a fleet operator is running.
- [ ] NLS/SRP-6 (Blizzard variant) — implement from the javaop write-up and RFC 2945,
      **never** from PvPGN's `bnetsrp3.cpp`, which is AGPL-3.0 (`docs/LEGAL.md` §1).
- [ ] WarCraft III: clans (`0x70`–`0x82`), `SID_WARCRAFTGENERAL`, W3 route listener, and an
      **MPQ reader** for `icons-WAR3.bni` — which is an MPQ of `.blp` images, not a BNI.
      Document that WC3 needs a patched client because of the 128-byte RSA server
      signature, and do not distribute that patch.
- [ ] **Bridges** — `docs/BRIDGES.md`. In its build order: the presence model first (all
      `bnetcc-core`, no transport), then the extended line protocol (which doubles as the
      telnet-gateway improvement), then the WebSocket transport, then an in-tree Discord
      bridge as the reference consumer, then a minimal Lua example for game addons.
      Bridges come after federation because a bridged user *is* a channel presence, and
      building against a channel model that is still moving would mean building it twice.

## Phase 4 — Diablo II closed realms, in one binary

Realms run **inside `bnetccd`**, not as separate daemons — `docs/ARCHITECTURE.md` §11 has the
reasoning, including that PvPGN's `d2dbs` still uses `select()` capped at `FD_SETSIZE` and
never received the fix `bnetd` got in 2003.

- [ ] MCP gateway as an in-process module. **The framing codec is done**
      (`bnetcc_proto::mcp`), including a test asserting that a BNCS frame fed to the MCP
      decoder does not silently succeed. What remains is the session state machine and the
      realm handlers.
- [ ] Character store behind the `Storage` trait, called as a function rather than over a
      socket. Replaces `d2cs` **and** `d2dbs`, and the custom `bnetd`↔`d2cs` binary protocol
      goes away with them.
- [ ] `GameHost` trait with an `External` implementation, so operators running the existing
      closed-source D2GS can point at it while still supervising exactly one binary.
- [ ] Document the realm constraint precisely: **port 4000 is hardcoded in the client**, so
      one realm per IP address. Additional addresses are fine; additional processes on one
      address are not.
- [ ] Realms are **not federated**: a character lives in one realm's database. The realm
      menu can be shared across nodes; the characters cannot.
- [ ] Hub HA: active/standby over shared Postgres with a virtual IP. Do not build a
      consensus protocol for a network that will have twelve nodes.

## Phase 5 — An in-house Diablo II game server

**Committed, not scheduled.** This is the one part of the project that is a game engine
rather than a protocol server, so it runs as its own long track and blocks nothing. The
`GameHost` trait exists so it lands as a module, not a fourth daemon.

Why own it: PvPGN stopped receiving updates and the existing D2GS is closed-source,
Windows-only, crash-loops on modern Windows, and hardcodes port 4000. Every closed realm in
the world currently depends on a binary nobody can fix.

The work decomposes, and the ordering matters because the risk is front-loaded:

1. **Protocol shim first, simulation second.** Implement the D2GS accept sequence
   (`D2GS_NEGOTIATECOMPRESSION` `0xAF` → `D2GS_GAMELOGON` `0x68` → `D2GS_STARTGAME` `0x5C`
   → `D2GS_ENTERGAMEENVIRONMENT` `0x6A`, then compressed) and get **two clients standing in
   town, seeing each other, with nothing else working.** That single milestone answers the
   only question that can kill the project — will the client accept packets we generate —
   before any simulation exists. Everything after it is incremental.
2. **Game data tables.** Item affixes, treasure classes, monster stats and skill tables all
   live in the client's MPQs. Read them **at runtime from the operator's own install**;
   never redistribute them. This is both the legally clean path and the one that keeps us
   correct across patches.
3. **Deterministic world generation** from the game seed, so every client in a game agrees
   on the map without us shipping map data.
4. **Entity and state model** — spawning, movement, visibility, the update packets the
   client expects and their cadence. Desync is the failure mode; a golden-capture test
   harness against a real client is worth building before this, not after.
5. **Combat, skills, items.** The largest surface, and the most amenable to being driven
   from the data tables in step 2 rather than hand-written.
6. **Quests and act progression.**
7. **The D2S save format**, read and write, so characters survive and can be inspected.

Realistic framing: this is a multi-year track measured against a moving target of client
expectations, and it should be resourced as its own project with its own contributors. The
value of writing it down now is that phases 1–4 leave the seam in the right place.

## Not scheduled, and why

**StarCraft: Remastered (1.18+), WarCraft III Reforged, Diablo II: Resurrected.**

These are blocked on facts outside our control, not on effort:

- Public protocol documentation stops in 2017. Patch 1.18 broke all bot compatibility, and
  PvPGN states plainly it will not support 1.18+.
- Reforged did the same in 2020. gowarcraft3, the best-maintained WC3 library, says BNCS
  "works up until patch 1.32" without saying what replaced it.
- D2R shipped with no TCP/IP or LAN mode at all. BNETDocs has no D2R entry, community
  projects publish no spec, and Blizzard has issued takedowns in this area.
- Blizzard's own answer to the 2017 break was CAPI — chat-only, key-gated — explicitly so
  bots would stop emulating the game protocol. That endpoint is now itself reported dead.

The work is therefore not "implement a documented protocol"; it is "reverse-engineer an
undocumented one, in a jurisdiction where the 8th Circuit has already ruled on exactly
that" (`docs/LEGAL.md` §2).

**What the architecture does instead:** the gateway boundary. A protocol front-end is a
crate implementing one trait over `bnetcc-core`; it owns its framing, its auth and its
session state machine, and knows nothing about channels or storage. If a modern protocol is
ever documented, it becomes `bnetcc-gateway-bgs` and the core does not change. A seam, not a
stub.
