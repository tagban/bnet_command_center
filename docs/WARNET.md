# Warnet mode

A **warnet** is a server run for channel warring — bots contesting operator status in
channels — rather than for playing games. It is a different product wearing the same
protocol, and the subsystem on the critical path is completely different:

| | Gaming server | Warnet |
|---|---|---|
| Hot path | Game advertisement and list | Channel event ordering |
| Latency that matters | Game list refresh | Op acquisition, to the millisecond |
| Dominant client | Game clients | Bots on the chat gateway |
| Failure that ruins it | Dead games in the list | Two observers disagreeing on who got op |

So warnet is not a feature flag over a gaming server. It is a mode that changes which
guarantees the server is trying to provide.

---

## 1. The mode switch

```toml
[server]
mode = "gaming"   # "gaming" | "warnet" | "both"
```

Set on the hub and pushed to nodes via `PolicyPush`; a node may narrow it further but
never widen it. A hub in `warnet` mode cannot have a node quietly re-enabling game hosting.

### What each mode gates

| Subsystem | `gaming` | `warnet` | `both` |
|---|---|---|---|
| `SID_STARTADVEX3` (host a game) | allowed | **refused**, status `0x02` "selected game type is currently unavailable" | allowed |
| `SID_GETADVLISTEX` (list games) | full list | **empty**, status `0x01` | full list |
| `SID_STOPADV` | normal | accepted and ignored | normal |
| `SID_QUERYREALMS2` / D2 realm | realm list | **count 0** | realm list |
| `SID_NOTIFYJOIN` | normal | ignored | normal |
| W3 route listener (`:6200`) | on | **not bound** | on |
| MCP / D2CS listener | on | **not bound** | on |
| Channel ordering | `LocalFirst` | **`HubSerialized`** | per-channel |
| Chat-gateway connections | tight limits | **relaxed, per-account** | per-class |
| Flood control | human-tuned | **bot-tuned** | per-class |
| Ladder | game results | **channel/war stats only** | both |

Refusing with the game's own documented status code rather than silently dropping matters:
`0x02` produces a real in-client message. A client that gets no response just hangs, and
the user blames the server.

---

## 2. Connection limits — resolving the tension in your requirement

You asked for the telnet/chat gateway to be limited to **one connection per IP**. That is
exactly right for a gaming server, where a chat-gateway connection is a bot and one bot per
IP is a reasonable anti-abuse default.

It is exactly wrong for a warnet, where the *entire point* is that operators run fleets of
bots, frequently from one box. A hard 1-per-IP cap makes a warnet unusable.

So the limit is a policy with mode-dependent defaults, not a constant:

```toml
[limits.chat_gateway]
per_ip       = 1      # gaming default
per_account  = 1
global       = 256

[limits.chat_gateway.warnet]   # applied when mode = "warnet"
per_ip       = 16
per_account  = 4
global       = 2048
allowlist    = []     # IPs or CIDRs exempt from per_ip, for known bot hosts
```

`per_account` is the more meaningful control in a warnet: it limits how much presence one
*identity* can project regardless of how many addresses they have, which is what you
actually want to bound. IP limits only inconvenience the honest.

Recommended warnet posture: modest `per_ip`, strict `per_account`, and an explicit
allowlist for known bot hosts — plus a **registered-bot flag** on the account, so a bot
account is a first-class thing the server knows about rather than a human account behaving
oddly.

---

## 3. Operator semantics — the part that has to be exactly right

Channel wars are decided by op acquisition, so these rules are the product. They are
stated here because "whatever the code happens to do" is not good enough when people are
competing over the outcome.

- **First occupant gets operator.** The first account to join an empty, non-official
  channel receives flag `0x02` (Channel Operator).
- **Op does not transfer on leave** unless a designated heir is present. If the sole
  operator leaves with no heir, the channel has no operator until it empties and is
  recreated.
- **`/designate <name>`** nominates an heir. The heir must be in the channel at the moment
  the operator leaves; otherwise the designation lapses. Designation is per-channel and
  clears on channel destruction.
- **Op is per-channel, never global.** A `Battle.net Administrator` (`0x08`) is a separate
  thing and is a server-staff flag.
