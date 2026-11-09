use super::cbs::{Draws, Recording, MAX_DRAWS};
use risunest_ir::clock::{Clock, Fixed, System};
use risunest_ir::rng::{OsEntropy, Rng, Tape};
use risunest_ir::Error;
use std::sync::{Arc, Mutex, PoisonError};

struct Cursor { rng: Box<dyn Rng + Send>, draws: usize }

#[derive(Clone)]
pub(super) struct SharedRng(Arc<Mutex<Cursor>>);

impl Rng for SharedRng {
    fn random(&mut self) -> Result<f64, Error> {
        let mut cursor = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if cursor.draws == MAX_DRAWS { return Err(Error::Limit("cbs-draws")); }
        let value = cursor.rng.random()?;
        cursor.draws += 1;
        Ok(value)
    }
}

pub(super) struct RunEntropy { pub rng: SharedRng, pub clock: Arc<dyn Clock + Send + Sync> }

impl RunEntropy {
    pub(super) fn new(draws: &Draws) -> Self {
        let (rng, now): (Box<dyn Rng + Send>, f64) = match draws {
            Draws::Record(recorder) => (Box::new(Recording(recorder.draws.clone())), recorder.now),
            Draws::Replay(entropy) => (Box::new(Tape::new(entropy.draws.iter().map(|draw| f64::from_bits(*draw)).collect())),
                f64::from_bits(entropy.now)),
            Draws::Fresh => (Box::new(OsEntropy), System.now_ms()),
        };
        Self { rng: SharedRng(Arc::new(Mutex::new(Cursor { rng, draws: 0 }))), clock: Arc::new(Fixed(now)) }
    }
}
