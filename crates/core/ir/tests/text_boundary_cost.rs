//! Measures `Text` boundary copies for the storage decision; run in release with
//! `cargo test --release -p risunest-ir --test text_boundary_cost -- --ignored --nocapture`.

use risunest_ir::{Budget, Limits, Text};
use std::hint::black_box;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

fn budget() -> Budget {
    let limits = Limits { max_owned_bytes: 1 << 34, max_json_bytes: 1 << 30, max_text_units: 1 << 30, max_values: 1 << 20,
        max_depth: 64, max_work: usize::MAX };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}

// Median nanoseconds per call over 15 samples, each sized to run about 20 ms.
fn measure(mut operation: impl FnMut()) -> f64 {
    let started = Instant::now();
    operation();
    let once = started.elapsed().as_nanos().max(1) as f64;
    let iterations = ((20_000_000.0 / once) as usize).clamp(1, 1_000_000);
    let mut samples = (0..15).map(|_| {
        let started = Instant::now();
        for _ in 0..iterations { operation(); }
        started.elapsed().as_nanos() as f64 / iterations as f64
    }).collect::<Vec<_>>();
    samples.sort_by(f64::total_cmp);
    samples[7]
}

fn source(kind: &str, bytes: usize) -> String {
    let pattern = match kind { "ascii" => "The quick brown fox jumps over the lazy dog. ", "hangul" => "다람쥐 헌 쳇바퀴에 타고파. ", _ => "mixed 한국어 text 🐿️ with emoji. " };
    let mut value = String::new();
    while value.len() + pattern.len() <= bytes { value.push_str(pattern); }
    while value.len() < bytes { value.push('x'); }
    value
}

#[test]
#[ignore = "release measurement"]
fn text_boundary_copy_cost() {
    let budget = budget();
    println!("| input | storage | UTF-8 bytes | UTF-16 units | from_str ns | units() collect ns | from_units ns | units() scan ns | std encode_utf16 collect ns | slice to_vec ns |");
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for bytes in [1 << 10, 64 << 10, 4 << 20] {
        for kind in ["ascii", "hangul", "mixed"] {
            let value = source(kind, bytes);
            for storage in ["utf8", "utf16"] {
                // A trailing isolated surrogate switches the hybrid storage to UTF-16.
                let mut units = value.encode_utf16().collect::<Vec<u16>>();
                if storage == "utf16" { units.push(0xd800); }
                let text = Text::from_units(&units, &budget).unwrap();
                assert_eq!(text.as_unicode().is_some(), storage == "utf8");
                let from_str = if storage == "utf8" { measure(|| { black_box(Text::from_str(black_box(&value), &budget).unwrap()); }) } else { f64::NAN };
                let collect = measure(|| { black_box(text.units().collect::<Vec<u16>>()); });
                let from_units = measure(|| { black_box(Text::from_units(black_box(&units), &budget).unwrap()); });
                let scan = measure(|| { black_box(text.units().fold(0u32, |sum, unit| sum.wrapping_add(unit as u32))); });
                // Baselines without `Text`: transcoding a `str`, and copying a ready UTF-16 slice.
                let transcode = measure(|| { black_box(black_box(&value).encode_utf16().collect::<Vec<u16>>()); });
                let copy = measure(|| { black_box(black_box(&units).to_vec()); });
                println!("| {kind} {} KiB | {storage} | {} | {} | {from_str:.0} | {collect:.0} | {from_units:.0} | {scan:.0} | {transcode:.0} | {copy:.0} |",
                    bytes >> 10, value.len(), units.len());
            }
        }
    }
}
