use super::*;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};

fn snapshot(tracker: &Tracker) -> AllocationSnapshot { tracker.snapshot(Signals::EMPTY) }
fn allocation(tracker: &mut Tracker, pointer: usize, bytes: usize) {
    tracker.record(Event::Alloc { pointer, bytes, zeroed: false });
}

struct Block { pointer: *mut u8, layout: Layout }
impl Drop for Block {
    fn drop(&mut self) { unsafe { ALLOCATOR.dealloc(self.pointer, self.layout); } }
}
unsafe fn block(bytes: usize, zeroed: bool) -> Block {
    let layout = Layout::from_size_align(bytes, 8).unwrap();
    let pointer = if zeroed { unsafe { ALLOCATOR.alloc_zeroed(layout) } } else { unsafe { ALLOCATOR.alloc(layout) } };
    assert!(!pointer.is_null());
    Block { pointer, layout }
}
unsafe fn resized(mut value: Block, bytes: usize) -> Block {
    let pointer = unsafe { ALLOCATOR.realloc(value.pointer, value.layout, bytes) };
    assert!(!pointer.is_null());
    value.pointer = pointer;
    value.layout = Layout::from_size_align(bytes, 8).unwrap();
    value
}
fn assert_disarmed() {
    MODE.with(|mode| {
        assert!(mode.pointer.get().is_null());
        assert!(!mode.scoped.get());
        assert!(!mode.callback.get());
    });
}

#[test]
fn successful_failed_and_zeroed_events_have_separate_requested_totals() {
    let mut tracker = Tracker::new(4).unwrap();
    allocation(&mut tracker, 8, 11);
    tracker.record(Event::Alloc { pointer: 16, bytes: 7, zeroed: true });
    allocation(&mut tracker, 0, 100);
    tracker.record(Event::Alloc { pointer: 0, bytes: 200, zeroed: true });
    let value = snapshot(&tracker);
    assert_eq!((value.alloc_successes, value.alloc_requested_bytes, value.alloc_failures), (1, 11, 1));
    assert_eq!((value.zeroed_successes, value.zeroed_requested_bytes, value.zeroed_failures), (1, 7, 1));
    assert_eq!((value.live_bytes, value.peak_live_bytes), (18, 18));
    assert!(value.qualified());
    tracker.record(Event::Dealloc { pointer: 8 });
    tracker.record(Event::Dealloc { pointer: 16 });
    let value = snapshot(&tracker);
    assert_eq!((value.live_bytes, value.peak_live_bytes, value.tracked_deallocations), (0, 18, 2));
    assert!(value.qualified());
}

#[test]
fn realloc_move_in_place_shrink_and_failure_keep_observable_live_state() {
    let mut tracker = Tracker::new(4).unwrap();
    allocation(&mut tracker, 8, 16);
    tracker.record(Event::Realloc { old: 8, pointer: 8, bytes: 40 });
    assert_eq!((snapshot(&tracker).live_bytes, snapshot(&tracker).peak_live_bytes), (40, 40));
    tracker.record(Event::Realloc { old: 8, pointer: 16, bytes: 12 });
    assert!(tracker.find(8).is_none());
    assert_eq!(tracker.find(16).unwrap().1, 12);
    tracker.record(Event::Realloc { old: 16, pointer: 16, bytes: 4 });
    tracker.record(Event::Realloc { old: 16, pointer: 0, bytes: 99 });
    assert_eq!(tracker.find(16).unwrap().1, 4);
    let value = snapshot(&tracker);
    assert_eq!((value.alloc_requested_bytes, value.realloc_successes, value.realloc_failures), (16, 3, 1));
    assert_eq!((value.tracked_realloc_old_requested_bytes, value.realloc_successful_new_requested_bytes), (72, 56));
    assert_eq!((value.live_bytes, value.peak_live_bytes), (4, 40));
    tracker.record(Event::Dealloc { pointer: 16 });
    assert_eq!(snapshot(&tracker).live_bytes, 0);
    assert!(snapshot(&tracker).qualified());
}

#[test]
fn pre_scope_free_and_failed_realloc_never_subtract_scoped_storage() {
    let mut tracker = Tracker::new(2).unwrap();
    allocation(&mut tracker, 8, 10);
    tracker.record(Event::Dealloc { pointer: 80 });
    tracker.record(Event::Realloc { old: 80, pointer: 0, bytes: 50 });
    let value = snapshot(&tracker);
    assert_eq!((value.unmatched_deallocations, value.unmatched_reallocations, value.realloc_failures), (1, 1, 1));
    assert_eq!((value.live_bytes, value.tracked_realloc_old_requested_bytes), (10, 0));
    assert!(value.qualified());
    tracker.record(Event::Dealloc { pointer: 8 });
    assert_eq!(snapshot(&tracker).live_bytes, 0);
}

