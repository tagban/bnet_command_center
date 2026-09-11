# WarCraft III (classic RoC / TFT): what gates a player today, and the plan

Investigation, 2026-09-10. Scope is the **classic** `WAR3`/`W3XP` client (≤ 1.28, with the
client-side server-signature patch the operator must obtain themselves — `LEGAL.md` §3).
Reforged is out of scope. The goal is: log in, chat, run commands, manage a clan, and
create and join games. A ladder page comes later from the same data.

Confidence marks follow `PROTOCOL-NOTES.md`: ✅ verified in code or against two
independent references · ⚠️ from one reference or extrapolated · ❌ not implemented.

---

## 1. The short version

**Status 2026-09-10: the login gate is built.** When this investigation started a
WarCraft III client could not get past the login screen: the version check passed, the
client sent `SID_AUTH_ACCOUNTLOGON` (0x53), and nothing answered it. That is now
implemented end to end (§6 lists what landed). The table below is the before/after:

| Piece | State | Where |
|---|---|---|
| `SID_AUTH_INFO` reply for WC3 (logon type `0x02`, 128 zero bytes where the RSA signature goes) | ✅ | `session.rs::auth_info` |
| `SID_AUTH_CHECK` with two CD keys (base + expansion) | ⚠️ layout extrapolated, never captured | `session.rs::auth_check` |
| State machine accepts 0x52/0x53/0x54 in `Authenticating` and advances to `LoggedIn` on 0x54 | ✅ | `bnetcc-core/src/session.rs` |
| `Credential::Srp { salt, verifier }` in the storage model and SQLite backend | ✅ | `bnetcc-storage`, `bnetcc-storage-sqlite` |
| The daemon's storage actor carries the full `Credential`; `CreateAccount` takes one; password reset derives the account's own family (a fresh salt + verifier for SRP) | ✅ was ❌ | `bnetccd/src/storage.rs` |
| NLS math: standard SHA-1 + 256-bit modexp, server and client sides, known-answer tests | ✅ was ❌ | `bnetcc-crypto/src/nls.rs` |
| Handlers for 0x52 `AUTH_ACCOUNTCREATE`, 0x53 `AUTH_ACCOUNTLOGON`, 0x54 `AUTH_ACCOUNTLOGONPROOF` | ✅ was ❌ | `session.rs` |
| Realm-scoped accounts `Name@<realm>` (`server.realm`, default `bncc`) | ✅ | `config.rs`, `session.rs`, `bnetcc-storage::split_realm` |
| Game list filtered by product, name and count (§4.1) | ✅ was ⚠️ | `session.rs::game_list` |
| Empty `SID_FRIENDSLIST` / `SID_NEWS_INFO` replies | ✅ | `session.rs` |
| A test client that speaks SRP | ✅ in-process tests in `session.rs`, and `massload`'s `srp_login` | `massload/src/bot.rs` |

⚠️ None of this has met a real patched client yet. The evidence is the crypto known-answer
vectors (cross-checked against an independent Python implementation), the in-process
end-to-end tests, and the bot. §7 lists what only a capture can settle.

Chat and commands are product-agnostic and work unchanged. Clans and the in-client
ladder/profile are the next phase (§5).

---

## 2. What a classic WC3 client sends, and what we must answer

Sequence assembled from BNETDocs, gowarcraft3's client (MPL-2.0, read for design only)
and Atlas (MIT). Items marked ⚠️ need a capture from a real patched client to confirm
whether an unanswered request blocks the UI.

### Login

