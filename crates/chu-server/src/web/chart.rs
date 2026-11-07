//! The rating-curve SVG of a league: one line per player over the league's
//! rounds (or, in a league without rounds, over its rating recomputes).
//!
//! The value at round k is the player's rating as it stood when round k + 1
//! began (the last round: now), so a round's games count once they ended.
//! Lines are dashed while the rating is provisional (under 15 games) and
//! solid once published; every line ends in a direct label, and the legend
//! names every series. Colours are the reference categorical palette's slots
//! in fixed order, assigned by player name, with a separate dark-mode set.
//! The hover layer (crosshair and tooltip) is `static/chart.js`, reading the
//! numbers from the `data-chart` attribute.

use maud::{html, Markup, PreEscaped};
use serde::Serialize;

use crate::rating::PUBLISH_AFTER;

/// The categorical palette has eight slots (`css()`); more players fold out.
pub const MAX_SERIES: usize = 8;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Series {
    pub name: String,
    /// Per x position: the rating and the games behind it, once rated.
    pub values: Vec<Option<(f64, u32)>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Chart {
    /// One label per x position, e.g. `round 12 · 2026-11-06 00:00`.
    pub labels: Vec<String>,
    /// Number shown on the x axis per position (round number or recompute).
    pub x_title: String,
    /// Sorted by name: the order of the colour slots.
    pub series: Vec<Series>,
    /// Players left out (more than MAX_SERIES); the lowest rated go first.
    pub omitted: usize,
}

/// One `rating_history` row.
pub struct HistoryRow {
    pub player: String,
    pub rating: f64,
    pub games: u32,
    /// ISO time as stored; compared as strings.
    pub computed_at: String,
}

/// Builds the chart from the league's round times (ISO, ascending) and its
/// rating history (ascending by time).
pub fn build(rounds: &[String], history: &[HistoryRow]) -> Chart {
    let (ends, labels, x_title): (Vec<Option<&str>>, Vec<String>, &str) = if rounds.is_empty() {
        // Every recompute is a position.
        let mut times: Vec<&str> = history.iter().map(|h| h.computed_at.as_str()).collect();
        times.dedup();
        let ends = (0..times.len()).map(|i| times.get(i + 1).copied()).collect();
        let labels = times.iter().enumerate().map(|(i, t)| format!("update {} · {}", i + 1, short_time(t))).collect();
        (ends, labels, "rating update")
    } else {
        let ends = (0..rounds.len()).map(|i| rounds.get(i + 1).map(|s| s.as_str())).collect();
        let labels = rounds.iter().enumerate().map(|(i, t)| format!("round {} · {}", i + 1, short_time(t))).collect();
        (ends, labels, "round")
    };
    let mut names: Vec<&str> = history.iter().map(|h| h.player.as_str()).collect();
    names.sort();
    names.dedup();
    let mut series: Vec<Series> = names
        .iter()
        .map(|&name| {
            let rows: Vec<&HistoryRow> = history.iter().filter(|h| h.player == name).collect();
            let values = ends
                .iter()
                .map(|end| {
                    rows.iter()
                        .rev()
                        .find(|h| end.is_none_or(|e| h.computed_at.as_str() < e))
                        .map(|h| (h.rating, h.games))
                })
                .collect();
            Series { name: name.to_owned(), values }
        })
        .collect();
    let mut omitted = 0;
    if series.len() > MAX_SERIES {
        let last = |s: &Series| s.values.iter().rev().flatten().next().map_or(f64::MIN, |v| v.0);
        series.sort_by(|a, b| last(b).total_cmp(&last(a)));
        omitted = series.len() - MAX_SERIES;
        series.truncate(MAX_SERIES);
        series.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Chart { labels, x_title: x_title.to_owned(), series, omitted }
}

/// `2026-11-06T00:00:00.001Z` → `2026-11-06 00:00`.
fn short_time(iso: &str) -> String {
    iso.get(..16).unwrap_or(iso).replace('T', " ")
}

const W: f64 = 720.0;
const ML: f64 = 60.0;
const MR: f64 = 190.0;
const MB: f64 = 52.0;
/// Plot height; the legend rows come on top of it.
const PH: f64 = 330.0;
/// Font size and an average character width at it, for laying out text.
const FONT: f64 = 18.0;
const CHAR: f64 = 10.2;
const ROW: f64 = 26.0;

/// Round tick step for a span, at most `max` ticks.
fn nice_step(span: f64, max: usize) -> f64 {
    for step in [10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0] {
        if span / step <= max as f64 {
            return step;
        }
    }
    10_000.0
}

pub fn css() -> &'static str {
    // Tokens per slot and the chrome, light and dark (both scopes).
    concat!(
        ".rc{--rc-s1:#2a78d6;--rc-s2:#eb6834;--rc-s3:#1baf7a;--rc-s4:#eda100;--rc-s5:#e87ba4;--rc-s6:#008300;",
        "--rc-s7:#6250d6;--rc-s8:#e34948;--rc-surface:#fcfcfb;--rc-ink:#0b0b0b;--rc-ink2:#52514e;--rc-muted:#898781;",
        "--rc-grid:#e1e0d9;--rc-axis:#c3c2b7;font:18px system-ui,-apple-system,\"Segoe UI\",sans-serif}",
        "@media (prefers-color-scheme:dark){:root:where(:not([data-theme=\"light\"])) .rc{--rc-s1:#3987e5;--rc-s2:#d95926;",
        "--rc-s3:#199e70;--rc-s4:#c98500;--rc-s5:#d55181;--rc-s6:#008300;--rc-s7:#9085e9;--rc-s8:#e66767;",
        "--rc-surface:#1a1a19;--rc-ink:#f0efec;--rc-ink2:#c3c2b7;--rc-muted:#898781;--rc-grid:#2c2c2a;--rc-axis:#383835}}",
        ":root[data-theme=\"dark\"] .rc{--rc-s1:#3987e5;--rc-s2:#d95926;--rc-s3:#199e70;--rc-s4:#c98500;--rc-s5:#d55181;",
        "--rc-s6:#008300;--rc-s7:#9085e9;--rc-s8:#e66767;--rc-surface:#1a1a19;--rc-ink:#f0efec;--rc-ink2:#c3c2b7;",
        "--rc-muted:#898781;--rc-grid:#2c2c2a;--rc-axis:#383835}",
        ".rc .bg{fill:var(--rc-surface)}.rc .grid{stroke:var(--rc-grid);stroke-width:1}",
        ".rc .axis{stroke:var(--rc-axis);stroke-width:1}.rc .tick{fill:var(--rc-muted);font-variant-numeric:tabular-nums}",
        ".rc .label{fill:var(--rc-ink2)}.rc .label b,.rc .val{fill:var(--rc-ink);font-weight:600}",
        ".rc .line{fill:none;stroke-width:2;stroke-linejoin:round;stroke-linecap:round}",
        ".rc .prov{stroke-dasharray:4 4}.rc .cross{stroke:var(--rc-muted);stroke-width:1;visibility:hidden}",
        ".rc .dot{stroke:var(--rc-surface);stroke-width:2;visibility:hidden}",
    )
}

pub fn svg(c: &Chart) -> Markup {
    let n = c.labels.len();
    let values: Vec<f64> = c.series.iter().flat_map(|s| s.values.iter().flatten().map(|v| v.0)).collect();
    if n == 0 || values.is_empty() {
        return html! {
            svg.rc xmlns="http://www.w3.org/2000/svg" viewBox=(format!("0 0 {W} 60")) role="img" aria-label="No rated games yet" {
                style { (PreEscaped(css())) }
                rect.bg x="0" y="0" width=(W) height="60" rx="8" {}
                text.label x=(W / 2.0) y="36" text-anchor="middle" { "No rated games yet." }
            }
        };
    }
    let (lo, hi) = values.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let step = nice_step((hi - lo).max(50.0), 6);
    let (lo, hi) = ((lo / step).floor() * step, (hi / step).ceil() * step);
    let (lo, hi) = if hi - lo < step { (lo - step, hi + step) } else { (lo, hi) };
    // Legend items (series, then the provisional style), wrapped into rows.
    let mut items: Vec<(f64, f64, f64)> = Vec::new(); // x, row y, text width
    let (mut at, mut row_y) = (ML, ROW);
    let names: Vec<String> = c.series.iter().map(|s| s.name.clone()).chain([format!("provisional (< {PUBLISH_AFTER})")]).collect();
    for name in &names {
        let width = 27.0 + CHAR * name.chars().count() as f64;
        if at + width > W - 8.0 && at > ML {
            at = ML;
            row_y += ROW;
        }
        items.push((at, row_y, width));
        at += width + 16.0;
    }
    let mt = row_y + ROW;
    let h = mt + PH + MB;
    let pw = W - ML - MR;
    let ph = PH;
    let x = |i: usize| if n == 1 { ML + pw / 2.0 } else { ML + pw * i as f64 / (n - 1) as f64 };
    let y = |v: f64| mt + ph * (hi - v) / (hi - lo);
    let y_ticks: Vec<f64> = (0..).map(|k| lo + step * k as f64).take_while(|v| *v <= hi + 0.5).collect();
    let x_step = [1usize, 2, 4, 6, 8, 12, 24, 48, 96, 192, 384].into_iter().find(|s| n / s <= 8).unwrap_or(n);
    let x_ticks: Vec<usize> = (0..n).filter(|i| (i + 1) % x_step == 0 || *i == 0).collect();

    // Path pieces per series: (d, provisional).
    let paths: Vec<Vec<(String, bool)>> = c
        .series
        .iter()
        .map(|s| {
            let mut out: Vec<(String, bool)> = Vec::new();
            let mut prev: Option<(usize, f64, bool)> = None;
            for (i, v) in s.values.iter().enumerate() {
                let Some((r, games)) = *v else {
                    prev = None;
                    continue;
                };
                let prov = games < PUBLISH_AFTER;
                let pt = format!("{:.1},{:.1}", x(i), y(r));
                match (prev, out.last_mut()) {
                    (Some((_, _, p)), Some(last)) if p == prov => {
                        last.0.push_str(&format!(" L{pt}"));
                    }
                    (Some((pi, pr, _)), _) => {
                        // A new style starts at the previous point, so the line
                        // stays unbroken where a rating becomes published.
                        out.push((format!("M{:.1},{:.1} L{pt}", x(pi), y(pr)), prov));
                    }
                    (None, _) => out.push((format!("M{pt}"), prov)),
                }
                prev = Some((i, r, prov));
            }
            out
        })
        .collect();

    // End labels, pushed apart to a line's height.
    let mut ends: Vec<(usize, f64, f64)> = c
        .series
        .iter()
        .enumerate()
        .filter_map(|(k, s)| {
            let (i, (r, _)) = s.values.iter().enumerate().rev().find_map(|(i, v)| v.map(|v| (i, v)))?;
            let _ = i;
            Some((k, r, y(r)))
        })
        .collect();
    ends.sort_by(|a, b| a.2.total_cmp(&b.2));
    for j in 1..ends.len() {
        if ends[j].2 - ends[j - 1].2 < FONT + 3.0 {
            ends[j].2 = ends[j - 1].2 + FONT + 3.0;
        }
    }
    let data = serde_json::json!({
        "labels": c.labels,
        "series": c.series.iter().map(|s| serde_json::json!({
            "name": s.name,
            "values": s.values.iter().map(|v| v.map(|(r, g)| [r.round(), g as f64])).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "x0": ML, "x1": ML + pw, "top": mt, "bottom": mt + ph, "lo": lo, "hi": hi, "w": W, "h": h,
        "publish": PUBLISH_AFTER,
    });
    html! {
        svg.rc xmlns="http://www.w3.org/2000/svg" viewBox=(format!("0 0 {W} {h:.0}")) role="img"
            aria-label=(format!("Rating by {} for {}", c.x_title, c.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")))
            data-chart=(data.to_string()) {
            style { (PreEscaped(css())) }
            rect.bg x="0" y="0" width=(W) height=(h) rx="8" {}
            // Legend: every series, then the provisional style.
            @for (k, (lx, ly, _)) in items.iter().enumerate() {
                @let prov = k == c.series.len();
                @let colour = if prov { "var(--rc-muted)".to_owned() } else { format!("var(--rc-s{})", k + 1) };
                line.line.prov[prov] x1=(lx) x2=(lx + 20.0) y1=(ly - 6.0) y2=(ly - 6.0) style=(format!("stroke:{colour}")) {}
                text.label x=(lx + 27.0) y=(ly) { (names[k]) }
            }
            // Grid and axes.
            @for v in &y_ticks {
                line.grid x1=(ML) x2=(ML + pw) y1=(format!("{:.1}", y(*v))) y2=(format!("{:.1}", y(*v))) {}
                text.tick x=(ML - 8.0) y=(format!("{:.1}", y(*v) + FONT * 0.35)) text-anchor="end" { (format!("{v:.0}")) }
            }
            line.axis x1=(ML) x2=(ML + pw) y1=(mt + ph) y2=(mt + ph) {}
            @for i in &x_ticks {
                text.tick x=(format!("{:.1}", x(*i))) y=(mt + ph + FONT + 6.0) text-anchor="middle" { (i + 1) }
            }
            text.tick x=(ML + pw / 2.0) y=(h - 8.0) text-anchor="middle" { (c.x_title) }
            // The lines.
            @for (k, pieces) in paths.iter().enumerate() {
                @for (d, prov) in pieces {
                    path.line.prov[*prov] d=(d) style=(format!("stroke:var(--rc-s{})", k + 1)) {}
                }
            }
            // Direct labels at the line ends.
            @for (k, r, ly) in &ends {
                text.label x=(ML + pw + 8.0) y=(format!("{:.1}", ly + FONT * 0.35)) {
                    tspan.val { (format!("{r:.0}")) } " " (c.series[*k].name)
                }
            }
            // Hover layer: crosshair, one dot per series, hit area.
            line.cross x1="0" x2="0" y1=(mt) y2=(mt + ph) {}
            @for k in 0..c.series.len() {
                circle.dot r="4" cx="0" cy="0" style=(format!("fill:var(--rc-s{})", k + 1)) {}
            }
            rect.hit x=(ML) y=(mt) width=(pw) height=(ph) fill="transparent" {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(p: &str, r: f64, g: u32, t: &str) -> HistoryRow {
        HistoryRow { player: p.into(), rating: r, games: g, computed_at: t.into() }
    }

    #[test]
    fn values_are_ratings_as_each_next_round_begins() {
        let rounds = ["2026-11-05T18:30:00.000Z", "2026-11-05T19:00:00.000Z", "2026-11-05T19:30:00.000Z"].map(String::from);
        let history = [
            row("a", 1600.0, 1, "2026-11-05T18:31:00.000Z"),
            row("b", 1400.0, 1, "2026-11-05T18:31:00.000Z"),
            // A round-1 game that ended during round 2 counts for round 2.
            row("a", 1650.0, 2, "2026-11-05T19:10:00.000Z"),
            row("b", 1350.0, 2, "2026-11-05T19:10:00.000Z"),
            row("c", 1500.0, 1, "2026-11-05T19:31:00.000Z"),
        ];
        let c = build(&rounds, &history);
        assert_eq!(c.labels[0], "round 1 · 2026-11-05 18:30");
        let names: Vec<&str> = c.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(c.series[0].values, [Some((1600.0, 1)), Some((1650.0, 2)), Some((1650.0, 2))]);
        assert_eq!(c.series[2].values, [None, None, Some((1500.0, 1))]);
        let svg = svg(&c).into_string();
        assert!(svg.starts_with("<svg class=\"rc\""), "{svg}");
        // Three solid-or-dashed lines (all provisional here), three end labels.
        assert_eq!(svg.matches("class=\"line prov\" d=").count(), 3);
        assert_eq!(svg.matches("<tspan class=\"val\">").count(), 3);
    }

    #[test]
    fn without_rounds_every_update_is_a_position() {
        let history = [row("a", 1600.0, 20, "t1"), row("b", 1400.0, 20, "t1"), row("a", 1610.0, 21, "t2"), row("b", 1390.0, 21, "t2")];
        let c = build(&[], &history);
        assert_eq!(c.x_title, "rating update");
        assert_eq!(c.labels.len(), 2);
        assert_eq!(c.series[1].values, [Some((1400.0, 20)), Some((1390.0, 21))]);
        // Published from the start: solid lines.
        let svg = svg(&c).into_string();
        assert_eq!(svg.matches("class=\"line\" d=").count(), 2);
    }

    #[test]
    fn at_most_eight_series_the_highest_rated() {
        let history: Vec<HistoryRow> = (0..10).map(|i| row(&format!("p{i}"), 1000.0 + 100.0 * i as f64, 1, "t")).collect();
        let c = build(&[], &history);
        assert_eq!(c.series.len(), 8);
        assert_eq!(c.omitted, 2);
        assert!(c.series.iter().all(|s| s.name != "p0" && s.name != "p1"));
    }
}
