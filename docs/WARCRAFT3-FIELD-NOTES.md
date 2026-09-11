# Getting a real WarCraft III client into chat — field notes

**Status: done.** As of 2026-09-11 an unmodified-protocol WarCraft III: The Frozen Throne
client (`W3XP`, platform `IX86`, version byte `0x1B` — the 1.27b era), driven through a
community loader that handles the client-side server-signature check, **logs in and chats**
against `bnetccd` over NLS: create account → `SID_AUTH_ACCOUNTLOGONPROOF` → `SID_ENTERCHAT`
→ `SID_JOINCHANNEL` → typing in a channel.

This document is the hard-won part: the handful of **non-obvious requirements** that each
silently blocked the connection until they were exactly right, the **red herrings** that
looked like the cause but weren't, and the **diagnostic method** that finally isolated it.
It complements `WARCRAFT3.md` (the plan and NLS spec) and `PROTOCOL-NOTES.md` (wire layouts);
here we only cover what a future implementer will actually get wrong.

If you are writing a Battle.net server emulator and WC3 connects but then drops "Your
connection to Battle.net has been lost" — start here.

---

## 1. The observed connection sequence

A single BNCS TCP connection (plus separate BNFTP connections for file downloads), in order:

```
C→S  0x01                      protocol selector byte ("game client"), before any frame
C→S  SID_AUTH_INFO      0x50   platform IX86, product W3XP, version byte, locale, country
S→C  SID_PING           0x25   server ping; client echoes it back
S→C  SID_AUTH_INFO      0x50   logon type 2, tokens, MPQ name, checkrevision formula, 128-byte sig
C→S  SID_AUTH_CHECK     0x51   version/CD-key check   (we fail OPEN — accept regardless)
       ... BNFTP v2 downloads on separate connections: ver-IX86-1.mpq, TOS/newaccount/
           chathelp/icons-WAR3.bni, bnserver-WAR3.ini (see §5) ...
C→S  SID_GETFILETIME    0x33   one per file the client caches by filetime
C→S  SID_GETICONDATA    0x2D
C→S  SID_AUTH_ACCOUNTCREATE 0x52   (only if the account is new: salt + verifier + name)
C→S  SID_AUTH_ACCOUNTLOGON  0x53   client public key A + name  → S→C: status, salt, B
C→S  SID_AUTH_ACCOUNTLOGONPROOF 0x54   client proof M1        → S→C: status, M2   ← see §3
C→S  SID_ENTERCHAT      0x0A   → S→C: the client's own unique name, statstring, account  ← see §4
C→S  SID_NETGAMEPORT    0x45 / SID_WARCRAFTGENERAL 0x44 / SID_UDPPINGRESPONSE 0x15 (benign)
C→S  SID_JOINCHANNEL    0x0C   → chat events; the client is now in a channel
C→S  SID_CHATCOMMAND    0x0E   the player is typing
```

Nothing here is unusual until `0x54`. The connection ran cleanly all the way to a logged-in
state on *every* attempt during debugging — the drop was always immediately after the server
answered `0x54`, before the client sent `SID_ENTERCHAT`. That timing is the key clue: an
instant drop with the client sending nothing means the client **rejected the `0x54` reply
itself**, not anything later.

---

## 2. The requirements that each blocked connection

### 2.1 `SID_AUTH_ACCOUNTLOGONPROOF` (0x54) must be *exactly* status + M2 — no trailing string

This was the final blocker and the least obvious. The documented layout is:

```
(DWORD) status
(BYTE[20]) server proof M2
(STRING) additional information
```

That "additional information" string tempts you to always append it, even empty (a single
`0x00`). **Do not, for a non-error status.** A real WC3 client validates the exact body
length of `0x54`: for a success/`email-wanted` status it expects **24 bytes** (4 + 20) and
**drops the connection on any trailing byte.** Our 25-byte reply (status + M2 + one empty-
string null) produced the exact "connection lost" symptom. A reference server (PvPGN Pro on
`pvpgn.bnetdocs.org`) sends 24 bytes. Only the custom-error status `0x0F` carries a string.

```
WRONG (drops the client):  00 00 00 00 <M2×20> 00        (25 bytes)
RIGHT:                     00 00 00 00 <M2×20>           (24 bytes)
```

See `session.rs::auth_account_logon_proof`. This one byte is the difference between "logs in"
and "connection lost."