| # | Client sends | Server must | bnetccd today |
|---|---|---|---|
| 1 | `0x50 SID_AUTH_INFO` (`IX86`, `W3XP`, verbyte `0x1E`) | logon type `0x02`, server token, UDP token, MPQ name, formula, **128-byte signature** | ✅ (signature zeroed; patched client) |
| 2 | `0x51 SID_AUTH_CHECK` (2 keys for W3XP, 1 for WAR3) | `0x000` passed | ✅ / ⚠️ two-key parse |
| 3 | `0x33 SID_GETFILETIME` (`tos_USA.txt`, `bnserver-WAR3.ini`) | filetime | ✅ generic |
| 4 | `0x53 SID_AUTH_ACCOUNTLOGON`: `A[32]`, username | `status`, `s[32]`, `B[32]` — **always 72 bytes**, zeros on failure | ✅ `auth_account_logon` |
| 4a | `0x52 SID_AUTH_ACCOUNTCREATE`: `s[32]`, `v[32]`, username (if we answered `0x01` no account) | `status` (§3.4 codes) — then the client re-sends 0x53 | ✅ `auth_account_create` |
| 5 | `0x54 SID_AUTH_ACCOUNTLOGONPROOF`: `M1[20]` | `status`, `M2[20]`, info string | ✅ `auth_account_logon_proof` |
| 6 | `0x59 SID_SETEMAIL` (only if we answered `0x0E`) | nothing | n/a — never answer `0x0E` |
| 7 | `0x45 SID_NETGAMEPORT` | nothing | ✅ |
| 8 | `0x0A SID_ENTERCHAT` | unique name (`Name@bncc`), **WC3-form statstring**, account name | ✅ / ⚠️ statstring is the bare reversed tag (§4.3) |
| 9 | `0x0B SID_GETCHANNELLIST` (`W3XP`) | channel list | ✅ |
| 10 | `0x0C SID_JOINCHANNEL` flag `0x01`, product default channel | join | ✅ per-product default channel exists |
| 11 | `0x65 SID_FRIENDSLIST` | friends list (may be empty) | ✅ empty list |
| 12 | `0x46 SID_NEWS_INFO` | news (may be empty) | ✅ zero entries |
| 13 | `0x44 SID_WARCRAFTGENERAL` sub `0x07 WID_TOURNAMENT`, `0x09 WID_ICONLIST`, later `0x04 WID_USERRECORD` (profile), `0x08 WID_CLANRECORD` | per-subcommand replies | ⚠️ unhandled; Atlas also ignores them |
| 14 | `0x7D SID_CLANMEMBERLIST`, and we push `0x75 SID_CLANINFO` at login for clan members | clan state | ❌ phase 3 (§5) |
| 15 | `0x2D SID_GETICONDATA` → BNFTP `icons-WAR3.bni` | file | ✅ (an MPQ, served verbatim) |

### Create a game (host side)

| Client sends | Notes | bnetccd today |
|---|---|---|
| `0x45 SID_NETGAMEPORT` | host's W3GS TCP port, default 6112 | ✅ |
| `0x1C SID_STARTADVEX3` | state `u32`, uptime `u32`, **flags `u32`** (`0x01` custom / `0x09` ladder · `0x800` private · `0x2000`/`0x4000` map author · `0x20000`.. size · `0x100000`.. observers), provider constant `0x03FF`, ladder `0/1`, name, **empty password**, encoded statstring | ✅ — our `u16 type + u16 parameter` is the same 4 bytes little-endian; echoed verbatim |
| `0x1C` again on every lobby change | slots-free digit and state bits change; same host re-advertises | ✅ treated as update |
| `0x02 SID_STOPADV` when the game starts | remove from directory | ✅ |
| `0x3C SID_CHECKDATAFILE2` | **not sent by WC3** (the host validates maps itself) | n/a |

No UDP check is involved: WC3 games are TCP, and the client does not gate Create/Join on
`PKT_SERVERPING`. Our unsolicited UDP ping to a WC3 client is harmless.

### Join a game

| Client sends | Notes | bnetccd today |
|---|---|---|
| `0x09 SID_GETADVLISTEX` | filter `u32`, filter mask `u32`, `0`, **count** (`20` for the list; **`1` with the exact game name when joining**), name, `""`, `""` | ✅ parsed; per product, by name, count-capped (§4.1) |
| ← `0x09` | per game: settings `u32`, language `u32`, `sockaddr_in`, status (`0x10` public · `0x11` private), elapsed, name, password, statstring | ✅ layout matches |
| W3GS over TCP to host `ip:port` | peer-to-peer; the server is out of the loop | n/a — host needs TCP 6112 forwarded, same as real Battle.net |
| `0x22 SID_NOTIFYJOIN` | informational | ✅ accepted |

