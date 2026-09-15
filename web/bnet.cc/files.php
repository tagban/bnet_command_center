<?php
// bnet.cc files: the downloads folder, browsable. ?cat=folder/path opens a folder (the old files.php
// links keep working), ?q=words searches every file, ?sort=name|date|size|downloads orders a folder.

declare(strict_types=1);

require __DIR__ . '/bnetcc/downloads.php';
require_once __DIR__ . '/bnetcc/markup.php';

$index = downloads_index();
$meta = downloads_meta();
$counts = downloads_counts();
$cat = trim(str_replace('\\', '/', (string) ($_GET['cat'] ?? '')), '/');
$query = trim((string) ($_GET['q'] ?? ''));
$sort = in_array($_GET['sort'] ?? '', ['name', 'date', 'size', 'downloads'], true) ? (string) $_GET['sort'] : 'name';
$missing = $cat !== '' && !isset($index['folders'][$cat]);
if ($missing) {
    http_response_code(404);
    $cat = '';
}
$folder = $index['folders'][$cat];

/** A link to a folder view. */
function files_href(string $path, array $extra = []): string
{
    $query = array_filter(['cat' => $path] + $extra, 'strlen');
    return '/files.php' . ($query ? '?' . http_build_query($query) : '');
}

/** One file's table row. */
function files_row($path, int $i, array $index, array $meta, array $counts, bool $showFolder): string
{
    $path = (string) $path; // a file named only with digits comes back from array_keys as an int
    $f = $index['files'][$path];
    $cls = $i % 2 ? 'rowAlt' : 'row';
    $about = downloads_file_about($path, $index, $meta);
    $html = '<tr><td class="' . $cls . '">';
    $html .= '<span class="dl-type">' . h(strtoupper($f['ext'] !== '' ? $f['ext'] : '—')) . '</span> ';
    $html .= '<a href="' . h(downloads_link($path)) . '">' . h($f['name']) . '</a>';
    if (!empty($meta[$path]['featured'])) {
        $html .= ' <span class="post-pin">Featured</span>';
    }
    if ($showFolder) {
        $trail = array_slice(downloads_trail($f['folder'], $index, $meta), 1);
        $html .= '<br><a class="tiny" href="' . h(files_href($f['folder'])) . '">' . h(implode(' › ', array_column($trail, 1))) . '</a>';
    }
    if ($about !== '') {
        $html .= '<div class="dl-about">' . markup_inline(markup_excerpt($about, 300)) . '</div>';
    }
    $html .= '<div class="dl-kind">' . h(downloads_type($f['ext'])) . '</div></td>';
    $html .= '<td class="' . $cls . '" align="right">' . h(downloads_size($f['size'])) . '</td>';
    $html .= '<td class="' . $cls . '" align="right">' . h(site_date($f['time'])) . '</td>';
    if (DOWNLOADS_COUNT) {
        $html .= '<td class="' . $cls . '" align="right">' . number_format((int) ($counts[$path] ?? 0)) . '</td>';
    }
    return $html . "</tr>\n";
}

/** A file table's heading row, with sort links when `$sortable`. */
function files_head(string $cat, string $sort, bool $sortable, string $first = 'File'): string
{
    $col = function (string $key, string $label, string $align) use ($cat, $sort, $sortable) {
        $text = $sortable && $sort !== $key ? '<a href="' . h(files_href($cat, $key === 'name' ? [] : ['sort' => $key])) . '">' . h($label) . '</a>' : h($label) . ($sortable && $sort === $key ? ' ▾' : '');
        return '<td class="tableHeader"' . ($align !== '' ? ' align="' . $align . '"' : '') . '>' . $text . '</td>';
    };
    return '<tr>' . $col('name', $first, '') . $col('size', 'Size', 'right') . $col('date', 'Added', 'right') . (DOWNLOADS_COUNT ? $col('downloads', 'Downloads', 'right') : '') . '</tr>';
}

$title = $query !== '' ? 'Search' : downloads_folder_title($cat, $meta);
site_template('header', $cat === '' ? 'Files' : $title . ' - Files');
echo site_extras_css();
?>
<b class="header"><?= h($cat === '' ? 'Files' : $title) ?></b>

<form class="dl-search" method="get" action="/files.php">
    <input type="text" name="q" value="<?= h($query) ?>" placeholder="Search all files" aria-label="Search all files">
    <button type="submit">Search</button>
</form>

<?php if ($missing): ?>
    <p class="post-empty">That folder is not here. Here is everything instead.</p>
<?php endif; ?>

<?php if ($query !== ''):
    $words = array_filter(preg_split('/\s+/', mb_strtolower($query)));
    $hits = [];
    foreach ($index['files'] as $path => $f) {
        $haystack = mb_strtolower($path . ' ' . downloads_file_about($path, $index, $meta));
        $all = true;
        foreach ($words as $w) {
            if (mb_strpos($haystack, $w) === false) {
                $all = false;
                break;
            }
        }
        if ($all) {
            $hits[] = $path;
        }
    }
?>
    <p class="post-meta"><?= count($hits) ?> file<?= count($hits) === 1 ? '' : 's' ?> match <b class="white"><?= h($query) ?></b> · <a class="tiny" href="/files.php">All files</a></p>
    <?php if ($hits): ?>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head('', 'name', false) ?>
            <?php foreach ($hits as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, true);
            } ?>
        </table>
    <?php endif; ?>

