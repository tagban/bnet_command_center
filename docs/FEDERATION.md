# Command Center — Federation

There is **no working precedent for this** in the Battle.net emulator ecosystem. PvPGN has
a fire-and-forget UDP stats beacon to a master list (`tracker.cpp`) — a server *directory*,
not federation: no cross-server chat, no remote whisper, no shared channels. Atlas has
`docs/IPC Protocol.txt`, a genuine cross-implementation draft that was abandoned mid-word
with no handler ever written.

So we get to design it properly, and we have two good briefs: Atlas's IPC draft, and
RFC 2813 §6, in which IRC lists its own scaling regrets. We take the opposite of most of
them.

---

## 1. Topology: star, hub-authoritative

```
        node-eu ────┐
        node-na ────┤
        node-sa ────┼──── bnetcc-hub ──── Postgres
        node-asia ──┤        │
        node-bob ───┘        └── admin / web
```

Every node holds **one outbound mTLS connection** to the hub. Nodes never dial each other.

### Why not a full mesh (Atlas's choice)

- **NAT.** A community operator on a home connection or a cheap VPS behind NAT can hold an
  outbound connection to the hub but cannot reliably accept inbound from ten peers. Star
  topology means a node needs *no* inbound federation port at all. This single fact
  decides it — the population of people willing to host a node is much larger than the
  population who can configure inbound firewall rules correctly.
- **O(n²) links.** Atlas's draft has each node dial every other on `IPC_CLUSTER_MEMBER_JOIN`.
  That is fine at 5 nodes and unmanageable at 30.
- **Moderation.** A semi-trusted network needs one place that sees everything. That place
  is also the natural rate limiter.

### What we give up, and the mitigation

The hub is a single point of failure **for cross-node traffic only**. A node whose hub link
is down keeps serving its own users: local channels work, local game hosting works, existing
sessions continue, and cached-credential logins continue if the operator opted in. What
stops is cross-node chat, the federated game list, ladder writes, and new global bans.
This is stated as an explicit degradation mode in §7, not discovered in production.

Hub HA is deliberately deferred: active/standby over a shared Postgres with a virtual IP is
straightforward and is a phase-4 problem. Do not build a consensus protocol for a network
that will have twelve nodes.

---

## 2. Trust model

Nodes are **semi-trusted**. A node operator is assumed to be a real person who wants their
server to work, and *not* assumed to be honest under pressure. Concretely:

| Capability | Node | Hub |
|---|---|---|
| Accept client connections, run channels, host game ads | ✅ | — |
| Read account attributes for its own logged-in users | ✅ | authoritative |
| Verify SRP logons at the edge | ✅ (see §4) | issues verifier |
| Verify XSHA-1 logons | ❌ proxied | ✅ authoritative |
| Create accounts / change passwords | ❌ proxied | ✅ |
| Write to the global ladder | ❌ submits results | ✅ validates and applies |
| Apply a network-wide ban | ❌ requests | ✅ approves |
| Ban a user from its own server | ✅ | notified |
| See plaintext passwords | ❌ never | ❌ never (stores verifiers only) |

**The threat we actually defend against** is a node operator who wants free ladder rank,
wants to unban their friends network-wide, or wants to read other people's credentials.
The threat we do *not* defend against is a node operator griefing their own users — they
own that server, and users choose which node to connect to.

---

## 3. Transport

**TLS 1.3, mutual authentication, `rustls`.** No custom crypto. Atlas's draft used a
preshared key with SHA-1 challenge-response; that is 2003 thinking and we are not
repeating it.

- Each node has a long-lived **Ed25519 identity key**. The public key *is* the node id.
- Enrolment is a one-time bootstrap token issued by a hub admin (`bnetcc node invite`).
  The node presents it once, registers its public key, and the token burns.
- The hub pins node public keys; the node pins the hub's. A rotated key requires an admin
  action, never an automatic accept.
- Message framing: `u32 length (payload only, max 1 MiB) | u8 type | CBOR payload`.

CBOR rather than a fixed binary layout because independently-operated nodes will run
different versions for months at a time. Unknown fields are ignored; unknown message types
are ignored with a counter, not a disconnect. **Forward compatibility is a hard
requirement of a federated system**, and it is exactly what a hand-packed struct format
makes painful.

---

## 4. Identity — and the one asymmetry that shapes everything

Accounts are hub-central: one namespace, so a user is `Zealot` on every node and there is
no `user@server` display problem inside a 15-character name field. That was the right
call, but it collides with how Battle.net authentication actually works, in a way worth
being precise about.

### XSHA-1 products (StarCraft/BW, Diablo I/II, Warcraft II BNE)

`SID_LOGONRESPONSE2` proves knowledge of `h1 = XSHA1(lowercase(password))` by sending
`XSHA1(clientToken ‖ serverToken ‖ h1)`. To verify that, the server needs **`h1` itself**,
and `h1` is password-equivalent: anyone holding it can log in as that user anywhere.