The "W3 route" listener (TCP 6200, PvPGN's `w3routeaddr`) serves **only** ladder and
arranged-team matchmaking (`WID_GAMESEARCH`). Custom games never touch it. It is not on
the path to "create and join games".

---

## 3. NLS, pinned down

### 3.1 Sources and provenance

Implemented from BNETDocs' "NLS/SRP Protocol" document, the javaop write-up (as quoted by
BNETDocs), and RFC 2945, cross-checked against tagban's own `XSRP3a.cpp` / `BigNumber.cpp`
(OpenSSL-backed, his code). PvPGN's `bnetsrp3.cpp` (AGPL) was **not** opened and must
not be — `LEGAL.md` §1, `HANDOFF.md` §5.

`scripts/nls_reference.py` is a runnable reference of everything below. It asserts the
client and server sides agree over random inputs and prints known-answer vectors.

### 3.2 The algorithm

Parameters: `g = 47 (0x2F)`, `N = 0xF8FF1A8B619918032186B68CA092B5557E976C78C73212D91216F6658523C787`
(256 bits). Hash is **standard SHA-1** everywhere, never XSHA-1.

```
x  = SHA1( s ‖ SHA1( upper(C) ‖ ":" ‖ upper(P) ) )   read as a LITTLE-endian integer
v  = g^x mod N                                        (stored with s; never the password)
A  = g^a mod N                                        client, a random < N per logon
B  = (v + g^b mod N) mod N                            server, b random < N per logon
u  = first 4 bytes of SHA1(B)  read as a BIG-endian integer   (computed, never sent)
S  = client: ((N + B − v) mod N)^(a + u·x) mod N
     server: (A · v^u mod N)^b mod N
K  = interleave( SHA1(S[even indexes]), SHA1(S[odd indexes]) )   40 bytes:
     K[2i] = SHA1(even)[i], K[2i+1] = SHA1(odd)[i]
I  = SHA1(0x2F) xor SHA1(N)                            20 bytes, constant
M1 = SHA1( I ‖ SHA1(upper(C)) ‖ s ‖ A ‖ B ‖ K )
M2 = SHA1( A ‖ M1 ‖ K )
```

**Byte-order rules — every one of these matters:**

- Every big integer on the wire (`s`, `v`, `A`, `B`) is **exactly 32 bytes, little-endian,
  zero-padded**. The same 32-byte encoding is what goes into SHA-1 for `u`, `K`, `M1`, `M2`.
  `K` is 40 bytes, `M1`/`M2` are 20.
- `x` is little-endian from its digest; `u` is big-endian from its 4 bytes. Yes, both.
- `I` hashes `N` in its **little-endian** 32-byte form. Verified: `SHA1(2F) xor SHA1(N_LE)`
  = bytes `6C0E97ED0AF96BABB15889EB8BBA25A4F08C01F8`, which read as a big-endian number is
  the `F8018CF0A425BA8BEB8958B1AB6BF90AED970E6C` constant javaop quotes and `XSRP3a.cpp`
  hard-codes. Hashing `N` big-endian gives a different, wrong `I`.
- NLS **version 1** (logon type `0x01`, pre-1.13 clients) reverses the byte order of `N`.
  We send logon type `0x02`; leave v1 alone unless a captured client asks for it.

Server-side checks that RFC 2945 requires and the game does not: refuse `A mod N == 0`;
draw `b` fresh per logon; compare `M1` in constant time; never reuse `(b, B)` after a
failure.

### 3.3 Notes on the C++ reference

Three things to know before treating `XSRP3a.cpp` as ground truth:

1. **It does not upper-case the username or password in `ClientPrivateKey()`.** The spec
   does, and the real client does. For a server this only matters where *we* compute a
   verifier ourselves (admin password reset, a test bot, cross-family credential fill).
2. **`BigNumber::AsByteArray()` returns the minimal-length encoding**, so whenever the top
   byte of `s`, `A`, `B` or `K` happens to be zero the value hashed into `M1` is short by a
   byte and the proofs disagree. That is roughly one logon in 256 per value — a plausible
   cause of the intermittent "looking for this M" mismatch in the comments. Fixed-width
   32/40-byte encodings remove it (`HashSecret` already forces 32 for `S`).
