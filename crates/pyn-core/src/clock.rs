use std::sync::Mutex;
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};

/// Source of "now". Injected so lease expiry is testable without sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Real time from a start instant, run `speed` times faster; for exercising lease expiry in development.
#[derive(Debug)]
pub struct ScaledClock {
    start: DateTime<Utc>,
    began: Instant,
    speed: f64,
}

impl ScaledClock {
    pub fn new(speed: f64) -> Self {
        Self {
            start: Utc::now(),
            began: Instant::now(),
            speed,
        }
    }
}

impl Clock for ScaledClock {
    fn now(&self) -> DateTime<Utc> {
        let micros = self.began.elapsed().as_secs_f64() * self.speed * 1e6;
        self.start + Duration::microseconds(micros as i64)
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self(Mutex::new(start))
    }
    pub fn advance(&self, by: Duration) {
        let mut t = self.0.lock().unwrap();
        *t += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_clock_runs_faster_than_real_time() {
        let clock = ScaledClock::new(1000.0);
        let first = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let moved = clock.now() - first;
        assert!(moved >= Duration::seconds(50), "moved {moved}");
    }

    #[test]
    fn scaled_clock_never_runs_backwards() {
        let clock = ScaledClock::new(5.0);
        let a = clock.now();
        assert!(clock.now() >= a);
    }
}
