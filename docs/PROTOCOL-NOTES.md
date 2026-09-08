# Protocol notes

Implementation reference, distilled from [BNETDocs](https://bnetdocs.org/) and cross-checked
against Atlas (MIT) and gowarcraft3. **Confidence is marked**: ✅ verified against two
sources, ⚠️ single source or derived, 🛑 unknown — do not guess in code, capture it.

---

## 1. Framing

Four different framings live in this protocol family, and mixing them up is the classic bug.

```
BNCS   :  FF | id:u8 | len:u16le        len INCLUDES the 4-byte header   ✅
MCP    :       len:u16le | id:u8        len INCLUDES header, NO 0xFF,
                                        length comes FIRST                ✅
W3GS   :  F7 | id:u8 | len:u16le        len includes header               ✅
BNFTP  :       len:u16le | …                                              ⚠️
Chat   :  line-oriented, CRLF, "<4-digit-id> <NAME> [data]"               ✅
```

**All numeric Battle.net types are little-endian.** A zero-payload BNCS message is
`FF 02 04 00`.

Types: `u8/u16/u32/u64`, `STRING` = NUL-terminated bytes, `STRINGLIST` = STRINGs terminated
by an empty item, `BOOL` = 1 or 4 bytes depending on packet, `FILETIME` = 8-byte Windows
FILETIME, `VOID` = untyped blob.

⚠️ **Four-character codes are `u32`s, so they appear reversed on the wire**: `STAR` is
`52 41 54 53`. BNETDocs states the endianness rule and states the codes but never spells out
the reversal; it follows from the rule. Verify against a capture before shipping.

---

## 2. Protocol selector (first byte of a new TCP connection)

| Byte | Meaning | Source |
|---|---|---|
| `0x01` | Game — BNCS binary. **Also used for the MCP/realm connection.** | ✅ |
| `0x02` | BNFTP — file transfer (patch MPQs, `icons.bni`, `tos.txt`, ad images) | ✅ |
| `0x03` | Telnet / chat gateway | ✅ |
| `0x43`, `0x63` | Chat gateway aliases (Atlas accepts these) | ⚠️ Atlas only |
| `0x04` | MCP → BNCS interserver | ⚠️ BNETDocs only |
| `0x80` | Atlas IPC (its unimplemented federation draft) | ⚠️ Atlas only |
| `0x06`, `0x81` | Unknown | 🛑 |

Anything else: close without responding.

---

## 3. Which client uses which login flow

| Product | Version check | Account logon | Password crypto |
|---|---|---|---|
| StarCraft / Brood War 1.16.1 (`STAR`/`SEXP`) | `0x50`/`0x51` AUTH_INFO/CHECK | `0x3A` LOGONRESPONSE2 | XSHA-1 double hash |
| SC Shareware (`SSHR`), Japanese (`JSTR`) | `0x06`/`0x07` old | `0x29` LOGONRESPONSE | XSHA-1 double hash |
| Warcraft II BNE (`W2BN`) | `0x06`/`0x07` old ⚠️ | `0x29` LOGONRESPONSE | XSHA-1 double hash |
| Diablo I retail/shareware (`DRTL`/`DSHR`) | `0x06`/`0x07` old | `0x29` LOGONRESPONSE | XSHA-1 double hash |
| Diablo II / LoD (`D2DV`/`D2XP`) | `0x50`/`0x51` | `0x3A`, then `0x40`/`0x3E` realm | XSHA-1 double hash |
| Warcraft III RoC/TFT (`WAR3`/`W3XP`) | `0x50`/`0x51` **+ 128-byte RSA sig** | `0x53`/`0x54` NLS/SRP | SRP, standard SHA-1 |

⚠️ W2BN appears in BNETDocs' old-logon list but PvPGN also drives it through `SID_AUTH_INFO`
in some configurations. Treat old-logon as canonical and support both.

Version bytes: `D2DV`/`D2XP` = `0x0E`, `WAR3`/`W3XP` = `0x1E`, `W2BN` = `0x4F`,
`DRTL`/`DSHR` = `0x2A`, `SSHR` = `0xA5`, `JSTR` = `0xA9`. STAR/SEXP are version-dependent.

### XSHA-1 double hash (`SID_LOGONRESPONSE2`, `0x3A`)

```
h1   = XSHA1(lowercase(password))
sent = XSHA1(clientToken ‖ serverToken ‖ h1)      // 4 + 4 + 20 = 28 bytes, LE tokens
```

Account creation (`SID_CREATEACCOUNT2`, `0x3D`) hashes the lowercased password **once**, not
twice. Usernames longer than 15 characters are truncated.

Status codes: `0x00` success · `0x01` no such account · `0x02` wrong password ·
`0x03` account corrupted (D2 only) · `0x06` account closed (+ reason string). An unknown
status makes the client show a default error and disconnect.

### XSHA-1 itself

Implemented and known-answer-tested in `cairn-crypto`. It differs from real SHA-1 in three
ways, and the first one is the actual "break":

1. **Message expansion has its operands swapped.** Standard SHA-1 computes
   `w[i] = ROL(w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16], 1)`. Blizzard's computes
   `w[i] = ROL(1, (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]) % 32)` — rotating the *constant 1*
   by the xor, rather than the xor by 1. This destroys essentially all of the diffusion the
   expansion is supposed to provide.
2. **Little-endian word interpretation** (standard SHA-1 is big-endian).
3. **No length padding.** The input is copied into a zeroed buffer; no `0x80` terminator, no
   length appended. **Only the first 64 bytes of input affect the digest** — a password
   longer than 64 characters is silently truncated.

The round functions and constants are otherwise standard SHA-1.

Verified vectors (as `u32` words, the form the reference prints):

```
xsha1("")            = 4d3aa0ee 94261d5a 584a6f57 6b8d9960 1546c680
xsha1("a")           = fe442493 6dc20078 a0339551 59f82303 6e513f13
xsha1("1234567890")  = 99f0fab8 b5b4523e 0d58e5ef e126fa5f 12633b4b
xsha1("password")    = 1d0dc8ec c058e776 258cdab9 ff6a10ff 1629248e
xsha1(64 × 'A')      = 193612de 92e1b17d 1d66d2f3 0fbb6387 177ecd60
```

Full double-hash chain, `ct=0xDEADBEEF`, `st=0x12345678`, password `"password"`:
`7488ad2d 82dc91a2 8aa43a7c 8d596822 a2920091`.

On the wire each word is written little-endian, so the digest bytes are
`ec c8 0d 1d …` for `xsha1("password")`.

### NLS / SRP (WarCraft III)

Blizzard's "New Logon System" is a **modified SRP-6, not stock SRP-6a**. Deviations:

- `x = H(s, H(C, ":", P))` with username and password **uppercased** — the username is
  folded into `x`.
- An extra term `I = H(g) XOR H(N)` is folded into `M1`.
- `M1 = H(I, H(C), s, A, B, K)` rather than `H(A, B, K)`.
- `u` = **first 4 bytes of `H(B)`**, computed by the client, not transmitted.
- `K` is derived by an **odd/even byte split**: split `S` into even-index and odd-index byte
  buffers, SHA-1 each independently, then interleave the digests back together (40 bytes).
- `x` is converted to an integer **little-endian**.
- Hash is **standard SHA-1** throughout (not XSHA-1).

Parameters: `g = 47` (`0x2F`); `N` (256-bit) =
`0xF8FF1A8B619918032186B68CA092B5557E976C78C73212D91216F6658523C787`.
⚠️ **NLS version 1 reverses the byte order of `N`** relative to version 2.

Server stores `(salt s, verifier v = g^x mod N)`. Because `v` does not permit client
impersonation, it can safely be cached at federation nodes — see `FEDERATION.md` §4.

---

## 4. Chat

`SID_ENTERCHAT` (`0x0A`) → `SID_JOINCHANNEL` (`0x0C`) → `SID_CHATCOMMAND` (`0x0E`) in,
`SID_CHATEVENT` (`0x0F`) out.

Join flags: `0x00` NoCreate · `0x01` First join (server picks home channel, sends MOTD) ·
`0x02` Forced join (no MOTD; banned users land in "The Void") · `0x05` D2 first join.
Unrecognised values are treated as NoCreate. Joining your current channel returns you to
your *previous* channel.

**Event IDs:** `0x01` SHOWUSER · `0x02` JOIN · `0x03` LEAVE · `0x04` WHISPER · `0x05` TALK ·
`0x06` BROADCAST · `0x07` CHANNEL · `0x09` USERFLAGS · `0x0A` WHISPERSENT ·
`0x0D` CHANNELFULL · `0x0E` CHANNELDOESNOTEXIST · `0x0F` CHANNELRESTRICTED · `0x12` INFO ·
`0x13` ERROR · `0x15` IGNORE · `0x16` ACCEPT · `0x17` EMOTE.
🛑 `0x08`, `0x0B`, `0x0C`, `0x10`, `0x11`, `0x14` are undocumented — treat as reserved, do
not assume they are unused.

**User flags:** `0x01` Blizzard Rep · `0x02` Channel Operator · `0x04` Channel Speaker ·
`0x08` Battle.net Administrator · `0x10` No UDP Support · `0x20` Squelched ·
`0x40` Special Guest · `0x100` Beep Enabled.

**Channel flags:** `0x0001` Public · `0x0002` Moderated · `0x0004` Restricted ·
`0x0008` Silent · `0x0010` System · `0x0020` Product-Specific · `0x1000` Globally Accessible ·
`0x4000` Redirected · `0x8000` Chat · `0x10000` Tech Support.

### Hard limits — enforce these, clients depend on them

- **Chat text: 224 bytes including the NUL** (223 usable). Max 255 in the field, but official
  clients restrict to 224.
- **Channel name: 31 characters**, trimmed beyond.
- **Username: 15 characters**, truncated beyond.
- **Encoding: UTF-8 for `STAR`/`SEXP`/`SSHR`/`JSTR`; ISO 8859-1 for everything else.**
- **A `\n` or `\r` in `SID_CHATCOMMAND` causes real Battle.net to disconnect and IP-ban for
  5 minutes.** Strip control bytes below `0x20` on ingress; do not propagate them.

---

## 5. Games

`SID_STARTADVEX3` (`0x1C`) to host, `SID_STOPADV` (`0x02`, no payload) to withdraw,
`SID_GETADVLISTEX` (`0x09`) to list, `SID_NOTIFYJOIN` (`0x22`) on join.

**Joins are peer-to-peer.** The list response hands the client a `sockaddr_in` and the client
dials the host directly: Storm UDP on 6112 for D1/SC/BW/W2, TCP for WC3 (host port announced
via `SID_NETGAMEPORT`, `0x45`). The server is a lobby and directory only — which is exactly
why federating the game list works (see `FEDERATION.md` §6). **Diablo II *closed* realm is
the exception**: the client connects to a server-side D2GS. D2 *open* is peer-to-peer.

`SID_STARTADVEX3` status: `0x00` Ok · `0x01` name already exists · `0x02` game type currently
unavailable · `0x03` error creating game. **`0x02` is what warnet mode returns.**

`SID_GETADVLISTEX` empty-list status: `0` Ok · `1` doesn't exist · `2` wrong password ·
`3` full · `4` already started · `5` spawned key not allowed · `6` too many requests.
**Warnet mode returns `1`.**

The **game statstring** is a product-specific delimited blob the server stores and echoes
verbatim — the *client* parses it. Never interpret it; never rewrite it. WC3's is an encoded
map blob rather than a classic CSV, and its password field is always empty.

⚠️ All `Battle.snp` clients (DRTL, DSHR, STAR/SEXP, JSTR, SSHR, W2BN) send `SID_STOPADV` on
logoff **even when not in a game**. Handle it as a no-op, never an error.

---

## 6. Diablo II realm (MCP)

```
BNCS:  SID_QUERYREALMS2 (0x40) → SID_LOGONREALMEX (0x3E)   [returns MCP IP:port + 16 u32s]
MCP:   protocol byte 0x01, then MCP framing
       MCP_STARTUP (0x01) → MCP_CHARLIST2 (0x19) → MCP_CHARLOGON (0x07)
       → MCP_CREATEGAME (0x03) | MCP_JOINGAME (0x04) | MCP_GAMELIST (0x05)
D2GS:  new TCP connection, port 4000 (hardcoded client-side, cannot be changed)
       D2GS_NEGOTIATECOMPRESSION (0xAF) → D2GS_GAMELOGON (0x68)
       → D2GS_STARTGAME (0x5C) → D2GS_ENTERGAMEENVIRONMENT (0x6A) → compressed
```

⚠️ `SID_LOGONREALMEX` rule: if the response is **longer than 8 bytes**, proceed; otherwise it
is an error (`0x80000001` realm unavailable, `0x80000002` logon failed). The 16 `u32`s must be
forwarded verbatim into `MCP_STARTUP`.
🛑 The "new format" (D2 1.14d+) is documented only as `Unknown[16] / Unknown[40]`; the
cryptographic construction is unknown. 🛑 `MCP_STARTUP` chunk semantics are only partially
mapped and reportedly changed by Blizzard later.

PvPGN's split — bnetd :6112, D2CS :6113, D2DBS :6114, third-party D2GS :4000 — is one valid
topology, not the only one. `jaenster/d2-dedicated-server` collapses BNCS and MCP onto 6112,
which is closer to what real Battle.net did. **D2GS is closed-source, Windows-only, and its
port is hardcoded, so you cannot run two per host.** Plan realms as containers/VMs.

---

## 7. Ports

| Port | Proto | Service |
|---|---|---|
| 6112 | TCP | BNCS (all classic clients + chat gateway) |
| 6112 | UDP | Storm P2P (D1/SC/BW/W2) and BNCS `PKT_*` messages |
| 6112 | TCP | W3GS hosting (client-configurable) |
| 6112 | UDP | WC3 LAN discovery broadcast ⚠️ BNETDocs labels `W3GS_SEARCHGAME` as TCP, which cannot be right for a LAN broadcast; independent implementations use UDP. Trust UDP. |
| dynamic | TCP | MCP realm — IP:port delivered in `SID_LOGONREALMEX` |
| 4000 | TCP | D2GS — hardcoded, unchangeable |
| 6200 | TCP | WC3 game routing (PvPGN's `w3routeaddr`) |

Cairn defaults: BNCS 6112, MCP 6113, admin/metrics 6115, federation outbound 7112.

---

## 8. Real-Battle.net quirks worth replicating

From BNETDocs' *Known Server Issues*. These are not bugs to fix; clients expect them.

- **CD keys stay "in use" if you reconnect within ~500 ms.** Enforce a ≥500 ms relogin
  cooldown or clients will see spurious "key in use".
- **150–230 ms delay after channel joins, self-whispers, and friend-list queries.** Some bots
  are written against this timing.
- **Servers do not respond to UDP at all for W2BN/DRTL/DSHR**, so those clients always get the
  No-UDP flag (`0x10`). Set it unconditionally for those products.
- **`SEXP` 1.16.1 may send `SID_STOPADV` before login completes.** Don't treat pre-auth
  `0x02` as a violation.
- **Diablo: Hellfire (`HRTL`) never receives `SID_STARTVERSIONING`** and hangs during
  handshake. Not supported; reject cleanly.
- Diablo clients that send an empty statstring get the default `LTRD 1 0 0 30 10 20 25 0 0`.

---

## 9. Modern clients — the honest status

🛑 **StarCraft: Remastered (1.18+), WarCraft III Reforged, and Diablo II: Resurrected are not
implementable from public documentation today.**

- StarCraft 1.18 (March 2017) made major protocol changes that broke all bot compatibility.
  PvPGN states plainly that 1.18+ will not be supported. `SSHR` and `JSTR` no longer log on
  to official servers at all.
- WarCraft III Reforged (2020) did the same. gowarcraft3, the best-maintained WC3 library,
  says BNCS "works up until patch 1.32" — implying a discontinuity after, without saying what
  replaced it.
- Diablo II: Resurrected shipped with **no TCP/IP or LAN mode** at all; it is online-only.
  BNETDocs has no D2R entry. Community emulation projects publish no protocol spec, and
  Blizzard has issued takedowns in this area.
- Blizzard's answer to the 2017 breakage was **CAPI** — chat-only, key-gated, over secure
  WebSockets — explicitly so bots would stop emulating the game protocol.

**What this means for your roadmap:** support for these titles cannot be scheduled, because
the work is not "implement a documented protocol", it is "reverse-engineer an undocumented
one, in a jurisdiction where the 8th Circuit has already ruled on that" (see `LEGAL.md`).

**What the architecture does about it:** the gateway boundary. A protocol front-end is a
crate implementing one trait over `cairn-core`; it owns its framing, its auth, and its
session state machine, and knows nothing about channels or storage. If a modern protocol is
ever documented, it becomes `cairn-gateway-bgs` and the core does not change. That is the
correct amount to invest in a maybe — a seam, not a stub.

Target for real work: **StarCraft/BW ≤ 1.16.1, Diablo I, Warcraft II BNE, Diablo II/LoD ≤
1.14d, Warcraft III ≤ 1.26/1.28 (patched client).**
