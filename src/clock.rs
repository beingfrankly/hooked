//! Clock seam: a thin abstraction over `chrono::{Utc, Local}::now()`
//! so commands can be tested with deterministic timestamps.
//!
//! Library code (src/ingest/, src/enrich/) does NOT use this — it has
//! no direct now() calls and operates on already-stamped events.

use chrono::{DateTime, Local, Utc};

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn now_local(&self) -> DateTime<Local>;
}

/// Production implementation backed by `chrono::Utc::now` and
/// `chrono::Local::now`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn now_local(&self) -> DateTime<Local> {
        Local::now()
    }
}

/// Fixed-time clock for tests.  Returns the same `DateTime<Utc>` for
/// every `now_utc()` call; `now_local()` converts that fixed UTC instant
/// to the system's local timezone.
#[cfg(test)]
pub struct FakeClock {
    pub fixed_utc: DateTime<Utc>,
}

#[cfg(test)]
impl FakeClock {
    pub fn new(fixed_utc: DateTime<Utc>) -> Self {
        Self { fixed_utc }
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.fixed_utc
    }
    fn now_local(&self) -> DateTime<Local> {
        self.fixed_utc.with_timezone(&Local)
    }
}
