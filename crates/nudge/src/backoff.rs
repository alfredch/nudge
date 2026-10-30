//! Reconnect delays: exponential with jitter.

use std::time::Duration;

/// First delay; the API asks clients to wait before reconnecting.
const MIN_DELAY: Duration = Duration::from_secs(5);
const MAX_DELAY: Duration = Duration::from_secs(300);
/// Each delay is varied by up to ±20 % so clients don't reconnect in lockstep.
const JITTER: f64 = 0.2;

#[derive(Debug, Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// Delay before the next attempt; grows with every call.
    pub fn next_delay(&mut self) -> Duration {
        let delay = delay_for(self.attempt, fastrand::f64());
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Starts over at the minimum delay.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Delay for the given attempt. `random` in `[0, 1)` selects the jitter.
fn delay_for(attempt: u32, random: f64) -> Duration {
    let factor = 2u32.saturating_pow(attempt.min(16));
    let base = MIN_DELAY.saturating_mul(factor).min(MAX_DELAY);
    let jitter = 1.0 + JITTER * (2.0 * random - 1.0);
    base.mul_f64(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_exponentially_up_to_max() {
        let secs: Vec<u64> = (0..9).map(|a| delay_for(a, 0.5).as_secs()).collect();
        assert_eq!(secs, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(delay_for(u32::MAX, 0.5), MAX_DELAY);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        assert_eq!(delay_for(0, 0.0), Duration::from_secs(4));
        assert_eq!(delay_for(0, 0.999_999).as_secs(), 5);
        assert!(delay_for(0, 0.999_999) < Duration::from_secs(6));
        assert_eq!(delay_for(10, 0.0), Duration::from_secs(240));
    }

    #[test]
    fn reset_starts_over() {
        let mut b = Backoff::default();
        b.next_delay();
        b.next_delay();
        assert!(b.next_delay() >= Duration::from_secs(16));
        b.reset();
        assert!(b.next_delay() <= Duration::from_secs(6));
    }
}
