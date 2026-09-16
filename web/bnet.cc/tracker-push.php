<?php
// Receives the tracked server list ([tracker_push] url) and keeps it for tracker.php.
// POST only, bearer token required.

declare(strict_types=1);

require __DIR__ . '/bnetcc/tracker.php';

const MAX_BYTES = 1024 * 1024;

function finish(int $status, string $message): void
{
    http_response_code($status);
    header('Content-Type: application/json; charset=utf-8');
    header('Cache-Control: no-store');
    echo json_encode(['status' => $status, 'message' => $message]);
    exit;
}

if (($_SERVER['REQUEST_METHOD'] ?? '') !== 'POST') {
    header('Allow: POST');
    finish(405, 'POST only');
}
// The list comes from the same server as the stats push, so it may use the same token: a site
// that already accepts that push needs no new secret for this one.
$token = defined('TRACKER_PUSH_TOKEN') ? (string) TRACKER_PUSH_TOKEN : '';
if ($token === '' && defined('SERVER_PUSH_TOKEN')) {
    $token = (string) SERVER_PUSH_TOKEN;
}
if (strlen($token) < 16) {
    finish(500, 'neither TRACKER_PUSH_TOKEN nor SERVER_PUSH_TOKEN is set in site-config.php (16 characters or more)');
}
// Some hosts drop the Authorization header before PHP sees it; the site's .htaccess puts it
// back, and these are the names it can arrive under.
$auth = $_SERVER['HTTP_AUTHORIZATION'] ?? $_SERVER['REDIRECT_HTTP_AUTHORIZATION'] ?? '';
if ($auth === '' && function_exists('getallheaders')) {
    foreach (getallheaders() as $name => $value) {
        if (strcasecmp($name, 'Authorization') === 0) {
            $auth = $value;
        }
    }
}
if (!preg_match('/^Bearer\s+(.+)$/i', $auth, $m) || !hash_equals($token, trim($m[1]))) {
    finish(401, 'bad token');
}
$body = file_get_contents('php://input', false, null, 0, MAX_BYTES + 1);
if ($body === false || strlen($body) > MAX_BYTES) {
    finish(413, 'too large');
}
$data = json_decode($body, true, 16);
if (!is_array($data)) {
    finish(400, 'not a server list');
}
// An empty list is legitimate — it means nothing is beaconing at us right now.
foreach ($data as $server) {
    if (!is_array($server) || !isset($server['address'])) {
        finish(400, 'not a server list');
    }
}
try {
    tracker_store($data);
} catch (Throwable $e) {
    finish(500, 'cannot save the list: check that the data folder is writable');
}
finish(200, 'saved');
