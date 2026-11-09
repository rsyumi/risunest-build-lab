use std::{alloc::{GlobalAlloc, Layout, System}, cell::{Cell, UnsafeCell}, marker::{PhantomData, PhantomPinned}, pin::Pin, rc::Rc};

const ARITHMETIC_OVERFLOW: u32 = 1;
const TRACKER_OVERFLOW: u32 = 1 << 1;
const DUPLICATE_IDENTITY: u32 = 1 << 2;
const UNMATCHED_REALLOC: u32 = 1 << 3;
const NESTED_SCOPE: u32 = 1 << 4;
const REENTRANT: u32 = 1 << 5;
const UNEXPECTED_DISABLED: u32 = 1 << 6;
const TLS_UNAVAILABLE: u32 = 1 << 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ObservationFlags {
    pub arithmetic_overflow: bool,
    pub tracker_overflow: bool,
    pub duplicate_identity: bool,
    pub unmatched_successful_realloc: bool,
    pub nested_scope: bool,
    pub reentrant: bool,
    pub unexpected_disabled: bool,
    pub tls_unavailable: bool,
}
impl ObservationFlags {
    fn from_bits(bits: u32) -> Self {
        Self {
            arithmetic_overflow: bits & ARITHMETIC_OVERFLOW != 0,
            tracker_overflow: bits & TRACKER_OVERFLOW != 0,
            duplicate_identity: bits & DUPLICATE_IDENTITY != 0,
            unmatched_successful_realloc: bits & UNMATCHED_REALLOC != 0,
            nested_scope: bits & NESTED_SCOPE != 0,
            reentrant: bits & REENTRANT != 0,
            unexpected_disabled: bits & UNEXPECTED_DISABLED != 0,
            tls_unavailable: bits & TLS_UNAVAILABLE != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AllocationSnapshot {
    pub alloc_successes: usize,
    pub alloc_requested_bytes: usize,
    pub alloc_failures: usize,
    pub zeroed_successes: usize,
    pub zeroed_requested_bytes: usize,
    pub zeroed_failures: usize,
    pub realloc_successes: usize,
    pub realloc_failures: usize,
    pub tracked_realloc_old_requested_bytes: usize,
    pub realloc_successful_new_requested_bytes: usize,
    pub tracked_deallocations: usize,
    pub unmatched_reallocations: usize,
    pub unmatched_deallocations: usize,
    pub live_bytes: usize,
    pub peak_live_bytes: usize,
    pub total_events: usize,
    pub tracker_capacity: usize,
    pub nested_scope_attempts: usize,
    pub reentrant_events: usize,
    pub unexpected_disabled_events: usize,
    pub flags: ObservationFlags,
    pub qualified: bool,
}
impl AllocationSnapshot {
    pub fn qualified(&self) -> bool { self.qualified }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DropObservation {
    pub at_return: AllocationSnapshot,
    pub after_drop: AllocationSnapshot,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ScopeError { ZeroCapacity, CapacityOverflow, Allocation, NestedScope, CallbackActive, TlsUnavailable }

#[derive(Clone, Copy)]
struct Signals { events: usize, nested: usize, reentrant: usize, disabled: usize, flags: u32 }
impl Signals {
    const EMPTY: Self = Self { events: 0, nested: 0, reentrant: 0, disabled: 0, flags: 0 };
}
struct Mode {
    pointer: Cell<*mut Tracker>,
    scoped: Cell<bool>,
    callback: Cell<bool>,
    signals: Cell<Signals>,
}
thread_local! {
    static OBSERVED: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    static ATTEMPT_OVERFLOW: Cell<bool> = const { Cell::new(false) };
    static MODE: Mode = const { Mode {
        pointer: Cell::new(std::ptr::null_mut()), scoped: Cell::new(false),
        callback: Cell::new(false), signals: Cell::new(Signals::EMPTY),
    } };
}
fn add(value: &mut usize, amount: usize, flags: &mut u32) {
    match value.checked_add(amount) { Some(next) => *value = next, None => *flags |= ARITHMETIC_OVERFLOW }
}
fn disabled(mode: &Mode) {
    let mut signals = mode.signals.get();
    signals.flags |= UNEXPECTED_DISABLED;
    add(&mut signals.disabled, 1, &mut signals.flags);
    mode.signals.set(signals);
}

#[derive(Clone, Copy)]
enum Slot { Empty, Tombstone, Live { pointer: usize, bytes: usize } }
enum Event {
    Alloc { pointer: usize, bytes: usize, zeroed: bool },
    Realloc { old: usize, pointer: usize, bytes: usize },
    Dealloc { pointer: usize },
}
struct Tracker {
    slots: Vec<Slot>,
    stats: AllocationSnapshot,
    flags: u32,
}
impl Tracker {
    fn new(capacity: usize) -> Result<Self, ScopeError> {
        if capacity == 0 { return Err(ScopeError::ZeroCapacity); }
        let bytes = capacity.checked_mul(std::mem::size_of::<Slot>()).ok_or(ScopeError::CapacityOverflow)?;
        if bytes > isize::MAX as usize { return Err(ScopeError::CapacityOverflow); }
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity).map_err(|_| ScopeError::Allocation)?;
        slots.resize(capacity, Slot::Empty);
        Ok(Self { slots, stats: AllocationSnapshot { tracker_capacity: capacity, ..Default::default() }, flags: 0 })
    }
    fn find(&self, pointer: usize) -> Option<(usize, usize)> {
        let capacity = self.slots.len();
        if capacity == 0 { return None; }
        let mut index = (pointer >> 3) % capacity;
        for _ in 0..capacity {
            match self.slots[index] {
                Slot::Empty => return None,
                Slot::Live { pointer: found, bytes } if found == pointer => return Some((index, bytes)),
                _ => {}
            }
            index = if index == capacity - 1 { 0 } else { index + 1 };
        }
        None
    }
    fn insertion(&mut self, pointer: usize) -> Option<usize> {
        let capacity = self.slots.len();
        if capacity == 0 { self.flags |= TRACKER_OVERFLOW; return None; }
        let mut index = (pointer >> 3) % capacity;
        let mut tombstone = None;
        for _ in 0..capacity {
            match self.slots[index] {
                Slot::Empty => return Some(tombstone.unwrap_or(index)),
                Slot::Tombstone => if tombstone.is_none() { tombstone = Some(index); },
                Slot::Live { pointer: found, .. } if found == pointer => {
                    self.flags |= DUPLICATE_IDENTITY;
                    return None;
                }
                _ => {}
            }
            index = if index == capacity - 1 { 0 } else { index + 1 };
        }
        if tombstone.is_none() { self.flags |= TRACKER_OVERFLOW; }
        tombstone
    }
    fn subtract_live(&mut self, bytes: usize) {
        match self.stats.live_bytes.checked_sub(bytes) {
            Some(next) => self.stats.live_bytes = next,
            None => self.flags |= ARITHMETIC_OVERFLOW,
        }
    }
    fn insert(&mut self, pointer: usize, bytes: usize) {
        if let Some(index) = self.insertion(pointer) {
            self.slots[index] = Slot::Live { pointer, bytes };
            add(&mut self.stats.live_bytes, bytes, &mut self.flags);
            self.stats.peak_live_bytes = self.stats.peak_live_bytes.max(self.stats.live_bytes);
        }
    }
    fn record(&mut self, event: Event) {
        match event {
            Event::Alloc { pointer, bytes, zeroed } => {
                if pointer == 0 {
                    let failures = if zeroed { &mut self.stats.zeroed_failures } else { &mut self.stats.alloc_failures };
                    add(failures, 1, &mut self.flags);
                    return;
                }
                let (count, total) = if zeroed { (&mut self.stats.zeroed_successes, &mut self.stats.zeroed_requested_bytes) }
                    else { (&mut self.stats.alloc_successes, &mut self.stats.alloc_requested_bytes) };
                add(count, 1, &mut self.flags);
                add(total, bytes, &mut self.flags);
                self.insert(pointer, bytes);
            }
            Event::Realloc { old, pointer, bytes } => {
                let tracked = self.find(old);
                if let Some((_, old_bytes)) = tracked { add(&mut self.stats.tracked_realloc_old_requested_bytes, old_bytes, &mut self.flags); }
                else { add(&mut self.stats.unmatched_reallocations, 1, &mut self.flags); }
                if pointer == 0 { add(&mut self.stats.realloc_failures, 1, &mut self.flags); return; }
                add(&mut self.stats.realloc_successes, 1, &mut self.flags);
                add(&mut self.stats.realloc_successful_new_requested_bytes, bytes, &mut self.flags);
                let Some((index, old_bytes)) = tracked else {
                    // A successful pre-scope realloc stays untracked; never adopt it into an exact baseline.
                    self.flags |= UNMATCHED_REALLOC;
                    return;
                };
                self.slots[index] = Slot::Tombstone;
                self.subtract_live(old_bytes);
                // Removing the old identity first measures the observable post-realloc state, not System's overlap.
                self.insert(pointer, bytes);
            }
            Event::Dealloc { pointer } => match self.find(pointer) {
                Some((index, bytes)) => {
                    self.slots[index] = Slot::Tombstone;
                    self.subtract_live(bytes);
                    add(&mut self.stats.tracked_deallocations, 1, &mut self.flags);
                }
                None => add(&mut self.stats.unmatched_deallocations, 1, &mut self.flags),
            },
        }
    }
    fn snapshot(&self, signals: Signals) -> AllocationSnapshot {
        let bits = self.flags | signals.flags;
        AllocationSnapshot {
            total_events: signals.events, nested_scope_attempts: signals.nested,
            reentrant_events: signals.reentrant, unexpected_disabled_events: signals.disabled,
            flags: ObservationFlags::from_bits(bits), qualified: bits == 0, ..self.stats
        }
    }
}

// No tracker borrow survives the System call. Reentry only touches nonallocating TLS cells.
struct CallbackGuard { pointer: *mut Tracker, completed: bool }
impl CallbackGuard {
    fn enter() -> Option<Self> {
        MODE.try_with(|mode| {
            let pointer = mode.pointer.get();
            if !mode.scoped.get() {
                if !pointer.is_null() { disabled(mode); }
                return None;
            }
            let mut signals = mode.signals.get();
            add(&mut signals.events, 1, &mut signals.flags);
            if pointer.is_null() {
                signals.flags |= UNEXPECTED_DISABLED;
                add(&mut signals.disabled, 1, &mut signals.flags);
                mode.signals.set(signals);
                return None;
            }
            if mode.callback.get() {
                signals.flags |= REENTRANT;
                add(&mut signals.reentrant, 1, &mut signals.flags);
                mode.signals.set(signals);
                return None;
            }
            mode.signals.set(signals);
            mode.callback.set(true);
            Some(Self { pointer, completed: false })
        }).ok().flatten()
    }
    fn complete(mut self, event: Event) {
        let _ = MODE.try_with(|mode| {
            if mode.scoped.get() && mode.pointer.get() == self.pointer && mode.callback.get() {
                // The pinned scope owns this pointer, and callback entry excludes every other mutable access.
                unsafe { (&mut *self.pointer).record(event); }
            } else { disabled(mode); }
        });
        self.completed = true;
    }
}
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        let _ = MODE.try_with(|mode| {
            if !self.completed { disabled(mode); }
            if mode.pointer.get() == self.pointer { mode.callback.set(false); }
            else { mode.signals.set(Signals { flags: mode.signals.get().flags | UNEXPECTED_DISABLED, ..mode.signals.get() }); }
        });
    }
}

struct ObservedSystem;
fn allocated(bytes: usize) {
    let _ = OBSERVED.try_with(|state| {
        if let Some((minimum, count)) = state.get() {
            if bytes >= minimum {
                if let Some(next) = count.checked_add(1) { state.set(Some((minimum, next))); }
                else { let _ = ATTEMPT_OVERFLOW.try_with(|overflow| overflow.set(true)); }
            }
        }
    });
}
unsafe impl GlobalAlloc for ObservedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        let callback = CallbackGuard::enter();
        let pointer = unsafe { System.alloc(layout) };
        if let Some(callback) = callback { callback.complete(Event::Alloc { pointer: pointer as usize, bytes: layout.size(), zeroed: false }); }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        let callback = CallbackGuard::enter();
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if let Some(callback) = callback { callback.complete(Event::Alloc { pointer: pointer as usize, bytes: layout.size(), zeroed: true }); }
        pointer
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated(size);
        let callback = CallbackGuard::enter();
        let pointer = unsafe { System.realloc(ptr, layout, size) };
        if let Some(callback) = callback { callback.complete(Event::Realloc { old: ptr as usize, pointer: pointer as usize, bytes: size }); }
        pointer
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let callback = CallbackGuard::enter();
        unsafe { System.dealloc(ptr, layout); }
        if let Some(callback) = callback { callback.complete(Event::Dealloc { pointer: ptr as usize }); }
    }
}
#[global_allocator]
static ALLOCATOR: ObservedSystem = ObservedSystem;

