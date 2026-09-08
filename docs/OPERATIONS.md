# Operations — storage, inspection, monitoring

Answering the practical question: **which database, and how do I look at it?**

---

## 1. Which backend

| Role | Backend | Why |
|---|---|---|
| **Node** (`bnetccd`) | **SQLite**, default | A community operator should be able to run a node with no external services at all. "Install and configure a database server" is the single biggest reason PvPGN deployments fail before they start. |
| **Hub** (`bnetcc-hub`) | **Network database** | The hub holds the ladder, the audit log and cross-node queries. It is also the thing you would actually build a website on, and it is run by you rather than by a stranger. |
| **Large node** (5,000+ accounts) | Network database | Optional. SQLite is fine well past the point most nodes reach. |

This is a **configuration choice, not a fork**. Every backend implements the same
`Storage` trait and proves itself by passing the same conformance suite
(`bnetcc_storage::conformance::run`), which is what makes swapping one for another a
decision rather than a rewrite — and what catches the divergences that would otherwise
only appear in production on one backend.

---

## 2. Yes, you can inspect a live SQLite database

The node opens SQLite with `journal_mode = WAL`, which means **readers do not block the
writer and the writer does not block readers**. So while the server is running you can:

```sh
sqlite3 -readonly /var/lib/bnetcc/users.db 'SELECT COUNT(*) FROM accounts;'
```

and point DB Browser for SQLite, Datasette, or a monitoring script at the same file. No
downtime, no locking the server out, consistent reads.

**Always pass `-readonly`.** Not because a write would corrupt anything — SQLite handles
concurrent writers correctly — but because a stray `UPDATE` from a dashboard is
indistinguishable from a server bug when you come to debug it.

### The one real limit

**WAL only works between processes on the same machine.** It coordinates through shared
memory and file locking, so it does *not* work over NFS, SMB, or any network filesystem.
A dashboard on the same box is fine. A dashboard on another host is not, and the failure
mode is silent corruption rather than a clean error — do not put the database on a network
share and reach for it remotely.

If you want a remote consumer, that is what the hub's network backend and the admin API
are for.

### Backups

```sh
sqlite3 /var/lib/bnetcc/users.db ".backup /backup/users-$(date +%F).db"
```

Use `.backup` or `VACUUM INTO`, **not** `cp`. Copying the file while the server is running
gives you a database missing the WAL contents, which restores as a silently stale snapshot.

---

## 3. Do not point a website at the database

Direct database access is the right **debugging** tool and the wrong **integration** shape,
for two specific reasons rather than stylistic ones:

1. **It bypasses the attribute ACLs.** `System\Password Digest` is `Internal`/`Internal`,
   so no client-facing code path can reach it *by construction*. A dashboard with a
   database connection has no such constraint, and re-creates precisely the exposure that
   CVE-2004-2705 was — arbitrary attribute read including the password hash. Every place
   credentials can be read is a place they can leak.
2. **The schema is an internal detail.** It will change across migrations, and a site bound
   to it breaks each time. The API is the contract; the tables are not.

### What a site or monitor should consume instead

| Surface | For |
|---|---|
| **Prometheus metrics** on the admin listener | Dashboards, alerting, capacity. Connections by class and state, per-packet decode and error counters, outbound queue depth, login latency split by edge-verified and hub-proxied, channel fanout size. |
| **Read-only admin API** (JSON over HTTP) | A website: who is online, channel list and rosters, the game list, ladder standings, server status. Filtered through the same `AttrSchema` every other read path uses, so it cannot return anything a client could not see. |
| **`/debug/slow`** | The connections with the deepest outbound queues. When someone says "it's laggy", this should answer it in thirty seconds. |

The admin API is a first-class deliverable rather than an afterthought precisely because
the alternative — everyone querying the database directly — is how a schema becomes
impossible to change.

---

## 4. Which network backend

**Recommendation: PostgreSQL**, for the hub.

- The ladder is a ranking problem, and window functions make it one query instead of a
  full-table load. PvPGN's ladder rebuild calls `accountlist_load_all(ST_FORCE)` — every
  account into RAM, synchronously — which is exactly the shape a real database exists to
  avoid.
- Stronger concurrency story under mixed read/write, which is what a hub with a public
  website attached actually looks like.

**MariaDB/MySQL is a legitimate alternative** and a contained one: it is one crate
implementing the same trait, passing the same conformance suite. The honest argument for
it is ecosystem familiarity — PvPGN's SQL backend is MySQL-first, so most operators in
this community already run MariaDB and know how to back it up and monitor it. That is a
real advantage, not a sentimental one.

The argument against having *both* is maintenance: two backends means every schema change
is written twice and every conformance failure is debugged twice. **Pick one network
backend and support it properly.** The trait means the choice can be revisited later
without touching anything above it.

---

## 5. File descriptor limits

The node derives `max_connections` from `RLIMIT_NOFILE` and **logs the effective ceiling at
startup**. If it is below 2,000 it warns, loudly, with the fix.

| Platform | Raise it with |
|---|---|
| Linux (systemd) | `LimitNOFILE=65536` in the unit file |
| Linux (shell) | `ulimit -n 65536` |
| macOS | `ulimit -n`, and `sysctl kern.maxfilesperproc` for the process ceiling |
| Windows | No equivalent limit; watch ephemeral port exhaustion under load tests instead |

PvPGN ships a hard-coded `max_connections = 1000` and refuses connections past it in
silence. That is most of why people concluded it could not scale, and it is the reason this
number is logged rather than assumed.
