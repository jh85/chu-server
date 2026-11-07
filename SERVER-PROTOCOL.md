# Chu-Server Protocol 1.0 — client ↔ tournament server

Status: draft 4, 2026-11-05. Derived from CSA Server Protocol 1.2.1 (shogi-server / floodgate); the session
grammar is CSA's, the game content is USI-Chu (see `USI-CHU.md`). Reference implementation: `chu-server`;
reference client: `chu-bridge`. "MUST / SHOULD / MAY" as in RFC 2119.

## 1. Transport

- TCP, UTF-8, lines end with `\n`. One message per line. Timestamps anywhere in the protocol are UTC.
- Limits per connection: 1 KiB per line; a token bucket of 500 lines refilled at 100 lines/s. Exceeding either
  is a protocol violation: `#ILLEGAL_ACTION` (a loss) if a game is in progress, then the connection is closed.
  The bucket is a ceiling, not a target: a client that can move faster than that MUST pace itself (the reference
  clients keep a slightly smaller copy of the bucket and wait only when it would run dry).
- An empty line is a keep-alive. A connection silent for 600 s outside a game is closed.
- Messages beginning with `%%` are lobby commands, `%` are game commands, `#` are server results, `+`/`-` are moves,
  `##[…]` are server notices (`##[ERROR] <text>` for a rejected lobby command).

## 2. Login

```
> LOGIN <name> <password> x1
< LOGIN:<name> OK          |  < LOGIN:incorrect
> LOGOUT
< LOGOUT:completed
```

`name` matches `[A-Za-z0-9_-]{1,32}`. An unknown name is created with the given password. `x1` (extended mode)
is mandatory. A second login with the same name closes the first connection.

## 3. Lobby commands

| Command | Reply |
| --- | --- |
| `%%GAME <game-name> <+\|-\|*>` | Queue for a game. `*` = either colour; `+`/`-` = sente/gote (challenge leagues only; `*` is accepted there too). |
| `%%GAME <game-name> * <sfen>` | Only in `chu-test-*` leagues: start from a given SFEN, unrated. |
| `%%WHO`, `%%LIST`, `%%SHOW <game-id>` | Player list, game list, moves of a game, each terminated by `##[…] +OK`. |
| `%%MONITORON <game-id>` / `%%MONITOROFF <game-id>` | Receive / stop receiving a game's moves as `##[MONITOR][<game-id>] <line>`. |
| `%%CHAT <utf8 text>` | Broadcast `##[CHAT][<name>] <text>`. |

Game-name grammar: `chu-<league>-<total-s>-<byoyomi-s>[F][-jcsa]`.
`chu-floodgate-600-10F` = 600 s + 10 s Fischer, lishogi rules; `chu-floodgate-900-30-jcsa` = 900 s + 30 s byoyomi, JCSA rules.
Leagues named `floodgate` pair on the server's schedule (`every_min` and `offset_min`, UTC, in `leagues.toml`);
other leagues pair as soon as two queued players match. A round whose time passed while the server was not running
is fired late only if the next round is not yet due; otherwise it is skipped with a warning and the gap is visible
in the rounds audit table.

## 4. Game start

```
< BEGIN Game_Summary
< Protocol_Version:1.2
< Protocol_Mode:Server
< Format:Chushogi 1.0
< Rules:lishogi
< Game_ID:chu-floodgate-600-10F+az_v12+random_bot+20261104213000
< Name+:az_v12
< Name-:random_bot
< Your_Turn:+
< Rematch_On_Draw:NO
< To_Move:+
< Max_Moves:1000
< BEGIN Time
< Time_Unit:1msec
< Total_Time:600000
< Byoyomi:0
< Increment:10000
< Least_Time_Per_Move:0
< END Time
< BEGIN Position
< SFEN:lfcsgekgscfl/a1b1txot1b1a/mvrhdqndhrvm/pppppppppppp/3i4i3/12/12/3I4I3/PPPPPPPPPPPP/MVRHDNQDHRVM/A1B1TOXT1B1A/LFCSGKEGSCFL b - 1
< END Position
< END Game_Summary
> AGREE
< START:<game-id>
```

