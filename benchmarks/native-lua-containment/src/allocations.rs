use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats { pub largest: usize, pub largest_old: usize, pub total: usize }

thread_local! {
    static ACTIVE: Cell<Option<Stats>> = const { Cell::new(None) };
}

struct Tracking;
#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

fn record(size: usize, old: usize) {
    let _ = ACTIVE.try_with(|slot| {
        if let Some(mut stats) = slot.get() {
            if size > stats.largest { stats.largest = size; stats.largest_old = old; }
            stats.total = stats.total.saturating_add(size);
            slot.set(Some(stats));
        }
    });
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size, layout.size());
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { unsafe { System.dealloc(ptr, layout); } }
}

pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, Stats) {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { ACTIVE.with(|slot| slot.set(None)); } }
    ACTIVE.with(|slot| { assert!(slot.get().is_none()); slot.set(Some(Stats::default())); });
    let reset = Reset;
    let result = operation();
    let stats = ACTIVE.with(|slot| slot.get().unwrap());
    drop(reset);
    (result, stats)
}
