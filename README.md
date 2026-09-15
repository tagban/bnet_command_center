# Command Center

[![CI](https://github.com/tagban/bnet_command_center/actions/workflows/ci.yml/badge.svg)](https://github.com/tagban/bnet_command_center/actions/workflows/ci.yml)

A federated Classic Battle.net server in Rust. **[bnet.cc](https://bnet.cc)**

Built clean-slate, informed by what PvPGN and BNETDocs Atlas got right and wrong. Two
roles from one codebase: **`bnetccd`**, the node a community operator runs, and
**`bnetcc-hub`**, which is authoritative for identity, the channel directory, ladder and
network-wide bans.

Called **Command Center**; `bnetcc`, from its earlier name BNET Command Center, in code and on
the command line.

| | |
|---|---|
| `bnetccd` | the node daemon — what an operator installs |
| `bnetcc-hub` | the hub: identity, directory, ladder, ban authority |
| `bnetcc` | the admin CLI |

---

## Status

**Phase 0 complete, phase 1 underway.** The protocol core, cryptography, domain rules and
a concurrency harness are written, tested and measured. `bnetccd` builds and runs; account
storage and CD-key uniqueness are wired end-to-end. See [`docs/HANDOFF.md`](docs/HANDOFF.md)
for exact state and what's next.

| Crate | State | Tests |
|---|---|---|
| `bnetcc-crypto` | X-SHA-1 verified against known-answer vectors | 7 |
| `bnetcc-proto` | BNCS, MCP and chat-gateway framing, wire codecs, BNI icons, BNFTP, statstrings | 89 |
| `bnetcc-core` | Policy, channels, admission, flood, key registry, ads, bridged identities, session FSM | 98 |
| `bnetcc-storage` | `Storage` trait, attribute ACLs, write-behind, conformance suite | 33 |
| `bnetcc-storage-sqlite` | SQLite backend with migrations | 6 |
| `bnetcc-smoke` | Real handshake, thread-per-connection, sized to the host | — |
| `bnetccd` | Node daemon — accounts persist, CD-key uniqueness enforced, storage actor | 26 |

259 tests, `cargo clippy -D warnings` clean, **zero third-party dependencies** in the four
core library crates (`crypto`/`proto`/`core`/`storage`) — deliberate: the crates that parse
attacker-controlled bytes and hold the domain rules are the ones you want cheap to fuzz and
cheap to audit. The database driver and async runtime live only in the two crates that
cannot avoid them.

Measured on 2 vCPU / 8 GB: **4,000/4,000 concurrent connections**, every one through a
real handshake with a verified X-SHA-1 logon proof, 30 KiB RSS per connection (an upper
bound — the harness is thread-per-connection), channel fanout p99 under 1.2 ms. Details
and caveats in [`docs/CAPACITY.md`](docs/CAPACITY.md).

---

## Why not just patch PvPGN

PvPGN is GPL-2.0, ~114,000 hand-written lines, and effectively dormant since 2021. More
to the point, its architecture is the thing you would have to change:

- **One process, one thread, one event loop.** Every packet parse, channel broadcast,
  account write and ladder rebuild runs on the socket thread — including a synchronous
  `mysql_query()`, which freezes every other connection for a network round trip.
- **`sd_tcpinput()` has no loop**: at most one packet per readiness event, however much
  is already buffered. Text protocols are read **one byte at a time**, so a 200-character
  IRC line costs 200 syscalls and 200 trips through the event loop.
- **41 "memory leak", 31 "crash", 8 "buffer overflow" and 4 "use after free" fix commits**,
  including *"fix for many possible server crashes through malformed packet"* and a 2020
  overflow in the WarCraft III authentication path. Most are classes Rust removes.
- **`max_connections = 1000` by default**, silently refusing past it.
- **No federation.** `tracker.cpp` is a fire-and-forget UDP stats beacon to a master list.

Atlas is MIT, ~16,000 lines, and much cleaner — but it has **no persistence at all**
(every account evaporates on restart, which is most of why it appears stable), an open
thread-safety bug filed by its own author, and an abandoned federation draft that was
never implemented.

The full evidence table is in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §2.

---

## Documentation

| Document | What it covers |
|---|---|
| [`docs/HANDOFF.md`](docs/HANDOFF.md) | **Start here.** Current state, decisions already made and why, landmines, what to do next |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Process model, crate map, connection lifecycle, backpressure, storage, observability, cross-platform notes |
| [`docs/FEDERATION.md`](docs/FEDERATION.md) | Star topology, trust model, mTLS transport, the X-SHA-1/SRP identity asymmetry, channel sequencing, ladder validation, partition behaviour |
| [`docs/WARNET.md`](docs/WARNET.md) | Warnet vs gaming mode, what each gates, connection limits for bot fleets, operator semantics, ordering fairness, flood control |
| [`docs/BRIDGES.md`](docs/BRIDGES.md) | Chat from outside Battle.net — Discord, Ragnarok, FFXI addons. Why bridged users are presences rather than relayed text, naming, encoding, loop prevention, moderation |
| [`docs/PROTOCOL-NOTES.md`](docs/PROTOCOL-NOTES.md) | Wire reference with confidence markers — ✅ verified, ⚠️ single-source, 🛑 unknown |
| [`docs/OPERATIONS.md`](docs/OPERATIONS.md) | Which database and why, inspecting a live SQLite file safely, why a website should not query it directly, file descriptor limits |
| [`docs/CAPACITY.md`](docs/CAPACITY.md) | Measured numbers, what they prove, where the real ceilings are |
| [`docs/LEGAL.md`](docs/LEGAL.md) | Licence contamination map, clean-room guidance, *Davidson v. Jung*, the WarCraft III signature problem |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Phases, and why modern clients are not on them |

---

## Download & run (no build)

Prebuilt archives for Linux, macOS (Intel + Apple Silicon), and Windows are attached to
each [GitHub Release](https://github.com/tagban/bnet_command_center/releases), and to every
manual run of the [Release workflow](https://github.com/tagban/bnet_command_center/actions/workflows/release.yml)
(as downloadable artifacts — use the **Run workflow** button to get a build without cutting a
version). Each archive holds two executables:

- **`bnetcc-launcher`** — start here. On first run it writes a `bnetccd.toml`, then launches
  the server, printing the game port and the admin-panel URL. The server prints a one-time
  admin password to the same console on its first run.
- **`bnetccd`** — the server itself, for when you want to manage the config by hand.

```sh
# macOS / Linux — unpack and run
tar xzf command-center-*-*.tar.gz && cd command-center-*/
./bnetcc-launcher
```

```
REM Windows — unzip, then in that folder
bnetcc-launcher.exe
```

Game clients then connect to the host machine on port **6112**; the admin panel is at
**https://127.0.0.1:6114** (self-signed cert — the browser warns once). To grant yourself
staff powers (`/tagban`, `/ipban`, `/mute`), add your account name to `[admins]` in the
generated `bnetccd.toml` and restart. macOS/Linux may need `ulimit -n` raised for large
tests; Windows SmartScreen may warn on first launch of an unsigned binary.

---

## Ports & interfaces

Three network surfaces, all configurable in `bnetccd.toml` (defaults shown):

| Interface | Address | Purpose |
|---|---|---|
| **Game & chat (BNCS)** | TCP + UDP `0.0.0.0:6112` | Where clients and chat-gateway bots connect; the UDP side is the login-time game check. Point clients here. |
| **Admin panel** | HTTPS `127.0.0.1:6114` | Password-gated dashboard: live status, settings editor, user management, restart. It can change the server, so it is **loopback-only** unless you enable remote access from its Settings page. |
| **Public status** | HTTP `0.0.0.0:6116` | Read-only, unauthenticated, safe to expose. Forward this port to publish stats. |

The admin panel's one-time password is printed to the log on first run (and stored hashed under `bnetccd-admin/`); the launcher prints where to sign in.

### Public status endpoint

Two routes on the public port, for anyone:

- **`/status.json`** — server name, MOTD, uptime, connections, users online, peak, and channel/game counts. It sends `Access-Control-Allow-Origin: *`, so a site (e.g. bnet.cc) can `fetch()` it cross-origin and render its own widget. Set `[status] public_show_users = true` to also include the online-usernames list (off by default).
- **`/`** — a ready-made status page that renders the feed and refreshes.

### Discord updates

Point the server at a Discord channel **webhook** (create it in Discord, paste the URL into `bnetccd.toml`; the secret never leaves your config). `[discord]` options control **when** data is posted, each switchable independently:

```toml
[discord]
webhook_url = "https://discord.com/api/webhooks/…"  # empty (default) disables Discord
status_interval_mins = 30    # periodic summary cadence
games_window_hours = 6       # window for "games hosted per client"
post_status = true           # users online, channels, live games, uptime, games/client
post_events = true           # server start / shutdown / restart
post_milestones = true       # e.g. a new peak-connections record
```

Posts are best-effort — a slow or failed post is logged and dropped, never blocking the server. The webhook client rides the existing rustls stack (no HTTP-client dependency).

### Push stats to your own site

If you run a website elsewhere, the server can **POST** the status JSON to it on an interval — the same payload as `/status.json`. This is outbound-only, so a node behind residential NAT needs no forwarded port for it.

```toml
[stats_push]
url = "https://mysite.com/bnet/ingest"   # empty (default) disables it
interval_secs = 60
token = ""              # optional; sent as "Authorization: Bearer <token>"
include_users = false   # include the online-usernames list in the payload
```

Your endpoint receives a JSON body like:

```json
{"server_name":"…","motd":"…","version":"0.2.3","uptime_secs":3600,
 "connections":42,"users_online":30,"peak_connections":51,"channels":4,"games":2}
```

### Server tracking (PvPGN-compatible)

Optional. `[tracker]` can **advertise** this server to public PvPGN trackers (a UDP beacon on port 6114, per the [PvPGN tracking protocol](https://bnetdocs.org/document/35/pvpgn-tracking-protocol)) so it appears on their lists, and/or **host your own list** — receive other servers' beacons and publish a page + `/servers.json`.

```toml
[tracker]
advertise_to = ["tracker.pvpgn.org"]   # beacon us to these trackers (empty = off)
description = "My Server"               # defaults to the server name
host_listen = "0.0.0.0:6114"            # receive other servers' beacons (empty = off)
list_listen = "0.0.0.0:8080"            # public list page + /servers.json (empty = off)
```

The wire codec is implemented from the published protocol spec (not PvPGN's GPL sources — see `docs/LEGAL.md`). Port 80 for the list page needs root, so bind a high port and forward `80 → 8080` at your router.

---

## Building

```sh
cargo build --workspace
cargo test --workspace
```

**`bash scripts/verify.sh`** is the one-command version: build, test, clippy, and a load
test sized to whatever your host's thread ceiling can sustain (macOS's is typically much
lower than Linux's — the script checks `kern.num_taskthreads` and scales down rather than
finding out the hard way), writing the full compiler output to `verify.log`. It finds
`cargo` via `~/.cargo/env` if it is not on `PATH`, and raises the open-file limit before
the load test (macOS defaults to 256).

To run the load test by hand instead:

```sh
cargo build --release -p bnetcc-smoke
ulimit -n 20000
./target/release/bnetcc-smoke 2500 40        # 2500 concurrent real handshakes
```
(`bnetcc-storage-sqlite` is a separate crate rather than a feature flag because an
*optional* dependency still forces registry resolution.)

```sh
bnetccd --config bnetccd.toml --check   # validate configuration and exit
bnetccd --config bnetccd.toml
```

---

## Configuration

Every field has a default, so a minimal config is a few lines. See
[`bnetccd.example.toml`](bnetccd.example.toml).

```toml
[server]
name = "Warzone"
mode = "warnet"        # gaming | warnet | both

[federation]
enabled = true
hub = "hub.example.net:7112"
```

`mode = "warnet"` refuses game hosting with `SID_STARTADVEX3` status `0x02` (the client's
own "game type currently unavailable" message), returns an empty game list, does not bind
the realm or WarCraft III listeners, and switches channel ordering to hub-sequenced.

It does **not** relax the one-bot-per-IP cap on the telnet/chat gateway. That cap is the
entry cost for a fleet: the gateway has no CD-key step, so the address is the only cost
available to charge, and together with CD-key session uniqueness on the game path it means
N simultaneous bots requires N keys and N addresses. Only the global ceiling rises.

Every client type is limited independently and every limit is configurable — the gateway,
BNFTP, a default for game clients, and per-product overrides:

```toml
[limits.clients.gateway]
per_ip = 1                     # keyless path, so the address is the cost

[limits.clients.game_default]
per_ip = 8                     # households and LAN cafés are real

[limits.clients.products.WAR3]
per_ip = 2                     # tighten one product without touching the rest
```

The product only arrives in `SID_AUTH_INFO`, so a game connection is admitted under a
pending class and promoted once it identifies itself. `docs/WARNET.md` §2 has the reasoning
and the complete matrix.

---

## Licence

`Apache-2.0 OR MIT`, the Rust convention. Apache-2.0 additionally carries an explicit
patent grant. Both `LICENSE-MIT` and `LICENSE-APACHE` are in the tree.

**Before publishing anything, read [`docs/LEGAL.md`](docs/LEGAL.md).** Two things matter:
PvPGN's WarCraft III SRP sources are **AGPL-3.0** and must not be read by anyone
implementing this; and *Davidson & Associates v. Jung* (8th Cir. 2005) — the bnetd case,
PvPGN's direct ancestor — held that emulating Battle.net violated the DMCA's
anti-circumvention provisions. That is a lawyer conversation, not an engineering one, and
it is independent of which open-source licence you pick.

Command Center ships no Blizzard assets, no CD keys, and no client patches, and it should stay
that way.

---

## Acknowledgements

- [BNETDocs](https://bnetdocs.org/) — the community protocol reference, maintained since
  2003 and independent of any GPL codebase. Nearly every wire fact here comes from it.
- [BNETDocs/Atlas](https://github.com/BNETDocs/Atlas) (MIT) — an effectively executable
  specification of the same protocol, and the source of the per-key attribute ACL model.
- [wjlafrance/broken-sha1](https://github.com/wjlafrance/broken-sha1) / MBNCSUtil by
  Robert Paveza (BSD-3-clause) — the X-SHA-1 reference this implementation was verified
  against.
- [PvPGN](https://github.com/pvpgn/pvpgn-server) (GPL-2.0) — twenty years of operational
  lessons, read for design and never for code.
