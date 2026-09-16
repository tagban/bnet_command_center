<?php
// The tracked server list, as pushed by the game server's [tracker_push].
//
// Servers self-report over UDP to our tracker; the game server collects them and posts the
// whole list here on an interval. The site only ever reads what it was handed, so the page
// keeps working while the game server is restarting.

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

/** How long a pushed list is worth showing before the page says it is stale. */
const TRACKER_FRESH_SECONDS = 2100;

/**
 * Save the pushed list.
 *
 * @param array<int, array<string, mixed>> $servers
 */
function tracker_store(array $servers): void
{
    site_locked('tracker', function () use ($servers) {
        $payload = ['pushed' => time(), 'servers' => array_values($servers)];
        if (!site_write_json(site_data_path('tracker.json'), $payload)) {
            throw new RuntimeException('cannot save the server list');
        }
    });
}

/**
 * The stored list: `pushed` (unix seconds, 0 when nothing has arrived) and `servers`.
 *
 * @return array{pushed: int, servers: array<int, array<string, mixed>>}
 */
function tracker_list(): array
{
    $stored = site_read_json(site_data_path('tracker.json'), null);
    if (!is_array($stored)) {
        return ['pushed' => 0, 'servers' => []];
    }
    return [
        'pushed' => (int) ($stored['pushed'] ?? 0),
        'servers' => array_values((array) ($stored['servers'] ?? [])),
    ];
}

/** Whether a list this old should still be shown as current. */
function tracker_is_fresh(int $pushed): bool
{
    return $pushed > 0 && (time() - $pushed) < TRACKER_FRESH_SECONDS;
}

/**
 * The games a listed server offers, as names to print.
 *
 * @param array<string, mixed> $server
 * @return array<int, array{code: string, name: string}>
 */
function tracker_games(array $server): array
{
    $out = [];
    foreach ((array) ($server['offers'] ?? []) as $offer) {
        if (!empty($offer['game'])) {
            $out[] = ['code' => (string) ($offer['code'] ?? ''), 'name' => (string) ($offer['name'] ?? '')];
        }
    }
    return $out;
}

/**
 * The notes that are not games — open or closed play, ladder.
 *
 * @param array<string, mixed> $server
 * @return array<int, string>
 */
function tracker_notes(array $server): array
{
    $out = [];
    foreach ((array) ($server['offers'] ?? []) as $offer) {
        if (empty($offer['game'])) {
            $out[] = (string) ($offer['name'] ?? '');
        }
    }
    return $out;
}

/**
 * Where a game's icon lives, or '' when we have none for it.
 *
 * These are the icons this server hands its own clients, taken out of `icons.bni` — the same
 * artwork players see beside their name in chat, rather than another site's pictures. They are
 * generated from an operator's own game files and uploaded to TRACKER_ICON_DIR; a game with no
 * icon there simply shows its name.
 */
function tracker_icon_url(string $code): string
{
    $code = preg_replace('/[^A-Z0-9]/', '', strtoupper($code)) ?? '';
    if ($code === '') {
        return '';
    }
    $dir = defined('TRACKER_ICON_DIR') ? (string) TRACKER_ICON_DIR : __DIR__ . '/../icons';
    $base = defined('TRACKER_ICON_URL') ? (string) TRACKER_ICON_URL : 'icons';
    return is_file($dir . '/' . $code . '.png') ? $base . '/' . $code . '.png' : '';
}

/** A link to somewhere else, made safe to click: http(s) only, and never a bare scheme-less. */
function tracker_link(string $url): string
{
    $url = trim($url);
    if ($url === '' || $url === 'none') {
        return '';
    }
    if (!preg_match('~^https?://~i', $url)) {
        $url = 'https://' . $url;
    }
    return filter_var($url, FILTER_VALIDATE_URL) === false ? '' : $url;
}

/** A duration in seconds as something readable: "3 minutes", "2 days". */
function tracker_duration(int $seconds): string
{
    $steps = [[86400, 'day'], [3600, 'hour'], [60, 'minute'], [1, 'second']];
    foreach ($steps as [$size, $unit]) {
        if ($seconds >= $size) {
            $n = intdiv($seconds, $size);
            return $n . ' ' . $unit . ($n === 1 ? '' : 's');
        }
    }
    return 'just now';
}
