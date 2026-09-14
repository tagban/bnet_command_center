<?php
// The bnet.cc ladders: StarCraft, Brood War, Warcraft II and Diablo II. The page is the site's
// own template around a #ladder box that ladder.js fills from ladder-data.php.

declare(strict_types=1);

require __DIR__ . '/ladder-config.php';

$header = defined('LADDER_HEADER_FILE') ? (string) LADDER_HEADER_FILE : '';
$footer = defined('LADDER_FOOTER_FILE') ? (string) LADDER_FOOTER_FILE : '';

if ($header !== '' && is_file($header)) {
    include $header;
} else {
    include __DIR__ . '/ladder-header.inc.php';
}
?>
<link rel="stylesheet" href="ladder.css">
<div id="main-content">
    <div id="ladder" data-src="ladder-data.php">
        <noscript><p>The ladder needs JavaScript turned on.</p></noscript>
    </div>
</div>
<script src="ladder.js"></script>
<?php
if ($footer !== '' && is_file($footer)) {
    include $footer;
} else {
    include __DIR__ . '/ladder-footer.inc.php';
}
