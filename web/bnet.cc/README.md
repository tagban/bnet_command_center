# bnet.cc ladder pages

These pages show the server's ladders on bnet.cc: StarCraft, Brood War, Warcraft II (standard
and Iron Man) and Diablo II (with its season), plus a WarCraft III page that stays closed until
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
| `ladder.js`, `ladder.css` | Draw the ladders: game tabs, Iron Man and Diablo II mode/game/class choices, sorting, player search, 50 per page. Every view is a shareable link (`ladder.php?g=d2&m=hardcore&c=barbarian`). |
| `ladder-header.inc.php`, `ladder-footer.inc.php` | Copies of the bnet.cc header and footer, used only when the site's own are not found. |
| `war3-ladder.php`, `war3-ladder.css` | The WarCraft III ladder: an iron frame with gold and red lettering in the spirit of the old classic ladder site, drawn in CSS, with no game art. |
| `ladder-config.sample.php` | Settings; copy it to `ladder-config.php`. |

## Installing

1. Upload the files to the site's root, next to `header.php`.
2. Copy `ladder-config.sample.php` to `ladder-config.php` and set:
   - `LADDER_PUSH_TOKEN` to a long random string;
   - `LADDER_DATA_FILE` to a path PHP can write, ideally outside the web root;
   - `LADDER_HEADER_FILE` / `LADDER_FOOTER_FILE` to the site's own template files. `header.php`
     ends by opening `#content-container`; the footer file must close it, along with the sidebar if
     there is one. If either is left empty, the built-in copy is used.
3. If the host runs Apache and pushes come back `401 bad token`, the host is dropping the
   `Authorization` header. Add this to `.htaccess`:
   ```apache
   SetEnvIf Authorization "(.*)" HTTP_AUTHORIZATION=$1
   ```
4. Add a **Ladder** link to the nav bar in `header.php`: `<a href="/ladder.php" class="menu">Ladder</a>`.
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

## What the snapshot holds

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
