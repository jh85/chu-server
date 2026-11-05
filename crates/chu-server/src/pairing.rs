//! Floodgate pairing for one round (DESIGN.md "Floodgate scheduling"):
//!
//! 1. The pool is given: idle players queued for the league before the cutoff
//!    (the session closes connections silent past the idle limit, so no
//!    silent player is still queued).
//! 2. Odd pool: the player who sat out least recently (never counts as least
//!    recently) sits out; among ties, the lowest rated.
//! 3. Sort by rating (unrated at 1500), then swap neighbours with 25%
//!    probability.
//! 4. Walk the list in adjacent pairs; if a pair met in the previous round,
//!    take the next candidate down that did not, if any.
//! 5. Colours alternate per player: whoever was sente last gets gote when
//!    the other was not also sente last; otherwise at random.

use std::collections::{HashMap, HashSet};

use chu_proto::Side;

pub const PROVISIONAL: f64 = 1500.0;

/// xorshift64*: deterministic for tests, seeded from the clock in the lobby.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// True with probability `num`/`den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next_u64() % den < num
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    /// The published rating; None until the player has enough games.
    pub rating: Option<f64>,
}

/// What the league remembers between rounds.
#[derive(Default, Debug)]
pub struct History {
    pub round: u64,
    /// Unordered pairs of the previous round.
    pub last_pairs: HashSet<(String, String)>,
    /// Round in which each player last sat out.
    pub sat_out: HashMap<String, u64>,
    pub last_side: HashMap<String, Side>,
}

fn key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Round {
    /// (sente, gote)
    pub pairs: Vec<(String, String)>,
    pub sat_out: Option<String>,
}

