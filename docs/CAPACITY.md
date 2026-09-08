# Capacity

Target: **2,000+ concurrent connections per node**, with headroom to 10,000 before
anything structural has to change.

---

## 1. Measured, in this tree, today

`crates/smoke` drives a real BNCS handshake — protocol selector, `SID_AUTH_INFO`,
`SID_AUTH_CHECK`, `SID_CREATEACCOUNT2`, `SID_LOGONRESPONSE2` with a genuine X-SHA-1
double-hash proof, `SID_ENTERCHAT`, `SID_JOINCHANNEL`, `SID_CHATCOMMAND` — over real TCP
sockets, then holds every connection open and times a channel broadcast.

Host: 2 vCPU, 8 GB RAM, `ulimit -n` 20000, Linux 6.18, `--release`.

| Connections | Established | Logons verified | Setup wall time | RSS | Fanout p50 | Fanout p99 |
|---|---|---|---|---|---|---|
| 500 | 500 / 500 | 500 | 0.4 s | 18 MB | 0.75 ms | 1.11 ms |
| 2,500 | 2,500 / 2,500 | 2,500 | 1.18 s | 78 MB | 0.47 ms | 0.98 ms |
| 2,500 (repeat) | 2,500 / 2,500 | 2,500 | 1.31 s | 78 MB | 0.72 ms | 1.17 ms |
| 4,000 | 4,000 / 4,000 | 4,000 | 1.50 s | 123 MB | 0.62 ms | 0.93 ms |
| 8,000 | 6,662 / 8,000 | 6,662 | — | — | — | — |

Fanout figures are the time from `SID_CHATCOMMAND` leaving the sender to the message
arriving at each of 40 channel members.

**The 8,000 row is a file-descriptor ceiling, not a software one.** The harness holds
*both* ends of every connection in one process, so N connections cost 2N descriptors;
6,662 × 2 ≈ 13,300 plus overhead is the 20,000 limit. A real server pays one descriptor
per connection, so the same budget carries roughly twice as many. The failure is a clean
`EMFILE` on `connect`, reported and counted — nothing crashed, and the 6,662 connections
that did establish kept working.

Reproduce with:

```
cargo build --release -p bnetcc-smoke
ulimit -n 20000
./target/release/bnetcc-smoke 2500 40
```

### What these numbers do and do not prove

**They do prove:** the framing survives real socket boundaries and partial reads; the
session state machine accepts a real handshake; X-SHA-1 verification works end-to-end
across two independent implementations of the hashing chain (the client computes the
proof, the server recomputes it, 4,000 times with zero false rejections); channel join,
roster and fanout are correct under concurrency; memory per connection is flat from 500
to 4,000, so nothing is O(n²) in connection count.

**They prove nothing about tokio.** The harness is thread-per-connection because it has
to build without an async runtime, so its RSS includes two thread stacks per connection
that `bnetccd` does not pay. 30 KiB per connection here is an **upper bound with a large
known constant in it** — a tokio task is a few hundred bytes plus its buffers. Re-measure
`bnetccd` itself on the target host before quoting anything.

One more caveat worth stating: the harness opens connections in waves of 100. `std`'s
`TcpListener` has a fixed 128-entry accept backlog, and 2,500 simultaneous `connect`
calls overrun it regardless of how the server is written. The requirement is 2,000+
connections *held concurrently*, which is what the table measures; accept-storm
resilience is a separate property that needs a tuned backlog and is a `bnetccd` concern.

---

## 2. The budget the design is working to

Per idle connection:

| Item | Budget |
|---|---|
| Receive buffer | 512 B initial, grows to 4 KiB on demand, compacts back |
| Outbound queue | 64 frames × pointer, bounded |
| Session state | ~1 KiB |
| **Target steady state** | **under 16 KiB** |

2,000 connections ≈ 32 MB of connection state. That is not the interesting number.

---

## 3. The actual bottleneck is channel fanout

Connection count is easy. One message to a 200-user channel is 200 sends; ten such
channels at 10 messages/second is 20,000 sends/second from chat alone. That is what
scales badly if you get it wrong.

Three mitigations, all implemented in `Node::broadcast`:

1. **Encode once, `Arc`-clone many.** A 200-user channel costs one encode and 200
   pointer clones, not 200 encodes. Atlas reallocates and copies its whole receive
   buffer on every chunk; PvPGN re-serialises per recipient.
2. **Bounded per-subscriber queue.** A subscriber whose queue is full is returned to the
   caller to be closed. It never blocks the other 199. PvPGN had to add exactly this cap
   in 2014 after crashes from unbounded queue growth, and its fix was the same one.
3. **No lock held across a write.** The subscriber list is locked long enough to clone
   handles and released before anything touches a socket.

That third point is not theoretical. The first version of the smoke harness held one
global subscriber lock across blocking socket writes, and at 500 connections it started
failing handshakes — every channel join serialised behind every other channel's writes.
Splitting the lock per channel took it from 498/500 to 4,000/4,000. **The lock shape was
worth more than any amount of connection-handling cleverness**, which is the general
lesson for this workload.

---

## 4. Where the ceilings actually are

| Ceiling | Symptom | Fix |
|---|---|---|
| **File descriptors** | `EMFILE`, refused connections | Raise `ulimit -n` / `LimitNOFILE=`; on macOS also `kern.maxfilesperproc`. `bnetccd` derives `max_connections` from `RLIMIT_NOFILE` and **logs the effective ceiling at startup** — PvPGN ships a hard-coded 1000 and refuses silently past it, which is why operators concluded it could not scale. |
| **Accept backlog** | Connection resets during a login storm | Tune the listen backlog; `bnetccd` should expose it. Matters after a netsplit, when everyone reconnects at once. |
| **Channel fanout** | Rising p99 chat latency | Cap channel size (real Battle.net used 40); shard large channels. |
| **Storage in the request path** | Global latency spikes | Never. Storage is behind a write-behind actor; account creation, password change and bans are write-through and everything else is batched. This is the single biggest thing PvPGN got wrong — a synchronous `mysql_query()` on the event-loop thread freezes every other connection for the duration. |
| **Ephemeral ports** | Only ever the load generator | Generate load from more than one source address. |

---

## 5. What to measure on the real thing

Once `bnetccd` builds with tokio, the numbers that matter:

- Connection setup latency, p50/p99, under a reconnect storm of 2,000.
- Steady-state RSS at 2,000 idle connections. Expect **well under** 30 KiB each.
- Chat round-trip p99 in a 40-user and a 200-user channel.
- Outbound queue depth histogram — if the tail is climbing, the fanout is losing.
- Login latency split by edge-verified (SRP) versus hub-proxied (X-SHA-1). These are
  structurally different paths and averaging them hides the interesting one.

For reference, the largest documented PvPGN deployment — Eurobattle.net — peaked at
**4,000–5,000 concurrent**, and got there by moving game hosting out of PvPGN entirely
into a fleet of GHost++/pd-manager bots, leaving `bnetd` as a chat/auth/directory node.
That is the bar, and it is a bar they cleared by routing around the server rather than
scaling it.
