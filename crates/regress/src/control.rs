//! Opt-in controls for matching already compiled UTF-16 expressions.
use crate::cursor::Direction;
use crate::indexing::InputIndexer;
use core::{convert::Infallible, fmt::Debug, ops::Range};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

pub(crate) trait ExecutionPolicy<Input: InputIndexer>: Debug {
    type Error;
    fn charge(&mut self, units: usize) -> Result<(), Self::Error>;
    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), Self::Error>;
    fn enter_lookaround(&mut self) -> Result<(), Self::Error>;
    fn leave_lookaround(&mut self);
    fn backref<Dir: Direction>(&mut self, input: &Input, dir: Dir,
        range: Range<Input::Position>, pos: &mut Input::Position, icase: bool,
    ) -> Result<bool, Self::Error>;
}

#[derive(Debug)]
pub(crate) struct Unbounded;
impl<Input: InputIndexer> ExecutionPolicy<Input> for Unbounded {
    type Error = Infallible;
    #[inline(always)]
    fn charge(&mut self, _: usize) -> Result<(), Infallible> { Ok(()) }
    #[inline(always)]
    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), Infallible> {
        values.reserve(additional);
        Ok(())
    }
    #[inline(always)]
    fn enter_lookaround(&mut self) -> Result<(), Infallible> { Ok(()) }
    #[inline(always)]
    fn leave_lookaround(&mut self) {}
    #[inline(always)]
    fn backref<Dir: Direction>(&mut self, input: &Input, dir: Dir,
        range: Range<Input::Position>, pos: &mut Input::Position, icase: bool,
    ) -> Result<bool, Infallible> {
        Ok(if icase { crate::matchers::backref_icase(input, dir, range, pos) }
            else { crate::matchers::backref(input, dir, range, pos) })
    }
}

#[cfg(all(feature = "std", feature = "utf16"))]
mod checked {
    use super::*;
    use std::{sync::atomic::{AtomicBool, Ordering}, time::Instant};

