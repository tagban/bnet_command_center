<?php
// The downloads folder, listed as it stands: every folder and file under DOWNLOADS_DIR, with
// friendly names, file types, descriptions and download counts.
//
// Descriptions come from the admin page, or from a text file beside a file (`Name.zip.txt`) or in a
// folder (`_about.txt`); those text files are never listed. Hidden files (`.htaccess`, `.DS_Store`)
// and index pages are skipped. The listing is rebuilt at most every DOWNLOADS_SCAN_SECONDS.

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

const DOWNLOADS_SCAN_SECONDS = 120;
const DOWNLOADS_MAX_ENTRIES = 5000;

/** Built-in names for folder names, used unless the admin page or DOWNLOADS_NAMES says otherwise. */
function downloads_default_names(): array
{
    return [
        'addons' => 'Add-ons', 'bots' => 'Bots', 'maps' => 'Maps', 'source' => 'Source Code',
        'win' => 'Windows', 'mac' => 'Mac', 'nix' => 'Linux & Unix', 'linux' => 'Linux',
        'sc' => 'StarCraft', 'sc2' => 'StarCraft II', 'bw' => 'Brood War', 'wc2' => 'Warcraft II',
        'wc3' => 'WarCraft III', 'd2' => 'Diablo II', 'd1' => 'Diablo',
    ];
}

/** The downloads folder on disk, without a trailing slash, or '' if it is missing. */
function downloads_root(): string
{
    $root = realpath((string) DOWNLOADS_DIR);
    return $root !== false && is_dir($root) ? rtrim($root, '/') : '';
}

/** The public URL of a file or folder path relative to the downloads folder. */
function downloads_url(string $path): string
{
    $segments = array_map('rawurlencode', array_filter(explode('/', $path), 'strlen'));
    return rtrim((string) DOWNLOADS_URL, '/') . '/' . implode('/', $segments);
}

/** Whether a directory entry is listed. */
function downloads_listable(string $name, bool $isDir): bool
{
    if ($name === '' || $name[0] === '.' || $name[0] === '_') {
        return false;
    }
    if (!$isDir && preg_match('/^index\.(html?|php)$/i', $name)) {
        return false;
    }
    return true;
}

/**
 * Walk the folder: ['folders' => [path => folder], 'files' => [path => file]]. Paths are relative,
 * '' is the top. A folder: name, parent, folders[], files[], count and size (including subfolders),
 * newest. A file: name, folder, size, time, ext.
 */
