//! Private terminal controls for the isolated Lua 5.4 fork.
use std::os::raw::{c_int, c_void};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Instant;
use crate::{Error, Lua, Result};

/// Sticky VM retirement reason. A retired state cannot be armed again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum TerminalReason { Deadline = 1, Cancelled = 2, Disposed = 3 }

pub(super) struct TerminalBudget {
    deadline: Instant,
    cancellation: Arc<AtomicBool>,
}

// No Lua calls, locks, allocation, user closure, or unwinding. C checks the
// returned reason only after this callback has returned through its Rust frame.
unsafe extern "C" fn poll(context: *mut c_void) -> c_int {
    let budget = unsafe { &*context.cast::<TerminalBudget>() };
    if budget.cancellation.load(Ordering::Relaxed) { TerminalReason::Cancelled as c_int }
    else if Instant::now() >= budget.deadline { TerminalReason::Deadline as c_int }
    else { 0 }
}

impl Lua {
    /// Permanently rejects binary bytecode, including Lua's native load reader.
    pub fn restrict_to_text_chunks(&self) {
        let lua = self.lock();
        unsafe { ffi::lua_risu_text_only(lua.main_state()); }
    }

    /// Enables trusted-native-only cleanup on final disposal, even while idle.
    pub fn enable_terminal_control(&self) {
        let lua = self.lock();
        unsafe { (*lua.extra.get()).terminal_managed = true; }
    }

    /// Arms an immutable, VM-owned budget shared by every coroutine.
    pub fn arm_terminal_budget(&self, deadline: Instant, cancellation: Arc<AtomicBool>) -> Result<()> {
        let lua = self.lock();
        unsafe {
            let extra = &mut *lua.extra.get();
            if extra.terminal_budget.is_some() || ffi::lua_risu_reason(lua.main_state()) != 0 {
                return Err(Error::runtime("terminal budget already armed or retired"));
            }
            let mut budget = Box::new(TerminalBudget { deadline, cancellation });
            if ffi::lua_risu_arm(lua.main_state(), poll, (&mut *budget as *mut TerminalBudget).cast()) == 0 {
                return Err(Error::runtime("cannot arm terminal budget"));
            }
            extra.terminal_managed = true;
            extra.terminal_budget = Some(budget);
            Ok(())
        }
    }

    /// Disarms a completed invocation without clearing terminal retirement.
    pub fn disarm_terminal_budget(&self) {
        let lua = self.lock();
        unsafe {
            ffi::lua_risu_disarm(lua.main_state());
            (*lua.extra.get()).terminal_budget = None;
        }
    }

    /// Retires the entire state. Native resources remain owned until disposal.
    pub fn retire_terminal(&self, reason: TerminalReason) {
        let lua = self.lock();
        unsafe {
            (*lua.extra.get()).terminal_managed = true;
            ffi::lua_risu_retire(lua.main_state(), reason as c_int);
        }
    }

    /// Returns the sticky terminal reason without entering Lua or polling.
    pub fn terminal_reason(&self) -> Option<TerminalReason> {
        let lua = self.lock();
        match unsafe { ffi::lua_risu_reason(lua.main_state()) } {
            0 => None,
            1 => Some(TerminalReason::Deadline),
            2 => Some(TerminalReason::Cancelled),
            _ => Some(TerminalReason::Disposed),
        }
    }
}
