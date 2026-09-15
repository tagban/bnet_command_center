<?php
// Sidebar widget: each tracked project's latest release. Include it in the sidebar:
//   include __DIR__ . '/widgets/releases.php';   (inside a PHP block)

require_once dirname(__DIR__) . '/bnetcc/releases.php';

(function () {
    echo site_extras_css();
    echo '<b class="header">Latest Releases</b>' . "\n";
    echo '<table class="rel-widget" width="100%" cellpadding="3" cellspacing="1">' . "\n";
    foreach (releases_latest() as [$repo, $settings, $release]) {
        echo '<tr><td class="row" width="55%">' . h($settings['name']) . '</td><td class="rowAlt">';
        if ($release === null) {
            echo '<span class="dim">—</span>';
        } else {
            $label = h($release['tag'] !== '' ? $release['tag'] : $release['name']);
            echo $settings['public'] && $release['url'] !== '' ? '<a href="' . h($release['url']) . '">' . $label . '</a>' : '<span class="white">' . $label . '</span>';
            echo '<br><small class="dim">' . h(site_date((int) $release['published'])) . '</small>';
        }
        echo "</td></tr>\n";
    }
    echo '</table>' . "\n";
    echo '<p class="post-nav"><a class="tiny" href="/releases.php">All releases &raquo;</a></p>' . "\n";
})();
