<?php
// bnet.cc server page: what is happening on the game server now, and over the last day and week.

declare(strict_types=1);

require __DIR__ . '/bnetcc/server.php';

[$feed, $age] = server_feed();
$history = server_history();
$online = $feed !== null && server_online($age);
$now = time();

$series = function (string $key, int $from) use ($history) {
    $out = [];
    foreach ($history['points'] as $p) {
        if ((int) $p['t'] >= $from) {
            $out[(int) $p['t']] = (int) $p[$key];
        }
    }
    return $out;
};

site_template('header', 'Server');
echo site_extras_css();
?>
<b class="header">Server Status</b>
<?php if ($feed === null): ?>
    <p class="post-empty">The server has not reported in yet.</p>
<?php else: ?>
    <table class="srv-top" width="100%" cellpadding="4" cellspacing="1">
        <tr>
            <td class="row srv-tile"><div class="srv-n"><?= $online ? '<span class="srv-up">Online</span>' : '<span class="srv-down">Offline</span>' ?></div><div class="srv-l"><?= $online ? h(SERVER_ADDRESS) : 'last seen ' . h(server_ago($age)) ?></div></td>
            <td class="rowAlt srv-tile"><div class="srv-n"><?= $online ? (int) $feed['users_online'] : 0 ?></div><div class="srv-l">players online</div></td>
            <td class="row srv-tile"><div class="srv-n"><?= (int) ($feed['players_24h'] ?? 0) ?></div><div class="srv-l">players today</div></td>
            <td class="rowAlt srv-tile"><div class="srv-n"><?= $online ? count((array) ($feed['game_list'] ?? [])) + count((array) ($feed['diablo2_games'] ?? [])) : 0 ?></div><div class="srv-l">games open</div></td>
        </tr>
    </table>
    <p class="post-meta">
        <?= h((string) $feed['server_name']) ?>
        <?php if ($online): ?> · up <?= h(server_duration((int) $feed['uptime_secs'])) ?><?php endif; ?>
        <?php if (is_array($history['record'])): ?> · most online at once: <?= (int) $history['record']['users'] ?> (<?= h(site_date((int) $history['record']['t'])) ?>)<?php endif; ?>
    </p>

    <b class="header">Last 24 Hours</b>
    <?= server_chart($series('users', $now - 86400), $now - 86400, $now, 520, 120, 'Players online over the last 24 hours') ?>
    <div class="chart-caption"><span>24h ago</span><span>players online</span><span>now</span></div>

    <b class="header">Last 7 Days</b>
    <?= server_chart($series('users', $now - 7 * 86400), $now - 7 * 86400, $now, 520, 100, 'Players online over the last 7 days') ?>
    <div class="chart-caption"><span>7 days ago</span><span>players online</span><span>now</span></div>

    <?php $products = array_values(array_filter((array) ($feed['products'] ?? []), function ($p) {
        return $p['online'] || $p['games_open'] || $p['games_hosted_24h'];
    })); ?>
    <b class="header">By Game</b>
    <?php if (!$products): ?>
        <p class="post-empty">Nobody has played in the last day.</p>
    <?php else: $top = max(1, max(array_map(function ($p) { return (int) $p['online']; }, $products))); ?>
        <table class="tableOutline" width="100%" cellpadding="4" cellspacing="1">
            <tr><td class="tableHeader">Game</td><td class="tableHeader">Online now</td><td class="tableHeader" align="right">Open games</td><td class="tableHeader" align="right">Games today</td></tr>
            <?php foreach ($products as $i => $p): $cls = $i % 2 ? 'rowAlt' : 'row'; ?>
                <tr>
                    <td class="<?= $cls ?>"><span class="white"><?= h(server_product((string) $p['product'])) ?></span></td>
                    <td class="<?= $cls ?>"><span class="srv-bar" style="width: <?= (int) round(90 * ($online ? $p['online'] : 0) / $top) ?>px"></span> <?= $online ? (int) $p['online'] : 0 ?></td>
                    <td class="<?= $cls ?>" align="right"><?= $online ? (int) $p['games_open'] : 0 ?></td>
                    <td class="<?= $cls ?>" align="right"><?= (int) $p['games_hosted_24h'] ?></td>
                </tr>
            <?php endforeach; ?>
        </table>
    <?php endif; ?>

    <?php if ($online && !empty($feed['user_list'])):
        $byGame = [];
        foreach ($feed['user_list'] as $u) {
            $byGame[server_product((string) $u['product'])][] = $u;
        }
        ksort($byGame);
    ?>
        <b class="header">Who's Online</b>
        <?php foreach ($byGame as $game => $users): ?>
            <div class="srv-who"><span class="srv-who-game"><?= h($game) ?></span>
                <?php foreach ($users as $j => $u): ?><?= $j ? ', ' : '' ?><span class="white"><?= h((string) $u['name']) ?></span><?php if (!empty($u['channel'])): ?> <span class="dim">in <?= h((string) $u['channel']) ?></span><?php endif; ?><?php endforeach; ?>
            </div>
        <?php endforeach; ?>
    <?php endif; ?>

    <?php if ($online): ?>
        <b class="header">Open Games</b>
        <?php $games = (array) ($feed['game_list'] ?? []); $d2 = (array) ($feed['diablo2_games'] ?? []); ?>
        <?php if (!$games && !$d2): ?>
            <p class="post-empty">No games open right now.</p>
        <?php else: ?>
            <table class="tableOutline" width="100%" cellpadding="4" cellspacing="1">
                <tr><td class="tableHeader">Game</td><td class="tableHeader">Type</td><td class="tableHeader">Map / difficulty</td><td class="tableHeader" align="right">Open</td></tr>
                <?php $i = 0; foreach ($games as $g): $cls = $i++ % 2 ? 'rowAlt' : 'row'; ?>
                    <tr>
                        <td class="<?= $cls ?>"><?= $g['name'] !== null ? '<span class="white">' . h((string) $g['name']) . '</span>' : '<span class="dim">Private game</span>' ?><br><small class="dim"><?= h(server_product((string) $g['product'])) ?></small></td>
                        <td class="<?= $cls ?>"><?= h((string) $g['kind']) ?></td>
                        <td class="<?= $cls ?>"><?= $g['map'] !== null ? h((string) $g['map']) : '<span class="dim">—</span>' ?></td>
                        <td class="<?= $cls ?>" align="right"><?= h(server_duration(60 * (int) $g['minutes'])) ?></td>
                    </tr>
                <?php endforeach; ?>
                <?php foreach ($d2 as $g): $cls = $i++ % 2 ? 'rowAlt' : 'row'; ?>
                    <tr>
                        <td class="<?= $cls ?>"><?= $g['name'] !== null ? '<span class="white">' . h((string) $g['name']) . '</span>' : '<span class="dim">Private game</span>' ?><br><small class="dim">Diablo II realm</small></td>
                        <td class="<?= $cls ?>"><?= (int) $g['players'] ?> player<?= (int) $g['players'] === 1 ? '' : 's' ?></td>
                        <td class="<?= $cls ?>"><?= h((string) $g['difficulty']) ?></td>
                        <td class="<?= $cls ?>" align="right"><?= h(server_duration(60 * (int) $g['minutes'])) ?></td>
                    </tr>
                <?php endforeach; ?>
            </table>
        <?php endif; ?>

        <b class="header">Channels</b>
        <?php $channels = (array) ($feed['channel_list'] ?? []); $other = (array) ($feed['other_channels'] ?? []); ?>
        <?php if (!$channels && empty($other['users'])): ?>
            <p class="post-empty">The channels are empty.</p>
        <?php else: ?>
            <table class="tableOutline" width="100%" cellpadding="4" cellspacing="1">
                <?php foreach ($channels as $i => $c): $cls = $i % 2 ? 'rowAlt' : 'row'; ?>
                    <tr><td class="<?= $cls ?>"><span class="white"><?= h((string) $c['name']) ?></span></td><td class="<?= $cls ?>" align="right"><?= (int) $c['users'] ?></td></tr>
                <?php endforeach; ?>
                <?php if (!empty($other['users'])): ?>
                    <tr><td class="row dim"><?= (int) $other['channels'] ?> private channel<?= (int) $other['channels'] === 1 ? '' : 's' ?></td><td class="row dim" align="right"><?= (int) $other['users'] ?></td></tr>
                <?php endif; ?>
            </table>
        <?php endif; ?>
    <?php endif; ?>

    <?php $results = array_slice((array) ($feed['recent_ladder'] ?? []), 0, 12); ?>
    <?php if ($results): ?>
        <b class="header">Recent Ladder Games</b>
        <table class="tableOutline" width="100%" cellpadding="4" cellspacing="1">
            <?php foreach ($results as $i => $r): $cls = $i % 2 ? 'rowAlt' : 'row'; $change = (int) $r['change']; ?>
                <tr>
                    <td class="<?= $cls ?>"><span class="white"><?= h((string) $r['player']) ?></span> <span class="srv-<?= h((string) $r['outcome']) ?>"><?= h(ucfirst((string) $r['outcome'])) ?></span></td>
                    <td class="<?= $cls ?>"><?= h(server_product((string) $r['product'])) ?><?= $r['league'] === 'ironman' ? ' Iron Man' : '' ?></td>
                    <td class="<?= $cls ?>" align="right"><?= (int) $r['rating'] ?> <span class="<?= $change >= 0 ? 'srv-plus' : 'srv-minus' ?>">(<?= $change >= 0 ? '+' : '' ?><?= $change ?>)</span></td>
                    <td class="<?= $cls ?> dim" align="right"><?= h(server_ago($now - (int) $r['time'])) ?></td>
                </tr>
            <?php endforeach; ?>
        </table>
        <p class="post-nav"><a class="tiny" href="/ladder.php">The ladders &raquo;</a></p>
    <?php endif; ?>

    <?php $days = array_reverse(array_slice($history['days'], -14)); ?>
    <?php if ($days): ?>
        <b class="header">Daily</b>
        <table class="tableOutline" width="100%" cellpadding="4" cellspacing="1">
            <tr><td class="tableHeader">Day</td><td class="tableHeader" align="right">Most online</td><td class="tableHeader" align="right">Players</td><td class="tableHeader" align="right">Games hosted</td></tr>
            <?php foreach ($days as $i => $d): $cls = $i % 2 ? 'rowAlt' : 'row'; ?>
                <tr><td class="<?= $cls ?>"><?= h(date('D, M j', strtotime((string) $d['date']))) ?></td><td class="<?= $cls ?>" align="right"><?= (int) $d['peak_users'] ?></td><td class="<?= $cls ?>" align="right"><?= (int) $d['players'] ?></td><td class="<?= $cls ?>" align="right"><?= (int) $d['games_hosted'] ?></td></tr>
            <?php endforeach; ?>
        </table>
    <?php endif; ?>
    <p class="post-meta">Updated <?= h(server_ago($age)) ?>. Players and games today count the last 24 hours.</p>
<?php endif; ?>
<?php
site_template('footer');
