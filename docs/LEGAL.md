# Legal and licensing notes

**Not legal advice.** This is an engineering summary of facts you should have on file, and
one of them is serious enough that you should talk to a lawyer before publishing anything.

---

## 1. Source hygiene — what you may read, and what you must not

| Source | License | Verdict |
|---|---|---|
| **[BNETDocs](https://bnetdocs.org/)** — packet layouts, IDs, flows | Community protocol reference, independent of any codebase | ✅ **Primary source.** Protocol facts are not copyrightable. Cite it; don't bulk-copy its prose. |
| **[BNETDocs/Atlas](https://github.com/BNETDocs/Atlas)** | **MIT** | ✅ **Readable and adaptable with attribution.** Its `Protocols/Game/Messages/SID_*.cs` are effectively an MIT-licensed executable specification of the same wire protocol. This is a large and under-appreciated advantage. |
| **[wjlafrance/broken-sha1](https://github.com/wjlafrance/broken-sha1)** / MBNCSUtil | BSD-3-clause style | ✅ Readable with attribution. Used as the reference for `bnetcc-crypto`'s XSHA-1 (see `crates/bnetcc-crypto/src/xsha1.rs` header). |
| **[jaenster/d2-dedicated-server](https://github.com/jaenster/d2-dedicated-server)** — Diablo II realm + game server (Zig) | **MIT** | ✅ **Readable and adaptable with attribution.** Its `apps/realmd` is a realm a retail 1.14d client renders; used to confirm MCP layouts for `crates/bnetccd/src/realm.rs` (credited there and in `docs/DIABLO2.md`). Its README documents its own RE/abandonware stance for game files — that is its operator's call, not a licence grant for Blizzard data. |
| **[pvpgn/pvpgn-server](https://github.com/pvpgn/pvpgn-server)** — general source | **GPL-2.0-or-later** (386 of 405 files carry the header) | ⚠️ **Do not copy.** Reading it to learn *design* is fine; copying any function, struct, table, or distinctive parser makes Command Center a derivative and forces GPL on the whole binary. Rust's static linking means there is no "mere aggregation" escape. |
| **`pvpgn/src/common/bnetsrp3.{cpp,h}`, `bigint.{cpp,h}`** | **AGPL-3.0-or-later** | 🛑 **Do not open.** This is the WarCraft III SRP-3 implementation — precisely the file a reimplementer is most tempted to read. AGPL §13 adds a network-use source-disclosure obligation, which is fatal for a hosted service. |
| PvPGN `conf/*.conf.in`, `versioncheck.json`, `lua/`, `bnxplevel.conf` | GPL | ⚠️ The *values* (version hashes, XP tables) are facts and are free. The *files* are GPL works. Regenerate; don't copy. |

### Where to get the cryptography

- **XSHA-1 / "Broken SHA-1"** — BNETDocs, plus the BSD-licensed `wjlafrance/broken-sha1`.
  Already ported and verified against known-answer vectors in `bnetcc-crypto`.
- **NLS / SRP-6 (Blizzard variant)** — the [javaop SRP write-up](http://www.javaop.com/@ron/documents/SRP.html)
  and [SkullSecurity's SRP page](https://www.skullsecurity.org/wiki/SRP), plus RFC 2945 for
  baseline SRP. **Never from `bnetsrp3.cpp`.** Note that PvPGN's own AGPL header points at
  the javaop document — read the document, not the file.
- **CD-key decode and CheckRevision** — BNETDocs, and MBNCSUtil (permissive).

If you ever *must* derive something from PvPGN, do it clean-room: one person reads the GPL
source and writes a plain-English spec; a different person implements from the spec alone.
Keep the paper trail. In practice you should not need to — between BNETDocs and Atlas,
almost everything is available under terms that don't bind you.

---

## 2. The `Davidson & Assocs. v. Jung` problem

This is the one you need a lawyer for, and it is independent of which open-source license
you pick.

*Davidson & Associates v. Jung*, 8th Cir. 2005 — **the bnetd case**, PvPGN's direct
ancestor. The court held that reverse-engineering and emulating Battle.net violated the
DMCA's anti-circumvention provisions, and that the EULA/TOU's prohibition on reverse
engineering was enforceable, waiving the §1201(f) interoperability defence.

Practical consequences that are visible in this ecosystem today:

- PvPGN is developed and hosted largely outside the United States.
- Blizzard has issued takedowns against emulation projects in this space; the D2R
  emulation threads on the PvPGN forums reference exactly this.
- You are running a hub in a jurisdiction you should identify
  deliberately rather than by default.

Things that reduce (not eliminate) exposure and are worth doing anyway:

- **Ship no Blizzard assets.** No MPQs, no `icons.bni`, no `tos.txt` content, no game
  files. BNFTP should serve files the *operator* provides, and the repo should ship
  placeholders.
- **Do not distribute or link client patches.** WarCraft III clients need a client-side
  patch to bypass the server signature check (§3). That patch is a circumvention tool;
  hosting it is a materially different act from running a server. Document that WC3
  requires a patched client and stop there.
- **Do not bundle CD-key generators or key databases.** Validate key *format* and
  uniqueness; never ship keys.
- **Name the project something that is not a Blizzard mark** and do not use Blizzard
  artwork, fonts, or the Battle.net logo. Describe it as compatible with a protocol, not as
  "Battle.net".

### Decision: the Diablo II game server is built from decompilation (2026-09-13)

**Made by tagban (project owner), 2026-09-13**, as `docs/D2GS-RUST.md` §4 asked before any
ported code lands. The Diablo II game server is written from decompiling the retail 1.14d
`Game.exe` in Ghidra and from porting `jaenster/libd2` (MIT), which is itself
decompilation-derived. tagban's reasoning: a working server needs it, and this version of the
game is no longer supported.

This records the choice. It does not change the exposure described above. What the project
still keeps to:

- **No Blizzard bytes in the repository.** Engine tables, excel data and MPQ contents are
  read at run time from the operator's own install (`diablo2.data_dir`). `scripts/d2re/`
  reads them out of a local `Game.exe` and commits nothing it reads.
- **Reimplementation, not copied code.** Ported functions are written fresh in Rust and cite
  the 1.14d address they reproduce, so each can be checked against the binary.
- **Attribution.** Files ported from libd2 carry its MIT notice and name their libd2 source.

---

## 3. WarCraft III will need a patched client, and you cannot fix that

`SID_AUTH_INFO` (S→C) appends a **128-byte RSA signature** that only WarCraft III verifies.
Its stated purpose is to prevent WarCraft III clients from connecting to third-party
servers. You do not have Blizzard's private key, so you cannot produce a valid signature.

This is not an implementation gap; it is a designed-in block. Every WC3-capable private
server in existence works because the *client* is patched to skip the check. Plan for it,
document it for your users, and do not distribute the patch yourself.

Everything else in the WC3 path — SRP logon, clans, `SID_WARCRAFTGENERAL`, W3GS routing —
is implementable normally.

---

## 4. What Command Center itself should be licensed as

Your call, but the considerations:

- **MIT or Apache-2.0 (or dual, the Rust convention)** maximises adoption and lets other
  emulator authors adopt the federation protocol — which matters, because a federation
  protocol is only worth anything if more than one implementation speaks it. Atlas chose
  MIT and that is precisely why it is useful to you today.
- **AGPL** would force node operators to publish modifications, which sounds appealing for
  a semi-trusted federation — but it does not actually give you what you want. Your defence
  against a malicious node is the hub's validation (see `FEDERATION.md` §6), not a licence
  term you would have to sue to enforce.

Recommendation: **Apache-2.0 OR MIT**, dual, as the workspace currently declares. Apache-2.0
additionally gives you an explicit patent grant, which MIT alone does not.

---

## 5. The protocol facts themselves are free

For the avoidance of doubt, and this is well settled: packet identifiers
(`SID_AUTH_INFO = 0x50`), wire layouts, field orders, endianness, byte offsets, state
machines, constants and error codes are unprotectable facts and methods of operation under
17 U.S.C. §102(b) and *Baker v. Selden*, reinforced for interface specifications by
*Google v. Oracle* (2021). Documenting "the header is `0xFF <id:u8> <len:u16le>`" and
implementing it independently is fine.

The DMCA question in §2 is a separate axis entirely, and it is the one that actually bites.
