// Hover layer for the rating chart: a crosshair snapped to the nearest
// round, a dot on every series there, and one tooltip listing every series
// (value first). Works with mouse and touch through pointer events.
(() => {
  "use strict";
  for (const svg of document.querySelectorAll("svg.rc[data-chart]")) {
    const d = JSON.parse(svg.dataset.chart);
    const n = d.labels.length;
    const fig = svg.closest("figure");
    const tip = fig && fig.querySelector(".tooltip");
    const cross = svg.querySelector(".cross");
    const dots = [...svg.querySelectorAll(".dot")];
    const hit = svg.querySelector(".hit");
    if (!tip || !hit || n === 0) continue;
    const xOf = (i) => (n === 1 ? (d.x0 + d.x1) / 2 : d.x0 + ((d.x1 - d.x0) * i) / (n - 1));
    const yOf = (v) => d.top + ((d.bottom - d.top) * (d.hi - v)) / (d.hi - d.lo);

    function show(ev) {
      const pt = new DOMPoint(ev.clientX, ev.clientY).matrixTransform(svg.getScreenCTM().inverse());
      const i = Math.max(0, Math.min(n - 1, Math.round(((pt.x - d.x0) / (d.x1 - d.x0)) * (n - 1))));
      const x = xOf(i);
      cross.setAttribute("x1", x);
      cross.setAttribute("x2", x);
      cross.style.visibility = "visible";
      const rows = [];
      d.series.forEach((s, k) => {
        const v = s.values[i];
        const dot = dots[k];
        if (!v) { dot.style.visibility = "hidden"; return; }
        dot.setAttribute("cx", x);
        dot.setAttribute("cy", yOf(v[0]));
        dot.style.visibility = "visible";
        rows.push({ k, name: s.name, rating: v[0], games: v[1] });
      });
      rows.sort((a, b) => b.rating - a.rating);
      const head = document.createElement("div");
      head.className = "tip-head";
      head.textContent = d.labels[i];
      const body = rows.map((r) => {
        const line = document.createElement("div");
        line.className = "tip-row";
        const sw = line.appendChild(document.createElement("span"));
        sw.className = "sw";
        sw.style.background = `var(--rc-s${r.k + 1})`;
        const val = line.appendChild(document.createElement("strong"));
        val.textContent = Math.round(r.rating) + (r.games < d.publish ? "?" : "");
        const name = line.appendChild(document.createElement("span"));
        name.textContent = ` ${r.name} · ${r.games} games`;
        return line;
      });
      tip.replaceChildren(head, ...body);
      tip.hidden = false;
      const box = fig.getBoundingClientRect();
      const left = ev.clientX - box.left;
      tip.style.left = Math.min(left + 12, box.width - tip.offsetWidth - 4) + "px";
      tip.style.top = Math.max(0, ev.clientY - box.top - tip.offsetHeight - 12) + "px";
    }
    function hide() {
      tip.hidden = true;
      cross.style.visibility = "hidden";
      dots.forEach((dot) => (dot.style.visibility = "hidden"));
    }
    hit.addEventListener("pointermove", show);
    hit.addEventListener("pointerdown", show);
    hit.addEventListener("pointerleave", hide);
  }
})();
