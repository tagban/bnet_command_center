<?php
// The game server's feed, as its stats push delivers it (server-push.php): the latest snapshot,
// and the history the site builds from every push, so charts survive server restarts and still
// show while the server is down.
//
// History: `points`, one per five minutes for eight days (the most seen in each); `days`, one per
// day for 180 days; `record`, the most players ever online at once.

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

const SERVER_FRESH_SECONDS = 180;
const SERVER_POINT_SECONDS = 300;
const SERVER_POINTS_KEEP = 8 * 24 * 12;
const SERVER_DAYS_KEEP = 180;

/** The latest snapshot and how old it is: [data or null, seconds since it arrived]. */
function server_feed(): array
{
    $file = site_data_path('server.json');
    $data = site_read_json($file, null);
    if (!is_array($data)) {
        return [null, PHP_INT_MAX];
    }
    $age = time() - (int) @filemtime($file);
    return [$data, max(0, $age)];
}

/** Whether the server pushed recently enough to call it online. */
function server_online(int $age): bool
{
    return $age <= SERVER_FRESH_SECONDS;
}

/** The history the site has built. */
function server_history(): array
{
    $history = site_read_json(site_data_path('server-history.json'), []);
    return is_array($history) ? $history + ['points' => [], 'days' => [], 'record' => null] : ['points' => [], 'days' => [], 'record' => null];
}

/** Take in a pushed snapshot: save it and fold it into the history. */
function server_store(array $data): void
{
    site_locked('server', function () use ($data) {
        if (!site_write_json(site_data_path('server.json'), $data)) {
            throw new RuntimeException('cannot save the snapshot');
        }
        $history = server_history();
        $now = time();
        $users = (int) ($data['users_online'] ?? 0);
        $games = count((array) ($data['game_list'] ?? [])) + count((array) ($data['diablo2_games'] ?? []));
        $byProduct = [];
        foreach ((array) ($data['products'] ?? []) as $p) {
            if (!empty($p['online'])) {
                $byProduct[(string) $p['product']] = (int) $p['online'];
            }
        }

        $bucket = intdiv($now, SERVER_POINT_SECONDS) * SERVER_POINT_SECONDS;
        $points = $history['points'];
        $last = $points ? count($points) - 1 : -1;
        if ($last >= 0 && (int) $points[$last]['t'] === $bucket) {
            $p = $points[$last];
            $p['users'] = max((int) $p['users'], $users);
            $p['games'] = max((int) $p['games'], $games);
            foreach ($byProduct as $code => $n) {
                $p['by'][$code] = max((int) ($p['by'][$code] ?? 0), $n);
            }
            $points[$last] = $p;
        } else {
            $points[] = ['t' => $bucket, 'users' => $users, 'games' => $games, 'by' => $byProduct ?: new stdClass()];
        }
        $history['points'] = array_values(array_slice($points, -SERVER_POINTS_KEEP));

        $date = date('Y-m-d', $now);
        $hosted = 0;
        foreach ((array) ($data['products'] ?? []) as $p) {
            $hosted += (int) ($p['games_hosted_24h'] ?? 0);
        }
        $days = $history['days'];
        $lastDay = $days ? count($days) - 1 : -1;
        if ($lastDay >= 0 && $days[$lastDay]['date'] === $date) {
            $days[$lastDay]['peak_users'] = max((int) $days[$lastDay]['peak_users'], $users);
            $days[$lastDay]['players'] = max((int) $days[$lastDay]['players'], (int) ($data['players_24h'] ?? 0));
            $days[$lastDay]['games_hosted'] = max((int) $days[$lastDay]['games_hosted'], $hosted);
        } else {
            $days[] = ['date' => $date, 'peak_users' => $users, 'players' => (int) ($data['players_24h'] ?? 0), 'games_hosted' => $hosted];
        }
        $history['days'] = array_values(array_slice($days, -SERVER_DAYS_KEEP));

        if (!is_array($history['record']) || $users > (int) $history['record']['users']) {
            $history['record'] = ['users' => $users, 't' => $now];
        }
        site_write_json(site_data_path('server-history.json'), $history);
    });
}

