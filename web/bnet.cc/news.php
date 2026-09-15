<?php
// bnet.cc news: the list of posts (?page=N), or one post (?p=slug).

declare(strict_types=1);

require __DIR__ . '/bnetcc/news.php';

const NEWS_PER_PAGE = 10;

$slug = isset($_GET['p']) ? (string) $_GET['p'] : '';
$post = $slug !== '' ? news_find($slug) : null;
if ($post !== null && !empty($post['draft'])) {
    $post = null;
}
if ($slug !== '' && $post === null) {
    http_response_code(404);
}
$posts = news_published();
$pages = max(1, (int) ceil(count($posts) / NEWS_PER_PAGE));
$page = min($pages, max(1, (int) ($_GET['page'] ?? 1)));

site_template('header', $post !== null ? $post['title'] : 'News');
echo site_extras_css();
?>
<?php if ($post !== null): ?>
    <b class="header"><?= h($post['title']) ?></b>
    <div class="post-meta">Posted by <?= h($post['author']) ?> on <?= h(site_date((int) $post['created'])) ?><?= (int) $post['updated'] > (int) $post['created'] + 300 ? ' · updated ' . h(site_date((int) $post['updated'])) : '' ?></div>
    <div class="post-body"><?= markup($post['body']) ?></div>
    <p class="post-nav"><a href="/news.php">&laquo; All news</a></p>
<?php elseif ($slug !== ''): ?>
    <b class="header">News</b>
    <p>That post is not here. <a href="/news.php">See all news</a>.</p>
<?php else: ?>
    <b class="header">News</b>
    <?php if (!$posts): ?>
        <p class="post-empty">No news yet.</p>
    <?php endif; ?>
    <?php foreach (array_slice($posts, ($page - 1) * NEWS_PER_PAGE, NEWS_PER_PAGE) as $p): ?>
        <div class="post">
            <div class="post-title"><?php if (!empty($p['pinned'])): ?><span class="post-pin">Pinned</span> <?php endif; ?><a href="/news.php?p=<?= h(rawurlencode($p['slug'])) ?>"><?= h($p['title']) ?></a></div>
            <div class="post-meta">Posted by <?= h($p['author']) ?> on <?= h(site_date((int) $p['created'])) ?></div>
            <div class="post-body"><?= markup($p['body']) ?></div>
        </div>
    <?php endforeach; ?>
    <?php if ($pages > 1): ?>
        <div class="post-pager">
            <?= $page > 1 ? '<a href="/news.php?page=' . ($page - 1) . '">&laquo; Newer</a>' : '<span class="dim">&laquo; Newer</span>' ?>
            <span>Page <?= $page ?> of <?= $pages ?></span>
            <?= $page < $pages ? '<a href="/news.php?page=' . ($page + 1) . '">Older &raquo;</a>' : '<span class="dim">Older &raquo;</span>' ?>
        </div>
    <?php endif; ?>
<?php endif; ?>
<?php
site_template('footer');
