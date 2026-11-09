use std::mem::size_of;
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_owned_bytes: usize,
    pub max_json_bytes: usize,
    pub max_text_units: usize,
    pub max_values: usize,
    pub max_depth: usize,
    pub max_work: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Syntax { byte: usize },
    Type { operation: &'static str },
    Limit(&'static str),
    Cancelled,
    Allocation,
    IsolatedUtf16,
    DecoderFinished,
    Invariant(&'static str),
}

#[derive(Debug)]
pub(crate) struct BudgetState {
    pub(crate) limits: Limits,
    cancel: Arc<AtomicBool>,
    owned: AtomicUsize,
    peak: AtomicUsize,
    values: AtomicUsize,
    work: AtomicUsize,
}

/// `work` and `json-values` charges assume one charger at a time: a parse or stringify leases blocks of them ahead of
/// use, so a concurrent one could hit a limit early. Tasks charged in parallel each need their own `Budget`.
#[derive(Debug, Clone)]
pub struct Budget(pub(crate) Arc<BudgetState>);

impl Budget {
    pub fn new(limits: Limits, cancel: Arc<AtomicBool>) -> Result<Self, Error> {
        if limits.max_owned_bytes < Self::context_bytes() || limits.max_json_bytes == 0 || limits.max_text_units == 0
            || limits.max_values == 0 || limits.max_work == 0 || limits.max_depth > 128 {
            return Err(Error::Limit("invalid-limits"));
        }
        Ok(Self(Arc::new(BudgetState { limits, cancel, owned: AtomicUsize::new(Self::context_bytes()), peak: AtomicUsize::new(Self::context_bytes()),
            values: AtomicUsize::new(0), work: AtomicUsize::new(0) })))
    }

    fn add(counter: &AtomicUsize, amount: usize, limit: usize, kind: &'static str) -> Result<usize, Error> {
        counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current|
            current.checked_add(amount).filter(|next| *next <= limit))
            .map(|previous| previous + amount).map_err(|_| Error::Limit(kind))
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.0.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }

    pub fn work(&self, amount: usize) -> Result<(), Error> {
        self.check()?;
        Self::add(&self.0.work, amount, self.0.limits.max_work, "work")?;
        Ok(())
    }

    // The reference parser's per-value charge, which `Meter::value` makes in blocks.
    #[cfg(test)]
    pub(crate) fn value(&self) -> Result<(), Error> {
        self.check()?;
        Self::add(&self.0.values, 1, self.0.limits.max_values, "json-values")?;
        Ok(())
    }

    fn charge(&self, bytes: usize) -> Result<Charge, Error> {
        self.check()?;
        // `owned` never exceeds its limit and `peak` never trails `owned`, so an empty charge changes neither.
        if bytes != 0 {
            let owned = Self::add(&self.0.owned, bytes, self.0.limits.max_owned_bytes, "owned-bytes")?;
            self.0.peak.fetch_max(owned, Ordering::Relaxed);
        }
        Ok(Charge { budget: self.clone(), bytes })
    }

    /// The charge a buffer growing from empty by `OwnedString`'s doubling ends with, made at once: its last growth held
    /// `previous` and `capacity` bytes together, the largest total any of its growths held, so that sum alone meets the
    /// limit and the peak. `None` where that growth would pass `max_owned_bytes`; nothing is charged then.
    pub(crate) fn grown(&self, previous: usize, capacity: usize) -> Result<Option<Charge>, Error> {
        self.check()?;
        let Some(held) = previous.checked_add(capacity) else { return Ok(None) };
        let limit = self.0.limits.max_owned_bytes;
        let Ok(before) = self.0.owned.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current|
            current.checked_add(held).filter(|next| *next <= limit).map(|_| current + capacity)) else { return Ok(None) };
        self.0.peak.fetch_max(before + held, Ordering::Relaxed);
        Ok(Some(Charge { budget: self.clone(), bytes: capacity }))
    }

    // Adds `need` or more, up to `block`, to `counter` without passing `limit`; returns the amount added.
    fn lease(counter: &AtomicUsize, need: usize, limit: usize, block: usize) -> Option<usize> {
        let mut taken = 0;
        counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let available = limit.checked_sub(current)?;
            if need > available { return None; }
            taken = available.min(need.max(block));
            Some(current + taken)
        }).ok().map(|_| taken)
    }

    pub fn context_bytes() -> usize { size_of::<BudgetState>() + 2 * size_of::<usize>() }
    pub fn reserve_owned(&self, bytes: usize) -> Result<Charge, Error> { self.charge(bytes) }
    pub fn owned_bytes(&self) -> usize { self.0.owned.load(Ordering::Relaxed) }
    pub fn peak_bytes(&self) -> usize { self.0.peak.load(Ordering::Relaxed) }
}

#[derive(Debug)]
pub struct Charge { budget: Budget, bytes: usize }
impl Drop for Charge {
    fn drop(&mut self) { if self.bytes != 0 { self.budget.0.owned.fetch_sub(self.bytes, Ordering::Relaxed); } }
}