#[test]
fn successful_pre_scope_realloc_stays_untracked_and_permanently_unqualified() {
    let mut tracker = Tracker::new(2).unwrap();
    tracker.record(Event::Realloc { old: 80, pointer: 88, bytes: 7 });
    assert!(tracker.find(88).is_none());
    let value = snapshot(&tracker);
    assert_eq!((value.realloc_successes, value.realloc_successful_new_requested_bytes), (1, 7));
    assert_eq!((value.live_bytes, value.peak_live_bytes, value.unmatched_reallocations), (0, 0, 1));
    assert!(value.flags.unmatched_successful_realloc);
    assert!(!value.qualified());
    tracker.record(Event::Dealloc { pointer: 88 });
    allocation(&mut tracker, 8, 3);
    tracker.record(Event::Dealloc { pointer: 8 });
    let value = snapshot(&tracker);
    assert_eq!((value.live_bytes, value.unmatched_deallocations), (0, 1));
    assert!(!value.qualified());
}

#[test]
fn collisions_tombstones_address_reuse_and_duplicate_identity_are_distinct() {
    let mut tracker = Tracker::new(3).unwrap();
    for (pointer, bytes) in [(8, 1), (32, 2), (56, 3)] { allocation(&mut tracker, pointer, bytes); }
    assert!(snapshot(&tracker).qualified());
    tracker.record(Event::Dealloc { pointer: 8 });
    assert_eq!(tracker.find(56).unwrap().1, 3);
    allocation(&mut tracker, 80, 4);
    tracker.record(Event::Dealloc { pointer: 32 });
    allocation(&mut tracker, 32, 5);
    assert_eq!(tracker.find(32).unwrap().1, 5);
    assert_eq!(snapshot(&tracker).live_bytes, 12);
    assert!(snapshot(&tracker).qualified());
    allocation(&mut tracker, 80, 99);
    let value = snapshot(&tracker);
    assert_eq!(value.live_bytes, 12);
    assert_eq!(tracker.find(80).unwrap().1, 4);
    assert!(value.flags.duplicate_identity);
    assert!(!value.flags.tracker_overflow);
    assert!(!value.qualified());
}

#[test]
fn full_tracker_and_checked_integer_overflow_remain_sticky_after_zero_balance() {
    let mut full = Tracker::new(1).unwrap();
    allocation(&mut full, 8, 1);
    allocation(&mut full, 16, 2);
    assert!(snapshot(&full).flags.tracker_overflow);
    assert!(full.find(16).is_none());
    full.record(Event::Dealloc { pointer: 8 });
    assert_eq!(snapshot(&full).live_bytes, 0);
    assert!(!snapshot(&full).qualified());
    allocation(&mut full, 24, 3);
    full.record(Event::Dealloc { pointer: 24 });
    assert!(!snapshot(&full).qualified());

    let mut overflow = Tracker::new(2).unwrap();
    allocation(&mut overflow, 8, usize::MAX);
    allocation(&mut overflow, 16, 1);
    assert_eq!(snapshot(&overflow).live_bytes, usize::MAX);
    assert_eq!(snapshot(&overflow).alloc_requested_bytes, usize::MAX);
    assert!(snapshot(&overflow).flags.arithmetic_overflow);
    overflow.record(Event::Dealloc { pointer: 8 });
    overflow.record(Event::Dealloc { pointer: 16 });
    assert_eq!(snapshot(&overflow).live_bytes, 0);
    assert!(!snapshot(&overflow).qualified());
}

