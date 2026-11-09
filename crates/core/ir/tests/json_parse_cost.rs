//! Measures `json::parse` and `json::stringify` on synthetic inputs of about 1, 8 and 32 MiB in six shapes, plus one
//! wide object at 16 to 256 KiB, whose cost is its duplicate-key lookups; run in
//! release with `cargo test --release -p risunest-ir --test json_parse_cost -- --ignored --nocapture`. With
//! `JSON_COST_DUMP` set to a directory, each input is also written there for
//! `benchmarks/rust-core-json/v8JsonCost.mjs`, which times V8 `JSON.parse` and `JSON.stringify` on the same bytes.

use risunest_ir::{json, Budget, Limits};
use std::hint::black_box;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

fn budget() -> Budget {
    let limits = Limits { max_owned_bytes: 1 << 36, max_json_bytes: 1 << 30, max_text_units: 1 << 30, max_values: 1 << 28,
        max_depth: 128, max_work: usize::MAX };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}

// xorshift64*, so every run builds the same bytes.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, bound: u64) -> u64 { self.next() % bound }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str { items[self.below(items.len() as u64) as usize] }
}

const WORDS: &[&str] = &["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "lantern", "harbor", "silver", "market",
    "whisper", "garden", "ember", "voyage", "castle", "meadow", "ribbon", "thunder"];
const HANGUL: &[&str] = &["다람쥐", "헌", "쳇바퀴에", "타고파", "한국어", "문장", "바다", "하늘", "기억", "이야기", "등불", "항구"];

fn prose(rng: &mut Rng, output: &mut String, bytes: usize, hangul: u64) {
    let start = output.len();
    while output.len() - start < bytes {
        output.push_str(if rng.below(100) < hangul { rng.pick(HANGUL) } else { rng.pick(WORDS) });
        match rng.below(40) {
            0 => output.push_str("\\n"),
            1 => output.push_str(". "),
            2 => output.push_str(", "),
            3 => output.push_str(" \\\""),
            _ => output.push(' '),
        }
    }
}

fn number(rng: &mut Rng, output: &mut String) {
    match rng.below(5) {
        0 => output.push_str(&rng.below(1_000_000).to_string()),
        1 => output.push_str(&format!("-{}", rng.below(100_000))),
        2 => output.push_str(&format!("{}.{}", rng.below(1000), rng.below(1_000_000))),
        3 => output.push_str(&format!("{:e}", (rng.next() >> 11) as f64 / (1u64 << 40) as f64)),
        _ => output.push_str(&format!("{}", f64::from_bits((rng.next() >> 2) | 0x3ff0_0000_0000_0000))),
    }
}

// Lorebook-like entries: many small objects with short keys and prose values.
fn lorebook(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("[");
    while output.len() < target {
        if output.len() > 1 { output.push(','); }
        output.push_str(&format!("{{\"key\":\"{}, {}\",\"secondkey\":\"\",\"insertorder\":{},\"comment\":\"", rng.pick(WORDS), rng.pick(HANGUL),
            rng.below(1000)));
        prose(rng, &mut output, 20, 30);
        output.push_str("\",\"content\":\"");
        let length = 100 + rng.below(600) as usize;
        prose(rng, &mut output, length, 30);
        output.push_str(&format!("\",\"mode\":\"normal\",\"alwaysActive\":false,\"selective\":{},\"extentions\":{{\"risu_case_sensitive\":false}},\
            \"activationPercent\":", rng.below(2) == 0));
        number(rng, &mut output);
        output.push_str(&format!(",\"id\":\"{:016x}-{:08x}\"}}", rng.next(), rng.next() as u32));
    }
    output.push(']');
    output
}

fn long_string(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("\"");
    prose(rng, &mut output, target, 0);
    output.push('"');
    output
}

// Chains of 96 nested arrays and objects, each holding a few scalars.
fn deep(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("[");
    while output.len() < target {
        if output.len() > 1 { output.push(','); }
        for level in 0..96 { output.push_str(if level % 2 == 0 { "[" } else { "{\"a\":" }); }
        number(rng, &mut output);
        for level in (0..96).rev() { output.push_str(if level % 2 == 0 { ",true]" } else { ",\"b\":null}" }); }
    }
    output.push(']');
    output
}

