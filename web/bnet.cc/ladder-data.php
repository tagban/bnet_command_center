<?php
// The latest ladder standings as JSON, for ladder.js.

declare(strict_types=1);

require __DIR__ . '/ladder-config.php';

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-cache');
$file = LADDER_DATA_FILE;
if (!is_file($file)) {
    http_response_code(404);
    echo '{"error":"no standings yet"}';
    exit;
}
$modified = filemtime($file);
if ($modified !== false) {
    header('Last-Modified: ' . gmdate('D, d M Y H:i:s', $modified) . ' GMT');
}
readfile($file);