#[test]
fn real_callbacks_preserve_bytes_and_drop_results_before_the_second_checkpoint() {
    let observed = observe_drop(8, || unsafe {
        let value = block(16, false);
        std::ptr::write_bytes(value.pointer, 0x5a, 16);
        let value = resized(value, 40);
        assert!((0..16).all(|offset| value.pointer.add(offset).read() == 0x5a));
        let zeroed = block(8, true);
        assert!((0..8).all(|offset| zeroed.pointer.add(offset).read() == 0));
        [value, zeroed]
    }).unwrap();
    let before = observed.at_return;
    assert_eq!((before.alloc_successes, before.alloc_requested_bytes, before.zeroed_successes, before.zeroed_requested_bytes), (1, 16, 1, 8));
    assert_eq!((before.realloc_successes, before.tracked_realloc_old_requested_bytes, before.realloc_successful_new_requested_bytes), (1, 16, 40));
    assert_eq!((before.live_bytes, before.peak_live_bytes, before.total_events), (48, 48, 3));
    let after = observed.after_drop;
    assert_eq!((after.live_bytes, after.peak_live_bytes, after.tracked_deallocations, after.total_events), (0, 48, 2, 5));
    assert!(before.qualified() && after.qualified());
    assert_disarmed();
}

#[test]
fn legacy_attempt_threshold_uses_new_realloc_size_and_ignores_deallocation() {
    let (values, count) = allocations_at_least(32, || unsafe {
        [block(16, false), block(32, true)]
    });
    assert_eq!(count, 1);
    let [first, second] = values;
    let (first, growth) = allocations_at_least(32, || unsafe { resized(first, 64) });
    assert_eq!(growth, 1);
    let (first, shrink) = allocations_at_least(32, || unsafe { resized(first, 8) });
    assert_eq!(shrink, 0);
    let (_, frees) = allocations_at_least(0, || drop([first, second]));
    assert_eq!(frees, 0);
    let (_, count) = allocations_at_least(1, || ());
    assert_eq!(count, 0);
}

#[test]
fn old_and_new_modes_coexist_and_setup_only_belongs_to_the_old_attempt_count() {
    let (empty, setup) = allocations_at_least(0, || observe_drop(8, || ()).unwrap());
    assert_eq!((empty.at_return.total_events, empty.after_drop.total_events), (0, 0));
    assert!(setup > 0);
    let (observed, attempts) = allocations_at_least(0, || observe_drop(8, || unsafe {
        let first = resized(block(16, false), 24);
        [first, block(8, true)]
    }).unwrap());
    let inside = observed.at_return.alloc_successes + observed.at_return.zeroed_successes + observed.at_return.realloc_successes;
    assert_eq!(attempts, setup + inside);
    assert_eq!(inside, 3);
    assert_eq!(observed.after_drop.live_bytes, 0);
    assert!(observed.after_drop.qualified());
}

#[test]
fn synthetic_failed_attempt_keeps_legacy_attempt_and_new_failure_semantics() {
    let (_, setup) = allocations_at_least(0, || observe_drop(4, || ()).unwrap());
    let (observed, attempts) = allocations_at_least(0, || observe_drop(4, || {
        // Exercise the callback transitions without inducing process OOM.
        allocated(64);
        CallbackGuard::enter().unwrap().complete(Event::Alloc { pointer: 0, bytes: 64, zeroed: false });
    }).unwrap());
    assert_eq!(attempts, setup + 1);
    assert_eq!((observed.at_return.alloc_successes, observed.at_return.alloc_failures, observed.at_return.live_bytes), (0, 1, 0));
    assert!(observed.after_drop.qualified());
}

#[test]
fn nested_entry_rejects_without_replacing_outer_tracking_and_next_scope_is_clean() {
    let observed = observe_drop(4, || {
        assert_eq!(observe_drop(4, || panic!("nested action ran")).unwrap_err(), ScopeError::NestedScope);
    }).unwrap();
    assert_eq!(observed.at_return.nested_scope_attempts, 1);
    assert!(observed.at_return.flags.nested_scope);
    assert!(!observed.after_drop.qualified());
    assert_disarmed();
    let next = observe_drop(4, || ()).unwrap();
    assert!(next.at_return.qualified() && next.after_drop.qualified());
    assert_eq!(next.after_drop.total_events, 0);
}

#[test]
fn reentrant_real_forwarding_skips_mutation_and_marks_incomplete_attribution() {
    let observed = observe_drop(4, || unsafe {
        let outer = CallbackGuard::enter().unwrap();
        let layout = Layout::from_size_align(16, 8).unwrap();
        let pointer = System.alloc(layout);
        assert!(!pointer.is_null());
        let inner = block(8, false);
        inner.pointer.write(0x5a);
        assert_eq!(inner.pointer.read(), 0x5a);
        drop(inner);
        outer.complete(Event::Alloc { pointer: pointer as usize, bytes: 16, zeroed: false });
        Block { pointer, layout }
    }).unwrap();
    assert_eq!((observed.at_return.alloc_successes, observed.at_return.live_bytes, observed.at_return.reentrant_events), (1, 16, 2));
    assert_eq!((observed.after_drop.live_bytes, observed.after_drop.tracked_deallocations), (0, 1));
    assert!(observed.after_drop.flags.reentrant);
    assert!(!observed.after_drop.qualified());
    assert_disarmed();
}

