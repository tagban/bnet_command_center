<?php
// The bnet.cc ladders: StarCraft, Brood War, Warcraft II, Diablo II and WarCraft III. The page is
// the site's own template (header.php opens #main-content, the footer closes it) around a #ladder
// box that ladder.js fills from ladder-data.php.

declare(strict_types=1);

require __DIR__ . '/bnetcc/bootstrap.php';

site_template('header', 'Ladder');
?>
<link rel="stylesheet" href="ladder.css">
<div id="ladder" data-src="ladder-data.php">
    <noscript><p>The ladder needs JavaScript turned on.</p></noscript>
</div>
<script src="ladder.js"></script>
<?php
site_template('footer');
