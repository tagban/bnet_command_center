<?php
// Home-page widget: the latest news. Include it where the posts should appear:
//   include __DIR__ . '/widgets/news.php';   (inside a PHP block)
// Set $newsWidgetCount first to show a different number of posts (default 3).

require_once dirname(__DIR__) . '/bnetcc/news.php';

(function (int $count) {
    $posts = array_slice(news_published(), 0, $count);
    echo site_extras_css();
    echo '<b class="header">Latest News</b>' . "\n";
    if (!$posts) {
        echo '<p class="post-empty">No news yet.</p>' . "\n";
        return;
    }
    foreach ($posts as $p) {
        $link = '/news.php?p=' . rawurlencode($p['slug']);
        echo '<div class="post post-short">';
        echo '<div class="post-title">' . (!empty($p['pinned']) ? '<span class="post-pin">Pinned</span> ' : '') . '<a href="' . h($link) . '">' . h($p['title']) . '</a></div>';
        echo '<div class="post-meta">' . h(site_date((int) $p['created'])) . ' · ' . h($p['author']) . '</div>';
        echo '<div class="post-excerpt">' . h(markup_excerpt($p['body'])) . ' <a class="tiny" href="' . h($link) . '">Read more</a></div>';
        echo "</div>\n";
    }
    echo '<p class="post-nav"><a class="tiny" href="/news.php">All news &raquo;</a></p>' . "\n";
})(isset($newsWidgetCount) ? (int) $newsWidgetCount : 3);