/// `work` and `json-values` charges drawn from a [`Budget`] in leased blocks, so a hot loop pays no atomic operation per
/// charge. Each charge succeeds or fails exactly as the same direct charge would while no other thread charges the
/// budget, and the unspent lease returns on [`Meter::release_work`] and on drop, leaving the counters as direct charges
/// would. A direct `work` charge made while a lease is held must follow `release_work`. Cancellation is seen when a
/// block is leased, at least every `WORK_BLOCK` units or `VALUE_BLOCK` values.
pub(crate) struct Meter<'a> { budget: &'a Budget, work: usize, values: usize }
const WORK_BLOCK: usize = 1 << 16;
const VALUE_BLOCK: usize = 1 << 10;
impl<'a> Meter<'a> {
    pub(crate) fn new(budget: &'a Budget) -> Self { Self { budget, work: 0, values: 0 } }
    #[inline]
    pub(crate) fn work(&mut self, amount: usize) -> Result<(), Error> {
        if amount > self.work && !self.lease_work(amount)? { return Err(Error::Limit("work")); }
        self.work -= amount;
        Ok(())
    }
    /// Whether `work(amount)` would succeed now, leasing what it needs without spending it.
    #[inline]
    pub(crate) fn covers(&mut self, amount: usize) -> Result<bool, Error> {
        if amount <= self.work { return Ok(true); }
        self.lease_work(amount)
    }
    #[cold]
    fn lease_work(&mut self, amount: usize) -> Result<bool, Error> {
        self.budget.check()?;
        let Some(taken) = Budget::lease(&self.budget.0.work, amount - self.work, self.budget.0.limits.max_work, WORK_BLOCK) else { return Ok(false) };
        self.work += taken;
        Ok(true)
    }
    #[inline]
    pub(crate) fn value(&mut self) -> Result<(), Error> {
        if self.values == 0 {
            self.budget.check()?;
            self.values = Budget::lease(&self.budget.0.values, 1, self.budget.0.limits.max_values, VALUE_BLOCK).ok_or(Error::Limit("json-values"))?;
        }
        self.values -= 1;
        Ok(())
    }
    pub(crate) fn release_work(&mut self) {
        if self.work != 0 { self.budget.0.work.fetch_sub(std::mem::take(&mut self.work), Ordering::Relaxed); }
    }
}
impl Drop for Meter<'_> {
    fn drop(&mut self) {
        self.release_work();
        if self.values != 0 { self.budget.0.values.fetch_sub(self.values, Ordering::Relaxed); }
    }
}

#[derive(Debug)]
pub struct OwnedVec<T> { pub(crate) data: Vec<T>, charge: Charge }
impl<T> OwnedVec<T> {
    pub fn new(budget: &Budget) -> Result<Self, Error> { Ok(Self { data: Vec::new(), charge: budget.charge(0)? }) }
    pub(crate) fn budget(&self) -> &Budget { &self.charge.budget }
    pub fn as_slice(&self) -> &[T] { &self.data }
    pub fn as_mut_slice(&mut self) -> &mut [T] { &mut self.data }
    fn ensure(&mut self, additional: usize) -> Result<(), Error> {
        let needed = self.data.len().checked_add(additional).ok_or(Error::Limit("owned-bytes"))?;
        if needed <= self.data.capacity() { return self.budget().check(); }
        let target = needed.checked_next_power_of_two().unwrap_or(needed);
        let requested = target.checked_mul(size_of::<T>()).ok_or(Error::Limit("owned-bytes"))?;
        let mut charge = self.budget().charge(requested)?;
        let mut next = Vec::new();
        next.try_reserve_exact(target).map_err(|_| Error::Allocation)?;
        let actual = next.capacity().checked_mul(size_of::<T>()).ok_or(Error::Limit("owned-bytes"))?;
        if actual > charge.bytes {
            let mut extra = self.budget().charge(actual - charge.bytes)?;
            charge.bytes += extra.bytes;
            extra.bytes = 0;
        }
        next.append(&mut self.data);
        let previous = std::mem::replace(&mut self.data, next);
        let old_charge = std::mem::replace(&mut self.charge, charge);
        drop(previous);
        drop(old_charge);
        Ok(())
    }
    pub fn push(&mut self, value: T) -> Result<(), Error> { self.ensure(1)?; self.data.push(value); Ok(()) }
    pub(crate) fn reserve(&mut self, additional: usize) -> Result<(), Error> { self.ensure(additional) }
}

