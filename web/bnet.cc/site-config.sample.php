<?php
// bnet.cc site settings for news, releases and the home-page widgets. Copy this file to
// site-config.php on the web host and fill it in. Never commit or share the copy.

// Where posts and cached data are kept. Outside the web root is best; PHP must be able to write
// here. The ladder pages keep their own LADDER_DATA_FILE in ladder-config.php.
define('SITE_DATA_DIR', __DIR__ . '/data');

// The site's own template, as in ladder-config.php: header.php opens #main-content, the footer
// closes it and adds the sidebar.
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
    'tagban/bnet_command_center' => ['name' => 'BNET Command Center', 'public' => true],
    'tagban/w3ClassicLoader' => ['name' => 'W3 Classic Loader', 'public' => false],
]);

// A GitHub token, needed only for private repositories. Make a fine-grained token with read-only
// "Contents" access to just those repositories. Empty is fine for public ones.
define('GITHUB_TOKEN', '');

// Minutes to keep GitHub's answer before asking again.
define('GITHUB_CACHE_MINUTES', 30);

// The server's public status feed, for the sidebar's Server Stats (read at most every 30 seconds).
define('SERVER_STATUS_URL', 'http://us.bnet.cc:6116/status.json');
