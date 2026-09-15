<?php
// bnet.cc releases: the recent releases of every project in GITHUB_REPOS.

declare(strict_types=1);

require __DIR__ . '/bnetcc/releases.php';
require_once __DIR__ . '/bnetcc/markup.php';

const RELEASES_SHOWN = 5;

site_template('header', 'Releases');
echo site_extras_css();
?>
    <b class="header">Releases</b>
    <p class="post-meta">The latest versions of the bnet.cc projects, straight from GitHub.</p>
<?php foreach (releases_repos() as $repo => $settings):
    $cache = releases_for($repo);
    $releases = array_slice($cache['releases'] ?? [], 0, RELEASES_SHOWN);
?>
    <div class="rel-project">
        <div class="rel-project-name"><?= h($settings['name']) ?><?php if ($settings['public']): ?> <a class="tiny" href="https://github.com/<?= h($repo) ?>">github.com/<?= h($repo) ?></a><?php endif; ?></div>
        <?php if (!$releases): ?>
            <p class="post-empty"><?= !empty($cache['error']) && $cache['error'] !== 'refreshing' ? 'Releases are not available right now.' : 'No releases yet.' ?></p>
        <?php endif; ?>
        <?php foreach ($releases as $i => $r): ?>
            <div class="rel<?= $i === 0 ? ' rel-latest' : '' ?>">
                <div class="rel-head">
                    <?php if ($settings['public'] && $r['url'] !== ''): ?>
                        <a href="<?= h($r['url']) ?>"><?= h($r['name']) ?></a>
                    <?php else: ?>
                        <span class="white"><?= h($r['name']) ?></span>
                    <?php endif; ?>
                    <?php if ($r['tag'] !== '' && $r['tag'] !== $r['name']): ?><span class="rel-tag"><?= h($r['tag']) ?></span><?php endif; ?>
                    <?php if ($i === 0): ?><span class="rel-badge">Latest</span><?php endif; ?>
                    <?php if ($r['prerelease']): ?><span class="rel-badge rel-pre">Pre-release</span><?php endif; ?>
                    <span class="rel-date"><?= h(site_date((int) $r['published'])) ?></span>
                </div>
                <?php if ($i === 0 && trim($r['notes']) !== ''): ?>
                    <div class="post-body rel-notes"><?= markup($r['notes']) ?></div>
                <?php endif; ?>
                <?php if ($settings['public'] && $r['assets']): ?>
                    <ul class="rel-assets">
                        <?php foreach ($r['assets'] as $a): ?>
                            <li><a href="<?= h($a['url']) ?>"><?= h($a['name']) ?></a> <span class="dim"><?= h(releases_size((int) $a['size'])) ?></span></li>
                        <?php endforeach; ?>
                    </ul>
                <?php endif; ?>
            </div>
        <?php endforeach; ?>
    </div>
<?php endforeach; ?>
<?php
site_template('footer');
