use super::{number_string, Budget, Units};

fn to_int32(value: f64) -> u32 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

pub struct Sfc32 {
    state: [u32; 4],
}

impl Sfc32 {
    pub fn new(seeds: [f64; 4]) -> Self {
        Self { state: seeds.map(to_int32) }
    }

    pub fn next_value(&mut self) -> f64 {
        let [a, b, c, d] = self.state;
        let value = a.wrapping_add(b).wrapping_add(d);
        self.state = [
            b ^ (b >> 9),
            c.wrapping_add(c.wrapping_shl(3)),
            c.rotate_left(21).wrapping_add(value),
            d.wrapping_add(1),
        ];
        f64::from(value) / 4_294_967_296.0
    }
}

pub fn pick_hash_rand(cid: f64, word: &[u16], budget: &mut Budget) -> Result<f64, String> {
    budget.tick()?;
    let mut address = 5515.0;
    let mut seeds = [0.0; 4];
    for seed in &mut seeds {
        for chunk in word.chunks(256) {
            budget.tick()?;
            for &unit in chunk {
                // The shift coerces to Int32, but the accumulator additions remain JS numbers.
                let shifted = to_int32(address).wrapping_shl(5) as i32;
                address = (f64::from(shifted) + address) + f64::from(unit);
            }
        }
        *seed = address;
    }
    let mut random = Sfc32::new(seeds);
    let mut skipped = 0;
    while f64::from(skipped) < cid % 1000.0 {
        budget.tick()?;
        random.next_value();
        skipped += 1;
    }
    Ok(random.next_value())
}

pub(super) fn evaluate(word: &[u16], budget: &mut Budget) -> Result<Units, String> {
    let value = (pick_hash_rand(0.0, word, budget)? * 10_000_000.0 + 1.0).round();
    let digits = number_string(value);
    let mut output = vec![u16::from(b'0'); 7usize.saturating_sub(digits.len())];
    output.extend(digits);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfc32_zero_state_and_seed_coercion() {
        let mut random = Sfc32::new([0.0; 4]);
        for expected in [0, 1, 2, 12] {
            assert_eq!(random.next_value(), f64::from(expected) / 4_294_967_296.0);
        }
        let mut coerced = Sfc32::new([f64::NAN, f64::INFINITY, -4_294_967_296.0, 4_294_967_296.9]);
        let mut zero = Sfc32::new([0.0; 4]);
        for _ in 0..16 {
            assert_eq!(coerced.next_value(), zero.next_value());
        }
    }

    #[test]
    fn empty_word_and_cid_skip_boundaries() {
        let word = [];
        let value = |cid| pick_hash_rand(cid, &word, &mut Budget::default()).unwrap();
        assert_eq!(value(0.0), 16_545.0 / 4_294_967_296.0);
        assert_eq!(evaluate(&word, &mut Budget::default()).unwrap(), super::super::units("0000040"));
        for cid in [-1.5, -1000.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(value(cid), value(0.0));
        }
        assert_eq!(value(0.5), value(1.0));
        assert_eq!(value(999.5), value(1000.0 + 999.5));
        assert_eq!(value(1000.0), value(0.0));
    }

    #[test]
    fn hashing_can_be_cancelled_during_a_word_pass() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_cancel = cancel.clone();
        let child_progress = progress.clone();
        let handle = std::thread::spawn(move || {
            let word = vec![0xffff; 2_000_000];
            let mut budget = Budget { cancel: child_cancel, progress: child_progress, ..Budget::default() };
            pick_hash_rand(0.0, &word, &mut budget)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 16 {
            assert!(!handle.is_finished(), "hashing finished before cancellation checkpoint");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }

    #[test]
    fn hashing_observes_budget_and_cancellation() {
        let mut budget = Budget { remaining: 1, ..Budget::default() };
        assert_eq!(pick_hash_rand(0.0, &[65], &mut budget).unwrap_err(), "budget");
        let mut budget = Budget::default();
        budget.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(pick_hash_rand(0.0, &[], &mut budget).unwrap_err(), "cancelled");
    }
}
