<?php
// Home-page widget: the server right now and its last 24 hours, from the stats push. Include it:
//   include __DIR__ . '/widgets/activity.php';   (inside a PHP block)

require_once dirname(__DIR__) . '/bnetcc/server.php';

(function () {
    [$feed, $age] = server_feed();
    echo site_extras_css();
    echo '<b class="header">Server Activity</b>' . "\n";
    if ($feed === null) {
        echo '<p class="post-empty">The server has not reported in yet.</p>' . "\n";
        return;
    }
    $online = server_online($age);
    $now = time();
    $users = [];
    foreach (server_history()['points'] as $p) {
        if ((int) $p['t'] >= $now - 86400) {
            $users[(int) $p['t']] = (int) $p['users'];
        }
    }
    $games = count((array) ($feed['game_list'] ?? [])) + count((array) ($feed['diablo2_games'] ?? []));
    echo '<table class="srv-top" width="100%" cellpadding="4" cellspacing="1"><tr>';
    echo '<td class="row srv-tile"><div class="srv-n">' . ($online ? '<span class="srv-up">Online</span>' : '<span class="srv-down">Offline</span>') . '</div><div class="srv-l">' . ($online ? h(SERVER_ADDRESS) : 'last seen ' . h(server_ago($age))) . '</div></td>';
    echo '<td class="rowAlt srv-tile"><div class="srv-n">' . ($online ? (int) $feed['users_online'] : 0) . '</div><div class="srv-l">online now</div></td>';
    echo '<td class="row srv-tile"><div class="srv-n">' . (int) ($feed['players_24h'] ?? 0) . '</div><div class="srv-l">players today</div></td>';
    echo '<td class="rowAlt srv-tile"><div class="srv-n">' . ($online ? $games : 0) . '</div><div class="srv-l">games open</div></td>';
    echo "</tr></table>\n";
    echo server_chart($users, $now - 86400, $now, 520, 80, 'Players online over the last 24 hours');
    echo '<p class="post-nav"><a class="tiny" href="/server.php">Server details &raquo;</a></p>' . "\n";
})();
