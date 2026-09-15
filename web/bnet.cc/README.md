# bnet.cc site additions

News posts, GitHub releases, and ladders for bnet.cc, built into the site's own look: its
`header.php`, `footer.php` and `sidebar.php` (updated copies are here), its colours and its images.
Everything is plain PHP with JSON files in a private data folder, and runs on PHP 7.4 or later. No
database is needed.

## What to upload

| File | What it is |
|---|---|
| `header.php` | The site header, with **News**, **Releases** and **Server** added to the nav and a page title per page. |
| `sidebar.php` | The sidebar: Active Projects, then **Latest Releases**, then Server Stats, now from the server's push (with players today and a link to the server page). Before the first push arrives it reads the live status feed, at most every 30 seconds and never waiting more than 3. |
| `footer.php` | Unchanged. |
| `index.php` | The front page: **Latest News**, **Server Activity**, **Ladder Leaders** and **Recently Added Files**. |
| `server.php` | The server page: online now, the last 24 hours and 7 days, players by game, who's online, open games (maps and Diablo II games too), channels, recent ladder games and a daily table. |
| `server-push.php` | Receives the server's stats push, keeps it, and builds the site's own history from it. |
| `files.php` | The downloads folder, browsable: categories with counts, Featured, Recently Added, Most Downloaded, folders with breadcrumbs, sortable file lists, and search. Old `files.php?cat=` links keep working. |
| `download.php` | Counts a download and sends the visitor on to the file. Only files the Files page lists can be reached this way. |
| `news.php` | All news, 10 to a page, and each post at `news.php?p=post-title`. |
| `releases.php` | Each project's recent releases, with the latest release's notes and its download links. |
| `admin/` | The news admin (`/admin/`): sign in, then write, preview, edit, pin, draft and delete posts. |
| `widgets/` | `news.php`, `releases.php`, `activity.php`, `ladder.php`, `downloads.php`: boxes to include anywhere. |
| `bnetcc/` | Shared code (its `.htaccess` blocks direct access). |
| `extras.css` | Styles for news, releases and the widgets, on top of the site's own classes. |
| `site-config.sample.php` | Settings; copy it to `site-config.php`. |
| ladder files | See **Ladders** below. |

## Setting up

1. Upload everything above to the site root.
2. Copy `site-config.sample.php` to `site-config.php` and check:
   - `SITE_DATA_DIR`: a folder PHP can write, ideally outside the web root. Inside it, the site
     writes a deny-all `.htaccess` the first time.
   - `GITHUB_REPOS`: the projects to track. `'public' => false` hides GitHub links for a private
     repository.
   - `GITHUB_TOKEN`: needed only for the private W3 Classic Loader. On GitHub, create a
     **fine-grained token** with read-only **Contents** access to that repository alone, and paste
     it here yourself.
3. Open `https://www.bnet.cc/admin/`. With no password set, the page turns a password you choose into
   a hash. Paste it into `ADMIN_PASSWORD_HASH` in `site-config.php`, reload, and sign in as
   `ADMIN_USER`.

## Files

The Files page reads `/downloads/` as it stands. Upload a file by FTP into any folder, and it
appears within two minutes (or press **Refresh the file list** on the admin page). Folders become
categories, and nested folders become breadcrumbs.

- **Names:** folder names get friendly names on their own (`win` is Windows, `nix` is Linux & Unix,
  `sc2` is StarCraft II, `wc2` is Warcraft II…). Set any other name on the admin page's **Files**
  section, or in `DOWNLOADS_NAMES`.
- **Descriptions:** write them on the admin page, or upload a text file beside the file with the same
  name plus `.txt` (`RippleChatBot.zip.txt`), or `_about.txt` in a folder. Those text files are not
  listed as downloads. Descriptions use the news formatting.
- **Featured:** tick a file on the admin page to list it at the top of the Files page.
- **Download counts:** links go through `download.php`, which counts each visitor once a day per
  file, skips obvious bots, and redirects to the file. Set `DOWNLOADS_COUNT` to `false` to link
  straight to the files.
- **Not listed:** hidden files (`.htaccess`, `.DS_Store`), anything starting with `_`, index pages
  and symbolic links. Apache's own directory listing of `/downloads/` can be turned off with
  `Options -Indexes` in `downloads/.htaccess`; the Files page does not need it.

## Server data

Every minute the server sends bnet.cc a snapshot of itself: its stats push. `server-push.php` keeps
the latest one and adds it to the site's own history (five-minute points for eight days, one row per
day for 180 days, and the most players ever online). So the charts don't reset when the server
restarts, and the pages still show the last known state, marked Offline, while it is down.

On the server, in `bnetccd.toml` (this replaces the old stats push URL):

```toml
[stats_push]
url = "https://www.bnet.cc/server-push.php"
token = "a long random string"        # the same as SERVER_PUSH_TOKEN in site-config.php
interval_secs = 60
include_users = true                  # names in Who's Online; false shows counts only
```