function downloads_scan(): array
{
    $root = downloads_root();
    $index = ['scanned' => time(), 'folders' => ['' => ['name' => '', 'parent' => null, 'folders' => [], 'files' => [], 'count' => 0, 'size' => 0, 'newest' => 0]], 'files' => []];
    if ($root === '') {
        return $index;
    }
    $sidecars = [];
    $queue = [''];
    $entries = 0;
    while ($queue && $entries < DOWNLOADS_MAX_ENTRIES) {
        $path = array_shift($queue);
        $dir = $root . ($path === '' ? '' : '/' . $path);
        $names = @scandir($dir);
        if ($names === false) {
            continue;
        }
        natcasesort($names);
        foreach ($names as $name) {
            if ($name === '.' || $name === '..' || ++$entries > DOWNLOADS_MAX_ENTRIES) {
                continue;
            }
            $full = $dir . '/' . $name;
            $rel = $path === '' ? $name : $path . '/' . $name;
            if (is_link($full)) {
                continue;
            }
            if (is_dir($full)) {
                if (!downloads_listable($name, true)) {
                    continue;
                }
                $index['folders'][$rel] = ['name' => $name, 'parent' => $path, 'folders' => [], 'files' => [], 'count' => 0, 'size' => 0, 'newest' => 0];
                $index['folders'][$path]['folders'][] = $rel;
                $queue[] = $rel;
            } elseif (is_file($full)) {
                if ($name === '_about.txt') {
                    $sidecars[$path] = $full;
                    continue;
                }
                if (!downloads_listable($name, false)) {
                    continue;
                }
                $index['files'][$rel] = ['name' => $name, 'folder' => $path, 'size' => (int) filesize($full), 'time' => (int) filemtime($full), 'ext' => strtolower(pathinfo($name, PATHINFO_EXTENSION))];
                $index['folders'][$path]['files'][] = $rel;
            }
        }
    }
    // A `.txt` beside a file of the same name is that file's description, not a download.
    $notes = [];
    foreach ($index['files'] as $rel => $file) {
        if ($file['ext'] === 'txt' && isset($index['files'][substr($rel, 0, -4)])) {
            $notes[substr($rel, 0, -4)] = trim((string) @file_get_contents($root . '/' . $rel, false, null, 0, 2000));
            unset($index['files'][$rel]);
            $folder = &$index['folders'][$file['folder']];
            $folder['files'] = array_values(array_diff($folder['files'], [$rel]));
            unset($folder);
        }
    }
    foreach ($sidecars as $path => $full) {
        $index['folders'][$path]['about'] = trim((string) @file_get_contents($full, false, null, 0, 2000));
    }
    foreach ($notes as $rel => $text) {
        $index['files'][$rel]['note'] = $text;
    }
    // Totals roll up to every parent folder.
    foreach ($index['files'] as $file) {
        $path = $file['folder'];
        while ($path !== null) {
            $index['folders'][$path]['count']++;
            $index['folders'][$path]['size'] += $file['size'];
            $index['folders'][$path]['newest'] = max($index['folders'][$path]['newest'], $file['time']);
            $path = $index['folders'][$path]['parent'];
        }
    }
    return $index;
}

/** The listing, from the saved copy while it is fresh. `$fresh` rebuilds it now. */
function downloads_index(bool $fresh = false): array
{
    $file = site_data_path('downloads-index.json');
    $index = $fresh ? null : site_read_json($file, null);
    if (!is_array($index) || time() - (int) ($index['scanned'] ?? 0) >= DOWNLOADS_SCAN_SECONDS) {
        $index = downloads_scan();
        site_write_json($file, $index);
    }
    return $index;
}

/** Titles, descriptions and featured flags set on the admin page, by path. */
function downloads_meta(): array
{
    $meta = site_read_json(site_data_path('downloads-meta.json'), []);
    return is_array($meta) ? $meta : [];
}

/** Save the admin page's settings for some paths: [path => ['title' => …, 'about' => …, 'featured' => bool]]. */
function downloads_save_meta(array $changes): void
{
    site_locked('downloads-meta', function () use ($changes) {
        $meta = downloads_meta();
        foreach ($changes as $path => $entry) {
            $entry = array_filter([
                'title' => trim((string) ($entry['title'] ?? '')),
                'about' => trim((string) ($entry['about'] ?? '')),
                'featured' => !empty($entry['featured']),
            ]);
            if ($entry) {
                $meta[$path] = $entry;
            } else {
                unset($meta[$path]);
            }
        }
        if (!site_write_json(site_data_path('downloads-meta.json'), $meta)) {
            throw new RuntimeException('cannot save');
        }
    });
}

/** A folder's display name. */
function downloads_folder_title(string $path, array $meta): string
{
    if ($path === '') {
        return 'Files';
    }
    if (!empty($meta[$path]['title'])) {
        return (string) $meta[$path]['title'];
    }
    $name = basename($path);
    $names = downloads_default_names();
    if (defined('DOWNLOADS_NAMES') && is_array(DOWNLOADS_NAMES)) {
        $names = array_change_key_case(DOWNLOADS_NAMES) + $names;
    }
    return $names[strtolower($name)] ?? $name;
}

/** A folder's description. */
function downloads_folder_about(string $path, array $index, array $meta): string
{
    return (string) ($meta[$path]['about'] ?? $index['folders'][$path]['about'] ?? '');
}

