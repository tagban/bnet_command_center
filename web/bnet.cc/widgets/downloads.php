<?php
// Home-page widget: the newest files in the downloads folder. Include it:
//   include __DIR__ . '/widgets/downloads.php';   (inside a PHP block)
// Set $downloadsWidgetCount first to show a different number (default 5).

require_once dirname(__DIR__) . '/bnetcc/downloads.php';

(function (int $count) {
    $index = downloads_index();
    $meta = downloads_meta();
    echo site_extras_css();
    echo '<b class="header">Recently Added Files</b>' . "\n";
    $recent = array_keys($index['files']);
    usort($recent, function ($a, $b) use ($index) {
        return [$index['files'][$b]['time'], $a] <=> [$index['files'][$a]['time'], $b];
    });
    $recent = array_slice($recent, 0, $count);
    if (!$recent) {
        echo '<p class="post-empty">No files yet.</p>' . "\n";
        return;
    }
    echo '<table width="100%" cellpadding="5" cellspacing="1" class="tableOutline">';
    echo '<tr><td class="tableHeader" width="50%">Filename</td><td class="tableHeader" width="30%">Category</td><td class="tableHeader" width="20%" align="center">Added</td></tr>';
    foreach ($recent as $i => $path) {
        $path = (string) $path;
        $f = $index['files'][$path];
        $cls = $i % 2 ? 'rowAlt' : 'row';
        $trail = array_column(array_slice(downloads_trail($f['folder'], $index, $meta), 1), 1);
        echo '<tr><td class="' . $cls . '"><a href="' . h(downloads_link($path)) . '">' . h($f['name']) . '</a></td>';
        echo '<td class="' . $cls . '"><a href="/files.php?cat=' . h(rawurlencode($f['folder'])) . '" class="tiny">' . h(strtoupper(implode(' / ', $trail))) . '</a></td>';
        echo '<td class="' . $cls . '" align="center"><small class="white">' . h(date('M j', $f['time'])) . '</small></td></tr>';
    }
    echo "</table>\n";
    echo '<p class="post-nav"><a class="tiny" href="/files.php">All files &raquo;</a></p>' . "\n";
})(isset($downloadsWidgetCount) ? (int) $downloadsWidgetCount : 5);
