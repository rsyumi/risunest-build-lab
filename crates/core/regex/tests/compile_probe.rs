//! Compile and first-search cost of counted-repeat patterns on the general engine, with the process heap capped at
//! 1 GiB. Run with `cargo test --release -p risunest-regex --test compile_probe -- --ignored --nocapture`.

use risunest_ir::budget::{Budget, Limits};
use risunest_regex::engine::{CompiledPattern, DEFAULT_MATCH_LIMITS};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

struct Capped;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
const CAP: usize = 1 << 30;

unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let current = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if current > CAP { CURRENT.fetch_sub(layout.size(), Ordering::Relaxed); return std::ptr::null_mut(); }
        PEAK.fetch_max(current, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Capped = Capped;

#[test]
#[ignore]
fn counted_repeat_compile_cost() {
    let budget = Budget::new(Limits { max_owned_bytes: 1 << 29, max_json_bytes: 1 << 24, max_text_units: 1 << 22, max_values: 1 << 20,
        max_depth: 64, max_work: 1 << 50 }, Arc::new(AtomicBool::new(false))).unwrap();
    let nested = (0..10).fold(String::from("a"), |inner, _| format!("(?:{inner}{{2}})"));
    let alternation = (0..5000).map(|index| format!("w{index}")).collect::<Vec<_>>().join("|");
    let input: Vec<u16> = "a".repeat(1 << 16).encode_utf16().collect();
    println!("pattern | compile us, peak bytes during compile, retained bytes | first test us, outcome");
    for source in ["a{65535}", "(?:a{65535}){65535}", "(?:(?:a{1000}){1000}){1000}", "((a{1000}){1000}){1000}",
        "(?:(?:(?:a{100}){100}){100}){100}", "(?:a|b){65535}", "[\\s\\S]{0,65535}", nested.as_str(), "a{4294967295}",
        "(?:a{5}b{5}c{5}){5}", "\\p{L}{1000}", alternation.as_str()] {
        let units: Vec<u16> = source.encode_utf16().collect();
        let flags = if source.starts_with("\\p") { "giu" } else { "g" };
        let before = CURRENT.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let start = Instant::now();
        let compiled = CompiledPattern::compile(&units, flags);
        let compile = start.elapsed().as_secs_f64() * 1e6;
        let (peak, retained) = (PEAK.load(Ordering::Relaxed) - before, CURRENT.load(Ordering::Relaxed) - before);
        let label = if source.len() > 40 { format!("{}... ({} units)", &source[..40], units.len()) } else { source.to_string() };
        match compiled {
            Ok(compiled) => {
                let start = Instant::now();
                let outcome = compiled.test(&input, DEFAULT_MATCH_LIMITS, &budget);
                println!("{label} | {compile:.0} {peak} {retained} | {:.0} {outcome:?}", start.elapsed().as_secs_f64() * 1e6);
            }
            Err(error) => println!("{label} | {compile:.0} {peak} {retained} | compile failed {error:?}"),
        }
    }
}

fn alternation(count: usize) -> String { (0..count).map(|index| format!("w{index}")).collect::<Vec<_>>().join("|") }

/// How compile time and the compiled size estimate grow with the pattern source. Alternations stop at 8000, past
/// which the fork's recursion over the alternation chain can overflow a 2 MiB stack (see `alternation_stack_probe`).
#[test]
#[ignore]
fn compile_scaling() {
    println!("shape count units | compile us, retained bytes, estimate bytes");
    for count in [1000usize, 2000, 4000, 8000, 16000, 32000, 64000] {
        let loops = "a?".repeat(count);
        let classes = (0..count).map(|index| { let low = (b'a' + (index % 25) as u8) as char; format!("[{low}-z]") }).collect::<String>();
        let shapes = [("alternation", (count <= 8000).then(|| alternation(count))), ("loops", Some(loops)), ("classes", Some(classes))];
        for (shape, source) in shapes.into_iter().filter_map(|(shape, source)| Some((shape, source?))) {
            let units: Vec<u16> = source.encode_utf16().collect();
            let before = CURRENT.load(Ordering::Relaxed);
            let start = Instant::now();
            let compiled = regex_compat_experimental::js::JsRegex::compile(&units, "g");
            let compile = start.elapsed().as_secs_f64() * 1e6;
            let retained = CURRENT.load(Ordering::Relaxed) - before;
            match compiled {
                Ok(compiled) => println!("{shape} {count} {} | {compile:.0} {retained} {}", units.len(), compiled.compiled_bytes()),
                Err(error) => println!("{shape} {count} {} | {compile:.0} compile failed {error:?}", units.len()),
            }
        }
    }
}

/// Compiles and drops one alternation on a thread with a set stack, in this process. A driver runs it once per size
/// in its own process, since a stack overflow aborts the process: `W4_PROBE_ALTERNATIVES`, `W4_PROBE_STACK` (bytes)
/// and `W4_PROBE_SHAPE` (`words` or `chars`).
#[test]
#[ignore]
fn alternation_stack_probe() {
    let (Ok(count), Ok(stack)) = (std::env::var("W4_PROBE_ALTERNATIVES"), std::env::var("W4_PROBE_STACK")) else { return };
    let (count, stack): (usize, usize) = (count.parse().unwrap(), stack.parse().unwrap());
    let source = if std::env::var("W4_PROBE_SHAPE").as_deref() == Ok("chars") { vec!["a"; count].join("|") } else { alternation(count) };
    let units: Vec<u16> = source.encode_utf16().collect();
    let start = Instant::now();
    let compiled = std::thread::Builder::new().stack_size(stack).spawn(move || {
        risunest_regex::engine::CompiledPattern::compile(&units, "g").map(|compiled| compiled.compiled_bytes())
    }).unwrap().join().unwrap();
    println!("probe alternatives={count} stack={stack} units={} us={:.0} result={compiled:?}", source.len(), start.elapsed().as_secs_f64() * 1e6);
}
