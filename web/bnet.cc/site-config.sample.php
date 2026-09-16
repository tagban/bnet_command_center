<?php
// bnet.cc site settings for news, releases and the home-page widgets. Copy this file to
// site-config.php on the web host and fill it in. Never commit or share the copy.

// Where posts, the ladder standings, the server's history and cached data are kept. Outside the web
// root is best; PHP must be able to write here.
define('SITE_DATA_DIR', __DIR__ . '/data');

// The site's own template: header.php opens #main-content, the footer closes it and adds the
// sidebar.
define('SITE_HEADER_FILE', __DIR__ . '/header.php');
define('SITE_FOOTER_FILE', __DIR__ . '/footer.php');

// The news admin (/admin/). Leave the hash empty and open /admin/ to make one: the page turns a
// password into a hash for you to paste here, and stores nothing.
define('ADMIN_USER', 'Tagban');
define('ADMIN_PASSWORD_HASH', '');

// GitHub projects whose releases the site shows, in this order: 'owner/repo' => settings.
// 'public' => false hides the GitHub links for a private repository visitors cannot open.
define('GITHUB_REPOS', [
    'tagban/invigoration' => ['name' => 'Invigoration 2', 'public' => true],
    'tagban/bnet_command_center' => ['name' => 'Command Center', 'public' => true],
    'tagban/w3ClassicLoader' => ['name' => 'W3 Classic Loader', 'public' => false],
]);

// A GitHub token, needed only for private repositories. Make a fine-grained token with read-only
// "Contents" access to just those repositories. Empty is fine for public ones.
define('GITHUB_TOKEN', '');

// Minutes to keep GitHub's answer before asking again.
define('GITHUB_CACHE_MINUTES', 30);

// The token the server sends with its ladder push ([ladder_push] token in bnetccd.toml), for
// ladder-push.php. Long and random.
define('LADDER_PUSH_TOKEN', '');

// The token the server sends with its stats push ([stats_push] token in bnetccd.toml), for
// server-push.php. Long and random. The site keeps what it receives and builds its own history
// from it.
define('SERVER_PUSH_TOKEN', '');

// The game server's address as players type it, shown on the server pages.
define('SERVER_ADDRESS', 'us.bnet.cc:6112');

// The downloads folder the Files page lists, and its address on the site.
define('DOWNLOADS_DIR', __DIR__ . '/downloads');
define('DOWNLOADS_URL', '/downloads');

// Count downloads (once a day per visitor per file) by sending download links through download.php.
define('DOWNLOADS_COUNT', true);

// Names for folders, by folder name, over the built-in ones (win => Windows, sc2 => StarCraft II, …).
// A folder's name and description can also be set on the admin page.
define('DOWNLOADS_NAMES', [
    // 'Classic Battle.net' => 'Classic Battle.net',
]);

// Before the first push arrives, the sidebar reads the public status feed directly (at most every
// 30 seconds).
define('SERVER_STATUS_URL', 'http://us.bnet.cc:6116/status.json');

// The token the server sends with its tracker push ([tracker_push] token in bnetccd.toml), for
// tracker-push.php. Leave empty to accept SERVER_PUSH_TOKEN, when both pushes come from the
// same server.
define('TRACKER_PUSH_TOKEN', '');

// The address other operators point their servers at to be listed, shown on the tracker page.
define('TRACKER_ADDRESS', 'us.bnet.cc');

// Game icons for the tracker page, taken from icons.bni. A game with no icon here shows its
// name instead, so this is optional.
define('TRACKER_ICON_DIR', __DIR__ . '/icons');
define('TRACKER_ICON_URL', '/icons');