/** A file's description. */
function downloads_file_about(string $path, array $index, array $meta): string
{
    return (string) ($meta[$path]['about'] ?? $index['files'][$path]['note'] ?? '');
}

/** The folders from the top down to `$path`: [[path, title]…]. */
function downloads_trail(string $path, array $index, array $meta): array
{
    $trail = [];
    while ($path !== null && isset($index['folders'][$path])) {
        array_unshift($trail, [$path, downloads_folder_title($path, $meta)]);
        $path = $index['folders'][$path]['parent'];
    }
    return $trail;
}

/** What kind of file an extension is. */
function downloads_type(string $ext): string
{
    $types = [
        'zip' => 'Zip archive', 'rar' => 'RAR archive', '7z' => '7-Zip archive', 'gz' => 'Gzip archive', 'tgz' => 'Tar archive',
        'tar' => 'Tar archive', 'exe' => 'Windows program', 'msi' => 'Windows installer', 'dmg' => 'Mac disk image',
        'pkg' => 'Mac installer', 'sit' => 'StuffIt archive', 'sitx' => 'StuffIt archive', 'hqx' => 'BinHex archive',
        'jar' => 'Java program', 'scm' => 'StarCraft map', 'scx' => 'Brood War map', 'pud' => 'Warcraft II map',
        'w3m' => 'WarCraft III map', 'w3x' => 'Frozen Throne map', 'sc2map' => 'StarCraft II map', 'mpq' => 'MPQ archive',
        'pdf' => 'PDF document', 'txt' => 'Text file', 'iso' => 'Disc image', 'deb' => 'Debian package', 'appimage' => 'Linux program',
    ];
    return $types[$ext] ?? ($ext !== '' ? strtoupper($ext) . ' file' : 'File');
}

/** "4.4 MB". */
function downloads_size(int $bytes): string
{
    if ($bytes >= 1073741824) {
        return number_format($bytes / 1073741824, 1) . ' GB';
    }
    if ($bytes >= 1048576) {
        return number_format($bytes / 1048576, 1) . ' MB';
    }
    return number_format(max(1, (int) ceil($bytes / 1024))) . ' KB';
}

// ---- download counts ----------------------------------------------------------------------------

/** Downloads by path. */
function downloads_counts(): array
{
    $counts = site_read_json(site_data_path('downloads-counts.json'), []);
    return is_array($counts) ? $counts : [];
}

/**
 * Count a download of `$path` by this visitor: once a day per visitor and file, and never for
 * something that says it is a bot.
 */
function downloads_count(string $path): void
{
    $agent = (string) ($_SERVER['HTTP_USER_AGENT'] ?? '');
    if ($agent === '' || preg_match('/bot|crawl|spider|slurp|preview|fetch|curl|wget|python|monitor/i', $agent)) {
        return;
    }
    $visitor = hash('sha256', (string) ($_SERVER['REMOTE_ADDR'] ?? '') . '|' . $path . '|' . date('Y-m-d'));
    site_locked('downloads-counts', function () use ($path, $visitor) {
        $seenFile = site_data_path('downloads-seen.json');
        $seen = site_read_json($seenFile, []);
        if (!is_array($seen) || ($seen['day'] ?? '') !== date('Y-m-d')) {
            $seen = ['day' => date('Y-m-d'), 'keys' => []];
        }
        if (isset($seen['keys'][$visitor])) {
            return;
        }
        $seen['keys'][$visitor] = 1;
        site_write_json($seenFile, $seen);
        $counts = downloads_counts();
        $counts[$path] = (int) ($counts[$path] ?? 0) + 1;
        site_write_json(site_data_path('downloads-counts.json'), $counts);
    });
}

/** The link a visitor downloads `$path` through: counted, or straight to the file. */
function downloads_link(string $path): string
{
    return DOWNLOADS_COUNT ? '/download.php?f=' . rawurlencode($path) : downloads_url($path);
}
