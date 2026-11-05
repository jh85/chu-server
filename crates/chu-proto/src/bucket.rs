//! The per-connection line limit of SERVER-PROTOCOL.md §1: a token bucket of
//! `LINE_BURST` lines refilled at `LINES_PER_SEC`. The server judges each
//! line with [`LineBucket::try_take`]; clients pace themselves with
//! [`LineBucket::wait`] on a slightly smaller bucket.

use std::time::{Duration, Instant};

use crate::{LINES_PER_SEC, LINE_BURST};

#[derive(Clone, Debug)]
pub struct LineBucket {
    cap: f64,
    rate: f64,
    tokens: f64,
    at: Instant,
}

impl LineBucket {
    /// The server's bucket, full.
    pub fn server(now: Instant) -> LineBucket {
        LineBucket::new(f64::from(LINE_BURST), f64::from(LINES_PER_SEC), now)
    }

    /// A client's bucket: 90% of the server's, so jitter between sending and
    /// receiving never empties the server's.
    pub fn client(now: Instant) -> LineBucket {
        LineBucket::new(0.9 * f64::from(LINE_BURST), 0.9 * f64::from(LINES_PER_SEC), now)
    }

    fn new(cap: f64, rate: f64, now: Instant) -> LineBucket {
        LineBucket { cap, rate, tokens: cap, at: now }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
        self.at = self.at.max(now);
    }

    /// Takes a token for a line at `now`; false if there is none.
    pub fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    /// How long to wait from `now` before sending a line; the token is taken.
    pub fn wait(&mut self, now: Instant) -> Duration {
        self.refill(now);
        let wait = Duration::from_secs_f64(((1.0 - self.tokens) / self.rate).max(0.0));
        self.tokens -= 1.0;
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_rate() {
        let t0 = Instant::now();
        let mut b = LineBucket::server(t0);
        assert!((0..500).all(|_| b.try_take(t0)));
        assert!(!b.try_take(t0));
        assert!(!b.try_take(t0 + Duration::from_millis(9)));
        assert!(b.try_take(t0 + Duration::from_millis(11)));
        // Refill stops at the burst size.
        let later = t0 + Duration::from_secs(60);
        assert!((0..500).all(|_| b.try_take(later)));
        assert!(!b.try_take(later));
    }

    #[test]
    fn a_paced_client_never_empties_the_server_bucket() {
        let t0 = Instant::now();
        let (mut server, mut client) = (LineBucket::server(t0), LineBucket::client(t0));
        let mut now = t0;
        for _ in 0..5000 {
            now += client.wait(now);
            assert!(server.try_take(now));
        }
        // 450 at once, then 90 lines/s.
        let secs = (now - t0).as_secs_f64();
        assert!((secs - (5000.0 - 450.0) / 90.0).abs() < 0.1, "{secs}");
    }
}