fn numbers(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("[");
    while output.len() < target {
        if output.len() > 1 { output.push(','); }
        output.push('[');
        for index in 0..16 {
            if index != 0 { output.push(','); }
            number(rng, &mut output);
        }
        output.push(']');
    }
    output.push(']');
    output
}

// Mostly Hangul and emoji values with no escapes.
fn unicode(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("[");
    while output.len() < target {
        if output.len() > 1 { output.push(','); }
        output.push_str(&format!("{{\"이름\":\"{} {}\",\"본문\":\"", rng.pick(HANGUL), rng.pick(HANGUL)));
        let start = output.len();
        let length = 80 + rng.below(400) as usize;
        while output.len() - start < length {
            output.push_str(rng.pick(HANGUL));
            output.push_str(if rng.below(12) == 0 { " 🐿️ " } else { " " });
        }
        output.push_str("\"}");
    }
    output.push(']');
    output
}

// One object with many distinct keys.
fn wide(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("{");
    let mut index = 0;
    while output.len() < target {
        if index != 0 { output.push(','); }
        output.push_str(&format!("\"{}_{index}\":", rng.pick(WORDS)));
        number(rng, &mut output);
        index += 1;
    }
    output.push('}');
    output
}

const ESCAPES: &[&str] = &["\\n", "\\t", "\\\"", "\\\\", "\\/", "\\u00e9", "\\u3042", "\\ud83d\\ude00", "\\u0001", "\\r\\n"];

fn escapes(rng: &mut Rng, target: usize) -> String {
    let mut output = String::from("[");
    while output.len() < target {
        if output.len() > 1 { output.push(','); }
        output.push('"');
        for _ in 0..(4 + rng.below(40)) {
            output.push_str(rng.pick(ESCAPES));
            output.push_str(rng.pick(WORDS));
        }
        output.push('"');
    }
    output.push(']');
    output
}

// Median milliseconds over samples sized to run at least 200 ms in total.
fn measure(mut operation: impl FnMut() -> f64) -> f64 {
    let once = operation();
    let samples = ((200.0 / once.max(0.001)) as usize).clamp(5, 200);
    let mut times = (0..samples).map(|_| operation()).collect::<Vec<_>>();
    times.sort_by(f64::total_cmp);
    times[samples / 2]
}

#[test]
#[ignore = "release measurement"]
fn json_parse_cost() {
    let dump = std::env::var_os("JSON_COST_DUMP").map(std::path::PathBuf::from);
    let shapes: [(&str, fn(&mut Rng, usize) -> String); 7] = [("lorebook", lorebook), ("long-string", long_string), ("deep", deep),
        ("numbers", numbers), ("unicode", unicode), ("escapes", escapes), ("wide", wide)];
    println!("| input | bytes | parse ms | parse ms/MiB | drop ms/MiB | stringify ms/MiB |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for (name, build) in shapes {
        let sizes: &[usize] = if name == "wide" { &[16, 64, 256] } else { &[1024, 8192, 32768] };
        for &kib in sizes {
            let label = if kib >= 1024 { format!("{name}-{}m", kib >> 10) } else { format!("{name}-{kib}k") };
            let source = build(&mut Rng(0x9e37_79b9_7f4a_7c15 ^ kib as u64), kib << 10);
            if let Some(directory) = &dump { std::fs::write(directory.join(format!("{label}.json")), &source).unwrap(); }
            let budget = budget();
            let size = source.len() as f64 / (1 << 20) as f64;
            let parse = measure(|| {
                let started = Instant::now();
                let value = json::parse(black_box(&source), &budget).unwrap();
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                drop(black_box(value));
                elapsed
            });
            let drop_time = measure(|| {
                let value = json::parse(&source, &budget).unwrap();
                let started = Instant::now();
                drop(black_box(value));
                started.elapsed().as_secs_f64() * 1000.0
            });
            let value = json::parse(&source, &budget).unwrap();
            let stringify = measure(|| {
                let started = Instant::now();
                let text = json::stringify(black_box(&value), &budget).unwrap();
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                drop(black_box(text));
                elapsed
            });
            println!("| {label} | {} | {parse:.1} | {:.2} | {:.2} | {:.2} |", source.len(), parse / size, drop_time / size, stringify / size);
        }
    }
}
