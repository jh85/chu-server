# USI-Chu 1.0 — engine protocol for chu shogi

Status: draft, 2026-11-04. The reference client is `chu-bridge`; the reference engine is `chushogi-gen`
(https://github.com/jh85/chushogi). "MUST / SHOULD / MAY" as in RFC 2119.

USI-Chu is USI (Universal Shogi Interface) with chu shogi moves and positions and a few rules tightened.
Anything not stated here is as in USI.

## 1. Transport

- stdin/stdout, UTF-8, lines end with `\n`. A line is one command; tokens are separated by one or more spaces.
- Unknown commands and unknown tokens MUST be ignored (not an error).
- Non-ASCII UTF-8 is allowed in `id name`, `id author` and `info string`. Invalid UTF-8 SHOULD be replaced, never crash.
- Maximum line length clients must accept: 64 KiB (a `position` with 1000 moves).

## 2. Notation

Squares: file `1`–`12` (12 is the left file from sente's view) then rank `a`–`l` (`a` = gote's back rank). Examples: `7e`, `12a`.

Moves (lishogi convention):

| Form | Example | Meaning |
| --- | --- | --- |
| `<from><to>` | `8i8h` | ordinary move |
| `<from><to>+` | `4d4b+` | move and promote |
| `<from><mid><to>` | `7e7f7g` | two-step lion-power move (lion, horned falcon, soaring eagle) |
| `<from><mid><from>` | `7e7f7e` | igui (capture on `mid`, return) |
| `resign` | | engine resigns |

There are no drops. The normative legal-move list for any position is the output of `chushogi-gen legal`;
an implementation MUST produce exactly those strings (open item: confirm the jitto spelling against lishogi).

Positions: SFEN with 12 files per rank, ranks `a`..`l` from the first field onward, lishogi piece letters
(`+` prefix for promoted). Third field is the last lion-capture square (`-` if none), fourth is the move number.

```
sfen lfcsgekgscfl/a1b1txot1b1a/mvrhdqndhrvm/pppppppppppp/3i4i3/12/12/3I4I3/PPPPPPPPPPPP/MVRHDNQDHRVM/A1B1TOXT1B1A/LFCSGKEGSCFL b - 1
```

`startpos` is the position above.

## 3. Commands, client → engine

| Command | Rule |
| --- | --- |
| `usi` | Engine answers `id name …`, `id author …`, any `option …` lines, then `usiok`. |
| `setoption name <n> [value <v>]` | Only before `isready` of a game. |
| `isready` | Allowed any time the engine is not searching. Engine answers `readyok`. |
| `usinewgame` | MUST precede the first `position` of every game. Resets repetition history and any per-game state. |
| `position startpos \| sfen <sfen> [moves <m1> …]` | Full history every time. Engines SHOULD treat an extension of the previous position incrementally. |
| `go [id <n>] [btime <ms>] [wtime <ms>] [binc <ms>] [winc <ms>] [byoyomi <ms>] [nodes <n>] [movetime <ms>] [infinite]` | Start searching the current position. `b` = sente, `w` = gote. All times in milliseconds. `id` is an opaque integer the engine echoes. |
| `stop` | Stop searching and answer `bestmove` now. `stop` with no search running is a no-op. |
| `gameover win \| lose \| draw` | Ends the game. The client MUST send `isready` → `usinewgame` before the next `position`. |
| `quit` | Exit. |

## 4. Commands, engine → client

| Command | Rule |
| --- | --- |
| `id name <utf8>` / `id author <utf8>` | |
| `option name <n> type <t> …` | As in USI. |
| `usiok`, `readyok` | |
| `info …` | As in USI (`depth`, `nodes`, `nps`, `score cp\|mate`, `pv`, `string`); clients MUST ignore unknown fields. |
| `bestmove <move> [id <n>]` or `bestmove resign [id <n>]` | Exactly one per `go`, including after `stop`. `id` echoes the `go id`. No `ponder` field. |

## 5. Timing and search state

- Exactly one `bestmove` per `go`. The client MUST NOT send a second `go` before the previous `bestmove` arrived.
  A client that gave up waiting MUST still consume the late `bestmove`; the `id` tokens tell them apart.
- Clock values in `go` are the client's belief; the server's clock is the authority. Engines SHOULD keep a margin
  (`chu-bridge` subtracts 1000 ms before sending `go`).
- Pondering is engine-private: after `bestmove` an engine MAY keep computing until the next `position`, `go`, `stop`
  or `gameover`, and MUST answer those promptly. There is no `go ponder` / `ponderhit`.
- `byoyomi` without `btime`/`wtime` means a fixed time per move; `binc`/`winc` means Fischer; neither means no time limit.

## 6. Session example

```
> usi
< id name chushogi-az 0.3
< id author jh85
< option name Sims type spin default 1024 min 1 max 1000000
< usiok
> setoption name Sims value 1024
> isready
< readyok
> usinewgame
> position startpos
> go id 1 btime 600000 wtime 600000 binc 10000 winc 10000
< info depth 1 nodes 1024 score cp 12 pv 8i8h
< bestmove 8i8h id 1
> position startpos moves 8i8h 6c5e
> go id 2 btime 597800 wtime 593100 binc 10000 winc 10000
> stop
< bestmove 7e7f7g id 2
> gameover win
> isready
< readyok
> usinewgame
```

## 7. Conformance

`chu-bridge --conformance <engine>` runs the session above and the following checks: one `bestmove` per `go`
(including `stop` twice in a row), `id` echo, `position` after `gameover` without `usinewgame` is reported,
every `bestmove` is in `chushogi-gen legal` for that position, and `readyok` within 5 s while idle.
