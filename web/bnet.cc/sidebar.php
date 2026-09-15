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
require_once __DIR__ . '/bnetcc/bootstrap.php';

// The server's status feed, kept for 30 seconds so a page never waits on the server more than
// twice a minute, and never longer than 3 seconds when it is down.
(function () {
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
    $get = function ($key) use ($d) {
        return isset($d[$key]) ? $d[$key] : 0;
    };
    // The users list is only present when public_show_users is on; it is an array.
    $users = isset($d['users']) && is_array($d['users']) ? $d['users'] : [];
    $u = (int) $get('uptime_secs');
    $uptime = $u >= 86400 ? floor($u / 86400) . 'd ' . floor(($u % 86400) / 3600) . 'h'
        : ($u >= 3600 ? floor($u / 3600) . 'h ' . floor(($u % 3600) / 60) . 'm' : floor($u / 60) . 'm');
    ?>
<table class="stats">
  <tr><td>Status:</td><td><?= $online ? '<span style="color:#4caf50">Online</span>' : '<span style="color:#e53935">Offline</span>' ?></td></tr>
<?php if ($online): ?>
  <tr><td>Server Address:</td><td>us.bnet.cc:6112</td></tr>
  <tr><td>Users Online:</td><td><?= (int) $get('users_online') ?></td></tr>
  <tr><td>Connections:</td><td><?= (int) $get('connections') ?></td></tr>
  <tr><td>Channels:</td><td><?= (int) $get('channels') ?></td></tr>
  <tr><td>Games:</td><td><?= (int) $get('games') ?></td></tr>
  <tr><td>Uptime:</td><td><?= h($uptime) ?></td></tr>
  <tr><td>Who's Online:</td><td><?= $users ? h(implode(', ', $users)) : 'None' ?></td></tr>
<?php endif; ?>
</table>
<?php
})();
?>
        
        <br><br>
    </div>
