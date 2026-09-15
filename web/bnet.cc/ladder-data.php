<?php
// The latest ladder standings as JSON, for ladder.js.

declare(strict_types=1);

require __DIR__ . '/bnetcc/bootstrap.php';

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-cache');
$file = (string) LADDER_DATA_FILE;
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