3. Its `I` constant and its LE conventions for `SetBinary`/`AsByteArray` match the rules
   above, which is what confirms the little-endian-`N` reading of `I`.
4. The companion `BigNumbers.cpp::ToFixedBytes` fixes the **wire** fields to 32 bytes, but
   the **hash inputs** in `ServerCalculateM`, `ClientCalculateM`, `ClientCalculateM2`,
   `Scrambler` and `HashSecret` still call `AsByteArray()`/`GetNumBytes()` directly, so
   point 2 applies there too (fixed widths: `I` 20, `s`/`A`/`B` 32, `K` 40).
5. `SHA1.cpp` is a plain FIPS 180-1 SHA-1 (standard IV, standard rounds, big-endian
   digest), which is the right hash for NLS; only `_byteswap_ulong` is MSVC-specific. In
   Rust we use a standard SHA-1 crate rather than porting it.

### 3.4 Wire status codes

Constants live in `bnetcc_proto::bncs::nls_status`.

`0x53 S>C`: `0x00` proof required · `0x01` no such account · `0x05` needs upgrade.
Always send the full 72 bytes (zeroed `s`/`B` on failure).

`0x54 S>C`: `0x00` success · `0x02` wrong password · `0x06` account closed ·
`0x0E` email wanted (client then sends `SID_SETEMAIL`; do not use) · `0x0F` custom error,
message in the trailing string (this is how to show a ban reason).

`0x52 S>C`: `0x00` created · `0x04` exists · `0x07` too short · `0x08` illegal char ·
`0x09` banned word · `0x0A` too few alphanumerics · `0x0B` adjacent punctuation ·
`0x0C` too much punctuation. Map our own name policy (min 2 chars, permissive charset —
a standing product decision) onto `0x07`/`0x08`; anything else is `0x04`.

### 3.5 Known-answer vectors (from `scripts/nls_reference.py`)

```
username Tagban  password hunter2
s  = 000102…1e1f (bytes 0x00..0x1f)
a  = 0102…1f20 (LE)         b = 0203…2021 (LE)
x  = 0x395e620c90a3919c75ede2fdd52aaf8cd6b28d39
v  = 7f2bf7dcd443e0f945a5a7bcbfec2f5e3fee8242d09b62aac46c194963dcafef
A  = f085e0261e42a247db6e0bb57ae40d56746b0c16783606718a2c500d3fed2594
B  = 423ac3a9427c556639f31804d59e3e2c409e182add1648440ed88622abb94580
u  = 0x672cb5c0
K  = 2eeb577075dfcbdd4ec5c17e13c8df4efc8bc573ce35f665c6a913bf0c6e2febf0b0c23a00428941
M1 = a4d99ac492f45207ce0ed8e1c9729f38a425d151
M2 = ba079a3bfcf36d706f1df1dd2934c3252aad086f
I  = 6c0e97ed0af96babb15889eb8bba25a4f08c01f8
```

These prove internal consistency and the byte-order rules; they are not from a live
client. The first real-client login is the true known-answer test.

### 3.6 Decided: WarCraft III accounts live in their own realm (`Name@bncc`)

The server never sees a WC3 password — `0x52` delivers `(s, v)` already computed. An
XSHA-1 digest cannot be turned into a verifier or vice versa, so an account created from
StarCraft cannot log in from WarCraft III, and the reverse, unless the plaintext is seen.

**Decision (tagban, 2026-09-10):** do not share passwords across families. WarCraft III
accounts are a separate namespace, designated with a realm suffix the way real Battle.net
shows WC3 users across gateways (`Name@Azeroth`). The realm name is **configuration**,
default **`bncc`**. Implemented as follows (`server.realm` in `bnetccd.toml`):

- The realm is folded into the stored account name (`Tagban@bncc`), and the validator
  applies the length rules to the bare part (`bnetcc_storage::split_realm`), so no schema
  change was needed and `#N`, bans, ops and whispers all see one plain string.
- A WarCraft III client that types `@` in its name gets "no such account": the client's
  verifier and proof hash the typed name exactly, so a suffixed name could never match a
  verifier created without one. Real clients never send a suffix.
