# Win bot

`winbot` (crate `crates/winbot`) tests game records and ladders without two people at two
keyboards. Two accounts log in, one hosts a game, the other joins, and after a set time one of
them surrenders. Both report the result, go back to chat, and the bot prints both records. Then it
starts the next game.

It only talks to the chat server. No game is actually played, and no packet goes to another
game client. Point it at your own server.

## Running it

```sh
cargo run --release -p bnetcc-winbot -- --server 127.0.0.1:6112 --product SEXP --ladder
```

| Option | Default | Meaning |
|---|---|---|
| `--server` | `127.0.0.1:6112` | The chat server. |
| `--product` | `SEXP` | `STAR`, `SEXP`, `W2BN`, `WAR3` or `W3XP`. |
| `--first` / `--second` | `WinBotA:winbot` / `WinBotB:winbot` | The two accounts, `name:password`. The first hosts every game. New accounts are created. |
| `--games` | `0` | Games to play; `0` plays until stopped. |
| `--length` | `125` | Seconds from a game's start to the surrender. |
| `--pause` | `5` | Seconds between games. |
| `--ladder` / `--iron-man` | off | Ladder games (game type 1), or Warcraft II Iron Man (3). |
| `--surrender` | `alternate` | Who gives up: `alternate`, `first` or `second`. |
| `--channel` | `Win Bots` | Where the accounts wait between games. |

The CD keys are made up for each account. This server does not check keys against real ones; it
only stops one key from being used by two sessions at once. To run two duels of the same product
at the same time, give each its own pair of accounts.

## The rules it tests

- A result counts only when the game ran **longer than two minutes**, ladder or not
  (`bnetcc_core::ladder::MIN_GAME_LENGTH`). The default length of 2:05 counts. `--length 90`
  checks that a short game changes no record for either player.
- A game starts when its host takes the ad down (`SID_STOPADV`). Time in the lobby does not count.
- Each player's own report counts for that player only. The player who surrenders reports its loss,
  and the other player reports its win.
- StarCraft and Warcraft II players need **ten normal-game wins** before the ladder: fewer, and
  the server refuses to host a ladder game and ignores their ladder results. With `--ladder` or
  `--iron-man` the bot first plays normal games, the account with fewer wins winning each, until
  both have ten. That is about twenty games, some 45 minutes at the default length.
- Ladder games change the rating, high rating and rank (ranks 1 to 500).

## One game on the wire

| Step | Account | Packets |
|---|---|---|
| Host | first | `SID_STARTADVEX3` (game type `0x02`, or `0x09` ladder / `0x10` Iron Man) |
| Join | second | `SID_NOTIFYJOIN`, `SID_LEAVECHAT` |
| Start | first | `SID_STOPADV`, `SID_LEAVECHAT` |
| Wait | both | `SID_NULL` every 30 s, pings answered |
| Surrender | loser, then winner | `SID_GAMERESULT` (both slots: the loser `2`, the winner `1`), `SID_LEAVEGAME`, `SID_ENTERCHAT`, `SID_JOINCHANNEL` |
| Records | both | `SID_READUSERDATA` for `Record\<product>\0\…` (and `\1\…` or `\3\…`), `SID_FINDLADDERUSER` |

## WarCraft III

The WarCraft III accounts log in with NLS/SRP (as `Name@<realm>`) and host and join the same
custom games, but nothing is recorded. WarCraft III clients never send `SID_GAMERESULT`. Its
ladder records games through anonymous matchmaking and the route server (TCP 6200), which are not
built yet (`docs/WARCRAFT3-MATCHMAKING.md`), and classic Battle.net never recorded custom games.
Once matchmaking exists, the bot will queue both accounts through it.