If the hub shipped `h1` to nodes, **every node operator would hold a password-equivalent
secret for every user who ever logged into their node** — and, because the namespace is
global, could then impersonate that user on every other node.

So XSHA-1 logons are **proxied**: the node forwards `(username, clientToken, serverToken,
clientProof)` to the hub, the hub verifies against its stored `h1` and answers yes/no. One
hub round-trip per login (~1–30 ms depending on geography), and `h1` never leaves the hub.

### SRP products (Warcraft III)

Blizzard's NLS is an *augmented* SRP variant: the server stores `(salt s, verifier v)` where
`v = g^x mod N`. Knowing `v` lets you impersonate the **server**, but not the **client** —
recovering `x` from `v` is a discrete log. So a node can hold `(s, v)` and verify a WC3
logon entirely at the edge with no hub round-trip.

**This asymmetry is not a wart, it is the design.** It falls straight out of the crypto,
and it means the two product families get genuinely different login paths:

| | Verification | Hub RTT per login | Node holds |
|---|---|---|---|
| XSHA-1 (SC/BW, D1, D2, W2BN) | Hub-proxied | 1 | nothing |
| SRP/NLS (WC3) | Edge | 0 | `(s, v)`, cacheable |

### Offline mode (hub unreachable)

Per-node, opt-in, and honest about the cost:

- **SRP accounts**: keep working, no caveat. The node already has `(s, v)`.
- **XSHA-1 accounts**: only work if the operator enabled `offline_login = true`, which
  causes the hub to push `h1` for *recently-active-on-this-node* accounts only. Enabling it
  is a deliberate trade of credential exposure for availability, and `bnetcc` says so at
  the moment you enable it.
- Sessions established during a partition are flagged `unverified`. Their game results go
  to a **quarantine pool** and are only merged into the ladder after the hub reconciles.
  Ladder integrity is not sacrificed for availability.

---

## 5. Channels — authority and ordering

Three channel classes:

| Class | Authority | Visible on |
|---|---|---|
| **Local** | The node | That node only |
| **Federated** | The hub | Every node that has joined it |
| **Official** | The hub | Every node, auto-joined, operator cannot delete |

For a federated channel the **hub owns the roster and the operator state**, and assigns a
**monotonic sequence number to every channel event**. Nodes hold a mirror and render
events in sequence order.

This matters far more than it looks, and it is the reason warnet mode is buildable. When
two bots on two different nodes race for operator status, "who got there first" must have
exactly one answer, and every observer must see the same one. A single sequencer gives you
that for free. An eventually-consistent replicated roster gives you channel wars whose
outcome depends on which node you were watching from — which, for a warnet, is the entire
product being broken.

### Two ordering modes

| Mode | Behaviour | Cost | Default for |
|---|---|---|---|
| `LocalFirst` | Node echoes local users immediately, forwards to hub for cross-node fanout. Local users see their own channel with sub-millisecond latency; global order may differ slightly between nodes. | 0 RTT local | Gaming |
| `HubSerialized` | Every channel event round-trips the hub sequencer before any user sees it. Identical ordering everywhere. | 1 RTT (~10–60 ms) | Warnet |

Selected per channel, defaulted by server mode. A gaming server does not need global chat
ordering; a warnet needs nothing else.

### What we deliberately do *not* replicate

RFC 2813 §6 names IRC's own failures: every server holding full network state, N²
consistency algorithms, races from propagation delay, and a flat label space guaranteeing
collisions. We avoid all four:

- Nodes hold **rosters only for channels they have users in** — not global user state.
- Presence lookups (`/whois`, friend lists) are **query-on-demand with a correlation
  cookie** — Atlas's IPC draft got this instinct right — not eagerly replicated.
- Single namespace via hub-central accounts kills the collision class outright. No `KILL`,
  no nick delay, no split-brain duplicate names.
- Netsplit is explicit: on link loss the node synthesizes `EID_LEAVE` for every remote user
  in its federated channels, so no ghosts remain. On reconnect the hub sends a roster
  snapshot with a fresh sequence base, and the node reconciles.

---

## 6. Games, ladder, bans

### Federated game list

Game advertisements are directory entries (`host IP:port`, game name, statstring, flags).
The node forwards its ads to the hub; the hub merges and serves the network-wide list back.

**This works because Battle.net joins are peer-to-peer.** `SID_GETADVLISTEX` hands the
client a `sockaddr_in` and the client dials the host directly — Storm UDP for SC/BW/D1/W2,
TCP for WC3. The server was never in the game data path, so a game hosted by a user on
node A is joinable by a user on node B exactly as well as by a user on node A.

The honest caveat: **exactly as well** includes NAT. A host behind an unforwarded NAT is
unreachable from anywhere, federated or not. Federation neither helps nor hurts. What we
add is a hub-side reachability probe — the hub attempts a connection to each advertised
endpoint and flags ads that fail, so unjoinable games can be de-prioritised in the list
instead of wasting everyone's time. PvPGN never did this and "the game list is full of
dead games" is a perennial complaint.

