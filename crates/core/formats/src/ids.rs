//! Fresh record IDs. TS draws them with `uuid`'s `v4()`, which reads `crypto.randomUUID`, not `Math.random`.

use risunest_ir::budget::Budget;
use risunest_ir::rng::Rng;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;

/// One `v4()` call per `next_id`, in the order the TS mapping makes them.
pub trait Ids {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError>;
}

/// Version 4 UUIDs from an entropy source such as `risunest_ir::rng::OsEntropy`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds<R>(pub R);

impl<R: Rng> Ids for RandomIds<R> {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        // Each draw carries 53 uniform bits.
        let mut bits: u128 = 0;
        for _ in 0..3 { bits = (bits << 53) | (self.0.random()? * (1u64 << 53) as f64) as u128; }
        let mut bytes = bits.to_be_bytes();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let id = format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]);
        Ok(Text::from_str(&id, budget)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use risunest_ir::rng::Tape;
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn random_ids_are_version_4_uuids() {
        let limits = Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 16, max_text_units: 1 << 16, max_values: 1 << 16, max_depth: 32, max_work: 1 << 20 };
        let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
        let mut ids = RandomIds(Tape::new(vec![0.0, 0.5, 0.999_999]));
        let id: String = String::from_utf16(&ids.next_id(&budget).unwrap().units().collect::<Vec<_>>()).unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        assert!(ids.next_id(&budget).is_err(), "a fourth draw runs past the tape");
    }
}
