# Chu-Server Protocol 1.0 — client ↔ tournament server

Status: draft, 2026-11-04. Derived from CSA Server Protocol 1.2.1 (shogi-server / floodgate); the session
grammar is CSA's, the game content is USI-Chu (see `USI-CHU.md`). Reference implementation: `chu-server`;
reference client: `chu-bridge`. "MUST / SHOULD / MAY" as in RFC 2119.

## 1. Transport

- TCP, UTF-8, lines end with `\n`. One message per line. Lines over 1 KiB or more than 100 lines/s from one
  connection → the server closes it (`#CHUDAN` for a game in progress).
- An empty line is a keep-alive. A connection silent for 600 s outside a game is closed.
- Messages beginning with `%%` are lobby commands, `%` are game commands, `#` are server results, `+`/`-` are moves.

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
| `%%GAME <game-name> <+\|-\|*>` | Queue for a game. `*` = either colour (floodgate leagues); `+`/`-` = sente/gote in challenge leagues. |
| `%%GAME <game-name> * <sfen>` | Only in `chu-test-*` leagues: start from a given SFEN, unrated. |
| `%%WHO`, `%%LIST`, `%%SHOW <game-id>` | Player list, game list, moves of a game, each terminated by `##[…] +OK`. |
| `%%MONITORON <game-id>` / `%%MONITOROFF <game-id>` | Receive / stop receiving a game's moves as `##[MONITOR][<game-id>] <line>`. |
| `%%CHAT <utf8 text>` | Broadcast `##[CHAT][<name>] <text>`. |

Game-name grammar: `chu-<league>-<total-s>-<byoyomi-s>[F][-jcsa]`.
`chu-floodgate-600-10F` = 600 s + 10 s Fischer, lishogi rules; `chu-floodgate-900-30-jcsa` = 900 s + 30 s byoyomi, JCSA rules.
Leagues named `floodgate` pair on the server's schedule; other leagues pair as soon as two queued players match.

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

- `Rules:` is `lishogi` or `jcsa`. `Time_Unit:` is `1msec` or `1sec` per league; all time fields use it.
- `REJECT`, or no answer within 30 s, cancels the game; both players return to the lobby.
- A resumed game adds `MOVES:<move>,T<n>` lines after `SFEN:`, one per move already played; `To_Move:` gives the side to move.

## 5. Moves and results

```
> +8i8h                 sente's move (gote sends -…)
< +8i8h,T2310           echoed to both players and monitors; T = time used in Time_Unit
> %TORYO                resign
```

- A move is `<colour><usi-move>` with `+` = sente, `-` = gote. Wrong colour, wrong turn, or not in the referee's
  legal list ends the game with `#ILLEGAL_MOVE`.
- The server's clock is the authority: time used is measured from the server's echo of the previous move to its
  receipt of this one. Byoyomi: a move within `Byoyomi` costs nothing from `Total_Time`. Fischer: `Increment` is added after the move.

Every game ends with one reason line, then one verdict line (`#WIN`, `#LOSE`, `#DRAW`, `#CENSORED` = void) to each player:

| Reason | Verdict | Trigger |
| --- | --- | --- |
| `#RESIGN` | opponent wins | `%TORYO` |
| `#TIME_UP` | opponent wins | clock exhausted (server time) |
| `#ILLEGAL_MOVE` | opponent wins | see above |
| `#ROYALS_LOST` | mover wins | last enemy royal captured |
| `#BARE_KING` | mover wins | bare-king rule under the game's `Rules:` |
| `#STALEMATE` | mover wins | side to move has no legal move |
| `#INSUFFICIENT` | draw | only two royals remain |
| `#SENNICHITE` | draw | same position (board, side, lion-capture field) for the 4th time |
| `#OUTE_SENNICHITE` | checking side loses | fourfold repetition with perpetual check |
| `#MAX_MOVES` | draw | ply 1000 reached |
| `#CHUDAN` | void, `#CENSORED` | disconnect not recovered in time, server shutdown, protocol violation |

After the verdict the players are back in the lobby and MAY send `%%GAME` again.

## 6. Disconnect and resume

A player whose connection drops during a game may reconnect (`LOGIN`, then `%%GAME` with the same game name)
while its clock still has time; it receives the Game_Summary with `MOVES:` and answers `AGREE`. Its clock keeps
running meanwhile. If the time runs out, `#TIME_UP`; if the server cannot keep the game (restart without
recovery), `#CHUDAN`.

## 7. Conformance

`chu-tools conformance --client <cmd>` plays a scripted opponent against a client and checks: login, `AGREE`
within 30 s, every move legal and in turn, `%TORYO` handling, a resumed game via `MOVES:`, and that the client
survives `#CHUDAN` and re-queues. The examples in this file are the fixtures of `chu-proto`'s parser tests.