/** "3h 20m". */
function server_duration(int $seconds): string
{
    if ($seconds >= 86400) {
        return floor($seconds / 86400) . 'd ' . floor(($seconds % 86400) / 3600) . 'h';
    }
    if ($seconds >= 3600) {
        return floor($seconds / 3600) . 'h ' . floor(($seconds % 3600) / 60) . 'm';
    }
    return max(0, (int) floor($seconds / 60)) . 'm';
}

/** "5 minutes ago". */
function server_ago(int $seconds): string
{
    if ($seconds < 90) {
        return 'just now';
    }
    if ($seconds < 5400) {
        return round($seconds / 60) . ' minutes ago';
    }
    if ($seconds < 129600) {
        return round($seconds / 3600) . ' hours ago';
    }
    return round($seconds / 86400) . ' days ago';
}

/** A product code's short name. */
function server_product(string $code): string
{
    $names = [
        'STAR' => 'StarCraft', 'SEXP' => 'Brood War', 'JSTR' => 'StarCraft (JP)', 'SSHR' => 'StarCraft Shareware',
        'W2BN' => 'Warcraft II', 'DRTL' => 'Diablo', 'DSHR' => 'Diablo Shareware', 'D2DV' => 'Diablo II',
        'D2XP' => 'Lord of Destruction', 'WAR3' => 'WarCraft III', 'W3XP' => 'Frozen Throne', 'CHAT' => 'Chat',
    ];
    return $names[$code] ?? $code;
}

/**
 * An SVG line chart of `$series` ([time => value]) from `$from` to `$to` seconds, drawn as an area
 * with the peak labelled. Gaps longer than 15 minutes (the server down) break the line.
 */
function server_chart(array $series, int $from, int $to, int $width = 520, int $height = 110, string $label = ''): string
{
    $pad = ['l' => 28, 'r' => 6, 't' => 10, 'b' => 18];
    $w = $width - $pad['l'] - $pad['r'];
    $h = $height - $pad['t'] - $pad['b'];
    $max = max(1, $series ? max($series) : 0);
    $max = (int) (ceil($max / 5) * 5);
    $x = function (int $t) use ($from, $to, $w, $pad) {
        return $pad['l'] + ($to > $from ? ($t - $from) / ($to - $from) : 0) * $w;
    };
    $y = function (float $v) use ($max, $h, $pad) {
        return $pad['t'] + $h - ($v / $max) * $h;
    };
    $svg = '<svg class="chart" viewBox="0 0 ' . $width . ' ' . $height . '" width="100%" role="img" aria-label="' . h($label) . '">';
    foreach ([0, 0.5, 1] as $f) {
        $gy = round($y($max * $f), 1);
        $svg .= '<line x1="' . $pad['l'] . '" x2="' . ($width - $pad['r']) . '" y1="' . $gy . '" y2="' . $gy . '" class="chart-grid"/>';
        $svg .= '<text x="' . ($pad['l'] - 4) . '" y="' . ($gy + 3) . '" class="chart-axis" text-anchor="end">' . (int) round($max * $f) . '</text>';
    }
    $runs = [];
    $run = [];
    $prev = null;
    ksort($series);
    foreach ($series as $t => $v) {
        if ($t < $from || $t > $to) {
            continue;
        }
        if ($prev !== null && $t - $prev > 900) {
            $runs[] = $run;
            $run = [];
        }
        $run[] = [round($x((int) $t), 1), round($y((float) $v), 1)];
        $prev = $t;
    }
    if ($run) {
        $runs[] = $run;
    }
    $base = round($y(0), 1);
    foreach ($runs as $r) {
        $line = implode(' ', array_map(function ($p) {
            return $p[0] . ',' . $p[1];
        }, $r));
        if (count($r) === 1) {
            $svg .= '<circle cx="' . $r[0][0] . '" cy="' . $r[0][1] . '" r="2" class="chart-dot"/>';
            continue;
        }
        $svg .= '<polygon points="' . $r[0][0] . ',' . $base . ' ' . $line . ' ' . end($r)[0] . ',' . $base . '" class="chart-area"/>';
        $svg .= '<polyline points="' . $line . '" class="chart-line"/>';
    }
    if (!$runs) {
        $svg .= '<text x="' . ($pad['l'] + $w / 2) . '" y="' . ($pad['t'] + $h / 2) . '" class="chart-axis" text-anchor="middle">No data yet</text>';
    }
    return $svg . '</svg>';
}