- X-SHA-1 account creation refuses `@`, so nothing can squat in the realm namespace.

Original design notes:

- Config: `[server] realm = "bncc"` (name TBD when the field lands). Patched clients'
  gateway entry should carry the same name so the client's own `@Realm` rendering agrees.
- Account identity becomes `(name, realm)`: XSHA-1 family accounts have no realm (today's
  accounts are unchanged); SRP accounts carry `realm = bncc`. Store the realm as its own
  column/field rather than folding it into the name, so the 15-character name cap in
  `validate_account_name` (`bnetcc-storage/src/lib.rs`) still applies to the bare name.
  `@` itself passes the validator (ASCII, no whitespace), so nothing there needs to change.
- Display name for a WC3 session is `Name@bncc`; the `#N` duplicate suffix goes after
  (`Name@bncc#2`), and `claim_name`'s lowercase registry keeps `Tagban` (SC) and
  `Tagban@bncc` (WC3) distinct in the same channel. Bans, ops and `/whisper` targets
  address the qualified form.
- `SID_AUTH_ACCOUNTLOGON` / `ACCOUNTCREATE` look up and create in the `bncc` realm; the
  bare name in the packet never collides with an XSHA-1 account of the same spelling.
- Whisper parsing accepts `Name@bncc` from any product (a StarCraft user can reach a WC3
  user), and a WC3 client typing `/w Name` without a realm resolves within its own realm
  first.

⚠️ To confirm with a capture: whether the classic WC3 client is happy receiving its *own*
unique name with an `@realm` suffix in `SID_ENTERCHAT` (real Battle.net sends the bare
name and the client adds the gateway suffix itself for cross-gateway users). If it is not,
send the bare name in `SID_ENTERCHAT` and use the qualified form everywhere else.

`to_account` in `bnetccd/src/storage.rs` stops discarding SRP accounts either way.

---

## 4. Game directory: WC3-specific gaps

### 4.1 `SID_GETADVLISTEX` request handling (`session.rs::game_list`) — ✅ done

Before 2026-09-10 the handler ignored the request body. Two consequences for WC3, both
now fixed as described:

1. **No product filter.** Every advertised game of every product comes back. A WC3 client
   handed a Warcraft II entry parses its CSV statstring as WC3's encoded map blob.
   Filter to the requester's product (Atlas does exactly this).
2. **No name filter.** On *join*, the WC3 client sends the exact game name with count `1`
   and takes the returned entry's `sockaddr` as the host. Returning the whole list is
   wrong. Honour the name (case-insensitive, as `advertise_game` keys it), the count cap,
   and reply count `0` + status `0x01` "doesn't exist" when a named game is gone.
3. **Empty-list status.** We send `0x01` for a plain empty listing; `0x00` OK is right
   there (Atlas sends `0`). Keep `0x01` only for a miss on a *named* lookup.

### 4.2 What already fits WC3 ✅

`SID_STARTADVEX3` parse (`u16`+`u16` == the WC3 `u32` flags), statstring echoed verbatim
(`cstr(512)` cap is ample for the encoded blob), host port from `SID_NETGAMEPORT`, the
`sockaddr_in` in the reply, status = host's state word (`0x10`/`0x11` is what WC3 sends
and expects back), re-advertise as update, `STOPADV`/`LEAVEGAME`/disconnect withdrawal.

### 4.3 `SID_ENTERCHAT` statstring shape

`statstring::build_default` emits the StarCraft nine-field form for every product. WC3's
form is product-reversed then up to three fields (icon code, level, reversed clan tag);
`statstring.rs` already parses that shape. Other WC3 users render this user's icon from
it, so emit the WC3 form for `WAR3`/`W3XP` (a bare reversed product tag is the safe
minimum). ⚠️ Confirm the exact bytes a real server returns with a capture.

### 4.4 Game results for the ladder page

⚠️ Classic WC3 does **not** send `SID_GAMERESULT` (0x2C) for custom games the way
StarCraft/Warcraft II do; PvPGN records WC3 results only for ladder/arranged-team games
that go through the route server. Whether any result packet reaches the chat server after
a custom game needs a capture. Plan the WC3 ladder on the assumption that custom games
yield no result, and that real WC3 ranking requires the route server (phase 3+).

