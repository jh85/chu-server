// The game page: a 12x12 SFEN board, the move list, the clocks, and for a
// live game the SSE stream (/game/<id>/events). No move generation: every
// position comes from the server as an SFEN.
(() => {
  "use strict";
  const el = (id) => document.getElementById(id);
  const data = JSON.parse(el("game-data").textContent);

  // Lishogi letters (lowercase = gote); "+" marks a promoted piece.
  const NAME = {
    p: "pawn", l: "lance", i: "go-between", a: "reverse chariot", m: "side mover", v: "vertical mover",
    c: "copper general", s: "silver general", f: "ferocious leopard", t: "blind tiger", g: "gold general",
    e: "drunk elephant", b: "bishop", r: "rook", h: "dragon horse", d: "dragon king", o: "kirin",
    x: "phoenix", k: "king", n: "lion", q: "queen",
  };
  const PNAME = {
    p: "tokin", l: "white horse", i: "drunk elephant", a: "whale", m: "free boar", v: "flying ox",
    c: "side mover", s: "vertical mover", f: "bishop", t: "flying stag", g: "rook", b: "dragon horse",
    r: "dragon king", h: "horned falcon", d: "soaring eagle", o: "lion", x: "queen", e: "prince",
  };
  const KANJI = {
    p: "歩", l: "香", i: "仲", a: "反", m: "横", v: "竪", c: "銅", s: "銀", f: "豹", t: "虎", g: "金",
    e: "象", b: "角", r: "飛", h: "馬", d: "龍", o: "麒", x: "鳳", k: "玉", n: "獅", q: "奔",
  };
  const PKANJI = {
    p: "と", l: "駒", i: "象", a: "鯨", m: "猪", v: "牛", c: "横", s: "竪", f: "角", t: "鹿", g: "飛",
    b: "馬", r: "龍", h: "鷹", d: "鷲", o: "獅", x: "奔", e: "太",
  };

  /** SFEN board field -> 12 ranks (a..l) of 12 cells (file 12 first); null = empty. */
  function parseBoard(sfen) {
    const ranks = sfen.split(" ")[0].split("/");
    return ranks.map((row) => {
      const cells = [];
      for (let i = 0; i < row.length; ) {
        const digits = /^\d+/.exec(row.slice(i));
        if (digits) {
          for (let k = 0; k < Number(digits[0]); k++) cells.push(null);
          i += digits[0].length;
          continue;
        }
        let promoted = false;
        if (row[i] === "+") { promoted = true; i++; }
        const ch = row[i++];
        cells.push({ code: ch.toLowerCase(), promoted, gote: ch === ch.toLowerCase() });
      }
      return cells;
    });
  }

  /** Squares named in a USI move, e.g. "7e7f7g" -> [[row, col], ...]. */
  function squares(usi) {
    return [...(usi || "").matchAll(/(\d{1,2})([a-l])/g)].map((m) => [m[2].charCodeAt(0) - 97, 12 - Number(m[1])]);
  }

  let kanji = true;
  try { kanji = localStorage.getItem("chu-pieces") !== "letters"; } catch (_) { /* storage blocked */ }

  // The game: positions[0] is the start, positions[k] after move k.
  let positions = [];
  let moves = [];
  let cur = 0;
  let follow = true;

  const board = el("board");
  function drawBoard() {
    const rows = parseBoard(positions[cur]);
    const hot = new Set(cur > 0 ? squares(moves[cur - 1].usi).map(([r, c]) => r * 12 + c) : []);
    const grid = document.createElement("div");
    grid.className = "grid";
    const label = (text, cls) => {
      const d = grid.appendChild(document.createElement("div"));
      d.className = cls;
      d.textContent = text;
    };
    for (let f = 12; f >= 1; f--) label(f, "coord");
    label("", "coord");
    rows.forEach((cells, r) => {
      cells.forEach((p, c) => {
        const sq = grid.appendChild(document.createElement("div"));
        sq.className = "sq" + (hot.has(r * 12 + c) ? " hot" : "");
        if (!p) return;
        const span = sq.appendChild(document.createElement("span"));
        span.className = "pc" + (p.gote ? " gote" : "") + (p.promoted ? " promoted" : "") + (kanji ? " kanji" : "");
        const letter = (p.promoted ? "+" : "") + (p.gote ? p.code : p.code.toUpperCase());
        span.textContent = kanji ? (p.promoted ? PKANJI : KANJI)[p.code] || letter : letter;
        span.title = (p.gote ? "gote " : "sente ") + ((p.promoted ? PNAME : NAME)[p.code] || letter);
      });
      label(String.fromCharCode(97 + r), "coord rank");
    });
    board.replaceChildren(grid);
    el("ply").textContent = cur === 0 ? "start" : `${cur} / ${moves.length}`;
    document.querySelectorAll("#moves li").forEach((li, k) => li.classList.toggle("cur", k === cur - 1));
  }

  const list = el("moves");
  function drawMoves() {
    list.replaceChildren(...moves.map((m, k) => {
      const li = document.createElement("li");
      li.appendChild(document.createElement("span")).className = "mark";
      li.firstChild.textContent = m.side === "b" ? "☗" : "☖";
      li.appendChild(document.createTextNode(m.usi));
      li.addEventListener("click", () => go(k + 1));
      return li;
    }));
  }

  function go(k) {
    cur = Math.max(0, Math.min(moves.length, k));
    follow = cur === moves.length;
    drawBoard();
    // Keep the current move visible by scrolling the list, never the page.
    const li = list.children[Math.max(0, cur - 1)];
    if (li) {
      const top = li.offsetTop, bottom = top + li.offsetHeight;
      if (top < list.scrollTop) list.scrollTop = top;
      else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight;
    }
  }

  document.querySelectorAll("[data-step]").forEach((b) => b.addEventListener("click", () => {
    const step = b.dataset.step;
    go(step === "first" ? 0 : step === "back" ? cur - 1 : step === "fwd" ? cur + 1 : moves.length);
  }));
  document.addEventListener("keydown", (e) => {
    if (e.key === "ArrowLeft") go(cur - 1);
    else if (e.key === "ArrowRight") go(cur + 1);
  });
  el("pieces").addEventListener("click", () => {
    kanji = !kanji;
    try { localStorage.setItem("chu-pieces", kanji ? "kanji" : "letters"); } catch (_) { /* storage blocked */ }
    drawBoard();
  });

  // ---- clocks
  const clockEl = { b: el("clock-b"), w: el("clock-w") };
  function fmt(ms) {
    const s = Math.max(0, Math.ceil(ms / 1000));
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), ss = s % 60;
    const two = (n) => String(n).padStart(2, "0");
    return h > 0 ? `${h}:${two(m)}:${two(ss)}` : `${m}:${two(ss)}`;
  }
  // Live clock: main times at the start of the turn, the side to move, and
  // how much of the turn was used at `t0`.
  let clock = null;
  function tick() {
    if (!clock) return;
    for (const side of ["b", "w"]) {
      let main = clock.remaining[side === "b" ? 0 : 1];
      let text;
      const running = clock.running && side === clock.toMove;
      if (running) main -= clock.used + (performance.now() - clock.t0);
      if (main >= 0 || clock.byoyomi === 0) text = fmt(main);
      else text = "byoyomi " + fmt(clock.byoyomi + main);
      clockEl[side].textContent = text;
      clockEl[side].classList.toggle("running", running);
    }
  }
  setInterval(tick, 200);

  // ---- data
  function load(initial, ms) {
    positions = [initial, ...ms.map((m) => m.sfen)];
    moves = ms.map((m) => ({ side: m.side, usi: m.usi }));
    drawMoves();
    go(follow ? moves.length : cur);
  }

  const status = el("status");
  if (!data.live) {
    load(data.initial_sfen, data.moves);
    clockEl.b.textContent = "used " + fmt(data.used_ms[0]);
    clockEl.w.textContent = "used " + fmt(data.used_ms[1]);
    return;
  }

  function fromState(st) {
    load(st.initial_sfen, st.moves);
    clock = {
      remaining: st.remaining_ms, toMove: st.to_move, used: st.turn_used_ms, t0: performance.now(),
      byoyomi: st.byoyomi_ms, running: st.started,
    };
    status.textContent = st.started ? "live" : "starting";
    tick();
  }
  fromState(data.state);

  let es = null;
  function connect() {
    es = new EventSource(data.events);
    es.addEventListener("state", (e) => fromState(JSON.parse(e.data)));
    es.addEventListener("start", () => {
      Object.assign(clock, { running: true, used: 0, t0: performance.now() });
      status.textContent = "live";
    });
    es.addEventListener("move", (e) => {
      const ev = JSON.parse(e.data);
      if (ev.ply !== moves.length + 1) { es.close(); connect(); return; } // missed one: start over
      positions.push(ev.sfen);
      moves.push({ side: ev.side, usi: ev.usi });
      const atEnd = follow;
      drawMoves();
      go(atEnd ? moves.length : cur);
      Object.assign(clock, { remaining: ev.remaining_ms, toMove: ev.side === "b" ? "w" : "b", used: 0, t0: performance.now() });
      tick();
    });
    es.addEventListener("end", (e) => {
      const ev = JSON.parse(e.data);
      es.close();
      clock.running = false;
      tick();
      const score = ev.winner === "sente" ? "1–0" : ev.winner === "gote" ? "0–1" : ev.reason === "#CHUDAN" ? "void" : "½–½";
      status.textContent = `${score} ${ev.reason.replace("#", "").toLowerCase().replace(/_/g, " ")}`;
      status.classList.remove("live");
    });
    es.addEventListener("resync", () => { es.close(); connect(); });
    es.addEventListener("gone", () => { es.close(); location.reload(); });
  }
  connect();
})();
