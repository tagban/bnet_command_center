# Handoff

Where this project is, what was decided and why, and what to do next. Written so a fresh
session with no prior context can pick it up cold.

**Recommended next environment: Claude Code running natively on the Mac.** This project's
next phase is a compile-fix loop against `tokio`, which needs a real toolchain and network
access. See §6 for why the cloud session hit a wall there.

---

## 1. What this is

A clean-slate, federated Classic Battle.net server in Rust, replacing PvPGN and BNETDocs
Atlas. Project name **BNET Command Center**, `bnetcc` in code, `bnet.cc` is the domain.

| Binary | Role |
|---|---|
| `bnetccd` | the node daemon — what a community operator installs |
| `bnetcc-hub` | the hub: identity, channel directory, ladder, ban authority |
| `bnetcc` | admin CLI |

Targets: StarCraft/Brood War ≤1.16.1, Diablo I, Warcraft II BNE, Diablo II/LoD, Warcraft
III ≤1.26/1.28. Linux, macOS, Windows. 2,000+ concurrent connections per node.

---

## 2. State of the tree

**227 tests, `clippy -D warnings` clean**, four library crates with **zero third-party
dependencies**.

| Crate | State |
|---|---|
| `bnetcc-crypto` | ✅ Built and tested. X-SHA-1 verified against known-answer vectors. |
| `bnetcc-proto` | ✅ Built and tested. BNCS + MCP + chat-gateway framing, BNI icons, BNFTP v1, statstrings. |
| `bnetcc-core` | ✅ Built and tested. Policy, channels, admission, flood, CD-key registry, ads, bridged identities, session FSM. |
| `bnetcc-storage` | ✅ Built and tested. Trait, attribute ACLs, write-behind, in-memory backend, conformance suite. |
| `smoke` | ✅ Built and run. 4,000/4,000 concurrent real handshakes on Linux. |
| **`bnetccd`** | ⚠️ **Never compiled.** Written in full. Needs `tokio`. |
| **`bnetcc-storage-sqlite`** | ⚠️ **Never compiled.** Written in full. Needs `rusqlite`. |

Both uncompiled crates **parse cleanly** (checked with `rustfmt`), so syntax errors are
ruled out. What remains is types and borrows. My guess at where the errors are: the
borrow of `buf` across the `timeout(deadline, rd.read(tail))` await in
`bnetccd/src/session.rs`, and the `stream` move into the protocol-selector closure in
`handle()`.

They are **excluded from the workspace** in `Cargo.toml` because the environment they were
written in had no crates.io access. `bnetcc-storage-sqlite` is a separate crate rather
than a feature flag because an *optional* dependency still forces registry resolution.

---

## 3. Do this first

```sh
cd /Volumes/AppStorage/bnet_command_center
bash scripts/verify.sh
```

That enables both excluded crates, runs build/test/clippy and the 2,500-connection load
test, and writes `verify.log`. If the build fails it restores the reduced workspace so the
library crates still build. Work through the compiler errors from that log.

Then, in order, from `docs/ROADMAP.md` phase 1:

1. **Test against a real Brood War client.** Everything else is theory until a client sits
   in a channel. Expect surprises at the ⚠️/🛑 items in §5.
2. Wire storage into `bnetccd` behind the actor (bounded channel → dedicated thread →
   `WriteBehind`), with `AttrSchema::filter_readable` on every client-facing read.
3. BNFTP file serving, then `SID_GETICONDATA` icon serving.
4. `KeyRegistry` into `SID_AUTH_CHECK`.
5. Real randomness for server tokens (currently a time-and-counter mix with a TODO).

---

## 4. Decisions already made, and why

Do not relitigate these without a reason; the reasoning is in the linked docs.

| Decision | Why | Where |
|---|---|---|
| **Rust + tokio**, one task per connection | PvPGN's 90+ memory-safety fix commits are classes the compiler removes. Its single event loop with synchronous `mysql_query()` on it means one cold login freezes every connection. | `ARCHITECTURE.md` §2–3 |
| **Star federation, hub-authoritative** | A community operator behind NAT can hold an outbound connection but not accept inbound from ten peers. That single fact rules out Atlas's full mesh. | `FEDERATION.md` §1 |
| **Nodes semi-trusted; hub owns identity, ladder, bans** | The threat is an operator wanting free ladder rank, not one griefing their own users. | `FEDERATION.md` §2 |
| **X-SHA-1 logons hub-proxied, SRP verified at the edge** | `h1` is password-equivalent, so shipping it to nodes would let any operator impersonate any user network-wide. An SRP verifier cannot impersonate the client, so it caches safely. **This asymmetry is the design, not a wart.** | `FEDERATION.md` §4 |
| **One chat-gateway connection per IP, in every mode including warnet** | The gateway has no CD-key step, so the address is the only cost available to charge. Twenty bots means twenty addresses, and that expense is the point — a fleet that costs nothing displays nothing. | `WARNET.md` §2 |
| **CD-key session uniqueness** is the gate on game-client fleets | Addresses are cheap to rent; keys are not. | `WARNET.md` §2 |
| **Per-client-type limits, two-stage classification** | The selector byte says "a game client", not *which* game — the product only arrives in `SID_AUTH_INFO`. So a connection is admitted as `GamePending` and promoted. | `bnetcc-core::limits` |
| **Warnet mode uses hub-sequenced chat ordering** | Two observers disagreeing about who got operator first is the product being broken. | `WARNET.md` §4 |
| **D2 Open first; closed realms in one binary; in-house game server committed but not scheduled** | Open games are peer-to-peer and need no game server at all — the cheap 80%. PvPGN's `d2dbs` still uses `select()` capped at `FD_SETSIZE`; folding it in deletes that. | `ROADMAP.md` phases 2/4/5, `ARCHITECTURE.md` §11 |
| **Storage trait is synchronous** | It sits behind an actor. Async in the trait buys nothing once that boundary exists and costs dyn-compatibility. | `bnetcc-storage/src/lib.rs` |
| **Attribute writes batch; accounts, credentials and bans write through** | Losing a profile edit is recoverable; losing a registration or a ban is not. | `write_behind.rs`, tested by `losing_the_buffer_loses_only_attributes` |
| **Per-key attribute ACLs** | Makes the password digest unreachable from client paths *by construction*. That is CVE-2004-2705's whole class. | `bnetcc-storage/src/attr.rs` |
| **Icons and ad banners are in scope** | A client that gets no `SID_GETICONDATA` before `SID_ENTERCHAT` terminates the connection. Ads are the only in-client announcement surface. | `ARCHITECTURE.md` §1 |
| **Bridged users are presences, not relayed text** | One bot posting `<Discord> alice:` gives one roster entry, no whisper, no per-user moderation, and one chatty user throttles fifty. | `BRIDGES.md` §1 |

