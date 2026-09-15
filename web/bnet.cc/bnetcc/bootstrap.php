<?php
// Shared by every bnet.cc page and widget: the settings, escaping, and the data folder.
// Written for PHP 7.4 and later.

declare(strict_types=1);

if (defined('BNETCC_SITE')) {
    return;
}
define('BNETCC_SITE', true);
define('BNETCC_ROOT', dirname(__DIR__));

// Without site-config.php the public pages still work, on these defaults; the admin page asks for
// the file.
$bnetccConfig = BNETCC_ROOT . '/site-config.php';
define('BNETCC_CONFIGURED', is_file($bnetccConfig));
if (BNETCC_CONFIGURED) {
    require_once $bnetccConfig;
}
foreach ([
    'SITE_DATA_DIR' => BNETCC_ROOT . '/data',
    'SITE_HEADER_FILE' => BNETCC_ROOT . '/header.php',
    'SITE_FOOTER_FILE' => BNETCC_ROOT . '/footer.php',
    'ADMIN_USER' => 'admin',
    'ADMIN_PASSWORD_HASH' => '',
    'GITHUB_REPOS' => [],
    'GITHUB_TOKEN' => '',
    'GITHUB_CACHE_MINUTES' => 30,
    'SERVER_STATUS_URL' => 'http://us.bnet.cc:6116/status.json',
] as $bnetccName => $bnetccDefault) {
    if (!defined($bnetccName)) {
        define($bnetccName, $bnetccDefault);
    }
}

/** Escape text for HTML. */
function h($text): string
{
    return htmlspecialchars((string) $text, ENT_QUOTES | ENT_SUBSTITUTE, 'UTF-8');
}

/** A path inside the data folder, creating the folder (and a deny-all .htaccess) the first time. */
function site_data_path(string $name): string
{
    $dir = rtrim(SITE_DATA_DIR, '/');
    if (!is_dir($dir)) {
        if (!@mkdir($dir, 0755, true) && !is_dir($dir)) {
            throw new RuntimeException('cannot create the data folder');
        }
    }
    $guard = $dir . '/.htaccess';
    if (!is_file($guard)) {
        @file_put_contents($guard, "<IfModule mod_authz_core.c>\n    Require all denied\n</IfModule>\n<IfModule !mod_authz_core.c>\n    Order allow,deny\n    Deny from all\n</IfModule>\n");
    }
    return $dir . '/' . $name;
}

/** Read a JSON file; `$default` when it is missing or unreadable. */
function site_read_json(string $file, $default)
{
    if (!is_file($file)) {
        return $default;
    }
    $text = @file_get_contents($file);
    if ($text === false) {
        return $default;
    }
    $value = json_decode($text, true);
    return $value === null && trim($text) !== 'null' ? $default : $value;
}

/** Write a JSON file whole: to a temporary file beside it, then renamed over it. */
function site_write_json(string $file, $value): bool
{
    $json = json_encode($value, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
    if ($json === false) {
        return false;
    }
    $temp = $file . '.' . bin2hex(random_bytes(6)) . '.tmp';
    if (@file_put_contents($temp, $json, LOCK_EX) === false) {
        return false;
    }
    if (!@rename($temp, $file)) {
        @unlink($temp);
        return false;
    }
    return true;
}

/** Run `$work` holding an exclusive lock named `$name`, so two requests never write at once. */
function site_locked(string $name, callable $work)
{
    $handle = fopen(site_data_path($name . '.lock'), 'c');
    if ($handle === false) {
        throw new RuntimeException('cannot open the lock file');
    }
    try {
        flock($handle, LOCK_EX);
        return $work();
    } finally {
        flock($handle, LOCK_UN);
        fclose($handle);
    }
}

/** "Sep 14, 2026". */
function site_date(int $time): string
{
    return date('M j, Y', $time);
}

/** Include the site's header or footer, or the built-in copy. `$pageTitle` names the page in the header. */
function site_template(string $which, ?string $pageTitle = null): void
{
    $file = $which === 'header' ? (defined('SITE_HEADER_FILE') ? SITE_HEADER_FILE : '') : (defined('SITE_FOOTER_FILE') ? SITE_FOOTER_FILE : '');
    if ($file !== '' && is_file($file)) {
        include $file;
    } else {
        include BNETCC_ROOT . '/ladder-' . $which . '.inc.php';
    }
}

/** The stylesheet the news, releases and widget markup use, linked once per page. */
function site_extras_css(): string
{
    static $linked = false;
    if ($linked) {
        return '';
    }
    $linked = true;
    return '<link rel="stylesheet" href="/extras.css">' . "\n";
}
