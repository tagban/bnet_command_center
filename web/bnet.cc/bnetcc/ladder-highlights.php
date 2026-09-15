<?php
// The ladder leaders for the home page, read from the standings the server pushes
// (ladder-config.php's LADDER_DATA_FILE).

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';

/** The pushed ladder snapshot, or null before the first push. */
function ladder_snapshot(): ?array
{
    $config = BNETCC_ROOT . '/ladder-config.php';
    if (!defined('LADDER_DATA_FILE') && is_file($config)) {
        require_once $config;
    }
    if (!defined('LADDER_DATA_FILE')) {
        return null;
    }
    $data = site_read_json(LADDER_DATA_FILE, null);
    return is_array($data) && isset($data['games'], $data['diablo2']) ? $data : null;
}

/**
 * The leaders: ['games' => [[label, key, [players…]]…], 'd2' => [[label, query, character]…], 'season' => …].
 * Each rated game lists its top `$top`; Diablo II its best softcore and hardcore expansion characters.
 */
function ladder_leaders(int $top = 3): ?array
{
    $data = ladder_snapshot();
    if ($data === null) {
        return null;
    }
    $labels = ['STAR' => ['StarCraft', 'sc'], 'SEXP' => ['Brood War', 'bw'], 'W2BN' => ['Warcraft II', 'w2']];
    $games = [];
    foreach ($data['games'] as $game) {
        if (!isset($labels[$game['product']])) {
            continue;
        }
        foreach ($game['leagues'] as $league) {
            if ($league['league'] === 'ladder') {
                $games[] = [$labels[$game['product']][0], $labels[$game['product']][1], array_slice($league['players'], 0, $top)];
            }
        }
    }
    $d2 = [];
    foreach ([['Softcore', false, ''], ['Hardcore', true, '&m=hardcore']] as [$label, $hardcore, $query]) {
        foreach ($data['diablo2']['characters'] as $c) {
            if ($c['expansion'] && $c['hardcore'] === $hardcore && $c['rank'] === 1) {
                $d2[] = [$label, $query, $c];
                break;
            }
        }
    }
    return ['games' => $games, 'd2' => $d2, 'season' => $data['diablo2']['season'], 'generated' => (int) $data['generated']];
}
