# Cairn

A federated Classic Battle.net server in Rust.

Built clean-slate, informed by what PvPGN and BNETDocs Atlas got right and wrong. Two
roles from one codebase: **`cairnd`**, the node a community operator runs, and
**`cairn-hub`**, which is authoritative for identity, the channel directory, ladder and
network-wide bans.

> **`Cairn` is a placeholder name** — a cairn is a waypoint built by many travellers each
> adding one stone, which is the federation model. Rename with one `sed` before release;
> nothing depends on it.

---

## Status

**Phase 0 complete.** The protocol core, cryptography, domain rules and a concurrency
harness are written, tested and measured. `cairnd` is written but not yet compiled — see
[Building](#building).

| Crate | State | Tests |
|---|---|---|
| `cairn-crypto` | X-SHA-1 verified against known-answer vectors | 7 |
| `cairn-proto` | BNCS + chat-gateway framing, checked wire codecs | 41 |
| `cairn-core` | Policy, channels, admission, flood, key registry, session FSM | 58 |
| `cairn-smoke` | Real handshake at 4,000 concurrent connections | — |
| `cairnd` | Written; needs a dependency-resolving build | — |

106 tests, `cargo clippy -D warnings` clean, zero third-party dependencies in the three
library crates.

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
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Process model, crate map, connection lifecycle, backpressure, storage, observability, cross-platform notes |
| [`docs/FEDERATION.md`](docs/FEDERATION.md) | Star topology, trust model, mTLS transport, the X-SHA-1/SRP identity asymmetry, channel sequencing, ladder validation, partition behaviour |
| [`docs/WARNET.md`](docs/WARNET.md) | Warnet vs gaming mode, what each gates, connection limits for bot fleets, operator semantics, ordering fairness, flood control |
| [`docs/PROTOCOL-NOTES.md`](docs/PROTOCOL-NOTES.md) | Wire reference with confidence markers — ✅ verified, ⚠️ single-source, 🛑 unknown |
| [`docs/CAPACITY.md`](docs/CAPACITY.md) | Measured numbers, what they prove, where the real ceilings are |
| [`docs/LEGAL.md`](docs/LEGAL.md) | Licence contamination map, clean-room guidance, *Davidson v. Jung*, the WarCraft III signature problem |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Phases, and why modern clients are not on them |

---

## Building

```sh
cargo test                                  # the three library crates + harness
cargo build --release -p cairn-smoke
ulimit -n 20000
./target/release/cairn-smoke 2500 40        # 2500 concurrent real handshakes
```

**`crates/cairnd` is currently excluded from the workspace.** It needs `tokio`, and the
environment this scaffold was authored in had no access to `crates.io`. To build it:

```diff
  members = [
      "crates/cairn-crypto",
      "crates/cairn-proto",
      "crates/cairn-core",
      "crates/smoke",
+     "crates/cairnd",
  ]
- exclude = ["crates/cairnd"]
```

then `cargo build`. It is real code, not a stub, but it has never been through a
compiler — expect to fix what `rustc` finds. The three library crates it depends on are
fully tested.

```sh
cairnd --config cairnd.toml --check   # validate configuration and exit
cairnd --config cairnd.toml
```

---

## Configuration

Every field has a default, so a minimal config is a few lines. See
[`cairnd.example.toml`](cairnd.example.toml).

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
patent grant. `LICENSE-MIT` is in the tree; add `LICENSE-APACHE` from
<https://www.apache.org/licenses/LICENSE-2.0.txt> before publishing.

**Before publishing anything, read [`docs/LEGAL.md`](docs/LEGAL.md).** Two things matter:
PvPGN's WarCraft III SRP sources are **AGPL-3.0** and must not be read by anyone
implementing this; and *Davidson & Associates v. Jung* (8th Cir. 2005) — the bnetd case,
PvPGN's direct ancestor — held that emulating Battle.net violated the DMCA's
anti-circumvention provisions. That is a lawyer conversation, not an engineering one, and
it is independent of which open-source licence you pick.

Cairn ships no Blizzard assets, no CD keys, and no client patches, and it should stay
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