> Note on status: a stock server returns `0x0E` ("register an email") for an account with no
> email on file, and `0x00` for one with an email. Both are 24-byte replies and both enter
> chat; the **length**, not the status value, is what the client rejects. `0x00` works.

### 2.2 NLS/SRP must be byte-exact — and there are two classic ways to get it subtly wrong

The math is standard SRP-6 with Blizzard's constants (`docs/WARCRAFT3.md` §3,
`bnetcc-crypto/src/nls.rs`, cross-checked against BNCSUtil's `nls.c` — the canonical
community reference — and an independent Python model in `scripts/nls_reference.py`). Two
mistakes pass your own round-trip tests but fail a real client:

1. **Minimal-length big-integer encoding.** Every value hashed into `M1`/`M2` — salt, `A`,
   `B`, and the session key `K` — must be a **fixed width** (32 bytes for salt/A/B, **40 for
   K**), zero-padded. If you serialize at natural length (dropping a leading zero byte), any
   value whose top byte is `0x00` (≈1/256 each) hashes one byte short and the proof diverges
   — so *some accounts* work and others never do, deterministically. This is a real trap seen
   in a third-party reference implementation; `nls.rs` has a regression test named exactly for
   it (`a_salt_with_a_zero_top_byte_still_works`).
2. **Not upper-casing the name/password.** `x = SHA1(s ‖ SHA1(UPPER(user) ‖ ":" ‖
   UPPER(pass)))` and the name-hash inside `M1` is `SHA1(UPPER(user))`. (Strictly it only has
   to *match* the peer's convention, but every stock server upper-cases, so match it.)

Byte orders that matter: wire fields and their SHA-1 inputs are 32-byte **little-endian**;
`x` is read little-endian from its digest; the scrambler `u` is the first 4 bytes of
`SHA1(B)` read **big-endian**. `K` is `S` split into even/odd bytes, each SHA-1'd, interleaved
back (40 bytes).

**Corollary that saves you days:** if the server *accepts* the client's `M1` (login
succeeds), `M2` is automatically correct — `M2 = SHA1(A ‖ M1 ‖ K)` is built from the same
`A`, `M1`, `K`, so identical inputs give an identical hash. "M1 passes but the client rejects
M2" is impossible with the canonical formula; do not go looking for an M2 bug. See §3.

### 2.3 The 128-byte server signature must be *present* (its value doesn't matter to the server)

`SID_AUTH_INFO` (0x50) for `WAR3`/`W3XP` ends with a **128-byte RSA server-signature field**.
It is a product field the client's parser expects to be there — omit it and the client
rejects the whole reply as an "invalid Battle.net server" *before* the version check. Send the
128 bytes; **all-zero is fine** — a stock PvPGN server sends zeros too (it can't sign either).

Verification of that signature is **entirely client-side** and only a patched/loadered client
skips it (see `docs/LEGAL.md` §3). It is not something the server can satisfy without
Blizzard's private key, and it is *not* the cause of a post-login drop (the reference server
sends zeros and works). Send the field, send zeros, move on.

### 2.4 Present the client its *own* name with no realm suffix