---

## 5. Landmines

**Legal — read `docs/LEGAL.md` before publishing anything.**

- 🛑 **Never open `pvpgn/src/common/bnetsrp3.{cpp,h}` or `bigint.{cpp,h}`.** They are
  **AGPL-3.0**, not GPL like the rest of PvPGN, and they are the WarCraft III SRP
  implementation — exactly the file a reimplementer reaches for. §13's network-use
  disclosure would be fatal to a hosted service. Get SRP from the javaop write-up and
  RFC 2945 instead.
- PvPGN generally is GPL-2.0-or-later: read for design, never copy. **Atlas is MIT and
  freely readable** — prefer it.
- *Davidson v. Jung* (8th Cir. 2005), the bnetd case, held that emulating Battle.net
  violated the DMCA's anti-circumvention provisions. Lawyer conversation, independent of
  open-source licence.
- WarCraft III will always need a **client-side patch** — `SID_AUTH_INFO` carries a
  128-byte RSA signature only Blizzard can produce. Document it; never distribute it.
- Ship no Blizzard assets, no CD keys, no client patches.

**Protocol facts that will bite.**

- 🛑 `icons-WAR3.bni` and `WAR3.bni` **are not BNI files** — they are MPQ archives of
  `.blp` images. WC3 icons need an MPQ reader. The parser detects this and says so.
- ⚠️ **MCP framing is length-first with no magic byte**, the inverse of BNCS. Most common
  bug in D2 realm implementations. There is a test asserting a BNCS frame fed to the MCP
  decoder does not silently succeed.
- ⚠️ `SID_GETICONDATA` **must be answered before `SID_ENTERCHAT`** or the client
  terminates the connection.
- ⚠️ A relayed `\r` or `\n` in chat makes the **receiving** client disconnect and IP-ban
  for five minutes. `sanitize_chat_text` handles it; anything bypassing that is a bug.

**Unverified — confirm against a real client or capture.** All marked in
`PROTOCOL-NOTES.md`:

- Four-character code byte order on the wire (derived from the endianness rule, never
  stated).
- The zero-game `SID_GETADVLISTEX` response shape (BNETDocs is ambiguous; getting it wrong
  makes the client hang rather than error).
- Whether a BNI icon entry's code list is present when `flags != 0`.
- The ad extension tag's wire bytes (`.smk`/`.mng`/`.pcx`).

---

## 6. Environment notes

- **crates.io is blocked** by the org egress policy in Anthropic-hosted cloud sessions.
  That is why `bnetccd` was never compiled there. An admin can allowlist it, but it would
  not help produce macOS binaries — cross-compiling from Linux needs the Apple SDK.
- **`device_bash` failed** in the cloud session: "the isolated Linux environment on this
  device failed to start". That sandbox is the desktop app's own, not anything on the Mac.
  **Best hypothesis: the connected folder is on an external volume (`/Volumes/AppStorage`)
  and the sandbox cannot mount it.** Worth testing by connecting a folder under `~`.
- **Terminal is granted click-only** through computer use, so a cloud session cannot type
  commands into it either.
- **macOS defaults to `ulimit -n 256`**, far below what 2,500 connections need.
  `verify.sh` raises it; anything else running the load test must too.

---

## 7. Open questions for tagban

1. **Postgres or MariaDB** for the hub's network backend? Recommendation is Postgres —
   window functions make ladder ranking one query instead of PvPGN's load-every-account
   rebuild. MariaDB is defensible on ecosystem familiarity, since PvPGN's SQL backend is
   MySQL-first. **Pick one, not both** — two backends means every schema change written
   twice. `OPERATIONS.md` §4.
2. **Project licence.** `Apache-2.0 OR MIT` is declared and recommended; `LICENSE-APACHE`
   still needs adding from apache.org.
3. **Does the in-house Diablo II game server get resourced** as its own track? It is
   committed in `ROADMAP.md` phase 5 and blocks nothing.

---

## 8. Reading order for a fresh session

1. `README.md` — what it is, how to build.
2. `docs/ARCHITECTURE.md` §2 — the evidence table from reading PvPGN and Atlas. Most of
   the design follows from it.
3. `docs/ROADMAP.md` — what is done and what is next.
4. This file's §4 — decisions, so you do not re-derive them.
5. `docs/PROTOCOL-NOTES.md` — when touching any wire format. Confidence markers matter.

Other docs as needed: `FEDERATION.md`, `WARNET.md`, `BRIDGES.md`, `OPERATIONS.md`,
`CAPACITY.md`, `LEGAL.md`.

Git history is intact and the commit messages carry reasoning — `git log` is worth reading
rather than skipping.
