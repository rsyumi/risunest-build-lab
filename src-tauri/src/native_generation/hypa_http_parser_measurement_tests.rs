use super::*;
use crate::persistent_store::commands::allocation_observer::{observe_drop, DropObservation};
use serde::Serialize;
use std::{cell::Cell, time::Instant};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Arm { Instrumented, TimingOnly }

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Initialization { FirstKnownParse, WarmSameWorker }

#[derive(Serialize)]
struct Record {
    schema: u32,
    boundary: &'static str,
    arm: Arm,
    initialization: Initialization,
    process_claim: &'static str,
    global_initialization: &'static str,
    allocator_wrapper: &'static str,
    family: String,
    size: usize,
    raw_bytes: usize,
    raw_capacity: usize,
    tracker_capacity: usize,
    warmup_calls: usize,
    iteration: usize,
    succeeded: bool,
    serialized_url_bytes: usize,
    parse_nanoseconds: u128,
    allocations: Option<DropObservation>,
    accounting: &'static str,
}

fn required(name: &str) -> String { std::env::var(name).unwrap_or_else(|_| panic!("missing {name}")) }

fn synthetic_url(family: &str, size: usize) -> String {
    assert!((1..=65536).contains(&size), "measurement size outside injected experiment range");
    match family {
        "path-query" => format!("http://synthetic.invalid/{0}?q={0}", "x".repeat(size)),
        "percent-host" => format!("http://{}/", "%61".repeat(size)),
        "combining-host" => format!("http://a{}/", "\u{0301}".repeat(size)),
        "unicode-expansion" => format!("http://{}/", "\u{0130}".repeat(size)),
        "many-labels" => format!("http://{}invalid/", "a.".repeat(size)),
        "punycode-label" => format!("http://xn--9c{}/", "a".repeat(size)),
        "malformed-ipv6" => format!("http://[{}", "a".repeat(size)),
        _ => panic!("unknown synthetic parser family"),
    }
}

fn one_parse(raw: &str, arm: Arm, capacity: usize) -> (bool, usize, u128, Option<DropObservation>) {
    let result = Cell::new((false, 0, 0));
    let parse = || {
        let start = Instant::now();
        let parsed = url::Url::parse(raw);
        let elapsed = start.elapsed().as_nanos();
        result.set((parsed.is_ok(), parsed.as_ref().map_or(0, |url| url.as_str().len()), elapsed));
        parsed
    };
    let observation = match arm {
        Arm::Instrumented => Some(observe_drop(capacity, parse).expect("parser observer scope unavailable")),
        Arm::TimingOnly => { drop(parse()); None },
    };
    let (succeeded, length, elapsed) = result.get();
    (succeeded, length, elapsed, observation)
}

#[test]
#[ignore = "opt-in parser allocation and latency measurement; use a fresh exact test process per arm/input"]
fn measure_initial_url_parser_allocations_and_latency() {
    assert_eq!(required("RISUNEST_HTTP_PARSER_MEASURE"), "yes");
    assert_eq!(required("RISUNEST_HTTP_PARSER_PROCESS"), "fresh-exact-test-process");
    let arm = match required("RISUNEST_HTTP_PARSER_ARM").as_str() {
        "instrumented" => Arm::Instrumented, "timing-only" => Arm::TimingOnly, _ => panic!("unknown measurement arm"),
    };
    let initialization = match required("RISUNEST_HTTP_PARSER_INITIALIZATION").as_str() {
        "first-known-parse" => Initialization::FirstKnownParse,
        "warm-same-worker" => Initialization::WarmSameWorker,
        _ => panic!("unknown parser initialization class"),
    };
    let family = required("RISUNEST_HTTP_PARSER_FAMILY");
    let size: usize = required("RISUNEST_HTTP_PARSER_SIZE").parse().expect("invalid measurement size");
    let capacity: usize = required("RISUNEST_HTTP_PARSER_TRACKER_CAPACITY").parse().expect("invalid tracker capacity");
    assert!((1..=65536).contains(&capacity), "tracker capacity outside injected experiment range");
    let raw = synthetic_url(&family, size);
    let raw_bytes = raw.len();
    let raw_capacity = raw.capacity();
    let warmup_calls = if matches!(initialization, Initialization::WarmSameWorker) { 3 } else { 0 };
    let iterations = if warmup_calls == 0 { 1 } else { 5 };
    let (operation, budget, slots) = super::cases::operation();
    let raw_charge = budget.reserve_owned(raw_capacity).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).build().unwrap();
    let records = runtime.block_on(async move {
        tokio::task::spawn_blocking(move || {
            operation.check().unwrap();
            for _ in 0..warmup_calls { drop(url::Url::parse(&raw)); }
            let mut records = Vec::with_capacity(iterations);
            for iteration in 0..iterations {
                operation.check().unwrap();
                let (succeeded, serialized_url_bytes, parse_nanoseconds, allocations) = one_parse(&raw, arm, capacity);
                records.push(Record {
                    schema: 1, boundary: "initial-url-parser-same-thread-result-drop", arm, initialization,
                    process_claim: "runner-declared-fresh-exact-test-process",
                    global_initialization: "first-known-call-or-explicit-warmup; process-global-state-not-directly-observed",
                    allocator_wrapper: "both arms use the System-forwarding test allocator; timing-only leaves its observer disabled",
                    family: family.clone(), size, raw_bytes, raw_capacity, tracker_capacity: capacity,
                    warmup_calls, iteration, succeeded, serialized_url_bytes, parse_nanoseconds, allocations,
                    accounting: "same-thread requested-layout event balance; excludes allocator overhead, pre-scope allocations, other threads, internal realloc overlap, stack and HTTP/codec work",
                });
            }
            drop(raw);
            drop(raw_charge);
            drop(operation);
            records
        }).await.expect("parser measurement worker failed")
    });
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    for record in records {
        println!("RISUNEST_HTTP_PARSER_MEASUREMENT {}", serde_json::to_string(&record).unwrap());
        if let Some(observation) = record.allocations {
            assert!(observation.at_return.qualified() && observation.after_drop.qualified(), "parser allocation observation is incomplete");
        }
    }
}
