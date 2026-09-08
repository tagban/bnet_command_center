# BNET Command Center — Architecture

A clean-slate, federated Classic Battle.net server in Rust. Two roles, one codebase:

- **`bnetccd`** — a *node*. Terminates game and chat-gateway client connections, owns its
  local channels, hosts game advertisements, serves BNFTP. This is what a community
  operator runs.
- **`bnetcc-hub`** — the *hub*. Authoritative for identity, the global channel directory,
  ladder, and network-wide bans. Relays federated traffic between nodes.

---

## 1. Design goals, in priority order

1. **Uptime measured in months.** No crash class reachable from attacker-controlled
   bytes. No unbounded growth. No global stall.
2. **2,000+ concurrent per node**, with headroom to 10,000 before anything structural
   has to change.
3. **Federation as a first-class concept**, not an afterthought bolted onto a
   single-server design.
4. **Cross-platform** — Linux, macOS, Windows, from one source tree, as one static
   binary per platform.
5. **Small enough to audit.** PvPGN is ~114,000 hand-written lines. The Battle.net core
   is 15–25% of that. We ship the core.

### Non-goals (explicit)

Westwood Online, the IRC client gateway, Lua scripting, tournaments, in-server mail,
news, anongame matchmaking. Every one of these lives in PvPGN and every one is a reason it
is 114k lines. If you want them, they belong in a separate process talking to the node over
the admin API.

**Deliberately *in* scope, despite being on PvPGN's "extras" pile:** icon serving
(`icons.bni` over BNFTP) and advertisement banners. Icons are not optional — a client that
gets no `SID_GETICONDATA` answer before `SID_ENTERCHAT` terminates the connection — and ad
banners are the only in-client announcement surface a private server has. Both are
implemented as data the *operator* supplies; Command Center ships no Blizzard assets.

---

## 2. What the prior art actually teaches

This is not folklore — it comes from reading both codebases. Sources in
`docs/PROTOCOL-NOTES.md` and `docs/LEGAL.md`.

### PvPGN (GPL-2.0-or-later, ~114k LOC, effectively dormant since 2021)

| Finding | Consequence for Command Center |
|---|---|
| **Single process, single thread, one event loop.** No thread pool, no async storage. Every packet parse, channel broadcast, account read/write and ladder rebuild runs on the socket thread. | Tokio multi-threaded runtime; **all storage behind an actor with a bounded channel**, never inline. |
| **`sd_tcpinput()` contains no loop** — one `recv()` per readiness event, so at most one packet per epoll wakeup. | Read until `WouldBlock`, decode every complete frame in the buffer. |
| **Text protocols read one byte at a time** (`packet_set_size(packet, 1)`), so a 200-char line costs 200 syscalls and 200 event-loop passes. | Line codec over `BytesMut`. |
| **`mysql_query()` — the synchronous API — on the event loop.** A cold login blocks every other connection for a network round-trip. | `sqlx` async, plus a write-behind queue. |
| **`accountlist_save()` runs every loop iteration**, writing up to 100 dirty accounts synchronously. | Periodic flush on its own task, batched. |
| **Ladder rebuild calls `accountlist_load_all(ST_FORCE)`** — full table into RAM, synchronously, defeating SQL lazy loading. | Ladder is computed by SQL aggregate on the hub. Never a full-table load on a node. |
| **`hashtable_size = 61` buckets by default.** At 100k accounts that is ~1,640 `strcasecmp` per login. | Sharded concurrent map, sized from account count; no fixed small constant anywhere. |
| **`max_connections = 1000` shipped default**, silently refusing above it. | Default derived from `RLIMIT_NOFILE`, logged loudly at boot. |
| **41 "memory leak", 31 "crash", 8 "buffer overflow", 4 "use after free" fix commits** — including *"fix for many possible server crashes through malformed packet"* and a 2020 overflow in the WarCraft III SRP path. | This is the single strongest argument for Rust. Most of these are classes the compiler removes. We additionally fuzz every decoder. |
| **Outqueue capped at 1000 packets then the connection is killed** (added reactively in 2014 after crashes). Lazy write-interest registration. | Keep both ideas — they are the one thing PvPGN's I/O layer got right. Bounded per-connection channel, explicit slow-consumer policy. |
| **Federation never existed** — `tracker.cpp` is a fire-and-forget UDP stats beacon to a master list, nothing more. | We are the precedent. See `docs/FEDERATION.md`. |

