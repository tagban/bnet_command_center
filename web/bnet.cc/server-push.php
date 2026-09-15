<?php
// Receives the game server's stats push ([stats_push] url) and keeps it for the site's server
// pages, adding it to the history. POST only, bearer token required.

declare(strict_types=1);

require __DIR__ . '/bnetcc/server.php';

const MAX_BYTES = 2 * 1024 * 1024;

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
$token = defined('SERVER_PUSH_TOKEN') ? (string) SERVER_PUSH_TOKEN : '';
if (strlen($token) < 16) {
    finish(500, 'SERVER_PUSH_TOKEN is not set in site-config.php (16 characters or more)');
}
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
if (!is_array($data) || !isset($data['server_name'], $data['users_online'])) {
    finish(400, 'not a server snapshot');
}
try {
    server_store($data);
} catch (Throwable $e) {
    finish(500, 'cannot save the snapshot: check that the data folder is writable');
}
finish(200, 'saved');
