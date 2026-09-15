<?php
// bnet.cc admin: sign in, then write, edit, preview, pin, draft and delete news posts.
//
// Safety: the password is checked against a hash in site-config.php; five wrong tries from one
// address lock it out for fifteen minutes; every change is a POST carrying a per-session token;
// the session cookie is HTTP-only, SameSite=Strict and Secure over HTTPS; pages are never cached
// or framed.

declare(strict_types=1);

require dirname(__DIR__) . '/bnetcc/news.php';
require dirname(__DIR__) . '/bnetcc/downloads.php';

const LOGIN_MAX_FAILURES = 5;
const LOGIN_WINDOW_SECONDS = 900;
const SESSION_IDLE_SECONDS = 7200;
const TITLE_MAX = 120;
const BODY_MAX = 20000;

header('Cache-Control: no-store');
header('X-Frame-Options: DENY');
header('X-Content-Type-Options: nosniff');
header('Referrer-Policy: same-origin');
header("Content-Security-Policy: default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; form-action 'self'; base-uri 'none'");

$https = (!empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off') || (($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '') === 'https');
session_name('bnetcc_admin');
session_set_cookie_params(['lifetime' => 0, 'path' => '/admin/', 'secure' => $https, 'httponly' => true, 'samesite' => 'Strict']);
session_start();

// ---- helpers ------------------------------------------------------------------------------------

function csrf_token(): string
{
    if (empty($_SESSION['csrf'])) {
        $_SESSION['csrf'] = bin2hex(random_bytes(32));
    }
    return $_SESSION['csrf'];
}

function csrf_field(): string
{
    return '<input type="hidden" name="csrf" value="' . h(csrf_token()) . '">';
}

function csrf_ok(): bool
{
    return is_string($_POST['csrf'] ?? null) && !empty($_SESSION['csrf']) && hash_equals($_SESSION['csrf'], $_POST['csrf']);
}

function flash(string $message, bool $ok = true): void
{
    $_SESSION['flash'] = [$message, $ok];
}

function go(string $query = ''): void
{
    $base = rtrim(str_replace('\\', '/', dirname((string) ($_SERVER['SCRIPT_NAME'] ?? '/admin/index.php'))), '/') . '/';
    header('Location: ' . $base . ($query !== '' ? '?' . $query : ''), true, 303);
    exit;
}

function client_ip(): string
{
    return (string) ($_SERVER['REMOTE_ADDR'] ?? 'unknown');
}

/** Failed sign-ins for this address in the last window. */
function login_failures(): int
{
    $all = site_read_json(site_data_path('admin-logins.json'), []);
    $mine = $all[client_ip()] ?? null;
    if (!is_array($mine) || time() - (int) $mine['first'] > LOGIN_WINDOW_SECONDS) {
        return 0;
    }
    return (int) $mine['count'];
}

function login_record(bool $success): void
{
    site_locked('admin-logins', function () use ($success) {
        $file = site_data_path('admin-logins.json');
        $all = site_read_json($file, []);
        $now = time();
        foreach ($all as $ip => $row) {
            if (!is_array($row) || $now - (int) $row['first'] > LOGIN_WINDOW_SECONDS) {
                unset($all[$ip]);
            }
        }
        $ip = client_ip();
        if ($success) {
            unset($all[$ip]);
        } else {
            $row = $all[$ip] ?? ['first' => $now, 'count' => 0];
            $row['count'] = (int) $row['count'] + 1;
            $all[$ip] = $row;
        }
        site_write_json($file, $all);
    });
}

function signed_in(): bool
{
    if (empty($_SESSION['admin'])) {
        return false;
    }
    if (time() - (int) ($_SESSION['seen'] ?? 0) > SESSION_IDLE_SECONDS) {
        $_SESSION = [];
        return false;
    }
    $_SESSION['seen'] = time();
    return true;
}

function page(string $title, string $body): void
{
    $flash = '';
    if (!empty($_SESSION['flash'])) {
        [$message, $ok] = $_SESSION['flash'];
        unset($_SESSION['flash']);
        $flash = '<p class="' . ($ok ? 'ok' : 'err') . '">' . h($message) . '</p>';
    }
    $nav = signed_in()
        ? '<nav><a href="./">Posts</a> · <a href="./?new=1">New post</a> · <a href="./?files=1">Files</a> · <a href="../news.php">View news</a> · <form method="post" class="inline">' . csrf_field() . '<input type="hidden" name="action" value="logout"><button class="link">Sign out</button></form></nav>'
        : '<nav><a href="../">bnet.cc</a></nav>';
    echo '<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1">';
    echo '<title>' . h($title) . ' - bnet.cc admin</title><link rel="stylesheet" href="admin.css"><script src="admin.js" defer></script></head><body>';
    echo '<header><span class="logo">BNET.cc</span><span class="sub">admin</span>' . $nav . '</header><main>' . $flash . $body . '</main></body></html>';
    exit;
}

// ---- first run: no settings or no password yet ---------------------------------------------------

if (!BNETCC_CONFIGURED) {
    page('Set up', '<h1>Set up</h1><p>Copy <code>site-config.sample.php</code> to <code>site-config.php</code> on the web host, then reload this page.</p>');
}

if (trim((string) ADMIN_PASSWORD_HASH) === '') {
    $hash = '';
    $problem = '';
    if (($_SERVER['REQUEST_METHOD'] ?? '') === 'POST' && csrf_ok()) {
        $a = (string) ($_POST['password'] ?? '');
        $b = (string) ($_POST['again'] ?? '');
        if (strlen($a) < 12) {
            $problem = 'Use at least 12 characters.';
        } elseif (!hash_equals($a, $b)) {
            $problem = 'The two passwords differ.';
        } else {
            $hash = password_hash($a, PASSWORD_DEFAULT);
        }
    }
    $body = '<h1>Set the admin password</h1><p>No password is set yet. Choose one below: this page turns it into a hash for you to paste into <code>site-config.php</code>. Nothing is saved here.</p>';
    if ($problem !== '') {
        $body .= '<p class="err">' . h($problem) . '</p>';
    }
    if ($hash !== '') {
        $body .= '<p class="ok">Put this line in <code>site-config.php</code>, replacing the empty one, then reload this page:</p><pre class="code">define(\'ADMIN_PASSWORD_HASH\', \'' . h($hash) . '\');</pre>';
    }
    $body .= '<form method="post" class="card">' . csrf_field() . '<label>Password<input type="password" name="password" autocomplete="new-password" required minlength="12"></label><label>Again<input type="password" name="again" autocomplete="new-password" required minlength="12"></label><button>Make the hash</button></form>';
    page('Set up', $body);
}

// ---- changes (POST) -----------------------------------------------------------------------------

if (($_SERVER['REQUEST_METHOD'] ?? '') === 'POST') {
    $action = (string) ($_POST['action'] ?? '');
    if (!csrf_ok()) {
        flash('That form had expired. Try again.', false);
        go();
    }
    if ($action === 'login') {
        if (login_failures() >= LOGIN_MAX_FAILURES) {
            flash('Too many wrong passwords. Try again in 15 minutes.', false);
            go();
        }
        $user = (string) ($_POST['user'] ?? '');
        $password = (string) ($_POST['password'] ?? '');
        if (hash_equals(strtolower((string) ADMIN_USER), strtolower($user)) && password_verify($password, (string) ADMIN_PASSWORD_HASH)) {
            login_record(true);
            session_regenerate_id(true);
            $_SESSION = ['admin' => true, 'seen' => time(), 'csrf' => bin2hex(random_bytes(32))];
            go();
        }
        login_record(false);
        flash('Wrong name or password.', false);
        go();
    }
    if (!signed_in()) {
        go();
    }
    if ($action === 'logout') {
        $_SESSION = [];
        session_regenerate_id(true);
        flash('Signed out.');
        go();
    }
    if ($action === 'save' || $action === 'preview') {
        $id = (string) ($_POST['id'] ?? '');
        $title = trim((string) ($_POST['title'] ?? ''));
        $body = str_replace("\r\n", "\n", (string) ($_POST['body'] ?? ''));
        $pinned = !empty($_POST['pinned']);
        $draft = !empty($_POST['draft']);
        $problem = '';
        if ($title === '' || mb_strlen($title) > TITLE_MAX) {
            $problem = 'A title is needed, up to ' . TITLE_MAX . ' characters.';
        } elseif (trim($body) === '' || mb_strlen($body) > BODY_MAX) {
            $problem = 'The post needs some text, up to ' . BODY_MAX . ' characters.';
        } elseif ($id !== '' && news_find($id) === null) {
            $problem = 'That post no longer exists.';
        }
        if ($action === 'save' && $problem === '') {
            try {
                $post = news_save($id, $title, $body, $pinned, $draft, (string) ADMIN_USER);
                flash($draft ? 'Saved as a draft.' : ($id === '' ? 'Posted.' : 'Saved.'));
                go('edit=' . rawurlencode($post['id']));
            } catch (Throwable $e) {
                $problem = 'The post could not be saved: check that the data folder is writable.';
            }
        }
        $_SESSION['draft_form'] = ['id' => $id, 'title' => $title, 'body' => $body, 'pinned' => $pinned, 'draft' => $draft, 'preview' => $action === 'preview' && $problem === ''];
        if ($problem !== '') {
            flash($problem, false);
        }
        go($id !== '' ? 'edit=' . rawurlencode($id) . '&form=1' : 'new=1&form=1');
    }
    if ($action === 'files_save') {
        $cat = (string) ($_POST['cat'] ?? '');
        $index = downloads_index(true);
        $changes = [];
        foreach ((array) ($_POST['meta'] ?? []) as $path => $entry) {
            $path = (string) $path;
            if (!isset($index['folders'][$path]) && !isset($index['files'][$path])) {
                continue;
            }
            $entry = is_array($entry) ? $entry : [];
            $changes[$path] = [
                'title' => isset($index['folders'][$path]) ? mb_substr((string) ($entry['title'] ?? ''), 0, 60) : '',
                'about' => mb_substr((string) ($entry['about'] ?? ''), 0, 1000),
                'featured' => isset($index['files'][$path]) && !empty($entry['featured']),
            ];
        }
        try {
            downloads_save_meta($changes);
            flash('Saved.');
        } catch (Throwable $e) {
            flash('Could not save: check that the data folder is writable.', false);
        }
        go('files=1&cat=' . rawurlencode($cat));
    }
    if ($action === 'files_rescan') {
        downloads_index(true);
        flash('The file list is up to date.');
        go('files=1&cat=' . rawurlencode((string) ($_POST['cat'] ?? '')));
    }
    if ($action === 'delete') {
        try {
            flash(news_delete((string) ($_POST['id'] ?? '')) ? 'Post deleted.' : 'That post was already gone.');
        } catch (Throwable $e) {
            flash('The post could not be deleted: check that the data folder is writable.', false);
        }
        go();
    }
    go();
}

// ---- pages (GET) --------------------------------------------------------------------------------

if (!signed_in()) {
    $locked = login_failures() >= LOGIN_MAX_FAILURES;
    $body = '<h1>Sign in</h1>';
    if ($locked) {
        $body .= '<p class="err">Too many wrong passwords from this address. Try again in 15 minutes.</p>';
    }
    $body .= '<form method="post" class="card">' . csrf_field() . '<input type="hidden" name="action" value="login"><label>Name<input type="text" name="user" autocomplete="username" required></label><label>Password<input type="password" name="password" autocomplete="current-password" required></label><button' . ($locked ? ' disabled' : '') . '>Sign in</button></form>';
    page('Sign in', $body);
}

if (isset($_GET['files'])) {
    $index = downloads_index();
    $meta = downloads_meta();
    $cat = (string) ($_GET['cat'] ?? '');
    if (!isset($index['folders'][$cat])) {
        $cat = '';
    }
    $folder = $index['folders'][$cat];
    $body = '<h1>Files</h1>';
    if (downloads_root() === '') {
        $body .= '<p class="err">The downloads folder was not found: check DOWNLOADS_DIR in site-config.php.</p>';
    }
    $trail = [];
    foreach (downloads_trail($cat, $index, $meta) as [$path, $name]) {
        $trail[] = $path === $cat ? '<b>' . h($name) . '</b>' : '<a href="./?files=1&amp;cat=' . h(rawurlencode($path)) . '">' . h($name) . '</a>';
    }
    $body .= '<p class="meta">' . implode(' › ', $trail) . ' · <a href="../files.php' . ($cat !== '' ? '?cat=' . h(rawurlencode($cat)) : '') . '">view on the site</a></p>';
    $body .= '<p class="help">Files come straight from the downloads folder: upload one and it appears here within two minutes (or press Refresh). Names and descriptions are optional. A description can also be a text file beside the file, named like <code>File.zip.txt</code>, or <code>_about.txt</code> for a folder.</p>';
    $body .= '<form method="post" class="card wide">' . csrf_field() . '<input type="hidden" name="cat" value="' . h($cat) . '">';
    if ($cat !== '') {
        $m = $meta[$cat] ?? [];
        $body .= '<label>Folder name<input type="text" name="meta[' . h($cat) . '][title]" maxlength="60" placeholder="' . h(downloads_folder_title($cat, [])) . '" value="' . h((string) ($m['title'] ?? '')) . '"></label>';
        $body .= '<label>Folder description<textarea name="meta[' . h($cat) . '][about]" rows="3" maxlength="1000">' . h((string) ($m['about'] ?? '')) . '</textarea></label>';
    }
    if ($folder['folders']) {
        $body .= '<table class="list"><tr><th>Folder</th><th>Files</th><th>Name on the site</th></tr>';
        foreach ($folder['folders'] as $path) {
            $f = $index['folders'][$path];
            $m = $meta[$path] ?? [];
            $body .= '<tr><td><a href="./?files=1&amp;cat=' . h(rawurlencode($path)) . '">' . h($f['name']) . '/</a></td><td>' . (int) $f['count'] . '</td>'
                . '<td><input type="text" name="meta[' . h($path) . '][title]" maxlength="60" placeholder="' . h(downloads_folder_title($path, [])) . '" value="' . h((string) ($m['title'] ?? '')) . '">'
                . '<input type="hidden" name="meta[' . h($path) . '][about]" value="' . h((string) ($m['about'] ?? '')) . '"></td></tr>';
        }
        $body .= '</table><br>';
    }
    if ($folder['files']) {
        $body .= '<table class="list"><tr><th>File</th><th>Description</th><th>Featured</th></tr>';
        foreach ($folder['files'] as $path) {
            $path = (string) $path;
            $f = $index['files'][$path];
            $m = $meta[$path] ?? [];
            $body .= '<tr><td>' . h($f['name']) . '<div class="meta">' . h(downloads_size($f['size'])) . ' · ' . h(site_date($f['time'])) . '</div></td>'
                . '<td><textarea name="meta[' . h($path) . '][about]" rows="2" maxlength="1000" placeholder="' . h((string) ($index['files'][$path]['note'] ?? '')) . '">' . h((string) ($m['about'] ?? '')) . '</textarea></td>'
                . '<td><input type="checkbox" name="meta[' . h($path) . '][featured]" value="1"' . (!empty($m['featured']) ? ' checked' : '') . '></td></tr>';
        }
        $body .= '</table><br>';
    }
    if (!$folder['folders'] && !$folder['files']) {
        $body .= '<p class="meta">This folder is empty.</p>';
    }
    $body .= '<div class="buttons"><button name="action" value="files_save">Save</button><button name="action" value="files_rescan" class="secondary">Refresh the file list</button></div></form>';
    page('Files', $body);
}

if (isset($_GET['new']) || isset($_GET['edit'])) {
    $editing = isset($_GET['edit']) ? news_find((string) $_GET['edit']) : null;
    if (isset($_GET['edit']) && $editing === null) {
        flash('That post no longer exists.', false);
        go();
    }
    $form = $editing ?? ['id' => '', 'title' => '', 'body' => '', 'pinned' => false, 'draft' => false];
    $preview = false;
    if (isset($_GET['form']) && !empty($_SESSION['draft_form']) && ($_SESSION['draft_form']['id'] ?? '') === $form['id']) {
        $form = array_merge($form, $_SESSION['draft_form']);
        $preview = !empty($_SESSION['draft_form']['preview']);
    }
    unset($_SESSION['draft_form']);
    $body = '<h1>' . ($editing ? 'Edit post' : 'New post') . '</h1>';
    if ($editing && empty($editing['draft'])) {
        $body .= '<p class="meta">Published ' . h(site_date((int) $editing['created'])) . ' · <a href="../news.php?p=' . h(rawurlencode($editing['slug'])) . '">view it</a></p>';
    }
    if ($preview) {
        $body .= '<div class="preview"><div class="preview-label">Preview</div><div class="preview-title">' . h($form['title']) . '</div><div class="post-body">' . markup($form['body']) . '</div></div>';
    }
    $body .= '<form method="post" class="card wide">' . csrf_field() . '<input type="hidden" name="id" value="' . h($form['id']) . '">';
    $body .= '<label>Title<input type="text" name="title" maxlength="' . TITLE_MAX . '" value="' . h($form['title']) . '" required></label>';
    $body .= '<label>Post<textarea name="body" rows="16" maxlength="' . BODY_MAX . '" required>' . h($form['body']) . '</textarea></label>';
    $body .= '<p class="help"><code>**bold**</code> <code>*italic*</code> <code>`code`</code> <code>[link text](https://…)</code> · a line starting <code>- </code> is a list item · <code>## </code> starts a heading · a blank line starts a new paragraph</p>';
    $body .= '<div class="checks"><label class="check"><input type="checkbox" name="pinned" value="1"' . (!empty($form['pinned']) ? ' checked' : '') . '> Pin to the top</label><label class="check"><input type="checkbox" name="draft" value="1"' . (!empty($form['draft']) ? ' checked' : '') . '> Draft (not shown on the site)</label></div>';
    $body .= '<div class="buttons"><button name="action" value="save">' . ($editing ? 'Save' : 'Post') . '</button><button name="action" value="preview" class="secondary">Preview</button><a href="./">Cancel</a></div></form>';
    if ($editing) {
        $body .= '<form method="post" class="danger-zone" data-confirm="Delete this post? This cannot be undone.">' . csrf_field() . '<input type="hidden" name="action" value="delete"><input type="hidden" name="id" value="' . h($editing['id']) . '"><button class="danger">Delete post</button></form>';
    }
    page($editing ? 'Edit post' : 'New post', $body);
}

$posts = news_all();
$body = '<h1>News posts</h1><p><a class="button" href="./?new=1">Write a new post</a></p>';
if (!$posts) {
    $body .= '<p class="meta">No posts yet.</p>';
} else {
    $body .= '<table class="list"><tr><th>Title</th><th>Date</th><th>State</th><th></th></tr>';
    foreach ($posts as $p) {
        $state = !empty($p['draft']) ? '<span class="tag draft">Draft</span>' : '<span class="tag live">Published</span>';
        if (!empty($p['pinned'])) {
            $state .= ' <span class="tag pin">Pinned</span>';
        }
        $body .= '<tr><td><a href="./?edit=' . h(rawurlencode($p['id'])) . '">' . h($p['title']) . '</a></td><td>' . h(site_date((int) $p['created'])) . '</td><td>' . $state . '</td><td>'
            . (empty($p['draft']) ? '<a href="../news.php?p=' . h(rawurlencode($p['slug'])) . '">View</a>' : '') . '</td></tr>';
    }
    $body .= '</table>';
}
page('Posts', $body);