**The most useful single datapoint:** Eurobattle.net, the largest documented deployment,
peaked at **4,000–5,000 concurrent** — and got there by moving game hosting *out* of
PvPGN into a fleet of GHost++ / pd-manager bots, leaving `bnetd` as a chat/auth/directory
node. They did not scale PvPGN; they routed around it. That tells you exactly where the
seams belong.

### Atlas (MIT, ~16k LOC, C#/.NET)

| Finding | Consequence for Command Center |
|---|---|
| **There is no persistence.** `AccountsDb` is a `ConcurrentDictionary` that is never serialized. Every account, friend list and clan evaporates on restart. | This is *why* it seems stable — a server with no persistence layer has no persistence bugs. It is not a model to copy, but it is the honest explanation. |
| **Per-key ACLs on account attributes** (`ReadLevel`/`WriteLevel` ∈ `{Any, Owner, Internal}`). | **Steal this.** PvPGN has no equivalent and its 2004 CVE (CVE-2004-2705, arbitrary attribute read incl. password hash via crafted statsreq) was exactly a missing check of this kind. |
| **Typed exception hierarchy on malformed packets** → drop the connection, don't corrupt the heap. | `Result` at every parse boundary; a decode error is a normal, logged, connection-scoped event. |
| **O(n²) receive buffering** — full realloc + copy of everything accumulated on every chunk. | `BytesMut` + `tokio_util::codec::Decoder`, amortized O(1). |
| **Open thread-safety bug filed by the author** (issue #14, still open), plus concrete races in `Channel.cs` and a disabled ping timer. | Do not import "Atlas is stable" as evidence its concurrency model is sound. It self-describes as alpha. |
| **`docs/IPC Protocol.txt`** — a real, cross-implementation federation draft (protocol byte `0x80`, full mesh, cookie-correlated `IPC_CHANNEL_QUERY`). Abandoned mid-word; no handler was ever written. | Its **query-on-demand instead of eager replication** instinct is right and we keep it. Its preshared-key + SHA-1 handshake is 2003 thinking; we use mTLS. |

---

## 3. Process and concurrency model

```
                        ┌───────────────────────────────────────────┐
                        │  bnetccd  (one node, one process)          │
                        │                                           │
   TCP :6112 ──┐        │  ┌─────────────┐                          │
   TCP :6113 ──┼──accept┼─▶│ per-conn    │  one tokio task per      │
   TCP :6114 ──┘        │  │ task        │  connection. Owns its    │
                        │  └──────┬──────┘  socket, its BytesMut,   │
                        │         │         its session state.      │
                        │         │ mpsc (bounded, 64)              │
                        │         ▼                                 │
                        │  ┌─────────────┐                          │
                        │  │ Hub actor   │  channels, sessions,     │
                        │  │ (sharded)   │  game ads, presence      │
                        │  └──┬───────┬──┘                          │
                        │     │       │                             │
                        │     │       └──▶ ┌──────────────┐         │
                        │     │            │ Storage actor│─▶ SQLite│
                        │     │            │ (write-behind)│  /PG   │
                        │     │            └──────────────┘         │
                        │     ▼                                     │
                        │  ┌─────────────┐   mTLS, one outbound     │
                        │  │ Fed client  │══ connection ════════════╪══▶ bnetcc-hub
                        │  └─────────────┘                          │
                        └───────────────────────────────────────────┘
```

**One task per connection.** A slow or hostile client can only stall itself. There is no
shared reactor thread whose progress every other user depends on — which is the single
structural difference from PvPGN, and the reason 2,000 connections is unremarkable here
and load-bearing there.

**Nothing blocking on any async task.** Storage is `sqlx` (async). SQLite goes through
`spawn_blocking` inside sqlx's own pool. CPU-bound work — CheckRevision hashing, SRP
modular exponentiation — goes to `spawn_blocking` explicitly. A single SRP handshake is
~1ms of bignum math; at 50 logins/sec that is 5% of one core, but it must not sit on a
task that also serves 200 chat users.

**Shared state is sharded, not global-locked.** Channels, sessions and account cache live
in a sharded map (`DashMap`-style, shard count = `2 × cores`, minimum 16). The pathology
to avoid is PvPGN's 61-bucket table; the pathology on the other side is Atlas's coarse
`lock(Users)` around every channel operation.

### Backpressure — the explicit contract

Every connection has a **bounded outbound channel (64 frames)**. On overflow the policy is
per-connection-class and *stated*, never implicit:

| Class | Overflow policy |
|---|---|
| Game client | Drop the connection with `EID_ERROR` "connection too slow". |
| Chat gateway / bot | Drop the connection. Bots must keep up. |
| Federation link | Never drop. Apply backpressure upstream and shed *channel* traffic first, keeping control-plane messages. |

Channel fanout is the real scaling risk, not connection count: one message to a 200-user
channel is 200 sends. We fan out by cloning an `Arc<Frame>` into each subscriber's channel
— the encode happens **once**, not 200 times. A subscriber whose queue is full is dropped
from the fanout, not blocking it.

---

## 4. Crate map

| Crate | Responsibility | Depends on I/O? |
|---|---|---|
| `bnetcc-crypto` | XSHA-1 ("Broken SHA-1"), BSHA-1, NLS/SRP-6 (Blizzard variant), CD-key decode, CheckRevision. Pure functions, heavily tested against known-answer vectors. | No |
| `bnetcc-proto` | Wire framing and packet types for BNCS, MCP, W3GS, chat gateway. Codecs only — `Decoder`/`Encoder`, zero policy, zero I/O. Fuzz targets live here. | No |
| `bnetcc-core` | Domain model: accounts, sessions, channels, game ads, policy engine, the state machines. Pure logic over traits. | No |
| `bnetcc-storage` | `Storage` trait, per-key attribute ACLs, write-behind batching, in-memory reference backend, and the conformance suite every backend must pass. Dependency-free. | No |
| `bnetcc-storage-sqlite` | SQLite backend. The only place a database driver appears. | Yes |
| `bnetcc-bridge` | External chat bridges — Discord, game addons, anything not speaking BNCS. Virtual presences rather than relayed text; see `docs/BRIDGES.md`. | Yes |
| `bnetcc-fed` | Federation: node↔hub protocol, mTLS transport, message types, reconnect and partition handling. | Yes |
| `bnetcc-gateway-bncs` | Protocol-byte demux and the BNCS session driver. | Yes |
| `bnetcc-gateway-chat` | Telnet / chat-gateway (protocol bytes `0x03`/`0x43`/`0x63`), with per-IP admission control. | Yes |
| `bnetcc-gateway-mcp` | Diablo II realm (MCP), **linked into `bnetccd` as a module, not a daemon** — see §11. Phase 4. | Yes |
| `bnetccd` | Node binary: config, listeners, wiring, observability. | Yes |
| `bnetcc-hub` | Hub binary: directory, identity, ladder, ban authority, relay. | Yes |
| `bnetcc` | Admin CLI over the local admin socket. | Yes |

The layering rule is enforced by the dependency graph: **`bnetcc-proto` and `bnetcc-core`
must not depend on `tokio`.** That keeps the protocol and the domain logic testable
without a runtime, and it is what makes fuzzing the decoders cheap.

---

## 5. Connection lifecycle (BNCS)

```
accept
  └─ admission control: per-IP connection cap, global cap, banned-IP check
  └─ read 1 byte: protocol selector
       0x01 Game  ──▶ BNCS session
       0x02 BNFTP ──▶ file transfer session
       0x03/43/63 ─▶ chat gateway session   (per-IP limit applies here)
       else       ──▶ close, no response

BNCS session state machine:
  Connected
    └─ SID_AUTH_INFO (0x50)      ──▶ Versioning     [emit server token, CheckRevision seed]
    └─ SID_AUTH_CHECK (0x51)     ──▶ Authenticating [version + CD-key verdict]
         ├─ XSHA-1 products ──▶ SID_LOGONRESPONSE2 (0x3A)          ──▶ hub-proxied verify
         └─ SRP products    ──▶ SID_AUTH_ACCOUNTLOGON (0x53) +
                                 SID_AUTH_ACCOUNTLOGONPROOF (0x54) ──▶ edge verify
    └─ SID_ENTERCHAT (0x0A)      ──▶ Chatting
    └─ SID_JOINCHANNEL (0x0C)    ──▶ InChannel
         └─ SID_CHATCOMMAND / SID_STARTADVEX3 / SID_GETADVLISTEX …
```

Every transition is a `match` on `(state, packet_id)`. A packet that arrives in the wrong
state is a protocol violation: log at debug, close the connection. No packet handler is
reachable before authentication except the handful above — which is the structural fix for
CVE-2004-2705's whole class.

**Timers we actually enforce**, because PvPGN's absence of them is how slowloris works:

- **Handshake deadline**: 30s from accept to authenticated, then close.
- **Idle deadline**: configurable, default 20 min without a client packet.
- **Relogin cooldown**: ≥500 ms. Real Battle.net keeps a CD key marked "in use" if you
  reconnect faster; replicate the cooldown so clients behave identically.

---

## 6. Storage

A `Storage` trait — the one genuinely good idea in PvPGN's design (its 19-function-pointer
vtable) expressed properly. **Built and tested; see `crates/bnetcc-storage`.**