/// Pairs one round and records it in `hist`.
pub fn pair(pool: &[Candidate], hist: &mut History, rng: &mut Rng) -> Round {
    hist.round += 1;
    let rating = |c: &Candidate| c.rating.unwrap_or(PROVISIONAL);
    let mut list: Vec<&Candidate> = pool.iter().collect();
    // Stable order first, so ties break by name and not by queue order.
    list.sort_by(|a, b| a.name.cmp(&b.name));

    let mut sat_out = None;
    if list.len() % 2 == 1 {
        let i = (0..list.len())
            .min_by(|&i, &j| {
                let (a, b) = (list[i], list[j]);
                let last = |c: &Candidate| hist.sat_out.get(&c.name).map_or(0, |r| r + 1);
                last(a).cmp(&last(b)).then(rating(a).total_cmp(&rating(b)))
            })
            .unwrap();
        let c = list.remove(i);
        hist.sat_out.insert(c.name.clone(), hist.round);
        sat_out = Some(c.name.clone());
    }

    list.sort_by(|a, b| rating(b).total_cmp(&rating(a)));
    let mut i = 0;
    while i + 1 < list.len() {
        if rng.chance(1, 4) {
            list.swap(i, i + 1);
            i += 2;
        } else {
            i += 1;
        }
    }

    let mut pairs = Vec::new();
    while list.len() >= 2 {
        let a = list.remove(0);
        let j = (0..list.len()).find(|&j| !hist.last_pairs.contains(&key(&a.name, &list[j].name))).unwrap_or(0);
        let b = list.remove(j);
        let (sa, sb) = (hist.last_side.get(&a.name), hist.last_side.get(&b.name));
        let a_sente = match (sa, sb) {
            (Some(Side::Sente), s) if s != Some(&Side::Sente) => false,
            (s, Some(Side::Sente)) if s != Some(&Side::Sente) => true,
            (Some(Side::Gote), s) if s != Some(&Side::Gote) => true,
            (s, Some(Side::Gote)) if s != Some(&Side::Gote) => false,
            _ => rng.chance(1, 2),
        };
        pairs.push(if a_sente { (a.name.clone(), b.name.clone()) } else { (b.name.clone(), a.name.clone()) });
    }

    hist.last_pairs = pairs.iter().map(|(s, g)| key(s, g)).collect();
    for (s, g) in &pairs {
        hist.last_side.insert(s.clone(), Side::Sente);
        hist.last_side.insert(g.clone(), Side::Gote);
    }
    Round { pairs, sat_out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(names: &[(&str, Option<f64>)]) -> Vec<Candidate> {
        names.iter().map(|(n, r)| Candidate { name: n.to_string(), rating: *r }).collect()
    }

    #[test]
    fn odd_pool_sits_out_least_recent_then_lowest_rated() {
        let p = pool(&[("a", Some(1800.0)), ("b", Some(1200.0)), ("c", None)]);
        let mut h = History::default();
        let mut rng = Rng::new(7);
        // Nobody has sat out: the lowest rated (b) sits out.
        assert_eq!(pair(&p, &mut h, &mut rng).sat_out.as_deref(), Some("b"));
        // b sat out most recently, so it plays; c (1500) is below a.
        assert_eq!(pair(&p, &mut h, &mut rng).sat_out.as_deref(), Some("c"));
        assert_eq!(pair(&p, &mut h, &mut rng).sat_out.as_deref(), Some("a"));
        assert_eq!(pair(&p, &mut h, &mut rng).sat_out.as_deref(), Some("b"));
    }

    #[test]
    fn avoids_last_rounds_pairs_and_alternates_colours() {
        let p = pool(&[("a", Some(1900.0)), ("b", Some(1700.0)), ("c", Some(1500.0)), ("d", Some(1300.0))]);
        let mut h = History::default();
        let mut rng = Rng::new(1);
        let mut prev: Option<Round> = None;
        for _ in 0..200 {
            let r = pair(&p, &mut h, &mut rng);
            assert_eq!(r.pairs.len(), 2);
            assert_eq!(r.sat_out, None);
            if let Some(prev) = &prev {
                for (s, g) in &r.pairs {
                    // With four players a fresh pairing always exists.
                    assert!(!prev.pairs.iter().any(|(s2, g2)| key(s, g) == key(s2, g2)), "{prev:?} then {r:?}");
                    // Sente last time and the opponent was not: gote now.
                    let was_sente = |n: &String| prev.pairs.iter().any(|(s2, _)| s2 == n);
                    if was_sente(s) {
                        assert!(was_sente(g), "{s} sente twice against {g}: {prev:?} then {r:?}");
                    }
                }
            }
            prev = Some(r);
        }
    }

    #[test]
    fn neighbours_are_close_in_rating_and_sometimes_swapped() {
        let names: Vec<(String, Option<f64>)> = (0..8).map(|i| (format!("p{i}"), Some(2000.0 - 100.0 * i as f64))).collect();
        let p: Vec<Candidate> = names.iter().map(|(n, r)| Candidate { name: n.clone(), rating: *r }).collect();
        let mut rng = Rng::new(3);
        let (mut top_pair, mut runs) = (0, 0);
        for _ in 0..400 {
            // A fresh history each time: the previous-round rule stays out of it.
            let r = pair(&p, &mut History::default(), &mut rng);
            runs += 1;
            for (s, g) in &r.pairs {
                let gap = (s[1..].parse::<i32>().unwrap() - g[1..].parse::<i32>().unwrap()).abs();
                assert!(gap <= 3, "{s} vs {g}");
                if key(s, g) == key("p0", "p1") {
                    top_pair += 1;
                }
            }
        }
        // p0-p1 is broken up when the swap at 1 fires and the one at 0 did not
        // (3/16 of rounds).
        assert!(top_pair > runs / 3 && top_pair < runs * 9 / 10, "{top_pair}/{runs}");
    }

    #[test]
    fn tiny_pools() {
        let mut h = History::default();
        let mut rng = Rng::new(5);
        assert_eq!(pair(&[], &mut h, &mut rng), Round { pairs: vec![], sat_out: None });
        let one = pool(&[("solo", None)]);
        assert_eq!(pair(&one, &mut h, &mut rng).sat_out.as_deref(), Some("solo"));
        // Two players who met last round still play: there is nobody else.
        let two = pool(&[("a", None), ("b", None)]);
        assert_eq!(pair(&two, &mut h, &mut rng).pairs.len(), 1);
        assert_eq!(pair(&two, &mut h, &mut rng).pairs.len(), 1);
    }
}
