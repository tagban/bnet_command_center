</div> <!-- Close main-content -->
    
    <div id="sidebar">
        <b class="header">Active Projects</b>
        <ul class="menu" style="margin-top: 10px; padding-left: 15px;">
            <li><a href="https://github.com/tagban/invigoration" class="forumHeader">Invigoration</a></li>
            <li><a href="https://superioritybot.com/" class="forumHeader">Superiority</a></li>
            <li><a href="https://github.com/tagban/bnet_command_center" class="forumHeader">Command Center</a></li>
        </ul>
        
        <br><br>

<?php include __DIR__ . '/widgets/releases.php'; ?>

        <br><br>
        
<!-- BNET COMMAND CENTER INTEGRATION -->
<b class="header">Server Stats</b>
<?php
require_once __DIR__ . '/bnetcc/server.php';

// The server's stats push, kept by server-push.php. Before the first push arrives, the public
// status feed is read directly instead, kept for 30 seconds and never waited on for more than
// 3 seconds.
(function () {
    [$d, $age] = server_feed();
    if ($d !== null) {
        $online = server_online($age);
    } else {
        $cacheFile = site_data_path('server-status.json');
        $cache = site_read_json($cacheFile, null);
        if (!is_array($cache) || time() - (int) ($cache['fetched'] ?? 0) >= 30) {
            $data = null;
            if (function_exists('curl_init')) {
                $ch = curl_init(SERVER_STATUS_URL);
                curl_setopt_array($ch, [CURLOPT_RETURNTRANSFER => true, CURLOPT_CONNECTTIMEOUT => 3, CURLOPT_TIMEOUT => 3]);
                $response = curl_exec($ch);
                $code = (int) curl_getinfo($ch, CURLINFO_HTTP_CODE);
                if (PHP_VERSION_ID < 80000) {
                    curl_close($ch);
                }
                $parsed = $code === 200 && is_string($response) ? json_decode($response, true) : null;
                $data = is_array($parsed) ? $parsed : null;
            }
            $cache = ['fetched' => time(), 'data' => $data];
            site_write_json($cacheFile, $cache);
        }
        $online = is_array($cache['data'] ?? null);
        $d = $online ? $cache['data'] : [];
        $age = 0;
    }
    $get = function ($key) use ($d) {
        return isset($d[$key]) ? $d[$key] : 0;
    };
    // The users list is only present when the server shares it; it is an array.
    $users = isset($d['users']) && is_array($d['users']) ? $d['users'] : [];
    $games = isset($d['game_list']) ? count((array) $d['game_list']) + count((array) ($d['diablo2_games'] ?? [])) : (int) $get('games');
    ?>
<table class="stats">
  <tr><td>Status:</td><td><?= $online ? '<span style="color:#4caf50">Online</span>' : '<span style="color:#e53935">Offline</span>' ?></td></tr>
<?php if ($online): ?>
  <tr><td>Server Address:</td><td><?= h(SERVER_ADDRESS) ?></td></tr>
  <tr><td>Users Online:</td><td><?= (int) $get('users_online') ?></td></tr>
  <tr><td>Connections:</td><td><?= (int) $get('connections') ?></td></tr>
<?php if (isset($d['players_24h'])): ?>
  <tr><td>Players Today:</td><td><?= (int) $d['players_24h'] ?></td></tr>
<?php endif; ?>
  <tr><td>Channels:</td><td><?= (int) $get('channels') ?></td></tr>
  <tr><td>Games:</td><td><?= $games ?></td></tr>
  <tr><td>Uptime:</td><td><?= h(server_duration((int) $get('uptime_secs'))) ?></td></tr>
  <tr><td>Who's Online:</td><td><?= $users ? h(implode(', ', $users)) : 'None' ?></td></tr>
<?php elseif ($age !== PHP_INT_MAX && $age > 0): ?>
  <tr><td>Last Seen:</td><td><?= h(server_ago($age)) ?></td></tr>
<?php endif; ?>
</table>
<a class="tiny" href="/server.php">Server details &raquo;</a>
<?php
})();
?>
        
        <br><br>
    </div>