/// The charges an [`OwnedVec`] of `size`-byte elements makes as it is pushed onto one element at a time, made while its
/// elements are kept elsewhere: [`Growth::push`] is `OwnedVec::push`'s accounting (the same limit check, peak and
/// release at the same point), and [`Growth::finish`] builds the vector with the capacity it would have.
pub(crate) struct Growth<'a> { budget: &'a Budget, size: usize, capacity: usize, charged: usize }
impl<'a> Growth<'a> {
    /// `OwnedVec::new`.
    pub(crate) fn new(budget: &'a Budget, size: usize) -> Result<Self, Error> {
        budget.check()?;
        Ok(Self { budget, size, capacity: 0, charged: 0 })
    }
    /// The accounting of a push onto a vector of `length` elements.
    pub(crate) fn push(&mut self, length: usize) -> Result<(), Error> {
        let needed = length.checked_add(1).ok_or(Error::Limit("owned-bytes"))?;
        if needed <= self.capacity { return self.budget.check(); }
        let target = needed.checked_next_power_of_two().unwrap_or(needed);
        let requested = target.checked_mul(self.size).ok_or(Error::Limit("owned-bytes"))?;
        self.budget.check()?;
        // The new buffer is charged while the old one is still held, then the old one is released.
        let (limit, charged) = (self.budget.0.limits.max_owned_bytes, self.charged);
        let before = self.budget.0.owned.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current|
            current.checked_add(requested).filter(|next| *next <= limit).map(|next| next - charged)).map_err(|_| Error::Limit("owned-bytes"))?;
        self.budget.0.peak.fetch_max(before + requested, Ordering::Relaxed);
        self.capacity = target;
        self.charged = requested;
        Ok(())
    }
    pub(crate) fn finish<T>(mut self, elements: impl Iterator<Item = T>) -> Result<OwnedVec<T>, Error> {
        debug_assert_eq!(size_of::<T>(), self.size);
        let mut data = Vec::new();
        data.try_reserve_exact(self.capacity).map_err(|_| Error::Allocation)?;
        data.extend(elements);
        debug_assert_eq!(data.capacity(), self.capacity);
        let bytes = std::mem::take(&mut self.charged);
        Ok(OwnedVec { data, charge: Charge { budget: self.budget.clone(), bytes } })
    }
}
impl Drop for Growth<'_> {
    fn drop(&mut self) { if self.charged != 0 { self.budget.0.owned.fetch_sub(self.charged, Ordering::Relaxed); } }
}

impl<T: Copy> OwnedVec<T> {
    pub fn extend(&mut self, values: &[T]) -> Result<(), Error> {
        self.budget().work(values.len())?;
        self.ensure(values.len())?;
        self.data.extend_from_slice(values);
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct OwnedString { pub(crate) data: String, charge: Charge }
impl OwnedString {
    pub(crate) fn new(budget: &Budget) -> Result<Self, Error> { Ok(Self { data: String::new(), charge: budget.charge(0)? }) }
    pub(crate) fn budget(&self) -> &Budget { &self.charge.budget }
    pub(crate) fn reserve(&mut self, additional: usize) -> Result<(), Error> { self.ensure(additional) }
    /// `data` with the charge for its capacity, as [`Budget::grown`] made it.
    pub(crate) fn charged(data: String, charge: Charge) -> Self {
        debug_assert_eq!(data.capacity(), charge.bytes);
        Self { data, charge }
    }
    pub(crate) fn from_utf8(value: OwnedVec<u8>) -> Result<Self, Error> {
        value.budget().work(value.data.len())?;
        let OwnedVec { data, charge } = value;
        let data = String::from_utf8(data).map_err(|_| Error::Invariant("utf8-decoder-output"))?;
        Ok(Self { data, charge })
    }
    fn ensure(&mut self, additional: usize) -> Result<(), Error> {
        let needed = self.data.len().checked_add(additional).ok_or(Error::Limit("owned-bytes"))?;
        if needed <= self.data.capacity() { return self.budget().check(); }
        self.budget().work(self.data.len())?;
        let target = needed.checked_next_power_of_two().unwrap_or(needed);
        let mut charge = self.budget().charge(target)?;
        let mut next = String::new();
        next.try_reserve_exact(target).map_err(|_| Error::Allocation)?;
        if next.capacity() > charge.bytes {
            let mut extra = self.budget().charge(next.capacity() - charge.bytes)?;
            charge.bytes += extra.bytes;
            extra.bytes = 0;
        }
        next.push_str(&self.data);
        let previous = std::mem::replace(&mut self.data, next);
        let old_charge = std::mem::replace(&mut self.charge, charge);
        drop(previous);
        drop(old_charge);
        Ok(())
    }
    pub(crate) fn extend(&mut self, value: &str) -> Result<(), Error> {
        self.budget().work(value.len())?;
        self.ensure(value.len())?;
        self.data.push_str(value);
        Ok(())
    }
}

/// The shared cancellation flag a [`Budget`] observes; storing `true` fails every later check and charge.
pub type Cancel = Arc<AtomicBool>;

impl Budget {
    pub fn limits(&self) -> &Limits { &self.0.limits }
    /// The `work` and `json-values` counters, which nothing outside tests reads.
    #[cfg(test)]
    pub(crate) fn counters(&self) -> (usize, usize) { (self.0.work.load(Ordering::Relaxed), self.0.values.load(Ordering::Relaxed)) }
}