#[test]
fn disabled_callback_does_not_dereference_its_saved_pointer() {
    let observed = observe_drop(4, || {
        let callback = CallbackGuard::enter().unwrap();
        MODE.with(|mode| mode.pointer.set(std::ptr::null_mut()));
        callback.complete(Event::Alloc { pointer: 8, bytes: 16, zeroed: false });
    }).unwrap();
    assert_eq!(observed.at_return.alloc_successes, 0);
    assert!(observed.after_drop.flags.unexpected_disabled);
    assert!(!observed.after_drop.qualified());
    assert_disarmed();
    assert!(observe_drop(4, || ()).unwrap().after_drop.qualified());
}

#[test]
fn event_counter_overflow_does_not_wrap_or_restore_qualification() {
    let observed = observe_drop(4, || unsafe {
        MODE.with(|mode| mode.signals.set(Signals { events: usize::MAX, ..Signals::EMPTY }));
        block(8, false)
    }).unwrap();
    assert_eq!(observed.at_return.total_events, usize::MAX);
    assert_eq!(observed.after_drop.total_events, usize::MAX);
    assert_eq!(observed.after_drop.live_bytes, 0);
    assert!(observed.after_drop.flags.arithmetic_overflow);
    assert!(!observed.after_drop.qualified());
}

#[test]
fn legacy_counter_overflow_is_reported_only_after_callback_return_and_disarm() {
    let returned = Cell::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| allocations_at_least(0, || {
        OBSERVED.with(|state| state.set(Some((0, usize::MAX))));
        allocated(1);
        returned.set(true);
    })));
    assert!(result.is_err());
    assert!(returned.get());
    OBSERVED.with(|state| assert!(state.get().is_none()));
    ATTEMPT_OVERFLOW.with(|overflow| assert!(!overflow.get()));
    assert_eq!(allocations_at_least(0, || ()).1, 0);
}

#[test]
fn action_and_result_drop_unwind_disarm_before_tracker_storage_is_freed() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop { fn drop(&mut self) { panic!("synthetic result drop"); } }
    for action_panics in [true, false] {
        let result = std::panic::catch_unwind(|| observe_drop(32, || {
            if action_panics { panic!("synthetic action"); }
            PanicOnDrop
        }));
        assert!(result.is_err());
        assert_disarmed();
        assert!(observe_drop(4, || ()).unwrap().after_drop.qualified());
    }
}

#[test]
fn invalid_capacity_never_runs_the_action_or_leaves_an_active_scope() {
    for (capacity, error) in [(0, ScopeError::ZeroCapacity), (usize::MAX, ScopeError::CapacityOverflow)] {
        let ran = Cell::new(false);
        assert_eq!(observe_drop(capacity, || ran.set(true)).unwrap_err(), error);
        assert!(!ran.get());
        assert_disarmed();
    }
}

#[test]
fn concurrent_threads_have_independent_scopes_and_balances() {
    let ready = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }
    let worker = |bytes| {
        observe_drop(4, || unsafe {
            let result = block(bytes, false);
            ready.fetch_add(1, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) { std::hint::spin_loop(); }
            result
        }).unwrap()
    };
    std::thread::scope(|scope| {
        let _release_on_unwind = Release(release.clone());
        let first = scope.spawn(|| worker(8));
        let second = scope.spawn(|| worker(24));
        while ready.load(Ordering::SeqCst) < 2 && !first.is_finished() && !second.is_finished() { std::thread::yield_now(); }
        release.store(true, Ordering::SeqCst);
        for (report, bytes) in [(first.join().unwrap(), 8), (second.join().unwrap(), 24)] {
            assert_eq!((report.at_return.alloc_successes, report.at_return.live_bytes, report.at_return.total_events), (1, bytes, 1));
            assert_eq!((report.after_drop.live_bytes, report.after_drop.total_events), (0, 2));
            assert!(report.after_drop.qualified());
        }
        assert_eq!(ready.load(Ordering::SeqCst), 2);
    });
    assert_disarmed();
}
