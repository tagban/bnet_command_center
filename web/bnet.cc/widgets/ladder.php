<?php
// Home or sidebar widget: the ladder leaders from the standings the server pushes. Include it:
//   include __DIR__ . '/widgets/ladder.php';   (inside a PHP block)

require_once dirname(__DIR__) . '/bnetcc/ladder-highlights.php';

(function () {
    $leaders = ladder_leaders(3);
    echo site_extras_css();
    echo '<b class="header">Ladder Leaders</b>' . "\n";
    if ($leaders === null) {
        echo '<p class="post-empty">The ladder has not been published yet.</p>' . "\n";
        return;
    }
    echo '<table class="lad-widget" width="100%" cellpadding="3" cellspacing="1">' . "\n";
    foreach ($leaders['games'] as [$label, $key, $players]) {
        echo '<tr><td class="lad-game" colspan="2"><a href="/ladder.php?g=' . h($key) . '">' . h($label) . '</a></td></tr>' . "\n";
        if (!$players) {
            echo '<tr><td class="row dim" colspan="2">No one ranked yet</td></tr>' . "\n";
        }
        foreach ($players as $i => $p) {
            $cls = $i % 2 ? 'rowAlt' : 'row';
            echo '<tr><td class="' . $cls . '">' . (int) $p['rank'] . '. ' . h($p['name']) . '</td><td class="' . $cls . ' lad-num">' . number_format((int) $p['rating']) . '</td></tr>' . "\n";
        }
    }
    echo '<tr><td class="lad-game" colspan="2"><a href="/ladder.php?g=d2">Diablo II</a> <span class="dim">Season ' . (int) $leaders['season']['number'] . '</span></td></tr>' . "\n";
    if (!$leaders['d2']) {
        echo '<tr><td class="row dim" colspan="2">No ladder characters yet</td></tr>' . "\n";
    }
    foreach ($leaders['d2'] as $i => [$label, $query, $c]) {
        $cls = $i % 2 ? 'rowAlt' : 'row';
        echo '<tr><td class="' . $cls . '"><a class="tiny" href="/ladder.php?g=d2' . h($query) . '">' . h($label) . '</a>: ' . h($c['name']) . ($c['dead'] ? ' <span class="lad-dead">(dead)</span>' : '') . '</td><td class="' . $cls . ' lad-num">' . h($c['class']) . ' ' . (int) $c['level'] . '</td></tr>' . "\n";
    }
    echo '</table>' . "\n";
})();
