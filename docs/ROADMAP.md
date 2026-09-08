# Roadmap

The governing rule: **get one real StarCraft 1.16.1 client into a channel before writing
a single line of realm, ladder, or clan code.** Every prior project in this space that
started with breadth ended at a hundred thousand lines. PvPGN has fourteen connection
classes and Westwood Online support; it also has a dormant repository and an open issue
titled "A new team for this project?".

---

## Phase 0 — Foundations ✅ done

- `cairn-crypto`: X-SHA-1 ported and verified against known-answer vectors, including the
  full `SID_LOGONRESPONSE2` double-hash chain.
- `cairn-proto`: BNCS framing, chat-gateway line framing, checked wire readers/writers,
  chat events and limits, product/auth-family classification. Zero dependencies.
- `cairn-core`: policy engine (gaming/warnet/both), channel operator rules, admission
  control, flood control, CD-key session uniqueness, session state machine. Zero
  dependencies.
- `crates/smoke`: real handshake over real sockets at 4,000 concurrent connections.
- 106 tests, `clippy -D warnings` clean.

## Phase 1 — A single-node gaming server that a real client can use

The milestone is a screenshot of Brood War sitting in a channel. Nothing else counts.

- [ ] **Resolve dependencies and build `cairnd`.** It is written and excluded from the
      workspace only because `crates.io` was unreachable where this was authored. Add it
      back to `members`, run `cargo build`, fix what the compiler finds.
- [ ] **Test against a real client.** Expect surprises; `docs/PROTOCOL-NOTES.md` marks
      the two most likely, both flagged 🛑: the zero-game `SID_GETADVLISTEX` response
      shape, and whether four-character codes really are byte-reversed on the wire.
      Capture the traffic and settle both.
- [ ] **Storage** (`cairn-storage`): `Storage` trait, SQLite implementation, migrations,
      write-behind actor. Account creation, password change and bans write through;
      everything else batches. **Do not ship without this** — Atlas has no persistence at
      all and loses every account on restart, which is most of why it looks stable.
- [ ] **Per-key attribute ACLs** (`Any`/`Owner`/`Internal`), lifted from Atlas's model.
      `System\Password Digest` becomes unreachable from any client-facing read path by
      construction. This is the structural fix for CVE-2004-2705's class.
- [ ] **BNFTP** (protocol byte `0x02`). Clients fetch `icons.bni`, `tos.txt` and patch
      MPQs; without it some clients hang. Serve **only operator-supplied files** — ship
      placeholders, never Blizzard assets (`docs/LEGAL.md` §2).
- [ ] **Wire `KeyRegistry` into the `SID_AUTH_CHECK` handler.** The registry and the
      `auth_check_status` codes exist; the handler still accepts every key unconditionally.
      One live session per CD key (result `0x201`, holder named in the info string) is the
      real economic gate on bot fleets — addresses are cheap, keys are not.
- [ ] **Real randomness for server tokens.** Currently a time-and-counter mix with a TODO.
- [ ] **`rlimit` crate**: read and raise `RLIMIT_NOFILE` on all three platforms, replacing
      the `/proc/self/limits` fallback.
- [ ] **Metrics and `tracing`**: Prometheus on the admin listener; connection counts by
      class and state, per-packet decode/error counters, outbound queue depth histogram,
      login latency. Plus `/debug/slow`, listing the deepest queues — "it's laggy" should
      take thirty seconds to diagnose, not a week.
- [ ] **`cargo fuzz` targets** for `decode_frame`, `decode_line` and every packet reader.
      The in-tree pseudo-random tests are a stand-in, not a substitute.
- [ ] **Packaging**: systemd unit, launchd plist, Windows service wrapper. A node operator
      on Windows is a real user here.

## Phase 2 — Federation v1

- [ ] `cairn-hub`: directory, identity, ladder, ban authority.
- [ ] `cairn-fed`: mTLS transport (`rustls`), Ed25519 node identities, one-time enrolment
      tokens, CBOR message framing, reconnect with jittered backoff.
