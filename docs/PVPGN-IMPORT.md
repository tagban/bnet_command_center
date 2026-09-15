# Moving from PvPGN

`bnetcc-pvpgn-import` brings a PvPGN server's players into Command Center, so they keep their
accounts and passwords. It ships beside `bnetccd` in every release archive.

Command Center is an educational project that keeps classic Battle.net games playable on older
computers that can no longer connect. If you and your players have relatively modern computers,
*Diablo II: Resurrected*, *Warcraft III: Reforged*, *StarCraft: Remastered* and *Warcraft II:
Remastered* on the real Battle.net are the better home, and well worth buying.

## What comes across

| From PvPGN | In Command Center |
|---|---|
| Account name and password (`BNET\acct\passhash1`) | The same account, same password, for StarCraft, Brood War, Diablo II and Warcraft II |
| WarCraft III password (`BNET\acct\salt`, `verifier`) | A `Name@realm` account (Command Center keeps WarCraft III logons in their own namespace), with its `WAR3`/`W3XP` records |
| `profile\…` (sex, age, location, description) | The same profile fields |
| `Record\…` (wins, losses, disconnects, ratings, last games) | The same records |
| Creation time, last logon time | The same |
| Locked accounts (`BNET\auth\lock`, with `lockreason`/`lockuntil` if present) | A ban on this server |
| Diablo II realm characters (d2cs `charinfo` + `charsave`) | The same characters, whole `.d2s` saves included, owned by the same accounts |

What does not come across, and is listed in the report instead:

- **E-mail addresses and last-logon addresses.** Command Center does not keep them.
- **Admin and operator rights.** The report names PvPGN's admins; add the ones you want to
  `[admins] accounts` in `bnetccd.toml`.
- **Mutes, friends lists and clans.** Command Center does not store these yet.
- **Diablo II saves older than 1.10.** Only 1.10–1.14 saves are read.
- **PvPGN's binary `cdb` storage.** Move PvPGN to plain-file or SQL storage first.

## Step by step

1. **Stop both servers**, and back up Command Center's database (`bnetccd.db`) and PvPGN's
   data. The tool writes straight into the database file.
2. **Give the tool PvPGN's accounts**, one of:
   - plain-file storage — the directory `storage_path` names (`file:mode=plain;dir=…`):
     `--users /var/lib/pvpgn/users`
   - SQL storage — a dump of the database:
     - MySQL: `mysqldump -u pvpgn -p pvpgn > pvpgn.sql`
     - PostgreSQL: `pg_dump --inserts pvpgn > pvpgn.sql`
     - SQLite: `sqlite3 pvpgn.db .dump > pvpgn.sql`

     then `--sql-dump pvpgn.sql`
3. **Add Diablo II characters** if you ran a closed realm: `--charinfo …/charinfo --charsave …/charsave`
   (the directories in d2cs's configuration).
4. **Read the report** (nothing is written without `--apply`):
   ```sh
   bnetcc-pvpgn-import --users /var/lib/pvpgn/users --db bnetccd.db --realm bncc
   ```
   `--realm` is `[server] realm` from `bnetccd.toml`.
5. **Confirm the password format with an account you know**, such as your own. The tool asks
   for its password (not shown as you type, never stored) and checks which way PvPGN wrote the
   hashes. Use an account that has played WarCraft III too, to confirm those as well:
   ```sh
   bnetcc-pvpgn-import --users /var/lib/pvpgn/users --db bnetccd.db --check-password YourName
   ```
   It prints whether the password hash (and the WarCraft III verifier) matched, and in which
   order. If nothing matches, check the password and try again — do not import.
6. **Import**: the same command with `--check-password YourName --apply`. (If you already know
   the formats, `--hash-order words` and `--srp-order reversed` or `as-is` stand in for the check.)
   WarCraft III accounts are only imported once their format is confirmed; the rest are imported
   either way.
7. **Start Command Center** and log in with an imported account.

Running it again is safe: accounts and characters whose names are already on the server are
left as they are and listed.

## Notes

- Names must follow Command Center's rules (2–15 characters, at least one letter or digit).
  Accounts with other names, and a second account whose name differs only in case, are listed
  and left out.
- The tool was written from PvPGN's data formats as operators see them — the account files, the
  database tables and the Diablo II saves — not from PvPGN's source code (see `docs/LEGAL.md`).
