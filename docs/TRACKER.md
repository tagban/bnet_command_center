# Command Center — Trackers

A tracker is a **server directory**, not federation: servers self-report over UDP, a tracker
collects them, and a page lists them. It is the oldest piece of shared infrastructure in this
ecosystem and the only one every emulator already speaks.

Command Center does both halves. It reports itself to public trackers, and it runs one.

---

## 1. The packet

A fixed **464-byte UDP datagram**, big-endian, packet version `2`, strings null-padded to
fixed widths rather than terminated. Written from BNETDocs document 35 — not from PvPGN's
GPL `tracker.cpp` (see `LEGAL.md`). Sent to port **6114**.

| Offset | Field | Bytes |
|---|---|---|
| 0 | packet version (`2`) | 2 |
| 2 | server port | 2 |
| 4 | flags | 4 |
| 8 | software | 32 |
| 40 | version | 16 |
| 56 | platform | 32 |
| 88 | description | 64 |
| 152 | location | 64 |
| 216 | URL | 96 |
| 312 | contact name | 64 |
| 376 | contact email | 64 |
| 440 | users, channels, games, uptime, total games, total logins | 6 × 4 |

Totals count from server start, not for all time — a restart begins again. That is what PvPGN
means by them, so it is what we mean.

### Never send an empty field

The hard-won one. A tracker writes its collected list as a flat file with `##` between
fields; at least one list site's page then **drops an empty field instead of keeping the
gap**, so every later field slides one column left. With an empty `location`, our version was
published as our software name, our players as our version, and our platform as our
description — and the game codes, having landed in a column meant for a count, were never
drawn as icons.

So nothing goes out blank: anything unset is sent as `none`, which is what other listed
servers do. `crates/bnetccd/src/tracker.rs` enforces it in one place, with a test.

Diagnosing this class of problem: dump a working server's row and yours **cell by cell** and
line them up. `scratchpad/row.py` in the session that found it did exactly that, and the
misalignment was obvious in one pass after an evening of guessing.

### Our own block, appended

The 1999 packet cannot say the address players dial, and its one free-text field is 64 bytes
shared with the server's name. Rather than bend it, Command Center sends the standard packet
**byte for byte** and appends: the four bytes `BNCC`, then JSON.

```json
{"host":"us.bnet.cc","products":["STAR","JSTR","D2XP"],"links":["eu.bnet.cc:6112"]}
```

A tracker that predates this reads the fixed struct it expects and never looks further. Ours
reads the rest and gets a hostname instead of a bare address, every product the server admits
(including ones no list site has an icon for), and — when federation ships — the addresses of
servers reachable through this one.

Keep the whole datagram under 1200 bytes so it is never fragmented, and **size the receiving
buffer for the whole thing**: a buffer sized for the packet alone silently truncates the
extra and leaves it unparseable.

---

## 2. Which games a server runs

The packet has no field for it, so list sites have always carried short codes inside the
description, run together with no separator: `WC2WC3D2LODSC My Server`. The site strips the
codes, prints what is left as the name, and draws the icons after it.

Command Center **works its own list out** and an operator configures nothing: when version
checking is restricted the products it names are exactly who can log on, otherwise every
product the server speaks. Setting `[tracker] description` overrides all of it.

Reading a list, both spellings are accepted — the old short codes, and the product codes a
client logs on with (`W2BN`, `D2XP`). A word is treated as codes only if **all** of it is and
it is **capitalised**; otherwise a server called `DISCO` loses its name to the `SC` inside it,
and one describing itself as open to "all" is read as offering every Blizzard game.

| Code | Product | | Code | Product |
|---|---|---|---|---|
| `SC` | `STAR` | | `WC2` | `W2BN` |
| `SBW` | `SEXP` | | `WC3` | `WAR3` |
| `SSHR` | `SSHR` | | `WCX` | `W3XP` |
| `D1` | `DRTL` | | `CHAT` | chat client |
| `DHR` | `DSHR` | | `OPE` `CLO` `LDR` | open play, closed realm, ladder |
| `D2` | `D2DV` | | `LOD` | `D2XP` |

Two gaps worth knowing on the public sites: `SBW` draws the StarCraft icon because there is
no separate Brood War picture, so asking for both prints StarCraft twice; and `JSTR` has no
icon at all, so a code for it is printed as text beside the server's name. Both are left out
of what we beacon and carried in our own block instead.

---

## 3. Running one

`[tracker]` in `bnetccd.toml`:

| Setting | What it does |
|---|---|
| `advertise_to` | Trackers to report to, `host` or `host:port` (6114 default). Empty: report nowhere. |
| `public_host` | The address players dial. Listed in place of the beacon's source address. |
| `location`, `url`, `contact_name`, `contact_email` | Listed as given; anything empty becomes `none`. |
| `host_listen` | UDP address to collect beacons on, e.g. `0.0.0.0:6114`. Empty: off. |
| `list_listen` | HTTP address for the list page and `/servers.json`. Loopback is fine when the site is fed by push. |
| `prune_after_secs` | Drop a server that has stopped reporting. |

`[tracker_push]` sends the collected list to a website the same way the stats and ladder
pushes work — outbound only, so the site needs no way in and its page keeps working while the
server restarts. On the site: `tracker-push.php` receives it, `tracker.php` shows it, and
`TRACKER_PUSH_TOKEN` in `site-config.php` must match.

Opening UDP 6114 in the firewall is what actually lets other operators list with you.

### Icons

The page shows each game's own Battle.net icon, from `icons.bni`:

```
cargo run -p bnetcc-icons -- /path/to/icons.bni web/icons
```

Then upload the PNGs. This is a separate, hand-run tool because `icons.bni` is an operator's
own game data — none of it belongs in this repository, and a running server has no business
writing images into a website. A game with no icon uploaded simply shows its name.

---

## 4. Listing on the public trackers

`pvpgn.mivabe.nl` is alive and collects beacons on UDP 6114. **Its own how-to page tells
operators to use `bnet.mivabe.nl`, which has not existed in DNS for some time** — no A or
AAAA record from any resolver. Beacons sent there go nowhere, silently, because UDP never
answers. If a server is missing from a list, check DNS for the target host before suspecting
the packet.

`tracker.pvpgn.org` resolves and is also worth reporting to.

Those sites rebuild every 15 minutes or so, so do not judge a change sooner than that.