What the snapshot holds, beyond the totals: players online per game, players seen in the last 24
hours, games hosted per game in the last 24 hours, public channels (defined `public` or `listed`;
other channels are only counted), open games with their type and StarCraft/Warcraft II map,
Diablo II realm games, and the last 30 ladder results with rating changes. A password-protected
game's name, a private channel's name and anyone's address are never sent. The same data is at
the server's `http://<server>:6116/status.json`.

## The news admin

- Formatting: `**bold**`, `*italic*`, `` `code` ``, `[link text](https://…)`, bare `https://` links,
  lines starting `- ` for lists, `## ` for headings, and blank lines between paragraphs. Everything
  else is shown as text; no HTML gets through.
- Pinned posts stay on top. Drafts are never shown on the site. A published post keeps its address
  when you edit its title.
- Security: five wrong passwords from one address lock sign-in for 15 minutes. Every change is a
  POST carrying a per-session token. The cookie is HTTP-only, `SameSite=Strict`, and Secure over
  HTTPS. Admin pages are never cached or framed.

## Releases

Releases come from GitHub's API and are kept for `GITHUB_CACHE_MINUTES` (30). One visitor's page
refreshes them; everyone else is served the saved copy, which is also kept when GitHub is
unreachable. Unauthenticated, GitHub allows 60 requests an hour from the host, which is plenty for
three projects every half hour.

---

## Ladders

These pages show the server's ladders on bnet.cc: StarCraft, Brood War, Warcraft II (standard
and Iron Man) and Diablo II (with its season), plus a WarCraft III tab that stays closed until
matchmaking exists. The server pushes its standings to the site, the same way it pushes its
stats. The site keeps the last copy it received, so the pages still work while the server is
down.

```
bnetccd ──POST /ladder-push.php (Bearer token)──▶ data/ladder.json
browser ──GET  /ladder.php ─▶ ladder.js ──GET /ladder-data.php──▶ data/ladder.json
```

| File | What it is |
|---|---|
| `ladder-push.php` | Receives the push. POST only, bearer token, 8 MB cap. Saves the file whole (written beside it, then renamed). |
| `ladder-data.php` | Serves the latest standings to `ladder.js`. |
| `ladder.php` | The ladder page: the site's `header.php` and footer around `#ladder`. |
| `ladder.js`, `ladder.css` | Draw the ladders: game tabs, Iron Man, Diablo II mode/game/class and WarCraft III game choices, sorting, player search, 50 per page. Every view is a shareable link (`ladder.php?g=d2&m=hardcore&c=barbarian`). |
| `ladder-header.inc.php`, `ladder-footer.inc.php` | Copies of the bnet.cc header and footer, used only when the site's own are not found. |
| `war3-ladder.php` | Sends old links to the ladder page's WarCraft III tab (`ladder.php?g=w3`). |
| `ladder-config.sample.php` | Settings; copy it to `ladder-config.php`. |

### Installing the ladders

1. Upload the files to the site's root, next to `header.php`.
2. Copy `ladder-config.sample.php` to `ladder-config.php` and set:
   - `LADDER_PUSH_TOKEN` to a long random string;
   - `LADDER_DATA_FILE` to a path PHP can write, ideally outside the web root;
   - `LADDER_HEADER_FILE` / `LADDER_FOOTER_FILE` to the site's own template files. `header.php`
     ends by opening `#main-content`, and `footer.php` closes it through `sidebar.php`. If either is
     left empty, the built-in copy is used.
3. If the host runs Apache and pushes come back `401 bad token`, the host is dropping the
   `Authorization` header. Add this to `.htaccess`:
   ```apache
   SetEnvIf Authorization "(.*)" HTTP_AUTHORIZATION=$1
   ```
4. The updated `header.php` already has the **Ladder** link.
5. On the server, in `bnetccd.toml`:
   ```toml
   [ladder_push]
   url = "https://www.bnet.cc/ladder-push.php"
   token = "the same long random string"   # empty = use [stats_push] token
   interval_secs = 300
   ```
   Restart the server. It pushes at startup, every `interval_secs`, and within a minute of a
   counted ladder game or the end of a Diablo II season.

To check the push by hand, the server serves the same JSON on its public status port:
`http://<server>:6116/ladder.json`.

### What the snapshot holds

Only what the games' own ladder screens show anyone: account names on the StarCraft and Warcraft II
ladders, and character names on the Diablo II ladder (never the account behind a character).

- `server_name`, `generated` (Unix seconds), `max_rank` (500), `ladder_min_wins` (10),
  `min_game_seconds` (120)
- `games[]`: `product` (`STAR`, `SEXP`, `W2BN`, `WAR3`, `W3XP`), `name`, `open`, and
  `leagues[]` with `league` (`ladder` or `ironman`) and `players[]` holding `rank`, `name`, `wins`,
  `losses`, `disconnects`, `rating`, `high_rating` and `last_game`
- `diablo2`: `season` (`number`, `started`), and `characters[]` holding `rank` and `class_rank`
  (null past 500), `name`, `class`, `level`, `experience`, `hardcore`, `dead` and `expansion`

The pages build their tables with JavaScript, from text only, so a player's name can never inject
markup. The PHP is kept to receiving, storing and serving the file.
