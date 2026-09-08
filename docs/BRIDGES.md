# Bridges — chat from outside Battle.net

Letting people join a Battle.net channel from Discord, from a Ragnarok Online guild chat,
from an FFXI Windower addon, or from anything else.

There is prior art for exactly one shape of this — [goop](https://github.com/nielsAD/goop),
a Go daemon bridging CAPI, Discord and BNCS — and it works by being an *external relay*
that logs in as a bot. That is the obvious design and it is the one worth not repeating.

---

## 1. The decision that shapes everything: presences, not relayed text

The obvious bridge logs in as one bot account and posts:

```
<Discord> alice: anyone up for 2v2?
```

It takes an afternoon to build and it is wrong in a way you cannot fix later. That line is
*text*, so to the server there is one user in the channel, named `Discord`. Which means:

- The user list shows one entry, no matter how many people are on the other side.
- `/whois alice` fails. `/squelch alice` fails. Whispering alice is impossible.
- An operator cannot kick or ban alice; they can only kick the entire bridge.
- Flood control applies to the bridge, so one chatty Discord user throttles everyone.
- On a warnet, alice cannot be given operator, and the bridge holding op on behalf of
  fifty people is not a thing the op rules can express.

So a bridged user is a **first-class presence** in the channel: it has a name, an account
id, chat flags, a roster entry, and it can be whispered, squelched, kicked and banned like
anyone else. Everything downstream — federation, moderation, the operator rules in
`WARNET.md` — then works without knowing bridges exist. That is the whole point: the
bridge is a *transport*, not a special case in the channel model.

The cost is that the hard problems move to the edge, where they can be solved once:
naming, encoding, and rate limiting. Sections 3–5.

---

## 2. Two transports, one model

The bridge API needs to serve two very different clients, and the split maps neatly onto
your other request for better telnet:

| Transport | For | Why |
|---|---|---|
| **JSON over WebSocket** (`wss://`) | Discord bots, web integrations, anything modern | Structured, easy to version, trivial from any modern language |
| **Extended line protocol** on the chat gateway | Game addons — FFXI Windower (Lua), RO plugins, embedded things | Many game addon environments give you a raw TCP socket and string handling and nothing else. Asking for a WebSocket client with TLS is asking them not to build it. |

Both speak the same message model; only the framing differs. The line protocol is a
**superset of the classic telnet gateway**, negotiated by an opt-in capability handshake,
so a 1998 bot connecting to the same port still works unchanged:

```
> 0x03                                  classic protocol selector
> CAPS BRIDGE/1 UTF8 PRESENCE
< 1200 CAPS BRIDGE/1 UTF8 PRESENCE      server confirms what it will honour
```

A bot that never sends `CAPS` gets exactly the 1998 behaviour. This is how the gateway gets
better without breaking the thing it is compatible with.

### Modelled on CAPI, without its ceiling

Blizzard's Classic Chat API is the right *shape* — a JSON envelope with `command`,
`request_id`, `payload`, and a `status` of `{area, code}`; a numeric `user_id` as the stable
handle with a separate display name; server events as `*EventRequest` notifications. We
copy that shape because it is sane and because anyone who has written a CAPI bot will
recognise it.

We do not copy its limits, which are implementation accidents rather than design: one key,
one channel, no game listings, no clan or friends visibility, and no kick/ban reasons. A
Cairn bridge token can be scoped to several channels, and bridged users can see the game
list.

Two things to steal exactly:

- **Derived, prefixed bot identity.** CAPI names a bot `[B]` + the registering account,
  lowercased. The bot cannot choose its own name, so bot presence is unspoofable and
  visible to every legacy client with no protocol change at all. §3 applies the same idea
  to bridges.
- **`user_id` as the handle, display name as a separate field.** Renames then cost nothing,
  and moderation targets survive them.

One CAPI trap worth writing down because it will bite anyone implementing against the
published spec: the documented `flags` and `attributes` payload keys are actually **`flag`
and `attribute`** on the wire, and `attribute` is an array of `{key, value}` objects rather
than a map. Two independent implementations disagree with Blizzard's own PDF. Accept both
spellings.

---

## 3. Naming — the part that will generate the most argument

Battle.net names are **15 characters, ASCII**. Discord names are up to 32 and admit
characters the protocol cannot carry. Something has to give, visibly and predictably.

Every bridge has a short **prefix tag** of 1–3 characters:

```
D:alice          a Discord user
RO:Bahamut       a Ragnarok Online guild member
FF:Tarutaru      an FFXI player
```

The rules:

- **The prefix namespace is reserved.** A real account cannot register a name containing
  the bridge separator. This is what makes bridged identity unspoofable — the same
  property CAPI gets from `[B]`, generalised. Without it, someone registers `D:alice` and
  impersonates a Discord user to everyone in the channel.
- **Names are derived, never chosen by the bridge.** The bridge supplies its remote user
  id and display name; the server derives the Battle.net name. A bridge cannot mint
  arbitrary identities, which matters because a bridge token is a delegation of trust.
- **Transliterate, then truncate.** Strip anything below `0x20` (a relayed CR or LF makes
  the *receiving* client disconnect and IP-ban us for five minutes — see
  `PROTOCOL-NOTES.md` §4), map to the channel's encoding, then truncate to fit the prefix
  plus 15 characters.
- **Collisions get a numeric suffix**, matching Battle.net's own `#2` convention for
  duplicate logins rather than inventing a new one.
- **The mapping is stable and stored.** `D:alice` is always the same Discord account id,
  across renames on either side and across restarts. Moderation depends on this: a ban
  must survive the user changing their Discord nickname.

Bridged users carry a distinguishing chat flag so clients and bots can tell them apart
without parsing names. There is no spare documented flag bit, so this is server-side
metadata surfaced through `/whois` and the bridge API rather than a new wire flag — inventing
a flag value the client does not know would produce undefined rendering.

---

## 4. Encoding, length, and the things that silently break

| Constraint | Consequence for a bridge |
|---|---|
| Chat text is **223 usable bytes** including the terminator | A 2,000-character Discord message is nine-plus lines. Split on word boundaries with a continuation marker, and count every fragment against the rate limit. |
| Encoding is **UTF-8 for `STAR`/`SEXP`/`SSHR`/`JSTR`, ISO 8859-1 otherwise** | The same message must be encoded differently per recipient product. Characters that do not map get transliterated, not dropped silently. |
| **Control bytes are fatal** | Already handled centrally in `sanitize_chat_text`, but a bridge is the most likely source of them, since remote platforms permit newlines in a single message. |
| Battle.net has **no message edit or delete** | Configurable: ignore edits, or post `(edited) …`. Deletes cannot be retracted; the honest default is to ignore them and say so in the bridge's documentation, because pretending otherwise misleads people about what the other side saw. |
| Attachments and embeds do not exist | Post the URL. Nothing else is possible. |
| Mentions are platform-specific | Render `@alice` as plain text; do not attempt to resolve cross-platform mentions in v1. |

---

## 5. Rate limiting, and the failure it prevents

**Flood control is per bridged identity, not per bridge connection.** One connection
carries many users; charging them all to one bucket means a single chatty Discord user
mutes the whole bridge, which reads as "the bridge is broken" to fifty people.

So a bridge gets:

- A per-identity bucket, using the same `FloodTracker` as everyone else.
- A per-bridge aggregate ceiling, so a compromised or looping bridge cannot saturate a
  channel even if every identity is individually within budget.
- Message *fragments* counted individually, so splitting a long message is not a way
  around the limit.

The penalty for a bridge is always **drop the message and report it**, never disconnect —
same reasoning as the chat gateway. A dropped bridge reconnect-storms and takes everyone
with it.

---

## 6. Loop prevention

Two bridges into one channel — a Discord bridge and an RO bridge — will ping-pong forever
unless this is designed in rather than patched later.

Every message carries an **origin token** identifying the bridge and remote message id that
produced it. A bridge never receives an event whose origin is itself, and the server drops
any message whose origin chain already contains the bridge it is about to be sent to. The
chain is bounded; a message that has traversed more than a small number of bridges is
dropped and counted, because at that point something is misconfigured.

This has to be in the event model from the first version. Retrofitting loop detection onto
a deployed bridge protocol means every existing bridge is now wrong.

---

## 7. Moderation and trust

A bridge token is a **delegation**: it speaks for many identities. So:

- Tokens are **scoped to named channels**. A bridge cannot join a channel it was not
  granted.
- A bridge **cannot claim operator**. Its users can hold operator only if a channel
  operator grants it explicitly, exactly like anyone else.
- A bridged user **cannot moderate real users** by default. Operator actions taken by a
  bridged identity are a per-bridge setting and off unless enabled.
- **Bans are enforced server-side, not by the bridge.** The server drops a banned bridged
  user's messages whether or not the bridge honours the `UserBanned` event. Enforcement
  that depends on the untrusted side co-operating is not enforcement.
- Bridge activity is in the audit log with its origin token, so "who relayed this" is
  answerable.

In a federation, a bridge attaches to a node, and its users appear network-wide through
the ordinary roster — the hub sequences their events like anyone else's. Where possible a
bridge should attach at the channel's authority node, since attaching elsewhere adds a
second hop to every message for no benefit.

---

## 8. Message set (v1 sketch)

Envelope, CAPI-shaped:

```json
{ "command": "Bridge.SendMessageRequest",
  "request_id": 17,
  "payload": { "channel": "op clan xyz", "user": "u_9f3", "message": "hi" } }
```

| Command | Direction | Purpose |
|---|---|---|
| `Bridge.Authenticate` | B→S | Token; returns the granted channel scopes |
| `Bridge.AttachChannel` / `DetachChannel` | B→S | Join or leave a granted channel |
| `Bridge.UpsertUser` | B→S | Declare a remote identity: remote id, display name, presence. Returns the derived Battle.net name and account id |
| `Bridge.RemoveUser` | B→S | Remote user left |
| `Bridge.SendMessage` / `SendEmote` | B→S | Speak as one of your identities |
| `Bridge.SendWhisper` | B→S | Private message to a channel member |
| `Bridge.MessageEvent` | S→B | Channel talk, emote, whisper, server info/error |
| `Bridge.UserJoinEvent` / `UserLeaveEvent` | S→B | Roster changes, including other bridges' users |
| `Bridge.UserUpdateEvent` | S→B | Flags changed, e.g. someone gained operator |
| `Bridge.ModerationEvent` | S→B | One of your users was kicked, banned or squelched |
| `Bridge.RateLimitEvent` | S→B | A message was dropped, and which identity was responsible |

Every message-bearing command carries the origin token from §6.

---

## 9. Build order

Bridges are a phase-3 item, after federation, for one reason: a bridged user is a channel
presence, and the channel model has to be finished and federated first or bridges will be
built against a moving target.

1. **The presence model.** Virtual accounts, reserved prefix namespaces, stable identity
   mapping, per-identity flood control. No transport yet — this is all `cairn-core`, and
   all testable without a socket.
2. **The extended line protocol**, since it is the smaller transport and it doubles as the
   telnet-gateway improvement. Capability handshake, UTF-8 negotiation, presence commands.
3. **The WebSocket transport**, once the model has been exercised by a real client.
4. **A Discord bridge as the reference implementation** — in-tree, because a bridge API
   with no first-party consumer drifts. It is also the one that will surface the encoding
   and length problems fastest, since Discord permits everything Battle.net forbids.
5. **A documented minimal example** for game addons — the smallest possible Lua client, so
   the FFXI and RO cases have a starting point rather than a specification.