The trait is **synchronous**, which is deliberate. Storage sits behind an actor: async
tasks send commands over a bounded channel and a dedicated thread runs plain blocking code
against the database. Once that boundary exists, making the trait `async` buys nothing and
costs a great deal — `async fn` in traits is not dyn-compatible, so every backend would
need `async_trait` boxing.

The failure this avoids is precise. PvPGN calls `mysql_query()` — the *synchronous*
libmysqlclient API — directly on its single event-loop thread, so a cold login freezes
every other connection for a network round trip. The problem was never that the call
blocked; it was that it blocked **on the reactor**.

```text
  handlers ──▶ WriteBehind<B> ──▶ B: Storage ──▶ SQLite / Postgres / memory
                    │
                    └── batches attribute writes; account creation,
                        credential changes and bans are write-through
```

That split is the durability contract, and it is tested rather than asserted
(`losing_the_buffer_loses_only_attributes`): an unclean shutdown costs at most one flush
interval of profile edits and ladder records, and never an account registration, a
password change, or a ban. PvPGN gets both halves wrong in one loop — `accountlist_save()`
runs on **every** iteration, writing up to 100 dirty accounts synchronously on the socket
thread, paying write-through latency for write-behind durability.

Every backend proves itself against one **conformance suite**
(`bnetcc_storage::conformance::run`) rather than its own tests. That is what makes swapping
SQLite for Postgres a decision rather than a rewrite, and it is what catches the
divergences that otherwise surface in production on one backend only: case folding on
names and attribute keys, merge-versus-replace on attribute writes, and which ban scope
wins when both are present.

