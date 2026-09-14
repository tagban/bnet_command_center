<?php
// Receives the ladder standings the BNET Command Center server pushes ([ladder_push] url) and
// keeps the latest copy for ladder.php. POST only, bearer token required.

declare(strict_types=1);

const MAX_BYTES = 8 * 1024 * 1024;

function finish(int $status, string $message): void
{
    http_response_code($status);
    header('Content-Type: application/json; charset=utf-8');
    header('Cache-Control: no-store');
    echo json_encode(['status' => $status, 'message' => $message]);
    exit;
}

$config = __DIR__ . '/ladder-config.php';
if (!is_file($config)) {
    finish(500, 'ladder-config.php is missing');
}
require $config;

if (($_SERVER['REQUEST_METHOD'] ?? '') !== 'POST') {
    header('Allow: POST');
    finish(405, 'POST only');
}

// The Authorization header, wherever this host puts it. Apache often drops it unless the
// .htaccess passes it on (see README.md).
$auth = $_SERVER['HTTP_AUTHORIZATION'] ?? $_SERVER['REDIRECT_HTTP_AUTHORIZATION'] ?? '';
if ($auth === '' && function_exists('getallheaders')) {
    foreach (getallheaders() as $name => $value) {
        if (strcasecmp($name, 'Authorization') === 0) {
            $auth = $value;
        }
    }
}
$token = defined('LADDER_PUSH_TOKEN') ? (string) LADDER_PUSH_TOKEN : '';
if (strlen($token) < 16) {
    finish(500, 'LADDER_PUSH_TOKEN is not set (16 characters or more)');
}
if (!preg_match('/^Bearer\s+(.+)$/i', $auth, $m) || !hash_equals($token, trim($m[1]))) {
    finish(401, 'bad token');
}

$length = (int) ($_SERVER['CONTENT_LENGTH'] ?? 0);
if ($length > MAX_BYTES) {
    finish(413, 'too large');
}
$body = file_get_contents('php://input', false, null, 0, MAX_BYTES + 1);
if ($body === false || strlen($body) > MAX_BYTES) {
    finish(413, 'too large');
}
$data = json_decode($body, true, 32);
if (!is_array($data) || !isset($data['generated'], $data['games'], $data['diablo2']) || !is_array($data['games'])) {
    finish(400, 'not a ladder snapshot');
}

$file = LADDER_DATA_FILE;
$dir = dirname($file);
if (!is_dir($dir) && !mkdir($dir, 0755, true)) {
    finish(500, 'cannot create the data directory');
}
// Write beside the file and rename over it, so a reader never sees half a file.
$temp = $file . '.' . bin2hex(random_bytes(6)) . '.tmp';
if (file_put_contents($temp, $body, LOCK_EX) === false || !rename($temp, $file)) {
    @unlink($temp);
    finish(500, 'cannot save the standings');
}
finish(200, 'saved');