- `Game_ID` is `<game-name>+<sente>+<gote>+<UTC yyyymmddhhmmss>`, with `-2`, `-3`, … appended when two games start in the same second.
- `Rules:` is `lishogi` or `jcsa` (JCSA leagues are deferred; v1 runs lishogi rules only). `Time_Unit:` is `1msec` or `1sec` per league; all time fields use it.
- `REJECT`, or no answer within 30 s, cancels the game: both players receive `REJECT:<game-id> by <name>` and return to the lobby.
- A resumed game adds `MOVES:<move>,T<n>` lines after `SFEN:`, one per move already played; `To_Move:` is the side to move after them.
  Its Time block also carries `Remaining+:<n>`, `Remaining-:<n>` and `Turn_Used:<n>` (all in `Time_Unit`):
  `Remaining±` is each side's main time as it stood at the start of the current turn (not yet charged for it),
  and `Turn_Used` is how much of the current turn the side in `To_Move:` has already used. The client applies
  the league's normal rule (byoyomi or increment) to those two numbers, so a player already in byoyomi is
  represented correctly.

## 5. Moves and results

```
> +8i8h                 sente's move (gote sends -…)
< +8i8h,T2310           echoed to both players and monitors; T = time used in Time_Unit
> %TORYO                resign
```

- A move is `<colour><usi-move>` with `+` = sente, `-` = gote, spelled exactly as the referee's legal list spells it
  (so `8i8h=` is illegal). Wrong colour, wrong turn, or not legal ends the game with `#ILLEGAL_MOVE`.
- Every accepted move is echoed, including a game-ending one; the result lines follow the echo.
- The server's clock is the authority: time used is measured from the moment the server queued the echo of the
  previous move to its receipt of this one. Byoyomi: a move within `Byoyomi` costs nothing from `Total_Time`.
  Fischer: `Increment` is added after the move.

Every game ends with one reason line, then one verdict line (`#WIN`, `#LOSE`, `#DRAW`, `#CENSORED` = void) to each player:

| Reason | Verdict | Trigger |
| --- | --- | --- |
| `#RESIGN` | opponent wins | `%TORYO` |
| `#TIME_UP` | opponent wins | clock exhausted (server time), including while disconnected |
| `#ILLEGAL_MOVE` | opponent wins | see above |
| `#ILLEGAL_ACTION` | opponent wins | protocol violation: line/rate limits, `AGREE`/`REJECT` during play; the violator receives the result lines before its connection closes |
| `#ROYALS_LOST` | mover wins | last enemy royal captured |
| `#BARE_KING` | winner per `Rules:` | bare-king rule; either side can be the winner |
| `#STALEMATE` | mover wins | side to move has no legal move |
| `#INSUFFICIENT` | draw | only two royals remain |
| `#SENNICHITE` | draw | same position (board, side, lion-capture field) for the 4th time |
| `#OUTE_SENNICHITE` | checking side loses | fourfold repetition with perpetual check (both rule sets) |
| `#MAX_MOVES` | draw | ply 1000 reached; rated |
| `#CHUDAN` | void, `#CENSORED` | referee failure, or server shutdown without recovery; never a player's fault |

After the verdict the players are back in the lobby and MAY send `%%GAME` again.

## 6. Disconnect and resume

A player whose connection drops during a game keeps its clock running. It may reconnect (`LOGIN`, then
`%%GAME` with the same game name) while time remains and receives the Game_Summary with `MOVES:`; it answers
`AGREE` and play continues. If its clock reaches zero first, the game ends with `#TIME_UP` and is rated.
`#CHUDAN` is reserved for causes on the server side.

## 7. Conformance

`chu-tools conformance --client <cmd>` plays a scripted opponent against a client and checks: login, `AGREE`
within 30 s, every move legal and in turn, `%TORYO` handling, a resumed game via `MOVES:`, and that the client
survives `#CHUDAN` and re-queues. The examples in this file are the fixtures of `chu-proto`'s parser tests.