pub(super) fn allocations_at_least<T>(minimum: usize, work: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVED.with(|state| state.set(None));
            ATTEMPT_OVERFLOW.with(|overflow| overflow.set(false));
        }
    }
    OBSERVED.with(|state| { assert!(state.get().is_none()); state.set(Some((minimum, 0))); });
    ATTEMPT_OVERFLOW.with(|overflow| overflow.set(false));
    let reset = Reset;
    let result = work();
    let count = OBSERVED.with(|state| state.get().unwrap().1);
    let overflow = ATTEMPT_OVERFLOW.with(Cell::get);
    drop(reset);
    assert!(!overflow, "allocation attempt count overflow");
    (result, count)
}

struct ScopeStorage {
    tracker: UnsafeCell<Tracker>,
    _pin: PhantomPinned,
    _thread: PhantomData<Rc<()>>,
}
struct ScopeGuard<'a> { storage: Pin<&'a ScopeStorage>, capacity: usize, _thread: PhantomData<Rc<()>> }
fn scope_available() -> Result<(), ScopeError> {
    OBSERVED.try_with(|_| ()).map_err(|_| ScopeError::TlsUnavailable)?;
    ATTEMPT_OVERFLOW.try_with(|_| ()).map_err(|_| ScopeError::TlsUnavailable)?;
    MODE.try_with(|mode| {
        if mode.scoped.get() || !mode.pointer.get().is_null() {
            let mut signals = mode.signals.get();
            signals.flags |= NESTED_SCOPE;
            add(&mut signals.nested, 1, &mut signals.flags);
            mode.signals.set(signals);
            return Err(ScopeError::NestedScope);
        }
        if mode.callback.get() { return Err(ScopeError::CallbackActive); }
        Ok(())
    }).map_err(|_| ScopeError::TlsUnavailable)?
}
impl<'a> ScopeGuard<'a> {
    fn arm(storage: Pin<&'a ScopeStorage>) -> Result<Self, ScopeError> {
        scope_available()?;
        let capacity = unsafe { (&*storage.get_ref().tracker.get()).slots.len() };
        MODE.try_with(|mode| {
            mode.signals.set(Signals::EMPTY);
            mode.scoped.set(true);
            mode.pointer.set(storage.get_ref().tracker.get());
        }).map_err(|_| ScopeError::TlsUnavailable)?;
        Ok(Self { storage, capacity, _thread: PhantomData })
    }
    fn snapshot(&self) -> AllocationSnapshot {
        let state = MODE.try_with(|mode| {
            if !mode.scoped.get() || mode.pointer.get() != self.storage.get_ref().tracker.get() || mode.callback.get() {
                let mut signals = mode.signals.get();
                signals.flags |= UNEXPECTED_DISABLED;
                mode.signals.set(signals);
            }
            (mode.signals.get(), mode.callback.get())
        });
        let (signals, unavailable) = match state {
            Ok((signals, callback)) => (signals, callback),
            Err(_) => (Signals { flags: TLS_UNAVAILABLE, ..Signals::EMPTY }, true),
        };
        if unavailable {
            return AllocationSnapshot {
                total_events: signals.events, tracker_capacity: self.capacity,
                nested_scope_attempts: signals.nested, reentrant_events: signals.reentrant,
                unexpected_disabled_events: signals.disabled,
                flags: ObservationFlags::from_bits(signals.flags), qualified: false,
                ..Default::default()
            };
        }
        // Read only known-live owned storage, with no callback borrowing it.
        unsafe { (&*self.storage.get_ref().tracker.get()).snapshot(signals) }
    }
}
impl Drop for ScopeGuard<'_> {
    fn drop(&mut self) {
        let _ = MODE.try_with(|mode| {
            mode.pointer.set(std::ptr::null_mut());
            mode.scoped.set(false);
            mode.callback.set(false);
        });
    }
}

/// Observes requested Layout bytes on this thread, excluding pre-scope storage and System realloc overlap.
/// Unqualified counters are diagnostic only. The action result is dropped before returning.
/// Tracker storage is preallocated and disarmed before destruction; no active guard or result escapes.
pub(crate) fn observe_drop<T>(capacity: usize, action: impl FnOnce() -> T) -> Result<DropObservation, ScopeError> {
    scope_available()?;
    let storage = std::pin::pin!(ScopeStorage {
        tracker: UnsafeCell::new(Tracker::new(capacity)?), _pin: PhantomPinned, _thread: PhantomData,
    });
    let guard = ScopeGuard::arm(storage.as_ref())?;
    let result = action();
    let at_return = guard.snapshot();
    drop(result);
    let after_drop = guard.snapshot();
    drop(guard);
    Ok(DropObservation { at_return, after_drop })
}

#[path = "allocation_observer_tests.rs"]
mod tests;
