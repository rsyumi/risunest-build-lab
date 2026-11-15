//! A render that outgrows its budget stops while it builds tokens: the heap it holds when it fails stays far below what
//! tokenizing the whole source would take. The allocator here counts this test binary's heap.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::{Error, Text};
use risunest_render::{render_markdown, Engine, JsEngine, RenderError};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() { grow(layout.size()); }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            if size > layout.size() { grow(size - layout.size()); } else { CURRENT.fetch_sub(layout.size() - size, Ordering::Relaxed); }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Room for the source, its copies and its block tokens, but not for one token per source unit.
fn budget(units: usize) -> Budget {
    let limits = Limits {
        max_owned_bytes: 12 * units,
        max_json_bytes: 1 << 24,
        max_text_units: 1 << 24,
        max_values: 1 << 22,
        max_depth: 128,
        max_work: 1 << 40,
    };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

/// Renders `source` under `budget(units)` and returns the result and the heap growth at its peak.
fn measure(source: &str, units: usize) -> (Result<Text, RenderError>, usize) {
    let budget = budget(units);
    let text = Text::from_str(source, &budget).expect("source");
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let result = render_markdown(&text, Engine::Markdown, &JsEngine::UNICODE_16, &budget);
    let peak = PEAK.load(Ordering::Relaxed) - base;
    eprintln!("{} source units, budget {units}: peak heap growth {peak} bytes", text.len_units());
    (result, peak)
}

// One test, so no other test thread allocates while the heap is measured.
#[test]
fn an_exhausted_budget_stops_tokenizing() {
    const UNITS: usize = 200_000;
    // Bytes of heap per source unit the failing render may reach; a token per unit is several times this.
    const BOUND: usize = 32;

    // Every `*` becomes a token and a delimiter inside one emphasis rule call.
    let (result, peak) = measure(&format!("a {}", "*".repeat(UNITS - 2)), UNITS);
    assert!(matches!(result, Err(RenderError::Budget(Error::Limit("owned-bytes")))), "{:?}", result.map(|_| ()));
    assert!(peak < BOUND * UNITS, "emphasis run held {peak} bytes");

    // Every row becomes 26 tokens inside one table rule call.
    let row = "|a|b|c|d|e|f|g|h|\n";
    let rows = UNITS / row.len() - 2;
    let (result, peak) = measure(&format!("{row}|-|-|-|-|-|-|-|-|\n{}", row.repeat(rows)), UNITS);
    assert!(matches!(result, Err(RenderError::Budget(Error::Limit("owned-bytes")))), "{:?}", result.map(|_| ()));
    assert!(peak < BOUND * UNITS, "table rows held {peak} bytes");

    // Every item becomes five tokens inside one list rule call, which tokenizes each item's paragraph as a nested rule.
    let (result, peak) = measure(&"- aaaa\n".repeat(UNITS / 7), UNITS);
    assert!(matches!(result, Err(RenderError::Budget(Error::Limit("owned-bytes")))), "{:?}", result.map(|_| ()));
    assert!(peak < BOUND * UNITS, "list items held {peak} bytes");

    // The same budget renders a short run.
    let (result, _) = measure(&format!("a {}", "*".repeat(998)), UNITS);
    assert_eq!(result.expect("short run").units().count(), "<p>a ".len() + 998 + "</p>\n".len());
}