<?php elseif ($cat === ''): ?>
    <?php
    $featured = array_values(array_filter(array_keys($index['files']), function ($p) use ($meta) {
        return !empty($meta[$p]['featured']);
    }));
    ?>
    <?php if ($featured): ?>
        <b class="header">Featured</b>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head('', 'name', false) ?>
            <?php foreach ($featured as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, true);
            } ?>
        </table>
    <?php endif; ?>

    <div class="dl-cats">
        <?php foreach ($folder['folders'] as $path): $f = $index['folders'][$path]; $about = downloads_folder_about($path, $index, $meta); ?>
            <a class="dl-cat<?= $f['count'] ? '' : ' dl-empty' ?>" href="<?= h(files_href($path)) ?>">
                <span class="dl-cat-name"><?= h(downloads_folder_title($path, $meta)) ?></span>
                <span class="dl-cat-count"><?= (int) $f['count'] ?> file<?= (int) $f['count'] === 1 ? '' : 's' ?><?= $f['count'] ? ' · ' . h(downloads_size((int) $f['size'])) : '' ?></span>
                <?php if ($about !== ''): ?><span class="dl-cat-about"><?= h(markup_excerpt($about, 90)) ?></span><?php endif; ?>
                <?php if ($f['newest']): ?><span class="dl-cat-date">updated <?= h(site_date((int) $f['newest'])) ?></span><?php endif; ?>
            </a>
        <?php endforeach; ?>
    </div>
    <?php foreach ($folder['files'] ? [1] : [] as $_): ?>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head('', 'name', false) ?>
            <?php foreach ($folder['files'] as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, false);
            } ?>
        </table>
    <?php endforeach; ?>

    <?php
    $recent = array_keys($index['files']);
    usort($recent, function ($a, $b) use ($index) {
        return [$index['files'][$b]['time'], $a] <=> [$index['files'][$a]['time'], $b];
    });
    $recent = array_slice($recent, 0, 10);
    ?>
    <?php if ($recent): ?>
        <b class="header">Recently Added</b>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head('', 'date', false) ?>
            <?php foreach ($recent as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, true);
            } ?>
        </table>
    <?php endif; ?>

    <?php
    $popular = array_filter($counts, function ($n, $p) use ($index) {
        return $n > 0 && isset($index['files'][$p]);
    }, ARRAY_FILTER_USE_BOTH);
    arsort($popular);
    $popular = array_slice(array_keys($popular), 0, 10);
    ?>
    <?php if (DOWNLOADS_COUNT && $popular): ?>
        <b class="header">Most Downloaded</b>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head('', 'downloads', false) ?>
            <?php foreach ($popular as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, true);
            } ?>
        </table>
    <?php endif; ?>
    <?php if (!$index['files']): ?>
        <p class="post-empty">No files yet.</p>
    <?php endif; ?>

<?php else: ?>
    <div class="dl-trail">
        <?php foreach (downloads_trail($cat, $index, $meta) as $j => [$path, $name]): ?>
            <?= $j ? '<span class="dim">›</span>' : '' ?>
            <?php if ($path === $cat): ?><span class="white"><?= h($name) ?></span><?php else: ?><a href="<?= h(files_href($path)) ?>"><?= h($name) ?></a><?php endif; ?>
        <?php endforeach; ?>
    </div>
    <?php $about = downloads_folder_about($cat, $index, $meta); if ($about !== ''): ?>
        <div class="post-body dl-folder-about"><?= markup($about) ?></div>
    <?php endif; ?>

    <?php if ($folder['folders']): ?>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <tr><td class="tableHeader">Folder</td><td class="tableHeader" align="right">Files</td><td class="tableHeader" align="right">Size</td><td class="tableHeader" align="right">Updated</td></tr>
            <?php foreach ($folder['folders'] as $i => $path): $f = $index['folders'][$path]; $cls = $i % 2 ? 'rowAlt' : 'row'; ?>
                <tr>
                    <td class="<?= $cls ?>"><span class="dl-type dl-dir">DIR</span> <?php if ($f['count']): ?><a href="<?= h(files_href($path)) ?>"><?= h(downloads_folder_title($path, $meta)) ?></a><?php else: ?><span class="dim"><?= h(downloads_folder_title($path, $meta)) ?> (empty)</span><?php endif; ?></td>
                    <td class="<?= $cls ?>" align="right"><?= (int) $f['count'] ?></td>
                    <td class="<?= $cls ?>" align="right"><?= $f['count'] ? h(downloads_size((int) $f['size'])) : '—' ?></td>
                    <td class="<?= $cls ?>" align="right"><?= $f['newest'] ? h(site_date((int) $f['newest'])) : '—' ?></td>
                </tr>
            <?php endforeach; ?>
        </table>
    <?php endif; ?>

    <?php
    $files = $folder['files'];
    usort($files, function ($a, $b) use ($index, $counts, $sort) {
        $fa = $index['files'][$a];
        $fb = $index['files'][$b];
        switch ($sort) {
            case 'date':
                return [$fb['time'], $a] <=> [$fa['time'], $b];
            case 'size':
                return [$fb['size'], $a] <=> [$fa['size'], $b];
            case 'downloads':
                return [(int) ($counts[$b] ?? 0), $a] <=> [(int) ($counts[$a] ?? 0), $b];
            default:
                return strnatcasecmp($fa['name'], $fb['name']);
        }
    });
    ?>
    <?php if ($files): ?>
        <table class="tableOutline dl-table" width="100%" cellpadding="4" cellspacing="1">
            <?= files_head($cat, $sort, true) ?>
            <?php foreach ($files as $i => $path) {
                echo files_row($path, $i, $index, $meta, $counts, false);
            } ?>
        </table>
    <?php elseif (!$folder['folders']): ?>
        <p class="post-empty">Nothing here yet.</p>
    <?php endif; ?>
    <p class="post-meta"><?= (int) $folder['count'] ?> file<?= (int) $folder['count'] === 1 ? '' : 's' ?> · <?= h(downloads_size((int) $folder['size'])) ?> in all</p>
<?php endif; ?>
<?php
site_template('footer');
