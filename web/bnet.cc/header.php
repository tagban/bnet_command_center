<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title><?= isset($pageTitle) ? htmlspecialchars((string) $pageTitle, ENT_QUOTES, 'UTF-8') . ' - bnet.cc' : 'bnet.cc - Bots &amp; Programs Archive' ?></title>
    <style>
        body, td, p, div, span { background-color: #000000; margin: 0; padding: 0; color: #CCCCCC; font-family: Arial, Helvetica, sans-serif; font-size: 13px; }
        #wrapper { width: 800px; margin: 0 auto; background-color: #0a0a0a; border-left: 1px solid #333333; border-right: 1px solid #333333; min-height: 100vh; display: flex; flex-direction: column; }
        #header { background-color: #111111; background-image: url('/images/metal-left.gif'); background-position: left center; background-repeat: repeat; border-bottom: 2px solid #333333; height: 71px; box-sizing: border-box; overflow: hidden; display: flex; flex-direction: column; justify-content: center; align-items: center; }
        .logo-text { font-family: Impact, "Arial Black", sans-serif; font-size: 32px; font-style: italic; color: #00C0FF; letter-spacing: 2px; text-shadow: 2px 2px 3px #000000, 0 0 12px #0055AA; margin: 0; line-height: 1; background-color: transparent; }
        .logo-subtext { font-size: 10px; color: #CCCCCC; letter-spacing: 4px; text-transform: uppercase; margin-top: 2px; text-shadow: 1px 1px 2px #000000; background-color: transparent; }
        #nav-bar { background-image: url('/images/rect.gif'), url('/images/menu-bg.gif'); background-repeat: repeat-x, repeat-x; background-position: 0 center, center center; height: 31px; line-height: 31px; padding: 0; text-align: center; border-bottom: none; overflow: hidden; animation: scrollRects 3s linear infinite; }
        @keyframes scrollRects { from { background-position: 0 center, center center; } to { background-position: 100px center, center center; } }
        #nav-bar a { margin: 0 14px; font-size: 11px; text-transform: uppercase; letter-spacing: 1px; color: #00C0FF; font-weight: bold; text-shadow: 1px 1px 2px #000000; background-color: transparent; }
        #nav-bar a:hover { color: #FFFFFF; text-shadow: 0 0 5px #00C0FF, 1px 1px 2px #000000; }
        #content-container { display: flex; flex: 1; background-color: transparent; }
        #main-content { width: 70%; padding: 20px; background-color: transparent; }
        #sidebar { width: 30%; padding: 20px; background-color: #12131C; border-left: 1px solid #2B2D42; }
        a { color: #FFAC04; text-decoration: none; font-weight: bold; background-color: transparent; }
        a:hover { color: #ffffff; }
        .tiny { font-size: 11px; font-weight: normal; color: #FFAC04; background-color: transparent; }
        .tiny:hover { color: #ffffff; }
        b.header { color: #FFFFFF; font-weight: bold; font-variant: small-caps; font-size: 14px; letter-spacing: 1px; display: block; border-bottom: 1px solid #333333; padding-bottom: 4px; margin-bottom: 10px; margin-top: 15px; background-color: transparent; }
        b.header:first-child { margin-top: 0; }
        .white { color: #FFFFFF; background-color: transparent; }
        ul.menu { list-style-type: square; color: #888888; background-color: transparent; margin-left: 15px; }
        ul.menu li { margin-bottom: 5px; background-color: transparent; }
        .tableOutline { border: 1px solid #575757; border-collapse: collapse; background-color: transparent; margin-bottom: 20px; }
        .tableOutline td { padding: 6px; }
        .tableHeader { background-color: #393731; border: 1px solid #171511; color: #ffffff; font-weight: bold; }
        .row { background-color: #161616; border: 1px solid #000000; }
        .rowAlt { background-color: #252525; border: 1px solid #000000; }
        .category-header { cursor: pointer; user-select: none; }
        .category-header:hover { color: #FFAC04; }
        .category-content { display: none; }
        #search-box { background-color: #161616; border: 1px solid #575757; color: #00C0FF; padding: 6px 8px; width: 100%; box-sizing: border-box; margin-bottom: 15px; font-family: Arial, Helvetica, sans-serif; }
        #search-box:focus { outline: none; border-color: #00C0FF; }
    </style>
</head>
<body>
    <div id="wrapper">
        <div id="header">
            <h1 class="logo-text">BNET.cc</h1>
            <div class="logo-subtext">... FOR THE HORDE! ...</div>
        </div>
        <div id="nav-bar">
            <a href="/" class="menu">Home</a>
            <a href="/news.php" class="menu">News</a>
            <a href="/ladder.php" class="menu">Ladder</a>
            <a href="/releases.php" class="menu">Releases</a>
            <a href="/files.php" class="menu">Files</a>
            <a href="https://discord.gg/dR4djHweh3" class="menu">Discord</a>
            <a href="https://www.bnetdocs.org/">BNETDocs</a>
        </div>
        <div id="content-container">
            <div id="main-content">