    /// Terminal matching failure. The first failure remains on the budget.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum MatchAbort {
        Cancelled, DeadlineExceeded, WorkLimit, AllocationBudget,
        AllocationFailure, LookaroundDepth,
    }

    /// Limits for already-compiled matching, not compilation or process memory.
    #[derive(Clone, Copy, Debug)]
    pub struct MatchLimits {
        pub work_units: u64,
        /// Cumulative full capacity reservations, without refunds.
        pub allocation_bytes: usize,
        pub lookaround_depth: usize,
    }

    /// Last completed reservation observation, including excess rejected afterward.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct ReservationObservation {
        pub requested_bytes: usize,
        pub observed_bytes: usize,
        pub excess_bytes: usize,
    }

    /// Conservative accounting. These values are neither live nor peak heap usage.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct MatchUsage {
        pub work_units: u64,
        pub charged_allocation_bytes: usize,
        /// Sum of requested bytes for completed reservations, excluding failed attempts.
        /// None means this cumulative observation overflowed usize.
        pub requested_bytes: Option<usize>,
        /// Sum of actual capacities for completed reservations.
        /// None means this cumulative observation overflowed usize.
        pub observed_bytes: Option<usize>,
        pub last_reservation: Option<ReservationObservation>,
        pub active_lookaround_depth: usize,
        pub maximum_lookaround_depth: usize,
    }

    /// A caller poll that receives the cumulative work units; `false` cancels.
    struct Pulse<'cancel>(&'cancel dyn Fn(u64) -> bool);
    impl Debug for Pulse<'_> {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { formatter.write_str("Pulse") }
    }

    /// Reusable cumulative budget with sticky terminal errors.
    ///
    /// Polls cancellation, the pulse and deadlines at entry/exit, reservations and at most
    /// 64 charged actions apart. Allocation and native scheduling can still stall.
    /// No hard wall-time, physical heap, constructor or destruction bound is made.
    #[derive(Debug)]
    pub struct MatchBudget<'cancel> {
        limits: MatchLimits,
        remaining_work: u64,
        remaining_allocation: usize,
        cancel: Option<&'cancel AtomicBool>,
        pulse: Option<Pulse<'cancel>>,
        deadline: Option<Instant>,
        terminal: Option<MatchAbort>,
        until_poll: usize,
        usage: MatchUsage,
        #[cfg(test)]
        pub(crate) progress: Option<&'cancel std::sync::atomic::AtomicU64>,
    }

    impl<'cancel> MatchBudget<'cancel> {
        pub fn new(limits: MatchLimits, cancel: Option<&'cancel AtomicBool>, deadline: Option<Instant>) -> Self {
            Self { limits, remaining_work: limits.work_units,
                remaining_allocation: limits.allocation_bytes, cancel, pulse: None, deadline,
                terminal: None, until_poll: 64,
                usage: MatchUsage { work_units: 0, charged_allocation_bytes: 0,
                    requested_bytes: Some(0), observed_bytes: Some(0), last_reservation: None,
                    active_lookaround_depth: 0, maximum_lookaround_depth: 0 },
                #[cfg(test)] progress: None }
        }
        /// Also polls `pulse` with the cumulative work units wherever cancellation is polled;
        /// `false` ends the search with [`MatchAbort::Cancelled`].
        pub fn with_pulse(mut self, pulse: &'cancel dyn Fn(u64) -> bool) -> Self {
            self.pulse = Some(Pulse(pulse));
            self
        }
        pub fn usage(&self) -> MatchUsage { self.usage }
        pub fn remaining_work(&self) -> u64 { self.remaining_work }
        pub fn remaining_allocation(&self) -> usize { self.remaining_allocation }
        pub fn terminal(&self) -> Option<MatchAbort> { self.terminal }
        fn fail<T>(&mut self, error: MatchAbort) -> Result<T, MatchAbort> {
            Err(*self.terminal.get_or_insert(error))
        }
        // Kept out of line: inlined into `charge`, the pulse branch slowed the matcher loop by about a third.
        #[inline(never)]
        pub(crate) fn check(&mut self) -> Result<(), MatchAbort> {
            if let Some(error) = self.terminal { return Err(error); }
            if self.cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
                return self.fail(MatchAbort::Cancelled);
            }
            if self.pulse.as_ref().is_some_and(|pulse| !(pulse.0)(self.usage.work_units)) {
                return self.fail(MatchAbort::Cancelled);
            }
            if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.fail(MatchAbort::DeadlineExceeded);
            }
            Ok(())
        }
        pub(crate) fn charge(&mut self, units: usize) -> Result<(), MatchAbort> {
            if let Some(error) = self.terminal { return Err(error); }
            let Ok(units64) = u64::try_from(units) else { return self.fail(MatchAbort::WorkLimit); };
            let Some(remaining) = self.remaining_work.checked_sub(units64) else {
                return self.fail(MatchAbort::WorkLimit);
            };
            // Large bulk actions poll before execution; element loops never skip a quantum.
            if units >= self.until_poll { self.check()?; self.until_poll = 64usize.saturating_sub(units); }
            else { self.until_poll -= units; }
            self.remaining_work = remaining;
            self.usage.work_units = self.limits.work_units - remaining;
            #[cfg(test)]
            if let Some(progress) = self.progress { progress.store(self.usage.work_units, Ordering::Release); }
            Ok(())
        }
        fn allocate(&mut self, bytes: usize) -> Result<(), MatchAbort> {
            let Some(remaining) = self.remaining_allocation.checked_sub(bytes) else {
                return self.fail(MatchAbort::AllocationBudget);
            };
            self.remaining_allocation = remaining;
            self.usage.charged_allocation_bytes = self.limits.allocation_bytes - remaining;
            Ok(())
        }
        fn observe(&mut self, requested: usize, observed: usize) -> Result<(), MatchAbort> {
            let excess = observed.saturating_sub(requested);
            self.usage.requested_bytes = self.usage.requested_bytes.and_then(|v| v.checked_add(requested));
            self.usage.observed_bytes = self.usage.observed_bytes.and_then(|v| v.checked_add(observed));
            self.usage.last_reservation = Some(ReservationObservation {
                requested_bytes: requested, observed_bytes: observed, excess_bytes: excess });
            self.allocate(excess)
        }
        pub(crate) fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), MatchAbort> {
            self.check()?;
            let Some(capacity) = values.len().checked_add(additional) else {
                return self.fail(MatchAbort::AllocationBudget);
            };
            if capacity <= values.capacity() { return Ok(()); }
            let Some(bytes) = capacity.checked_mul(core::mem::size_of::<T>()) else {
                return self.fail(MatchAbort::AllocationBudget);
            };
            self.charge(values.len())?;
            self.allocate(bytes)?;
            if values.try_reserve_exact(additional).is_err() { return self.fail(MatchAbort::AllocationFailure); }
            let Some(actual) = values.capacity().checked_mul(core::mem::size_of::<T>()) else {
                self.usage.observed_bytes = None;
                return self.fail(MatchAbort::AllocationBudget);
            };
            self.observe(bytes, actual)?;
            self.check()
        }
        fn enter(&mut self) -> Result<(), MatchAbort> {
            self.check()?;
            if self.usage.active_lookaround_depth >= self.limits.lookaround_depth {
                return self.fail(MatchAbort::LookaroundDepth);
            }
            self.usage.active_lookaround_depth += 1;
            self.usage.maximum_lookaround_depth = self.usage.maximum_lookaround_depth.max(self.usage.active_lookaround_depth);
            Ok(())
        }
        fn leave(&mut self) { self.usage.active_lookaround_depth -= 1; }
    }


    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{sync::atomic::AtomicU64, time::Duration};
        fn limits() -> MatchLimits { MatchLimits { work_units: 1_000_000_000, allocation_bytes: 64 * 1024 * 1024, lookaround_depth: 32 } }

        #[test]
        fn actual_search_observes_live_cancellation_after_progress() {
            let regex = crate::Regex::new("(?:a+)+$").unwrap();
            let input: Vec<u16> = format!("{}!", "a".repeat(64)).encode_utf16().collect();
            let cancelled = AtomicBool::new(false);
            let finished = AtomicBool::new(false);
            let progress = AtomicU64::new(0);
            let outcome = std::thread::scope(|scope| {
                let setter = scope.spawn(|| {
                    while progress.load(Ordering::Acquire) < 512 && !finished.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    if progress.load(Ordering::Acquire) >= 512 { cancelled.store(true, Ordering::Release); }
                });
                let mut budget = MatchBudget::new(limits(), Some(&cancelled), Some(Instant::now() + Duration::from_secs(5)));
                budget.progress = Some(&progress);
                let outcome = regex.find_first_from_ucs2_controlled(&input, 0, &mut budget).err();
                finished.store(true, Ordering::Release);
                setter.join().unwrap();
                assert_eq!(budget.usage().active_lookaround_depth, 0);
                outcome
            });
            assert!(progress.load(Ordering::Acquire) >= 512);
            assert_eq!(outcome, Some(MatchAbort::Cancelled));
            assert!(regex.find_first_from_ucs2_controlled(&[97], 0, &mut MatchBudget::new(limits(), None, None)).unwrap().is_some());
        }

        #[test]
        fn pulse_sees_progress_and_cancels_within_one_poll() {
            let regex = crate::Regex::new("(?:a+)+$").unwrap();
            let input: Vec<u16> = format!("{}!", "a".repeat(64)).encode_utf16().collect();
            let pulse = |work: u64| work < 4096;
            let mut budget = MatchBudget::new(MatchLimits { work_units: 1_000_000, ..limits() }, None, None).with_pulse(&pulse);
            assert_eq!(regex.find_first_from_ucs2_controlled(&input, 0, &mut budget).err(), Some(MatchAbort::Cancelled));
            let work = budget.usage().work_units;
            assert!((4096..4096 + 64).contains(&work), "{work}");
        }

        #[test]
        fn anchored_search_tries_only_the_start() {
            let regex = crate::Regex::new("a.*b").unwrap();
            let input = [97u16; 4096];
            let mut anchored = MatchBudget::new(limits(), None, None);
            assert!(regex.find_at_ucs2_controlled(&input, 0, &mut anchored).unwrap().is_none());
            let mut scan = MatchBudget::new(limits(), None, None);
            assert!(regex.find_first_from_ucs2_controlled(&input, 0, &mut scan).unwrap().is_none());
            let (one, all) = (anchored.usage().work_units, scan.usage().work_units);
            assert!(one < 4 * 4096 && one * 1000 < all, "{one} {all}");
            let text: Vec<u16> = "xab".encode_utf16().collect();
            let budget = || MatchBudget::new(limits(), None, None);
            assert!(regex.find_at_ucs2_controlled(&text, 0, &mut budget()).unwrap().is_none());
            assert_eq!(regex.find_at_ucs2_controlled(&text, 1, &mut budget()).unwrap().unwrap().range(), 1..3);
            assert_eq!(regex.find_at_utf16_controlled(&text, 1, &mut budget()).unwrap().unwrap().range(), 1..3);
            assert!(regex.find_at_utf16_controlled(&text, 0, &mut budget()).unwrap().is_none());
            assert_eq!(regex.find_first_from_ucs2_controlled(&text, 0, &mut budget()).unwrap().unwrap().range(), 1..3);
        }

        #[test]
        fn a_full_comparison_chunk_requires_polling_before_the_next_action() {
            let cancelled = AtomicBool::new(false);
            let mut budget = MatchBudget::new(limits(), Some(&cancelled), None);
            budget.charge(64).unwrap();
            cancelled.store(true, Ordering::Release);
            assert_eq!(budget.charge(1), Err(MatchAbort::Cancelled));
            assert_eq!(budget.usage().work_units, 64);
            cancelled.store(false, Ordering::Release);
            assert_eq!(budget.charge(0), Err(MatchAbort::Cancelled));
        }

        #[test]
        fn reservation_overflow_and_observed_excess_are_terminal() {
            let mut budget = MatchBudget::new(limits(), None, None);
            let mut values: Vec<u64> = Vec::new();
            assert_eq!(budget.reserve(&mut values, usize::MAX), Err(MatchAbort::AllocationBudget));
            assert_eq!(values.capacity(), 0);
            let mut failure = MatchBudget::new(MatchLimits { allocation_bytes: usize::MAX, ..limits() }, None, None);
            let mut bytes: Vec<u8> = Vec::new();
            assert_eq!(failure.reserve(&mut bytes, usize::MAX), Err(MatchAbort::AllocationFailure));
            assert_eq!(bytes.capacity(), 0);
            assert_eq!(failure.usage().charged_allocation_bytes, usize::MAX);
            assert_eq!(failure.usage().requested_bytes, Some(0));
            assert_eq!(failure.usage().observed_bytes, Some(0));
            let mut budget = MatchBudget::new(MatchLimits { allocation_bytes: 8, ..limits() }, None, None);
            budget.allocate(8).unwrap();
            // Exercise the allocator-observation admission boundary deterministically.
            assert_eq!(budget.observe(8, 16), Err(MatchAbort::AllocationBudget));
            assert_eq!(budget.usage().observed_bytes, Some(16));
            assert_eq!(budget.usage().last_reservation.unwrap().excess_bytes, 8);
            assert_eq!(budget.usage().charged_allocation_bytes, 8);
            budget.usage.observed_bytes = Some(usize::MAX);
            let _ = budget.observe(0, 1);
            assert_eq!(budget.usage().observed_bytes, None);
        }
    }

    #[derive(Debug)]
    pub(crate) struct Checked<'budget, 'cancel>(pub(crate) &'budget mut MatchBudget<'cancel>);
    impl<Input: crate::indexing::ControlledInput> ExecutionPolicy<Input> for Checked<'_, '_> {
        type Error = MatchAbort;
        fn charge(&mut self, units: usize) -> Result<(), MatchAbort> { self.0.charge(units) }
        fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), MatchAbort> { self.0.reserve(values, additional) }
        fn enter_lookaround(&mut self) -> Result<(), MatchAbort> { self.0.enter() }
        fn leave_lookaround(&mut self) { self.0.leave(); }
        fn backref<Dir: Direction>(&mut self, input: &Input, dir: Dir,
            range: Range<Input::Position>, pos: &mut Input::Position, icase: bool,
        ) -> Result<bool, MatchAbort> {
            if icase { crate::matchers::backref_icase_controlled(input, dir, range, pos, self.0) }
            else { input.subrange_eq_controlled(dir, pos, range, self.0) }
        }
    }
}
#[cfg(all(feature = "std", feature = "utf16"))]
pub use checked::{MatchAbort, MatchBudget, MatchLimits, MatchUsage, ReservationObservation};
#[cfg(all(feature = "std", feature = "utf16"))]
pub(crate) use checked::Checked;
