# Chu Shogi Tournament Server: Design and Implementation Plan

Nov 4, 2026, updated Nov 5, 2026 · Eii

## Goals and scope

The server is a floodgate for chu shogi: engines log in over TCP, get paired on a fixed schedule, play rated games under a server-side referee, and every game is recorded and published on a web page. The reference is [shogi-server](https://github.com/TadaoYamaoka/shogi-server) (CSA protocol 1.2.1, Ruby) as run at [floodgate](https://wdoor.c.u-tokyo.ac.jp/shogi/); the referee is the move generator in [jh85/chushogi](https://github.com/jh85/chushogi).

In scope for v1:

- CSA-style session protocol with chu shogi moves in USI notation and positions in SFEN (the formats the chushogi repo and lishogi already use)
- Server-side legality, clocks (initial time + byoyomi or Fischer increment), and all chu shogi end conditions, under either the lishogi or JCSA rule set
- Floodgate mode (scheduled rounds, rating-based pairing, odd player sits out) and plain challenge mode (two players naming the same game)
- Game records in the chushogi repo's own format (`moves.usi`, `positions.sfen`, `record.json`) plus a CSA-like text kifu, a rating table, and a small web UI with a live board
- A USI bridge so `chushogi-az` (and any USI chu engine) can play without knowing the server protocol

The protocols are normative in two short files that ship with the server, `USI-CHU.md` (engine ↔ bridge) and `SERVER-PROTOCOL.md` (client ↔ server), each about a page; their wire examples are the fixtures of the parser tests, and a conformance mode in the reference tools checks outside engines and clients against them. That is how the spec and the code are kept from drifting: a change to one fails the other.

Out of scope for v1: human play through a browser, accounts with email, multiple variants, and compatibility with existing CSA GUI clients (chu moves do not fit the `+7776FU` grammar, see Wire protocol).

## Language decision: Rust server, C++ referee

Write the server in Rust (tokio + axum) and keep the rules engine in C++, exposed to Rust through a small C ABI. A tournament server is a long-running network program with hundreds of sockets, per-game timers, a scheduler and a web front end; that is where Rust's async runtime, ownership model and crate ecosystem (tokio, axum, sqlx, serde) save the most work and the most 3 a.m. crashes. The rules engine is the part that is already written, tested against 8688 lishogi positions and 289 perft counts, and that you keep extending; rewriting it in Rust would be a second source of truth with its own bugs.

| Option | Pros | Cons |
| --- | --- | --- |
| C++ server linking `src/` directly | One language, zero FFI, in-process referee | Networking, timers, HTTP and JSON by hand (asio, nlohmann, …); concurrency bugs are yours to find; the server crashes with the engine |
| Rust server + C ABI over the C++ engine (chosen) | Async networking, web and persistence are library calls; referee stays the tested code; `unsafe` confined to one wrapper crate | Need a \~150-line `capi.cpp` in the chushogi repo and a `build.rs`; two toolchains in CI |
| Rust server driving `chushogi-gen` as a subprocess | No C++ changes at all, crash isolation per game | One process per game, string parsing per move; fine for a dozen games, awkward for hundreds |

The subprocess route is still useful: the existing `position … / move … / legal / status / sfen / rules` stdin protocol is a complete referee, so phase 1 uses it to get games running before the C ABI exists. The `chu-rules` crate hides which backend is in use behind one trait, so switching is a feature flag.

C ABI surface to add to jh85/chushogi (`src/capi.h`, `src/capi.cpp`, built into `libchushogi.a` by the Makefile):

```c
typedef struct chu_pos chu_pos;
chu_pos* chu_new(void);
void     chu_free(chu_pos*);
int      chu_set_sfen(chu_pos*, const char* sfen);        /* 0 ok, <0 parse error */
int      chu_set_rules(chu_pos*, int rules);              /* 0 lishogi, 1 jcsa */
int      chu_move(chu_pos*, const char* usi);             /* 0 ok, -1 illegal, -2 bad syntax */
int      chu_legal(const chu_pos*, char* buf, int cap);   /* space-separated USI moves, returns count */
int      chu_status(const chu_pos*, int* winner);         /* playing/royalslost/bareking/stalemate/draw; winner out-param */
int      chu_in_check(const chu_pos*, int side);          /* for perpetual-check detection */
int      chu_sfen(const chu_pos*, char* buf, int cap);
int      chu_ply(const chu_pos*);
```

No exceptions cross the boundary, no C++ types, every buffer is caller-owned. Every function wraps logic that already exists in the engine (`chu_in_check` wraps `inCheck`). `chu_move` accepts only the exact spelling `chu_legal` prints. Implemented on the `capi` branch of jh85/chushogi (Nov 2026).

## System architecture

One Rust binary does everything: it listens on the game port, runs the scheduler, hosts the games and serves the web pages. The C++ referee is linked in as a static library, and the AlphaZero agents never see the server protocol because a bridge speaks USI to them.

```
chushogi-az / any USI engine --USI--> chu-bridge --TCP--> Protocol layer (sessions)
Browser --HTTP--> Web (axum)                            |
                                                        v
                               Lobby and floodgate scheduler
                                                        |
                                                        v
                               Game actor (one task per game) --> chu-rules (FFI to libchushogi.a)
                                   |                             --> Storage (SQLite, kifu files)
                                   v  live events
                               Web (axum): pages, SSE live board, downloads
```

Moves flow top to bottom: a session parses the line, the lobby decides who plays whom, the game task owns the clocks and asks `chu-rules` whether each move is legal and whether the game is over, and every state change is pushed to the web layer for the live board and written to storage.

| Crate | Role | Key dependencies |
| --- | --- | --- |
| `chu-rules` | Position, legal moves, status, SFEN; one trait with two backends: `ffi` (libchushogi.a) and `subprocess` (`chushogi-gen`) | `cc`, `libc` |
| `chu-proto` | Line protocol: tokenizer, command enum, Game\_Summary encoder, message framing; shared by server and bridge | `serde` |
| `chu-server` | TCP listener, sessions, lobby, floodgate scheduler, game actor, clocks, record writer, rating refresh, axum web | `tokio`, `axum`, `sqlx` (SQLite), `tokio-util`, `tracing` |
| `chu-bridge` | Runs a USI engine as a child process and plays it on the server; handles clock translation and reconnects | `tokio`, `chu-proto` |
| `chu-tools` | CLI: replay a record, recompute ratings, export CSA-like kifu, migrate DB | `clap`, `sqlx` |

Concurrency model: one tokio task per connection (reads lines, forwards to the owning game or lobby over an mpsc channel), one task per game (sole owner of the position and clocks, so no locks around the referee), one scheduler task per floodgate game name, and axum on the same runtime. State that the web layer reads (current games, standings) lives behind a `watch` channel snapshot, not a shared mutex.

### Repository layout

The server lives in a new repo, `chu-server`, with `jh85/chushogi` as a git submodule at `vendor/chushogi` pinned to a tag. Engine and server change for different reasons and have different toolchains (Makefile + Python vs cargo), and outsiders who want to write a client should not have to clone the training scripts and tsume collection. Only engine code goes into `jh85/chushogi`: the C ABI and `libchushogi.a` target, the USI additions to `chushogi-az`, and a tag per revision the server pins.

```
chu-server/
  Cargo.toml            workspace
  DESIGN.md             this document
  USI-CHU.md            engine protocol spec
  SERVER-PROTOCOL.md    client–server protocol spec
  crates/chu-rules      Referee trait; ffi and subprocess backends; build.rs builds vendor/chushogi
  crates/chu-proto      line protocol, Game_Summary, test fixtures from the spec examples
  crates/chu-server     the server binary
  crates/chu-bridge     USI bridge, --conformance mode
  crates/chu-tools      replay, rate, export, conformance for clients
  leagues.toml          league config
  vendor/chushogi       submodule, pinned
```

Build flags: the server builds generic (`--release`, no `-march`), so one binary runs on both the AVX2 box and the Ryzen 9; engines are built with `-march=native` on the machine they play on, and are never copied between machines.

## Wire protocol: CSA session, USI moves, SFEN positions

Keep the CSA 1.2.1 session grammar (LOGIN, `%%GAME`, Game\_Summary, AGREE, `+move,Tsec` echoes, `#` result lines) and change only the two things that cannot carry chu shogi: the move token and the position block. Anyone who has written a floodgate client recognizes the flow; the chushogi repo's own strings (`7e7f7g`, SFEN with the lion-capture field) carry the game content unchanged. Call it `Format:Chushogi 1.0` in the summary so a shogi client that connects by mistake rejects cleanly.

Line protocol, UTF-8, `\n` terminated, one message per line:

| Direction | Message | Notes |
| --- | --- | --- |
| C→S | `LOGIN <name> <password> x1` | Extended mode only; `name` is `[A-Za-z0-9_-]{1,32}`, password creates the account on first use (floodgate behaviour) |
| S→C | `LOGIN:<name> OK` / `LOGIN:incorrect` |  |
| C→S | `%%GAME <game-name> <+\|-\|*>` | Join a floodgate queue (`*`) or challenge by name with a colour |
| S→C | `BEGIN Game_Summary … END Game_Summary` | See block below |
| C→S | `AGREE` / `REJECT` | 30-second timeout counts as REJECT |
| S→C | `START:<game-id>` | Clocks start |
| C→S | `+7e7f7g` / `-8i8h+` | USI move with the mover's colour prefix; `+` = sente (first player) |
| S→C | `+7e7f7g,T2310` | Echo to both players and observers with time used in the league's Time\_Unit (1msec by default) |
| C→S | `%TORYO` | Resign |
| S→C | `#RESIGN` `#WIN` | Result pair: a reason line then `#WIN` / `#LOSE` / `#DRAW` / `#CENSORED` per player |
| C→S | `%%WHO` `%%LIST` `%%SHOW <id>` `%%MONITORON <id>` `%%MONITOROFF <id>` `%%CHAT <text>` `LOGOUT` | Lobby commands, same semantics as shogi-server |
| C→S | empty line | Keep-alive; server drops a connection silent for 10 minutes outside a game |

Game\_Summary for a chu shogi game:

```
BEGIN Game_Summary
Protocol_Version:1.2
Protocol_Mode:Server
Format:Chushogi 1.0
Rules:lishogi
Game_ID:chu-floodgate-600-10F+az_v12+random_bot+20261104213000
Name+:az_v12
Name-:random_bot
Your_Turn:+
Rematch_On_Draw:NO
To_Move:+
Max_Moves:1000
BEGIN Time
Time_Unit:1msec
Total_Time:600000
Byoyomi:0
Increment:10000
Least_Time_Per_Move:0
END Time
BEGIN Position
SFEN:lfcsgekgscfl/a1b1txot1b1a/mvrhdqndhrvm/pppppppppppp/3i4i3/12/12/3I4I3/PPPPPPPPPPPP/MVRHDNQDHRVM/A1B1TOXT1B1A/LFCSGKEGSCFL b - 1
END Position
END Game_Summary
```

A position block may carry `MOVES:` lines after `SFEN:` for a game resumed after a server restart; clients replay them before answering AGREE.

Game-name grammar, in the floodgate style: `chu-<league>-<total-seconds>-<byoyomi-seconds>[F][-jcsa]`. `chu-floodgate-600-10F` is 600 s + 10 s Fischer increment under lishogi rules; `chu-floodgate-900-30-jcsa` is 900 s + 30 s byoyomi under JCSA rules. The scheduler treats each distinct game name as its own league with its own rating list, exactly as floodgate separates `floodgate-300-10F` from `floodgate-600-10`.

Result reason lines (`#` messages) follow CSA where the meaning matches and add the chu-specific ones:

| Reason line | Winner | Trigger |
| --- | --- | --- |
| `#RESIGN` | opponent | `%TORYO` |
| `#TIME_UP` | opponent | Clock reached zero, judged on server receive time |
| `#ILLEGAL_MOVE` | opponent | Move not in the referee's legal list, or wrong colour prefix, or sent out of turn |
| `#ROYALS_LOST` | mover | Last enemy royal captured (`status royalslost`) |
| `#BARE_KING` | mover | Referee reports `bareking` under the game's rule set and names the winner, since either side can be the bared one |
| `#STALEMATE` | mover | Side to move has no legal move (a loss in chu shogi) |
| `#INSUFFICIENT` | draw | Referee reports `draw` (two royals only) |
| `#SENNICHITE` | draw | Same SFEN (board, side, lion-capture field) for the 4th time |
| `#OUTE_SENNICHITE` | opponent | Fourfold repetition where every move of one side in the cycle gave check (perpetual check loses, as on lishogi); needs `chu_in_check` |
| `#MAX_MOVES` | draw | Ply 1000 reached; a rated draw (lishogi's rule), not floodgate's void `#CENSORED` |
| `#CHUDAN` | none | Server shutdown without recovery or referee failure only; a disconnected player's clock keeps running and it loses on `#TIME_UP`. Protocol violations (line or rate limits, commands out of place) are `#ILLEGAL_ACTION`, a loss for the violator |

Every `#` outcome is computed once in the game actor and sent to both players, every monitor, the record writer and the rating queue from the same event, so the four never disagree.

## Game rules, clocks and adjudication

The server trusts nothing from a client: every move is checked against the referee's legal list, every end condition is decided server-side, and the clock is the server's clock. The referee owns the position rules (legality, bare king, stalemate, insufficient material, lishogi vs JCSA); the game actor owns the history-dependent rules (repetition, perpetual check, ply cap) and time.

Move handling in the game actor, per received line:

1. Reject if it is not the sender's turn, the colour prefix is wrong, or the game has ended; wrong-turn moves cost `#ILLEGAL_MOVE` as on floodgate.
2. Stop the mover's clock at receive time; if elapsed exceeds remaining + byoyomi, end with `#TIME_UP` and do not apply the move.
3. Ask `chu-rules` to apply the move; an illegal move ends the game with `#ILLEGAL_MOVE`.
4. Push the new SFEN (without the move number) onto the repetition map; on the 4th occurrence end with `#SENNICHITE`, or `#OUTE_SENNICHITE` against the side that gave check on every one of its moves since the first occurrence.
5. Ask `chu-rules` for status; `royalslost`, `bareking`, `stalemate`, `draw` end the game with the matching reason line.
6. If the ply count has reached 1000, end with `#MAX_MOVES`.
7. Echo `+move,T<ms>` to both players and all monitors (every accepted move is echoed, a game-ending one before its result lines), start the opponent's clock from the moment the echo is queued, and append to the record.

Clocks are integers in the league's `Time_Unit`, `1msec` by default (CSA 1.2.1 allows it; `1sec` stays available per league), measured with `tokio::time::Instant` from the moment the previous echo was written to the socket. Milliseconds matter at fast leagues such as 60+1, where whole-second rounding would charge a 0.9 s move zero or one second at random, a systematic bias between engines with different move-time distributions. Two modes, selected by the game name: byoyomi (`Total_Time` then `Byoyomi` per move; a move within byoyomi costs nothing from the main clock) and Fischer (`Increment` added after each move, `Byoyomi:0`). Each clock has a timer task; a timer firing with no move received ends the game with `#TIME_UP`, so a hung engine cannot stall a round.

Rule set is a property of the league (`-jcsa` suffix) and is sent as `Rules:` in Game\_Summary and passed to `chu_set_rules` once per game. Under JCSA the referee already knows that an unpromoted last-rank lance is a dead piece and that bare king needs a counting piece; the server has no rule-set code of its own.

Start position is the standard chu shogi setup for every rated game. A `chu-test-…` league accepts `%%GAME` with an SFEN argument for engine testing; those games are unrated and never appear in standings.

Disconnects: a player whose socket closes mid-game keeps its clock running and loses on `#TIME_UP` if it does not reconnect before the clock expires; a reconnect (same LOGIN, then `%%GAME` with the same name) is attached to the running game and receives the Game\_Summary with `MOVES:` so far. This matches floodgate's practice of letting engines survive a network blip without rewarding a stall, and it is what the AlphaZero workers will need when an inference server restarts.

## Floodgate scheduling, pairing and rating

Each floodgate league runs rounds on a fixed cadence from a config file (shogi-server's `floodgate-0-240.conf` idea): `chu-floodgate-600-10F` every 30 minutes, with a cutoff a few seconds before the round so late joiners wait for the next one. A game with 600 s + 10 s per side and a 1000-ply cap can in theory run about 3.5 hours, so the scheduler must not assume a round finishes before the next one starts; a player still in a game is simply not in the pool.

Pairing per round, in order, as floodgate's `Pairing` chain does:

1. Pool = players who sent `%%GAME <league> *` and are idle; drop anyone whose connection has been silent past the keep-alive limit.
2. Sort by rating (unrated players at a provisional 1500), then swap neighbours with 25% probability so the same two engines do not meet every round.
3. Walk the sorted list in adjacent pairs; if a pair met in the previous round, try the next candidate down before accepting it.
4. Odd pool: the player who sat out most recently plays, and the one who has sat out least recently sits out (floodgate's `ExcludeUnratedPlayers` and `StartGame` steps). Among ties, the lowest rated sits out, so new engines get games quickly.
5. Colours alternate per player across rounds; a player who was sente last time gets gote when possible.

A `chu-test` league pairs instantly on demand instead of on a schedule.

Rating follows floodgate's `mk_rate`: a maximum-likelihood fit of a Bradley-Terry model over all rated games in the league, scaled so that a 200-point gap is a 76% expected score, and anchored so the whole list has mean 1500. Players need 15 rated games before a rating is published (floodgate uses 15). Recompute after every game in a tokio task (a few thousand games fit in milliseconds with a fixed-point iteration), store the current value per player, and keep a history row per recompute so a rating curve per engine can be drawn. A Glicko-2 variant can be added later as a second column; the Bradley-Terry fit is kept because it is what floodgate publishes and what your scaling-law work can compare against.

Accounts: first `LOGIN` with an unknown name creates it with that password (floodgate practice); an admin CLI can ban a name or merge an engine's versions under an alias for the web page. Name convention for versions, as floodgate users do: `az_v12_1024sims`.

## Persistence, records and web front end

One SQLite file (WAL mode, via `sqlx`) holds accounts, games and ratings; game records are also written as plain files so they can be used without the server, exactly like floodgate's `.csa` tree.

| Table | Columns | Purpose |
| --- | --- | --- |
| `players` | name, password\_hash (argon2), created\_at, banned, alias\_of | Accounts; `alias_of` groups engine versions on the web page |
| `games` | id, league, rules, sente, gote, started\_at, ended\_at, result (`+`/`-`/`=`/void), reason, ply\_count, initial\_sfen, final\_sfen, record\_path | One row per game, written at start and updated once at the end |
| `moves` | game\_id, ply, usi, elapsed\_ms, sfen\_after | Enables replay, live resume after restart, and position queries for research |
| `ratings` | league, player, rating, games, computed\_at | Current rating per league |
| `rating_history` | league, player, rating, computed\_at | One row per recompute, for curves |
| `rounds` | league, scheduled\_at, paired\_json | Audit trail of who was paired and who sat out |

Record files under `records/<league>/<yyyy>/<mm>/<dd>/<game-id>/`: `moves.usi`, `positions.sfen`, `record.json` in the same layout `tools/random_match.py` writes, so `tests/replay_test.py` and the az tooling read tournament games directly; plus `game.csa`, a CSA-like text with the Game\_Summary header, one `+7e7f7g,T2310` line per move, and the `#` result lines, for humans and for `grep`. The `moves` table is the source of truth; the files are regenerated by `chu-tools export` if lost.

Web pages (axum + HTMX + a small board renderer, the stack you used for the paper manager), all server-rendered:

- `/` — leagues, next round time, players online, games in progress
- `/league/<name>` — standings (rating, games, win/draw/loss, last played), latest results, rating curves as an SVG per engine
- `/game/<id>` — move list and board; while the game is live, an SSE stream (`/game/<id>/events`) pushes each move and the clocks, and the board is drawn client-side from the SFEN in the event (12×12 board, kanji or letter pieces toggle)
- `/player/<name>` — history, head-to-head table, record downloads
- `/records/…` — static serving of the record tree, and `/api/games?league=&since=` as JSON for scripts

The live board is the one place that needs chu shogi knowledge in the browser: a JS SFEN parser for 12 files and the 21-piece-type letter set, about 150 lines; no move generation in the browser.

## USI bridge for the AlphaZero agents

`chu-bridge` is the equivalent of floodgate's `usiToCsa.rb`: it launches a USI engine as a child process, logs in to the server, joins a league, and translates between the two protocols for as many games as you tell it to play. The agents stay pure USI, which is what `chushogi-az` and `chushogi-gen` already speak (`position sfen … moves …`, `go …`, `bestmove …`).

```
chu-bridge --server tourney.example:4081 --name az_v12 --password … \
           --game chu-floodgate-600-10F --engine ./chushogi-az --engine-arg '--ckpt iter_0172.pt' \
           --games 100 --usi-option Threads=8 --usi-option Sims=1024
```

Translation rules:

| Server side | Engine side |
| --- | --- |
| Game\_Summary `SFEN:` + `MOVES:` | `usinewgame`, then `position sfen <sfen> moves …` rebuilt from the full history before every `go` (the engine's `position` handling is incremental, so this stays cheap) |
| Opponent move echo `-7e7f7g,T8` | Append to history; send `position …` and `go btime <ms> wtime <ms> binc <ms> winc <ms>` (Fischer) or `go … byoyomi <ms>` |
| `bestmove 8i8h` | `+8i8h` to the server; `bestmove resign` → `%TORYO` |
| Own move echo `+8i8h,T3` | Update the clock model from the server's `T` value, never from local timing |
| `#…` result lines | `gameover win\|lose\|draw` to the engine, then either the next `%%GAME` or `LOGOUT` |
| Server unreachable | Reconnect with backoff for up to the remaining clock, re-send `LOGIN` and `%%GAME`, resume from `MOVES:` |

Time safety: the bridge subtracts a configurable margin (default 1 s) from the clock values it sends to the engine, and if the engine has not answered by remaining-time minus 200 ms it sends `stop` and forwards whatever `bestmove` comes back. An engine that answers nothing loses on time by the server's clock, not the bridge's guess.

Protocol decisions settled on 4 Nov 2026 and written into `USI-CHU.md`:

- Exactly one `bestmove` per `go`, including after `stop`; the bridge never sends a second `go` before the previous `bestmove`. `go id <n>` / `bestmove … id <n>` is an optional echo so a late answer after a timeout is discarded instead of misattributed.
- All engine-side times are milliseconds; the bridge subtracts a 1000 ms margin.
- Between games the sequence `gameover` → `isready` → `readyok` → `usinewgame` is mandatory; a `position` without it is a reported protocol error.
- UTF-8 everywhere, non-ASCII allowed in `id name` and `info string`; login and game names stay `[A-Za-z0-9_-]`.
- No `ponder`/`ponderhit` handshake: pondering is engine-private after `bestmove`, and the engine must answer the next `position`/`go` promptly.

Two small additions make `chushogi-az` a conformant engine: respond to `usi`/`isready`/`usinewgame`/`setoption`/`stop`/`gameover`, and honour `go btime wtime binc winc byoyomi` by converting milliseconds into a simulation budget (a time-to-sims mapping measured per machine, falling back to the fixed `--sims`). The bridge also runs `chushogi-gen` with `go random`, so a random player is always available for the league.

## Implementation plan

Six phases, each ending in a gate that is a test you can run, not a judgement; the referee binding comes first because every later phase's test leans on it. Phases 0 to 3 give a working rated league; 4 and 5 make it something other people can watch and that survives a reboot.

| Phase | Exit gate |
| --- | --- |
| 0 Referee binding | FFI perft matches chushogi-gen on all 289 fixture positions |
| 1 Protocol and game actor | 100 random-vs-random games; every record passes replay_test.py |
| 2 USI bridge | az vs random, 20 timed games, no bridge-caused time losses |
| 3 Floodgate scheduler and rating | 4 bots, 48 h unattended; ratings order the bots correctly |
| 4 Web front end | live game followed from a phone; moves and clocks update without reload |
| 5 Hardening and deployment | kill -9 mid-round; games resume, nothing lost |

**Phase 0: Referee binding** (in jh85/chushogi and the new `chu-server` repo)

1. Create the `chu-server` repo with the layout above, `DESIGN.md`, `USI-CHU.md`, `SERVER-PROTOCOL.md`, and `vendor/chushogi` as a submodule pinned to a tag.
2. On a branch of `jh85/chushogi`, add `src/capi.{h,cpp}` with the C ABI from the Language section and a `make libchushogi.a` target; no other existing file changes. Tag it once merged.
3. `chu-rules/build.rs` builds the library with the `cc` crate; trait `Referee { set_sfen, set_rules, apply, legal, status, in_check, sfen, ply }` with the `ffi` backend and a `subprocess` backend driving `chushogi-gen`.
4. Tests: perft depth 1–2 from the start position and the 287 scalashogi fixtures through both backends; replay the 49 lishogi records through the FFI and compare SFEN per ply; `chu_in_check` against a dozen crafted positions.

**Phase 1: Protocol and game actor**

1. `chu-proto`: tokenizer, `Command` and `Message` enums, Game\_Summary encoder and parser (the parser is reused by the bridge), property tests that encode→parse is identity.
2. Session task: LOGIN, keep-alive, `%%GAME` in challenge mode (two names match), LOGOUT; SQLite accounts.
3. Game actor: clocks (byoyomi and Fischer), the seven-step move handling, repetition map, all `#` outcomes, monitors.
4. Record writer: `moves` rows, the `records/` tree in `random_match.py` layout, `game.csa`.
5. Tests: an in-process fake client pair; a table of end-condition scenarios (one SFEN each for bare king, stalemate, insufficient material; a scripted fourfold repetition; a 1000-ply game built from `go random`); clock tests with a paused `tokio::time`.
6. Gate: 100 `chushogi-gen go random` vs `go random` games through two subprocess clients at 60+1; every record passes `tests/replay_test.py`, every result agrees with `random_match.py`'s referee on the same move list.

**Phase 2: USI bridge**

1. `chu-bridge` with the translation table from the bridge section, `go id` echo, `--games N`, backoff reconnect, structured logs.
2. `chushogi-az`: `usi`, `isready`, `setoption`, `stop`, `gameover`, and `go` with millisecond clock fields; a `--time-model` file mapping milliseconds to simulations per machine.
3. `chu-bridge --conformance <engine>`: the session in `USI-CHU.md` §6 plus the §7 checks; `chushogi-gen` and `chushogi-az` must both pass.
4. Gate: `az_v12` vs the random bot, 20 games at 600+10F, zero `#TIME_UP` and zero `#ILLEGAL_MOVE` attributable to the bridge (every move the bridge sent was the engine's `bestmove`).

**Phase 3: Floodgate scheduler and rating**

1. League config (TOML): name, cadence, cutoff, rule set, rated flag.
2. Scheduler task per league with the five-step pairing; `rounds` audit rows.
3. Bradley-Terry rating task with the 15-game publication threshold; `chu-tools rate` recomputes from the `games` table and must reproduce the server's numbers.
4. Gate: four bots of known strength order (random, random+mate-in-1, az iter\_0009, az iter\_0172) for 48 hours unattended; rounds fire on schedule; final ratings in the expected order; no round skipped.

**Phase 4: Web front end**

1. axum routes, Askama or Maud templates, HTMX for standings refresh.
2. SSE endpoint per game; browser SFEN board renderer (12×12, letters/kanji toggle).
3. Downloads and `/api/games` JSON.
4. Gate: follow a live game on a phone; moves and clocks update without reload; download a record and replay it with `tests/replay_test.py`.

**Phase 5: Hardening and deployment**

1. Restart recovery: on boot, games with `ended_at IS NULL` are re-created from the `moves` table and wait for both players to reconnect, else `#CHUDAN` after their remaining clock.
2. Fuzz `chu-proto` with `cargo-fuzz`; per-IP connection and login rate limits; password hashing with argon2.
3. `systemd` unit, nightly SQLite backup, `tracing` to journald, a `/metrics` endpoint with games in progress, round timing and referee latency.
4. Gate: `kill -9` the server mid-round, restart; both bridges reconnect and the games finish; the records are complete.

Rough size: phases 0–3 are about 4,000 lines of Rust plus 150 lines of C++, so a few weeks of evenings; 4 and 5 are smaller and can be interleaved with running real rounds.

## Open questions and risks

Decisions that change code in phase 0 or 1 and are worth settling before starting:

- [ ] Perpetual check: lishogi makes it a loss, JCSA's `docs/jcsa/` may say something different for over-the-board play. Confirm the JCSA reading before wiring `#OUTE_SENNICHITE` to the `-jcsa` leagues, or make it a per-league flag.
- [ ] Jitto (lion pass) spelling in USI: `USI-CHU.md` defers to `chushogi-gen legal` as the normative list; replace that with the literal form once checked against lishogi.
- [ ] Should the AlphaZero workers' evaluation tournaments (`az/checkpoint_tournament.py`) move onto the server, so every checkpoint comparison is a rated league with a public record? It is the same bridge; only a `chu-ckpt-…` league name and an instant-pairing mode are needed.
- [ ] Where to host: the same Ubuntu box as the mail server (one more systemd unit behind nginx) or a separate small VM, so a runaway engine cannot touch mail delivery.
- [x] Colour prefix on moves: kept (`+7e7f7g`); the bridge adds it.
- [x] Time unit: milliseconds by default, per league.
- [x] Repo: new `chu-server` repo, chushogi as a pinned submodule.

Settled on 5 Nov 2026 after the phase-1 report: disconnects keep the clock running and end in `#TIME_UP` (`#CHUDAN` only for server-side causes); `#MAX_MOVES` is a rated draw; perpetual check is a loss under both rule sets until the JCSA text says otherwise; protocol violations are `#ILLEGAL_ACTION`.

Status 5 Nov 2026: phases 0 and 1 done and both gates pass (commits 5686e86, e0fb11b; submodule branch `capi` at 33c3575, to be pushed and tagged). Phase-1 gate: 100 games at chu-test-60-1F, 32,053 positions, all records pass `replay_test.py`, results agree with `random_match.py`. The replay test compares recorded SFENs only when `CHU_REPLAY_GAMES` points at the lishogi game directory, which is not on the server machine.

Risks and how the plan contains them:

| Risk | Effect | Containment |
| --- | --- | --- |
| FFI boundary bug (exception, lifetime, buffer size) crashes the whole server | All games in progress lost | `capi.cpp` catches every exception and returns a code; buffers are caller-sized with a documented maximum (legal list of 500 moves at 10 chars); the subprocess backend remains available per league as a fallback |
| Repetition rule disagrees with lishogi | Rated results differ from the lishogi records you test against | Replay the lishogi games that ended in `repetition` through the game actor in phase 1 and require the same ply and outcome |
| Scheduler and clocks drift under load | Rounds start late, engines lose on time unfairly | Clocks are measured per game from socket write/receive; the scheduler is a separate task; `/metrics` reports referee and echo latency so a slow host shows up before engines complain |
| An engine floods the connection | Server CPU spent on parsing | Per-connection line length cap (1 KiB) and a token bucket of 500 lines refilled at 100/s (a plain 100/s cap was tripped by the random players in the phase-1 gate), then `#ILLEGAL_ACTION` and disconnect |
| Rating fit unstable with few games | Early standings look random | Provisional ratings until 15 games, mean-1500 anchoring, and a random bot as a permanent floor the way floodgate relies on known engines |
| Two rule sets drift apart in the engine | A JCSA league quietly plays lishogi rules | `Rules:` is echoed in Game\_Summary and stored per game; `jcsa_test.py` runs in the Rust CI through the FFI |

Sources: [jh85/chushogi README](https://github.com/jh85/chushogi) (USI/SFEN conventions, rule sets, status rules, record layout), [TadaoYamaoka/shogi-server](https://github.com/TadaoYamaoka/shogi-server) (CSA 1.2.1 session flow, floodgate pairing and `mk_rate`), [floodgate](https://wdoor.c.u-tokyo.ac.jp/shogi/) (league naming, round cadence, 15-game rating threshold).