---

## 5. Clans, profile, and the in-client ladder

Not on the path to playing, but wanted. All ❌ today; none block chat or games.

- **Clans (0x70–0x82).** The WC3 UI drives clans by packet, not chat. Minimum for a
  member to *see* their clan: push `0x75 SID_CLANINFO` (tag, rank) right after login, and
  answer `0x7D SID_CLANMEMBERLIST` and `0x7C SID_CLANMOTD`. Full management adds
  `0x70`/`0x71`/`0x72` (create), `0x77`/`0x79` (invite/respond), `0x78` (remove), `0x7A`
  (rank), `0x7B` (set MOTD), `0x73` (disband), `0x74` (chieftain), and the `0x7E`/`0x7F`/
  `0x81`/`0x82` notifications. Must sit on the **universal clan model** (server-side clans
  for every product, chat-trigger driven — a standing product decision), with the WC3
  packets as one view onto it.
- **`SID_WARCRAFTGENERAL` (0x44).** `0x09 WID_ICONLIST` and `0x07 WID_TOURNAMENT` at
  login, `0x04 WID_USERRECORD` for the profile screen, `0x08 WID_CLANRECORD`,
  `0x0A WID_SETICON`. Empty-but-well-formed replies are enough to start; the profile
  reply is where a per-account W/L record surfaces in-client, and a web ladder page
  reads the same storage.
- **`SID_FRIENDSLIST` (0x65)** and **`SID_NEWS_INFO` (0x46)** deserve empty replies so
  the panels populate deterministically.

---

## 6. Build order

Done 2026-09-10 (all tests and clippy green):

1. ✅ **`bnetcc-crypto::nls`** — `sha1` + `num-bigint`; `verifier`, `client_public`,
   `server_public`, `client_proof`, `server_verify` (constant-time compare, refuses
   `A ≡ 0`), plus the §3.5 known-answer vectors and round-trip tests.
2. ✅ **Storage/daemon** — `Account.credential`, `create_account(name, credential)`,
   plaintext `reset_password` that keeps the account's family, `split_realm`, and
   `server.realm`.
3. ✅ **`session.rs`** — `auth_account_create` / `auth_account_logon` /
   `auth_account_logon_proof`, modexp under `spawn_blocking`, shared `finish_logon`,
   per-logon `SrpPending` dropped once the proof is answered; tag bans surface as the
   `0x0F` custom message.
4. ✅ **`game_list`** — parses the request; per-product, by-name, count-capped; `0x00` for
   an empty listing, `0x01` for a named miss.
5. ✅ **`statstring::build_default`** — bare reversed tag for `WAR3`/`W3XP` (⚠️ §4.3).
6. ✅ **`massload`** — `srp_login`, sharing the version handshake with the modern flow.
7. ✅ **`SID_FRIENDSLIST` / `SID_NEWS_INFO`** empty replies. Not done: `0x44`
   `SID_WARCRAFTGENERAL` replies (still logged as unhandled; the reply layouts for
   `WID_ICONLIST`/`WID_TOURNAMENT` are not pinned well enough to answer blindly) and
   `0x75 SID_CLANINFO` (no clans yet).

Next: `0x55`/`0x56` change-password, `SID_WARCRAFTGENERAL` replies, then clans on the
universal model (§5).

Docs to touch when each lands: `PROTOCOL-NOTES.md` §3 (mark ✅ what a capture confirms),
`ROADMAP.md` phase 3, `HANDOFF.md` state table, `bnetcc-crypto/src/lib.rs` doc comment.

---

## 7. Open questions only a real-client capture answers

- The exact `SID_AUTH_CHECK` body from a two-key W3XP client.
- Whether an unanswered `SID_WARCRAFTGENERAL` / `SID_FRIENDSLIST` / `SID_NEWS_INFO` merely
  leaves a panel empty or stalls the client.
- The `SID_ENTERCHAT` statstring bytes a real server returns for a fresh WC3 account.
- What, if anything, a WC3 host sends to the chat server when a custom game ends.
- Which version bytes and `exe_info` strings the community's patched clients report, for
  `[versions]` when restriction is turned on.