**Two implementations.** SQLite is the default — a community node operator should be able
to run `bnetccd` with zero external services. Postgres is for the hub and for nodes past a
few thousand accounts.

**Explicitly not inherited from PvPGN:**

- No file-per-account. PvPGN rewrites the whole account file on every save with `fopen(w)`,
  no atomic rename, no fsync — and because the *filename* is the account name, it had to
  restrict legal usernames (`account_allowed_symbols = "-_[]"`) to satisfy a storage
  choice. Username validation should be a product decision, not a filesystem artifact.
- No full-directory scan at boot.
- No synchronous write in the request path. Attribute writes go to the write-behind actor,
  which batches and flushes on an interval or at a dirty-count threshold. A crash loses at
  most one flush interval of non-critical attribute updates; account creation, password
  change and ban application are **write-through** and never batched.

**Account attributes carry Atlas's ACL model**: every key has a read level and a write
level in `{Any, Owner, Internal}`. `System\Password Digest` is `Internal`/`Internal` and
is therefore unreachable by any client-facing read path by construction.

---

## 7. Policy engine — how warnet mode works

Server behaviour that an operator can change is not scattered through handlers as `if`
statements. It is one `Policy` value, resolved once per session at login and carried in
the session:

```rust
pub enum ServerMode { Gaming, Warnet, Both }

pub struct Policy {
    pub mode: ServerMode,
    pub game_hosting: Gate,        // StartAdv / StopAdv
    pub game_listing: Gate,        // GetAdvListEx
    pub realms: Gate,              // QueryRealms2 / LogonRealmEx
    pub chat_ordering: Ordering,   // LocalFirst | HubSerialized
    pub conn_limits: ConnLimits,   // per-IP, per-account, per-class
    pub flood: FloodPolicy,
}
```

