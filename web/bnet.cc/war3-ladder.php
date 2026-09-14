<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>WarCraft III Ladder - bnet.cc</title>
    <link rel="stylesheet" href="war3-ladder.css">
</head>
<body>
<div class="w3-frame">
    <div class="w3-bar w3-top">
        <a class="w3-crest" href="war3-ladder.php">
            <span class="w3-title">WarCraft III</span>
            <span class="w3-sub">bnet.cc Ladder</span>
        </a>
    </div>

    <div class="w3-main">
        <div class="w3-page">
            <div class="w3-row">
                <span class="w3-label">Gateway:</span>
                <select class="w3-select" aria-label="Gateway" disabled>
                    <option>bnet.cc (us.bnet.cc)</option>
                </select>
            </div>

            <div class="w3-tabs" role="tablist" aria-label="Ladder">
                <span class="w3-tab w3-tab-on" role="tab" aria-selected="true" aria-disabled="true">Solo</span>
                <span class="w3-tab" role="tab" aria-disabled="true">Random Team</span>
                <span class="w3-tab" role="tab" aria-disabled="true">Arranged Team</span>
                <span class="w3-tab" role="tab" aria-disabled="true">Free for All</span>
            </div>

            <form class="w3-search" onsubmit="return false">
                <input type="text" placeholder="Search for a player" aria-label="Search for a player" disabled>
                <button type="submit" disabled>Search</button>
            </form>

            <div class="w3-message">
                <div class="w3-sign" aria-hidden="true"></div>
                <div>
                    <h2>Ladder Not Yet Open</h2>
                    <p>WarCraft III ladder games are found through <b>Play Game</b> (anonymous matchmaking), which bnet.cc has not opened yet. Custom games are never recorded.</p>
                    <p>When matchmaking opens, solo, random team, arranged team and free-for-all standings will appear here: level, experience, wins and losses, and rank down to 500.</p>
                    <p><a href="ladder.php">See the StarCraft, Warcraft II and Diablo II ladders</a></p>
                </div>
            </div>

            <hr class="w3-line">

            <div class="w3-heading">Top Solo Players</div>
            <table class="w3-table">
                <thead>
                    <tr><th>Rank</th><th>Player</th><th>Level</th><th>Experience</th><th>Wins</th><th>Losses</th></tr>
                </thead>
                <tbody>
                    <tr><td colspan="6">No games have been played on the WarCraft III ladder.</td></tr>
                </tbody>
            </table>
        </div>
    </div>

    <div class="w3-bar w3-bottom">
        <a class="w3-back" href="ladder.php"><span class="w3-arrow" aria-hidden="true"></span>Return to the bnet.cc ladders</a>
        <span class="w3-mark">bnet.cc</span>
    </div>
</div>
</body>
</html>