**Diablo II closed realms are not federated.** A character lives in exactly one realm's
database, and the client connects to a specific D2GS. The realm *menu* can be shared across
nodes, so a user sees all realms and picks one; the characters do not follow. Pretending
otherwise would require replicating character save data, which is a distributed database
problem we are not going to solve for this.

### Ladder — semi-trusted means results are validated, not trusted

A node submits `GameResultReport`. The hub applies, in order:

1. **Session attestation.** Every claimed participant must have had a hub-observed session
   on that node during the claimed window. A node cannot report results for players who
   were never there.
2. **Plausibility.** Duration bounds, participant count bounds, no self-play, no result
   for a game that was never advertised.
3. **Rate limits**, per node and per account. A node reporting 400 games/minute is either
   compromised or broken; either way it gets throttled and flagged.
4. **Per-node reputation.** Anomaly rates accumulate. A node past threshold has its results
   quarantined pending admin review rather than silently dropped, so a false positive is
   recoverable.

None of this is exotic; it is just the work that has to exist the moment a stranger's
server can write to your ranking table.

### Bans

Three scopes, and the distinction is enforced:

- **Node ban** — the node's own decision, applied locally, reported to the hub for the
  audit log. No approval needed.
- **Network ban** — hub-applied, pushed to every node as a delta. A node can *request* one
  with evidence; a hub moderator (or a node above a trust threshold, if the operator
  configures it) approves.
- **IP / range ban** — same two scopes. Network-scope IP bans need approval because a
  malicious node could otherwise ban a competitor's whole subnet.

---

## 7. Partition behaviour, stated up front

| Failure | Node behaviour | User-visible |
|---|---|---|
| Hub unreachable | Local channels, local game list, existing sessions all continue. SRP logins continue. XSHA-1 logins only with `offline_login`. | "Network features unavailable" broadcast; federated channels show only local users. |
| Node disconnects from hub | Hub synthesizes departure for that node's users in every federated channel; withdraws its game ads. | Remote users see clean `EID_LEAVE`, no ghosts. |
| Node reconnects | Hub sends roster + sequence base; node reconciles and re-advertises. | Users reappear. |
| Hub restarts | Nodes reconnect with backoff (1s → 60s, jittered). Session state rebuilt from node reports. | Brief cross-node gap. |
| Clock skew between nodes | All ordering is by hub sequence number, never by timestamp. | Nothing. |

The last row is deliberate. Nothing in this design orders anything by wall-clock time,
because a federation of volunteer-run servers will absolutely contain a machine whose clock
is forty seconds off.

---

## 8. Message set (v1)

| Type | Direction | Purpose |
|---|---|---|
| `Hello` / `Welcome` | N→H / H→N | Version, node id, capabilities, policy snapshot |
| `Ping` / `Pong` | both | Liveness + RTT measurement (feeds the ordering-mode decision) |
| `AuthVerify` / `AuthVerdict` | N→H / H→N | XSHA-1 proxied login |
| `CredentialFetch` / `CredentialBundle` | N→H / H→N | SRP `(s, v)` for edge verification |
| `AccountCreate` / `AccountResult` | N→H / H→N | Registration, password change |
| `SessionOpened` / `SessionClosed` | N→H | Presence; feeds ladder attestation |
| `ChannelJoin` / `ChannelLeave` | N→H | Roster mutation request |
| `ChannelEvent` | both | Chat, emote, op change, kick — carries hub sequence number |
| `ChannelRoster` | H→N | Snapshot on join or reconnect |
| `UserQuery` / `UserResult` | N→H / H→N | `/whois`, friends — cookie-correlated, on demand |
| `GameAdvertise` / `GameWithdraw` | N→H | Game ad lifecycle |
| `GameListQuery` / `GameList` | N→H / H→N | Federated list, with reachability flags |
| `GameResultReport` | N→H | Ladder submission (validated, §6) |
| `BanRequest` / `BanDelta` | N→H / H→N | Moderation |
| `PolicyPush` | H→N | Mode changes, limits, official channel list |

All request/response pairs carry a `u64` cookie. All messages carry the node id implicitly
from the authenticated connection — never from a field in the payload, because a field can
be forged and a TLS client certificate cannot.

---

## 9. Hub capacity

The hub is not on the game data path and not on the local chat path. Its load is:

- One long-lived TLS connection per node (tens, not thousands).
- One `AuthVerify` per XSHA-1 login. At 20 nodes × 2,000 users with a 30-minute average
  session, that is ~22 logins/second — trivial.
- Channel event relay. A busy federated channel is perhaps 5 messages/second; a hundred
  such channels is 500/second in, fanned out to the subset of nodes that have members.
- Ladder writes, which are batched.

A single hub on modest hardware handles this by a wide margin. The hub's real constraint is
**Postgres write throughput for the ladder and audit log**, which is why those are batched
and why the ladder is a SQL aggregate rather than PvPGN's load-every-account-into-RAM
rebuild.
