<?php
// Counts a download, then sends the visitor to the file itself. Only files the Files page lists can
// be reached this way; anything else is "not found".

declare(strict_types=1);

require __DIR__ . '/bnetcc/downloads.php';

$path = isset($_GET['f']) ? (string) $_GET['f'] : '';
$path = trim(str_replace('\\', '/', $path), '/');
$root = downloads_root();
$full = $root !== '' && $path !== '' && strpos($path, "\0") === false ? realpath($root . '/' . $path) : false;
$listed = $full !== false && strpos($full, $root . '/') === 0 && isset(downloads_index()['files'][$path]);
if (!$listed) {
    http_response_code(404);
    header('Content-Type: text/plain; charset=utf-8');
    exit('Not found.');
}
downloads_count($path);
header('Cache-Control: no-store');
header('Location: ' . downloads_url($path), true, 302);