- **`/ban`, `/kick`, `/squelch`** are operator actions; `/squelch` is per-account, matching
  real Battle.net's behaviour after they moved it off per-IP.
- **Rejoin does not restore op.** A kicked or departed operator returns as a normal user.
- **The channel is destroyed when the last user leaves**, and its ban list and designations
  go with it, unless it is an official (hub-owned) channel.

Every one of these is a hub-sequenced event in a federated channel, so the answer to "who
got op" is a single number, identical on every node.

---

## 4. Ordering and fairness

In `HubSerialized` mode every channel event is assigned a monotonic sequence number by the
hub before any user sees it. That buys identical ordering everywhere — the guarantee a
warnet actually needs — at the cost of one hub round-trip.

**The honest problem this creates:** a node with 5 ms to the hub beats a node with 90 ms,
every time, deterministically. Federation turns network position into a competitive
advantage in a way a single server never did.

Three options, and the choice is the operator's:

1. **Accept it.** Defensible: on real Battle.net, ping mattered, and everyone understood
   that. Simple, predictable, no machinery. This is the default.
2. **Fairness window** (`arrival_jitter_window_ms`, default off). The hub buffers competing
   events for a fixed window and orders them randomly within it rather than by arrival.
   Blunts the latency advantage; adds that window to *everyone's* latency; makes outcomes
   non-deterministic by design, which some communities will hate and others will consider
   the only fair option.
3. **Per-node handicap.** Hub measures each node's RTT continuously and offsets ordering to
   compensate. Fairest in principle, most gameable in practice — a node that inflates its
   measured RTT gets a head start. Not recommended without signed timing, which is more
   machinery than this deserves.

Ship (1), implement (2) behind a config flag, document (3) as rejected and why.

**Non-federated channels are unaffected.** A channel local to one node is ordered by that
node with zero added latency, which is the right home for a serious competitive war
between bots that are all on the same server anyway.

---

## 5. Flood control

Bots legitimately send far faster than humans, so a single flood policy cannot serve both.
Policy is per connection class:

```toml
[flood.game_client]
messages_per_10s = 12
burst            = 5
penalty          = "mute_30s"

[flood.chat_gateway]
messages_per_10s = 60
burst            = 20
penalty          = "drop_message"     # never disconnect a bot for pace alone

[flood.chat_gateway.warnet]
messages_per_10s = 200
burst            = 60
penalty          = "drop_message"
```

Two rules learned from how real Battle.net got this wrong:

- **A dropped message must be observable.** Real Battle.net's anti-spam silently discards
  messages with no notification, which is maddening to debug. We emit `EID_ERROR` to the
  sender on drop, and count it in metrics.
- **Never disconnect for pace alone** on the gateway. Dropping the message applies
  backpressure; dropping the connection makes a bot reconnect-storm, which is worse for
  everyone.

`SID_FLOODDETECTED` (`0x13`) exists in the protocol and is sent to game clients before a
flood disconnect, so the client shows the right message.

---

## 6. The chat gateway itself

Protocol bytes `0x03` (and the `0x43` / `0x63` aliases Atlas also accepts). Line-oriented:
`<4-digit-id> <NAME> [data]`, CRLF-terminated, user flags as 4-character zero-padded hex.
Blizzard deprecated it in 2005 and disabled it on official servers; on a private server it
is alive and it is the natural bot interface.

Two implementation notes that are easy to get wrong and expensive to get wrong:

- **Read with a line codec over a growable buffer.** PvPGN reads the chat gateway *one byte
  per event-loop pass* — a 200-character line costs 200 syscalls and 200 full trips through
  the loop. On a warnet, where the gateway is the primary interface rather than an
  afterthought, that is the difference between working and not.
- **Cap the line length** (default 1024 bytes) and drop the connection past it. An
  unbounded line buffer on an unauthenticated socket is a memory-exhaustion primitive.

The modern alternative, Blizzard's CAPI (JSON over secure WebSockets), is chat-only,
key-gated, and not applicable to a private server — but its message shape is a reasonable
model if you later want a cleaner bot API than the 1998 line protocol. That would be an
additive gateway, not a replacement.
