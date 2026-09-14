<?php
// Copy this file to ladder-config.php on the web host and fill it in. Never commit the copy.

// The token the server sends with each ladder push: the [ladder_push] token in bnetccd.toml,
// or its [stats_push] token when [ladder_push] token is empty. Long and random.
define('LADDER_PUSH_TOKEN', '');

// Where the latest standings are kept. Outside the web root is best; the directory must be
// writable by PHP.
define('LADDER_DATA_FILE', __DIR__ . '/data/ladder.json');

// The site's own page template, so ladder.php looks like the rest of bnet.cc. header.php opens
// #content-container; the footer file closes it (and may add the sidebar). Leave a name empty to
// use the ladder page's built-in copy of the bnet.cc header or footer.
define('LADDER_HEADER_FILE', __DIR__ . '/header.php');
define('LADDER_FOOTER_FILE', __DIR__ . '/footer.php');
