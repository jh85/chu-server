//! Bradley–Terry ratings per league (DESIGN.md, floodgate's `mk_rate`).
//!
//! A maximum-likelihood fit over every rated, finished game of the league:
//! P(i beats j) = 1 / (1 + 10^((r_j − r_i) / SCALE)), with SCALE chosen so a
//! 200-point gap is a 76% expected score; draws count half a win to each
//! side. Every player also gets one virtual draw against a fixed 1500
//! opponent, so a player who has only lost (or only won) still has a finite
//! rating (pure maximum likelihood sends it to ∓∞). The list is then shifted
//! to mean 1500. A rating is published from 15 games on.
//!
//! The fit is Hunter's MM iteration, run in a fixed order, so the server and
//! `chu-tools rate` compute bit-identical numbers from the same games.

use std::collections::BTreeMap;

use sqlx::SqlitePool;

pub const PUBLISH_AFTER: u32 = 15;
pub const MEAN: f64 = 1500.0;

/// Rating points per factor of 10 in strength: 200 points ↔ 76%.
pub fn scale() -> f64 {
    200.0 / (0.76f64 / 0.24).log10()
}

/// One finished rated game: sente, gote, and the result `+`, `-` or `=`.
#[derive(Clone, Debug)]
pub struct Game {
    pub sente: String,
    pub gote: String,
    pub result: char,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rating {
    pub player: String,
    pub rating: f64,
    pub games: u32,
}

impl Rating {
    pub fn published(&self) -> bool {
        self.games >= PUBLISH_AFTER
    }
}

/// Fits the games; players sorted by name.
pub fn fit(games: &[Game]) -> Vec<Rating> {
    let mut r = solve(games);
    let n = r.len();
    let shift = if n == 0 { 0.0 } else { MEAN - r.iter().map(|x| x.rating).sum::<f64>() / n as f64 };
    for x in &mut r {
        x.rating += shift;
    }
    r
}

/// The maximum-likelihood ratings with the virtual opponent at 0.
fn solve(games: &[Game]) -> Vec<Rating> {
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for g in games {
        index.entry(&g.sente).or_default();
        index.entry(&g.gote).or_default();
    }
    for (i, v) in index.values_mut().enumerate() {
        *v = i;
    }
    let n = index.len();
    // Score of each player, the virtual draw included, and games per pair.
    let mut score = vec![0.5; n];
    let mut count = vec![0u32; n];
    let mut met = vec![vec![0.0f64; n]; n];
    for g in games {
        let (s, w) = (index[g.sente.as_str()], index[g.gote.as_str()]);
        let (ss, gs) = match g.result {
            '+' => (1.0, 0.0),
            '-' => (0.0, 1.0),
            _ => (0.5, 0.5),
        };
        score[s] += ss;
        score[w] += gs;
        count[s] += 1;
        count[w] += 1;
        met[s][w] += 1.0;
        met[w][s] += 1.0;
    }
    // gamma_i = 10^(r_i / SCALE); the virtual opponent has gamma 1.
    let mut gamma = vec![1.0f64; n];
    for _ in 0..100_000 {
        let next: Vec<f64> = (0..n)
            .map(|i| {
                let mut denom = 1.0 / (gamma[i] + 1.0);
                for j in 0..n {
                    if met[i][j] > 0.0 {
                        denom += met[i][j] / (gamma[i] + gamma[j]);
                    }
                }
                score[i] / denom
            })
            .collect();
        let change = next.iter().zip(&gamma).map(|(a, b)| (a / b).ln().abs()).fold(0.0, f64::max);
        gamma = next;
        if change < 1e-13 {
            break;
        }
    }
    index
        .iter()
        .map(|(name, &i)| Rating { player: name.to_string(), rating: scale() * gamma[i].log10(), games: count[i] })
        .collect()
}

/// The rated, finished, non-void games of a league, in the order played.
pub async fn league_games(pool: &SqlitePool, league: &str) -> Result<Vec<Game>, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT sente, gote, result FROM games
         WHERE league = ? AND rated = 1 AND result IN ('+', '-', '=')
         ORDER BY ended_at, id",
    )
    .bind(league)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(sente, gote, r)| Game { sente, gote, result: r.chars().next().unwrap_or('=') }).collect())
}

/// Leagues with at least one rated game.
pub async fn rated_leagues(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT league FROM games WHERE rated = 1 AND result IN ('+', '-', '=') ORDER BY league")
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str, w: &str, r: char) -> Game {
        Game { sente: s.into(), gote: w.into(), result: r }
    }

    fn expected(a: f64, b: f64) -> f64 {
        1.0 / (1.0 + 10f64.powf((b - a) / scale()))
    }

    #[test]
    fn two_hundred_points_is_76_percent() {
        assert!((expected(1700.0, 1500.0) - 0.76).abs() < 1e-12);
    }

    #[test]
    fn fit_is_a_likelihood_maximum_with_mean_1500() {
        // a > b > c with some upsets and draws.
        let mut games = Vec::new();
        for _ in 0..20 {
            games.extend([g("a", "b", '+'), g("b", "a", '-'), g("b", "c", '+'), g("c", "a", '-')]);
        }
        games.extend([g("a", "b", '-'), g("c", "b", '+'), g("a", "c", '='), g("c", "a", '='), g("b", "c", '=')]);
        let r = fit(&games);
        assert_eq!(r.iter().map(|x| x.player.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(r[0].rating > r[1].rating && r[1].rating > r[2].rating, "{r:?}");
        let mean = r.iter().map(|x| x.rating).sum::<f64>() / 3.0;
        assert!((mean - MEAN).abs() < 1e-9);
        assert_eq!(r.iter().map(|x| x.games).collect::<Vec<_>>(), [63, 63, 44]);
        // At the maximum, each player's expected score (virtual draw included)
        // equals its actual score.
        let raw: Vec<f64> = solve(&games).iter().map(|x| x.rating).collect();
        for (i, p) in r.iter().enumerate() {
            let mut exp = expected(raw[i], 0.0);
            let mut act = 0.5;
            for x in &games {
                let (opp, s) = if x.sente == p.player {
                    (&x.gote, match x.result { '+' => 1.0, '-' => 0.0, _ => 0.5 })
                } else if x.gote == p.player {
                    (&x.sente, match x.result { '-' => 1.0, '+' => 0.0, _ => 0.5 })
                } else {
                    continue;
                };
                let j = r.iter().position(|y| &y.player == opp).unwrap();
                exp += expected(raw[i], raw[j]);
                act += s;
            }
            assert!((exp - act).abs() < 1e-6, "{}: expected {exp}, actual {act}", p.player);
        }
    }

    #[test]
    fn a_player_who_only_loses_stays_finite() {
        let games: Vec<Game> = (0..30).map(|i| if i % 2 == 0 { g("az", "rnd", '+') } else { g("rnd", "az", '-') }).collect();
        let r = fit(&games);
        assert!(r.iter().all(|x| x.rating.is_finite()));
        assert!(r[0].rating - r[1].rating > 600.0, "{r:?}");
        assert!(r[0].published() && r[1].published());
        assert!(fit(&[]).is_empty());
    }
}