If you namespace accounts internally (we store WC3 accounts as `Name@bncc` to keep them apart
from the X-SHA-1 products' accounts), **strip the realm before showing the name to the
client** — in the `SID_ENTERCHAT` reply, chat events, and the user list. Real Battle.net never
puts an `@` in your *own* name (the client appends a gateway suffix itself, only for users
from *other* gateways), and a WC3 client refuses an `@` in its own name. For a single-realm
server, present bare names everywhere. See `session.rs::finish_logon`.

### 2.5 Stock WC3 speaks NLS only — do not try to force old-hash

WC3 will not use the old X-SHA-1 logon (`SID_LOGONRESPONSE2`) on its own. Advertise `logon
type = 0` in `SID_AUTH_INFO` and the client hangs up before the version check ("invalid
server"). The only way onto the old-hash path is a loader that rewrites the client's logon
code (the heavyweight route). Serve `logon type = 2` (NLS) and let the loader handle the
signature; that is the working combination.

### 2.6 BNFTP v2, and fail-open version checking

- WC3 downloads its files (version-check MPQ, TOS, `icons-WAR3.bni`, `bnserver.ini`) over
  **BNFTP v2**, a 3-phase handshake distinct from v1: request → server challenge (a random
  DWORD) → challenge response carrying the filename → file. Get this wrong and the client
  can't fetch the MPQ it needs for the version check. See `bnetcc-proto/src/bnftp.rs`.
- The `SID_AUTH_INFO` checkrevision **value string** should be the real formula for the MPQ
  you advertise (WC3 `ver-IX86-1.mpq`: `B=454282227 C=2370009462 A=2264812340 4 A=A^S B=B-C
  C=C-A A=A+B`). We **fail open** on the version check (never recompute the hash), so a
  placeholder technically works — but ship the real one; it is a protocol fact, not a secret.

---

## 3. Red herrings (things that looked like the cause but were not)

Every one of these cost real time. Skip them:

- **"The server's M2 is wrong."** It cannot be, if `M1` was accepted (§2.2). We chased this
  hard on a collaborator's advice; it was always the `0x54` trailing byte.
- **The `icons-WAR3.bni` file.** A stub icons file (14 entries vs the real ~280 KB set) is
  *not* a login blocker — icons load at channel render, long after login. Replace it for
  correct icons, but it never causes "connection lost."
- **The UDP test / port 6112.** WC3 games are TCP; the client does not gate login on the UDP
  round-trip. A missing/mismatched UDP value greys out game hosting at worst, it does not drop
  login. (And an *instant* drop is never a UDP timeout — timeouts take seconds.)
- **The server signature being the server's problem.** It's zero on every private server,
  including the one that works; the client-side bypass is the loader's job (§2.3).
- **The realm suffix causing the drop.** It causes a drop at *chat entry* (§2.4), not the
  post-`0x54` drop — a real client that dies before sending `SID_ENTERCHAT` never saw its name.

---

## 4. How to actually diagnose this

Speculation is expensive; two cheap tools settle it:

1. **A minimal probe client that reuses your own crypto.** We wrote `scratchpad/nlsprobe`
   (path-depending on `bnetcc-proto`/`bnetcc-crypto`) to run the full NLS flow against a live
   server and **dump every reply's raw bytes**. It reaches chat on our server — proving the
   server path is open — and, crucially, it is *lenient*: when it succeeds but the real client
   drops, the difference is something the real client is stricter about (the exact `0x54`
   length). A probe that shares your crypto also independently confirms your NLS is correct.
2. **Byte-for-byte diff against a known-good reference.** Point the same probe at a working
   server. `pvpgn.bnetdocs.org` (PvPGN Pro) **fails open on the version check**, so the probe
   can complete a full login there and capture its `0x50` and `0x54` replies. Laying them
   beside ours is what revealed the 24-vs-25-byte `0x54` difference in minutes after days of
   theorizing. (Compare only the *wire bytes* — protocol facts are free; do not read a
   copyleft server's source, see `docs/LEGAL.md` §1.)
3. **Per-frame server logging.** Log every inbound frame id + state once the product is known.
   Seeing the client stop at `logon accepted` with no following `SID_ENTERCHAT` (id `0x0A`) is
   what pinned the failure to the `0x54` reply rather than anything downstream.

The winning method was: probe reaches chat on our server → same loader reaches chat on
bnetdocs but not us → therefore a server-side *wire* difference → diff the bytes → fix the one
byte.

---

## 5. Files the client fetches (operator data, not shipped in-repo)

Served over BNFTP from the files directory: `ver-IX86-1.mpq` (version-check MPQ),
`termsofservice-enUS.txt`, `newaccount-enUS.txt`, `chathelp-war3-enUS.txt`, `icons-WAR3.bni`
(the real ~280 KB icon set — a stub renders wrong icons), and `bnserver-WAR3.ini` (the gateway
list; its `ENU=`/`FRA=` labels are what the client shows as the gateway name, so brand them to
your server, not a reference server's).

---

## 6. See also

- `docs/WARCRAFT3.md` — the NLS spec, status codes, known-answer vectors, realm decision.
- `docs/PROTOCOL-NOTES.md` — framing, the login-flow table, BNI/BNFTP/advertisement layouts.
- `docs/LEGAL.md` §1, §3 — clean-room provenance, and why WC3 needs a client-side loader.
- `bnetcc-crypto/src/nls.rs` — the NLS implementation and its byte-order/fixed-width tests.
- `scripts/nls_reference.py` — the independent Python model / known-answer vector generator.