- [ ] **Hub-proxied X-SHA-1 verification** and **edge SRP verification** — the asymmetry in
      `docs/FEDERATION.md` §4, which is the load-bearing part of the identity design.
- [ ] Federated channels with hub sequencing; netsplit synthesises `EID_LEAVE` so no
      ghosts remain; reconnect sends a roster snapshot with a fresh sequence base.
- [ ] Federated game list, with hub-side reachability probing so dead ads are
      de-prioritised. "The game list is full of dead games" is a perennial complaint that
      nobody in this ecosystem has fixed.
- [ ] Ladder submission with session attestation, plausibility checks, rate limits and
      per-node reputation.
- [ ] Ban scopes: node / network / IP-range, with approval for network scope.
- [ ] `cairnctl`: node enrolment, moderation, policy push.

## Phase 3 — Warnet hardening and WarCraft III

- [ ] `HubSerialized` ordering end to end, with the optional `arrival_jitter_window_ms`
      fairness window (default off) described in `docs/WARNET.md` §4.
- [ ] Full operator command set: `/designate`, `/kick`, `/ban`, `/squelch`, `/rejoin`,
      moderated channels, `EID_USERFLAGS` propagation.
- [ ] Registered-bot accounts as a first-class concept, so a bot is something the server
      knows about rather than a human account behaving oddly.
- [ ] Key-registry admin surface: list live keys and holders, ban/unban, and a report of
      how many distinct keys a fleet operator is running.
- [ ] NLS/SRP-6 (Blizzard variant) — implement from the javaop write-up and RFC 2945,
      **never** from PvPGN's `bnetsrp3.cpp`, which is AGPL-3.0 (`docs/LEGAL.md` §1).
- [ ] WarCraft III: clans (`0x70`–`0x82`), `SID_WARCRAFTGENERAL`, W3 route listener.
      Document clearly that WC3 needs a patched client because of the 128-byte RSA server
      signature, and do not distribute that patch (`docs/LEGAL.md` §3).

## Phase 4 — Diablo II realms, and hub availability

- [ ] MCP gateway. Note its framing differs from BNCS — `len:u16le` first, **no** `0xFF`
      magic. This is the single most common bug in D2 realm implementations.
- [ ] Character storage and the realm/game-server split.
- [ ] Decide the D2GS story. It is closed-source, Windows-only, and its port 4000 is
      hardcoded client-side, so you cannot run two per host — realms mean containers or
      VMs. Realms are **not federated**: a character lives in one realm's database. The
      realm menu can be shared; the characters cannot.
- [ ] Hub HA: active/standby over shared Postgres with a virtual IP. Do not build a
      consensus protocol for a network that will have twelve nodes.

## Not scheduled, and why

**StarCraft: Remastered (1.18+), WarCraft III Reforged, Diablo II: Resurrected.**

These are not "later" items; they are blocked on facts outside our control:

- Public protocol documentation for this family effectively stops in 2017. Patch 1.18
  made major changes that broke all bot compatibility, and PvPGN states plainly it will
  not support 1.18+.
- Reforged did the same in 2020. gowarcraft3, the best-maintained WC3 library, says BNCS
  "works up until patch 1.32" without saying what replaced it.
- D2R shipped with no TCP/IP or LAN mode at all. BNETDocs has no D2R entry, community
  projects publish no spec, and Blizzard has issued takedowns in this area.
- Blizzard's own answer to the 2017 break was CAPI — chat-only, key-gated — explicitly so
  bots would stop emulating the game protocol.

The work is therefore not "implement a documented protocol"; it is "reverse-engineer an
undocumented one, in a jurisdiction where the 8th Circuit has already ruled on exactly
that" (`docs/LEGAL.md` §2).

**What the architecture does about it instead:** the gateway boundary. A protocol
front-end is a crate implementing one trait over `cairn-core`; it owns its framing, its
auth and its session state machine, and knows nothing about channels or storage. If a
modern protocol is ever documented, it becomes `cairn-gateway-bgs` and the core does not
change. That is the correct amount to invest in a maybe: a seam, not a stub.
