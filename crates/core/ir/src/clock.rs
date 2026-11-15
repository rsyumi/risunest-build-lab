//! `Date.now()` sources.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, as `Date.now()` returns them.
pub trait Clock {
    fn now_ms(&self) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub f64);
impl Clock for Fixed {
    fn now_ms(&self) -> f64 { self.0 }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct System;
impl Clock for System {
    fn now_ms(&self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_millis() as f64,
            Err(before) => -(before.duration().as_millis() as f64),
        }
    }
}