Handlers ask the policy; they do not know what mode means. That keeps mode a
*configuration* concern rather than a code path that rots. See `docs/WARNET.md` for the
full semantics.

---

## 8. Observability

Non-negotiable, because "PvPGN gets slow above 1000 users" was a diagnosis nobody could
make from the outside.

- `tracing` throughout, with a span per connection carrying account name and node id.
- Prometheus metrics on the admin listener: connections by class and state, per-packet-id
  decode counts and error counts, channel fanout sizes, outbound-queue depth histogram,
  storage batch latency, federation link state and lag, login latency split by
  edge-verified vs hub-proxied.
- **A `/debug/slow` endpoint** listing the connections with the deepest queues. When
  someone reports lag, the answer should take thirty seconds to find.

---

## 9. Cross-platform notes

Tokio gives us epoll on Linux, kqueue on macOS/BSD, and IOCP on Windows behind one API,
so the I/O layer is genuinely portable — this is the part PvPGN spent a whole subsystem
(`fdwatch`, four backends, 2003) hand-rolling.

Real portability work, in order of how much it will actually cost:

1. **File descriptor limits.** Linux/macOS: read `RLIMIT_NOFILE` at boot, attempt to raise
   the soft limit to the hard limit, derive `max_connections` from it, and **log the
   effective ceiling at INFO**. macOS ships a low default (256 in some contexts) and needs
   `kern.maxfilesperproc` attention on a real deployment. Windows has no equivalent limit
   but does have ephemeral-port exhaustion under load-test conditions.
2. **Path handling.** `PathBuf` everywhere; no assumptions about separators; config paths
   resolved relative to the config file, not the CWD.
3. **Service integration.** systemd unit (Linux), launchd plist (macOS), and a Windows
   service wrapper. Ship all three; a node operator on Windows is a real user here.
4. **Case-insensitive account names** must be case-folded *in our code*, not delegated to
   a case-insensitive filesystem. This is a correctness bug waiting to happen if we ever
   touch the filesystem for account identity — which is another reason not to.

---

## 10. Capacity target and how it is verified

Budget per idle connection: ~8 KB read buffer (starts at 512 B, grows on demand), 64-frame
bounded write queue, ~1 KB session struct. Target **under 16 KB steady-state per
connection**, so 2,000 connections ≈ 32 MB of connection state.

