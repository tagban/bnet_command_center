<?php
// GitHub releases for the projects in GITHUB_REPOS: fetched from GitHub's API, kept in the data
// folder for GITHUB_CACHE_MINUTES, and served from that copy (the last good one if GitHub is
// unreachable).

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

const RELEASES_KEEP = 10;
const RELEASES_RETRY_SECONDS = 300;

/** The tracked projects: 'owner/repo' => ['name', 'public']. */
function releases_repos(): array
{
    $repos = defined('GITHUB_REPOS') && is_array(GITHUB_REPOS) ? GITHUB_REPOS : [];
    $out = [];
    foreach ($repos as $repo => $settings) {
        if (!preg_match('#^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$#', (string) $repo)) {
            continue;
        }
        $settings = is_array($settings) ? $settings : [];
        $out[$repo] = [
            'name' => (string) ($settings['name'] ?? $repo),
            'public' => (bool) ($settings['public'] ?? true),
        ];
    }
    return $out;
}

/** A GitHub API request: [status, body]; status 0 when it failed. */
function releases_http(string $url): array
{
    $headers = ['User-Agent: bnet.cc-site', 'Accept: application/vnd.github+json', 'X-GitHub-Api-Version: 2022-11-28'];
    $token = defined('GITHUB_TOKEN') ? trim((string) GITHUB_TOKEN) : '';
    if ($token !== '') {
        $headers[] = 'Authorization: Bearer ' . $token;
    }
    if (function_exists('curl_init')) {
        $curl = curl_init($url);
        curl_setopt_array($curl, [
            CURLOPT_HTTPHEADER => $headers,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_FOLLOWLOCATION => false,
            CURLOPT_CONNECTTIMEOUT => 5,
            CURLOPT_TIMEOUT => 5,
        ]);
        $body = curl_exec($curl);
        $status = $body === false ? 0 : (int) curl_getinfo($curl, CURLINFO_RESPONSE_CODE);
        if (PHP_VERSION_ID < 80000) {
            curl_close($curl); // closes itself from PHP 8
        }
        return [$status, $body === false ? '' : (string) $body];
    }
    $context = stream_context_create(['http' => ['header' => implode("\r\n", $headers), 'follow_location' => 0, 'timeout' => 5, 'ignore_errors' => true]]);
    $body = @file_get_contents($url, false, $context);
    $status = 0;
    foreach ($http_response_header ?? [] as $line) {
        if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
            $status = (int) $m[1];
        }
    }
    return [$body === false ? 0 : $status, $body === false ? '' : $body];
}

function releases_cache_file(string $repo): string
{
    return site_data_path('releases-' . str_replace('/', '--', $repo) . '.json');
}

/** A project's releases, newest first, from the cached copy (refreshed when stale). */
function releases_for(string $repo): array
{
    $file = releases_cache_file($repo);
    $cache = site_read_json($file, null);
    $ttl = max(5, (int) (defined('GITHUB_CACHE_MINUTES') ? GITHUB_CACHE_MINUTES : 30)) * 60;
    $age = is_array($cache) ? time() - (int) ($cache['fetched'] ?? 0) : PHP_INT_MAX;
    $wait = is_array($cache) && !empty($cache['error']) ? RELEASES_RETRY_SECONDS : $ttl;
    if ($age < $wait) {
        return $cache;
    }
    // One request refreshes; others keep serving the old copy meanwhile.
    $lock = fopen(site_data_path('releases.lock'), 'c');
    if ($lock === false || !flock($lock, LOCK_EX | LOCK_NB)) {
        return is_array($cache) ? $cache : ['fetched' => 0, 'releases' => [], 'error' => 'refreshing'];
    }
    try {
        return releases_refresh($repo, is_array($cache) ? $cache : null);
    } finally {
        flock($lock, LOCK_UN);
        fclose($lock);
    }
}

/** Ask GitHub now and save what it says. */
function releases_refresh(string $repo, ?array $previous): array
{
    [$status, $body] = releases_http('https://api.github.com/repos/' . $repo . '/releases?per_page=' . RELEASES_KEEP);
    $list = $status === 200 ? json_decode($body, true) : null;
    if (!is_array($list)) {
        $error = $status === 0 ? 'GitHub could not be reached' : ($status === 404 ? 'not found (a private repository needs GITHUB_TOKEN)' : 'GitHub answered ' . $status);
        $cache = ['fetched' => time(), 'releases' => $previous['releases'] ?? [], 'error' => $error];
        site_write_json(releases_cache_file($repo), $cache);
        return $cache;
    }
    $releases = [];
    foreach ($list as $r) {
        if (!is_array($r) || !empty($r['draft'])) {
            continue;
        }
        $assets = [];
        foreach ((array) ($r['assets'] ?? []) as $a) {
            $assets[] = ['id' => (int) $a['id'], 'name' => (string) $a['name'], 'size' => (int) $a['size'], 'url' => (string) $a['browser_download_url']];
        }
        $releases[] = [
            'tag' => (string) ($r['tag_name'] ?? ''),
            'name' => (string) ($r['name'] ?: ($r['tag_name'] ?? '')),
            'published' => strtotime((string) ($r['published_at'] ?? '')) ?: 0,
            'url' => (string) ($r['html_url'] ?? ''),
            'prerelease' => !empty($r['prerelease']),
            'notes' => mb_substr((string) ($r['body'] ?? ''), 0, 4000),
            'assets' => $assets,
        ];
    }
    $cache = ['fetched' => time(), 'releases' => $releases, 'error' => ''];
    site_write_json(releases_cache_file($repo), $cache);
    return $cache;
}

/** Every tracked project's latest release: [repo, settings, release or null], newest first. */
function releases_latest(): array
{
    $rows = [];
    foreach (releases_repos() as $repo => $settings) {
        $releases = releases_for($repo)['releases'] ?? [];
        $rows[] = [$repo, $settings, $releases[0] ?? null];
    }
    usort($rows, function ($a, $b) {
        return ($b[2]['published'] ?? 0) <=> ($a[2]['published'] ?? 0);
    });
    return $rows;
}

/** "2.4 MB". */
function releases_size(int $bytes): string
{
    if ($bytes >= 1048576) {
        return number_format($bytes / 1048576, 1) . ' MB';
    }
    return number_format(max(1, (int) round($bytes / 1024))) . ' KB';
}
