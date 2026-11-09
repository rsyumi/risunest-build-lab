//! `Math.random()` sources.

use crate::budget::Error;

/// A `Math.random()` source: each call yields a double in `[0, 1)`.
pub trait Rng {
    fn random(&mut self) -> Result<f64, Error>;
}

/// Replays recorded `Math.random()` values in order; running past the end is `Error::Limit("rng-tape")`.
#[derive(Debug, Clone)]
pub struct Tape { values: Vec<f64>, position: usize }
impl Tape {
    pub fn new(values: Vec<f64>) -> Self { Self { values, position: 0 } }
    /// How many values the consumer has drawn.
    pub fn consumed(&self) -> usize { self.position }
}
impl Rng for Tape {
    fn random(&mut self) -> Result<f64, Error> {
        let value = *self.values.get(self.position).ok_or(Error::Limit("rng-tape"))?;
        self.position += 1;
        Ok(value)
    }
}

/// Draws from operating-system entropy: 53 uniform bits per value, so every result is a multiple of 2^-53 in
/// `[0, 1)`. A failing entropy source is `Error::Invariant("rng-os-entropy")`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsEntropy;
impl Rng for OsEntropy {
    fn random(&mut self) -> Result<f64, Error> {
        let bits = getrandom::u64().map_err(|_| Error::Invariant("rng-os-entropy"))?;
        Ok((bits >> 11) as f64 / (1u64 << 53) as f64)
    }
}