The bottleneck at this scale is not connection count — it is **channel fanout**. A 200-user
channel at 10 messages/second is 2,000 sends/second from one channel. The design answer is
encode-once/`Arc`-clone-many plus bounded per-subscriber queues.

`loadtest/` drives synthetic connections through a full handshake and into a channel, and
reports connection setup latency, steady-state RSS, and p50/p99 chat round-trip. See
`docs/CAPACITY.md` for the method and the numbers this tree currently produces.

---

## 11. One binary — why the Diablo II realm is not separate daemons

PvPGN ships four processes for Diablo II: `bnetd` (6112), `d2cs` (the MCP/realm server,
6113), `d2dbs` (the character database, 6114), and a third-party `d2gs` (the actual game
server, 4000). Command Center folds the first three into `bnetccd`.

### Why the split is not worth keeping

- **It costs a wire protocol that should be a function call.** `bnetd` and `d2cs` talk
  over a custom binary protocol (`d2cs_bnetd_protocol.h`) with its own connection class,
  retry interval, timeout and keepalive settings. All of that is machinery to move a
  character list between two parts of one server.
- **`d2dbs` never got the fdwatch treatment.** It still calls `psock_select()` directly,
  rebuilding `fd_set`s by walking every connection each iteration — O(n) per loop, hard
  capped at `FD_SETSIZE` (1024 on Linux). `bnetd` and `d2cs` were fixed in 2003; the
  character database was not, and it is a 2003-era hole that has never been patched.
  Folding it in deletes that entire class of problem rather than porting it.
- **Three processes means three configs, three supervision units, three log streams, and
  three ways to have a version mismatch.** For a community operator that is the difference
  between "install the server" and "install the server, four times, in the right order".
- There is precedent: `jaenster/d2-dedicated-server` collapses BNCS and MCP onto a single
  port 6112 and argues that this is closer to what real Battle.net actually did than
  PvPGN's split.

So in Command Center the realm is a **gateway module**, exactly like BNCS and the chat gateway:
it owns MCP framing (`len:u16le` first, **no** `0xFF` magic — the classic mistake) and its
own session state machine, and it reaches the character store through the `Storage` trait
as a normal function call. One process, one config, one binary to supervise.

### The exception, stated honestly

**D2GS cannot simply be "part of the main app", because it is not a protocol server.** It
runs the actual Diablo II simulation: monsters, items, skills, map generation, save
handling. PvPGN does not ship one and says so plainly; the D2GS people use is
closed-source, Windows-only, and prone to crash-looping on modern Windows. Writing our own
means writing a Diablo II game simulation, which is an enormous project in its own right
and the reason nobody in this ecosystem has one.

What the architecture does instead is make the *operator's* experience single-binary
regardless, through one trait:

```rust
pub trait GameHost: Send + Sync {
    /// Allocate a game and return the endpoint the client should dial.
    async fn create(&self, req: CreateGame) -> Result<GameEndpoint>;
    async fn destroy(&self, id: GameId) -> Result<()>;
}
```

- **`ExternalGameHost`** speaks to an existing D2GS for operators who already run one. It
  is a client of that process, not a sibling daemon of ours — `bnetccd` is still the only
  thing you install and supervise.
- **`EmbeddedGameHost`** is where an in-process game server plugs in if one is ever
  written. It becomes a module, not a fourth daemon.

One constraint survives either way and is worth knowing before planning realms: **port
4000 is hardcoded in the Diablo II client**, so the client always dials 4000 on the
address the realm hands it. That caps you at **one realm per IP address** — not per host,
since additional addresses work fine. Embedding the game server does not lift this,
because the constraint lives in the client.

---

## 12. Build order

Phases are in `docs/ROADMAP.md`. The short version: get one real StarCraft 1.16.1 client
into a channel before writing a single line of realm, ladder, or clan code. Every prior
project in this space that started with breadth ended at 114k lines.
