<?php
// bnet.cc tracker page: the servers that report to us, and how to get listed.

declare(strict_types=1);

require __DIR__ . '/bnetcc/tracker.php';

$list = tracker_list();
$servers = $list['servers'];
$fresh = tracker_is_fresh($list['pushed']);
$trackerAddress = defined('TRACKER_ADDRESS') ? (string) TRACKER_ADDRESS : 'us.bnet.cc';

site_template('header', 'Tracker');
echo site_extras_css();
?>
<b class="header">Server List</b>
<?php if (!$fresh): ?>
    <p class="post-empty">
        <?= $list['pushed'] === 0
            ? 'The tracker has not reported in yet.'
            : 'Last updated ' . h(tracker_duration(time() - $list['pushed'])) . ' ago — this list may be out of date.' ?>
    </p>
<?php endif; ?>
<?php if (!$servers): ?>
    <p class="post-empty">No servers are listed right now. Yours could be the first — see below.</p>
<?php else: ?>
    <table class="trk" width="100%" cellpadding="4" cellspacing="1">
        <tr>
            <td class="header-row">Server</td>
            <td class="header-row">Games</td>
            <td class="header-row" align="right">Players</td>
            <td class="header-row" align="right">Games open</td>
            <td class="header-row" align="right">Up</td>
        </tr>
        <?php foreach ($servers as $i => $server): $row = $i % 2 ? 'rowAlt' : 'row'; ?>
            <tr>
                <td class="<?= $row ?>">
                    <b><?= h((string) ($server['description'] ?? 'Unnamed server')) ?></b>
                    <div class="trk-sub">
                        <?= h((string) ($server['address'] ?? '')) ?>
                        · <?= h((string) ($server['software'] ?? '')) ?> <?= h((string) ($server['version'] ?? '')) ?>
                        <?php $link = tracker_link((string) ($server['url'] ?? '')); if ($link !== ''): ?>
                            · <a href="<?= h($link) ?>" rel="nofollow noopener"><?= h((string) $server['url']) ?></a>
                        <?php endif; ?>
                    </div>
                    <?php $notes = tracker_notes($server); if ($notes): ?>
                        <div class="trk-notes"><?= h(implode(' · ', $notes)) ?></div>
                    <?php endif; ?>
                    <?php $links = (array) ($server['links'] ?? []); if ($links): ?>
                        <div class="trk-links">
                            Also reachable at:
                            <?php foreach ($links as $n => $link): ?><?= $n ? ' · ' : ' ' ?><code><?= h((string) $link) ?></code><?php endforeach; ?>
                        </div>
                    <?php endif; ?>
                </td>
                <td class="<?= $row ?>">
                    <?php $games = tracker_games($server); ?>
                    <?php if (!$games): ?>
                        <span class="trk-none">not stated</span>
                    <?php else: ?>
                        <?php foreach ($games as $game): $icon = tracker_icon_url($game['code']); ?>
                            <?php if ($icon !== ''): ?>
                                <img src="<?= h($icon) ?>" alt="<?= h($game['name']) ?>" title="<?= h($game['name']) ?>" class="trk-icon">
                            <?php endif; ?>
                        <?php endforeach; ?>
                        <div class="trk-sub"><?= h(implode(', ', array_column($games, 'name'))) ?></div>
                    <?php endif; ?>
                </td>
                <td class="<?= $row ?>" align="right"><?= (int) ($server['users'] ?? 0) ?></td>
                <td class="<?= $row ?>" align="right"><?= (int) ($server['games'] ?? 0) ?></td>
                <td class="<?= $row ?>" align="right"><?= h(tracker_duration((int) ($server['uptime_secs'] ?? 0))) ?></td>
            </tr>
        <?php endforeach; ?>
    </table>
<?php endif; ?>

<b class="header">List Your Server</b>
<p class="post-body">
    Any server speaking the Battle.net tracking protocol can be listed here — this tracker
    accepts <b>PvPGN servers</b> as readily as it accepts our own. Nothing is charged, nothing
    is approved by hand: a server that reports appears, and one that stops reporting drops off.
</p>

<p class="post-meta">Command Center</p>
<p class="post-body">
    Set the tracker address and restart. Nothing else — the server works out which games it
    serves and reports them itself, so there is no list of codes to keep up to date:
</p>
<pre class="trk-conf">[tracker]
advertise_to = ["<?= h($trackerAddress) ?>"]
public_host = "your.server.address"</pre>
<p class="post-body">
    <code>public_host</code> is the address players actually dial. Set it and the list shows
    that name instead of a bare IP.
</p>

<p class="post-meta">PvPGN</p>
<p class="post-body">
    Open <code>bnetd.conf</code>, set the two lines below and restart (or <code>/rehash</code>).
    You can report to several trackers at once by separating them with commas:
</p>
<pre class="trk-conf">track = 180
trackaddr = "<?= h($trackerAddress) ?>"</pre>
<p class="post-body">
    PvPGN has no field for which games a server runs, so it is taken from the codes in your
    <code>description</code> — the same ones the older list sites use
    (<code>WC2</code>, <code>WC3</code>, <code>WCX</code>, <code>D1</code>, <code>D2</code>,
    <code>LOD</code>, <code>SC</code>, <code>SBW</code>, <code>CHAT</code>, and
    <code>OPE</code>, <code>CLO</code> or <code>LDR</code> for open play, a closed realm or a
    ladder). Product codes work too, and read better:
    <code>description = "W2BN D2XP STAR My Server"</code>. Either way the name is whatever is
    left once the codes are taken out.
</p>
<p class="post-body">
    Beacons are UDP on port <b>6114</b>. Allow it outbound. The list here updates every few
    minutes, so give it a little time before checking.
</p>
<p class="post-body">
    The icons beside each server are the real ones — the artwork the games themselves use in
    chat, taken from an <code>icons.bni</code> rather than redrawn. They are generated from a
    copy of the game files and are not distributed with the server.
</p>
<?php
site_template('footer');
