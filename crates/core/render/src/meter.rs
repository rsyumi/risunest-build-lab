//! Budget accounting for one render: work is charged in batches, owned memory as reserved chunks held until the render
//! returns, and recursion is bounded so the Rust stack stays small.

use crate::error::{RenderError, Result};
use risunest_ir::{Budget, Charge};
use std::cell::{Cell, RefCell};

const WORK_BATCH: usize = 4096;
const MIN_CHUNK: usize = 16 * 1024;
/// Recursion depth across nested block tokenizes, link label scans, link content and image labels. A safety net no
/// input reaches: closed nestings stop near 100 because markdown-it's skipToken caches a skip to the end at
/// `maxNesting`, and the deepest oracle cases recurse 102 levels.
const MAX_DEPTH: usize = 256;

pub(crate) struct Meter<'b> {
    budget: &'b Budget,
    pending: Cell<usize>,
    used: Cell<usize>,
    reserved: Cell<usize>,
    charges: RefCell<Vec<Charge>>,
    depth: Cell<usize>,
}

impl<'b> Meter<'b> {
    pub(crate) fn new(budget: &'b Budget) -> Self {
        Self { budget, pending: Cell::new(0), used: Cell::new(0), reserved: Cell::new(0), charges: RefCell::new(Vec::new()), depth: Cell::new(0) }
    }

    pub(crate) fn work(&self, amount: usize) -> Result<()> {
        let pending = self.pending.get().saturating_add(amount);
        if pending >= WORK_BATCH {
            self.pending.set(0);
            self.budget.work(pending)?;
        } else {
            self.pending.set(pending);
        }
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<()> {
        let pending = self.pending.replace(0);
        if pending > 0 { self.budget.work(pending)?; }
        Ok(())
    }

    /// Charges `bytes` of memory the render keeps until it returns.
    pub(crate) fn alloc(&self, bytes: usize) -> Result<()> {
        let used = self.used.get().saturating_add(bytes);
        self.used.set(used);
        let reserved = self.reserved.get();
        if used > reserved {
            let chunk = (used - reserved).max(reserved).max(MIN_CHUNK);
            let charge = self.budget.reserve_owned(chunk)?;
            self.charges.borrow_mut().push(charge);
            self.reserved.set(reserved.saturating_add(chunk));
        }
        Ok(())
    }

    /// Refuses a string longer than the budget allows, as JS refuses one past its maximum string length.
    pub(crate) fn text(&self, units: usize) -> Result<()> {
        if units > self.budget.limits().max_text_units {
            return Err(RenderError::Budget(risunest_ir::Error::Limit("text-units")));
        }
        Ok(())
    }

    pub(crate) fn enter(&self) -> Result<Depth<'_, 'b>> {
        let depth = self.depth.get() + 1;
        if depth > MAX_DEPTH { return Err(RenderError::Unsupported("markdown-depth")); }
        self.depth.set(depth);
        Ok(Depth(self))
    }
}

pub(crate) struct Depth<'m, 'b>(&'m Meter<'b>);
impl Drop for Depth<'_, '_> {
    fn drop(&mut self) { self.0.depth.set(self.0.depth.get() - 1); }
}
