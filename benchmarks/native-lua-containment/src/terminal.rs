use mlua::{Lua, LuaOptions, StdLib, TerminalReason, UserData, UserDataMethods, MetaMethod, Function, Thread, WeakLua, ffi};
use std::alloc::{alloc, dealloc, realloc, Layout};
use std::cell::RefCell;
use std::ffi::{c_int, c_void};
use std::rc::Rc;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

pub struct CountDrop(pub Arc<AtomicUsize>);
impl UserData for CountDrop {}
impl Drop for CountDrop {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
}

fn lua() -> Lua {
    let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::COROUTINE, LuaOptions::default()).unwrap();
    lua.enable_terminal_control();
    lua.set_memory_limit(16 * 1024 * 1024).unwrap();
    lua
}

fn arm(lua: &Lua) {
    lua.arm_terminal_budget(Instant::now() + Duration::from_millis(40), Arc::new(AtomicBool::new(false))).unwrap();
}

fn reentrant() {
    let lua = lua();
    let drops = Arc::new(AtomicUsize::new(0));
    let borrow = Rc::new(RefCell::new(0));
    let count = drops.clone();
    let borrow_callback = borrow.clone();
    lua.globals().set("inner", lua.create_function(move |lua, ()| {
        let _guard = CountDrop(count.clone());
        let mut value = borrow_callback.borrow_mut();
        *value += 1;
        lua.load("while true do pcall(function() while true do end end) end").exec()
    }).unwrap()).unwrap();
    let count = drops.clone();
    lua.globals().set("outer", lua.create_function(move |lua, ()| {
        let _guard = CountDrop(count.clone());
        lua.load("return inner()").exec()
    }).unwrap()).unwrap();
    arm(&lua);
    assert!(lua.load("return outer()").exec().is_err());
    assert_eq!(lua.terminal_reason(), Some(TerminalReason::Deadline));
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert_eq!(*borrow.borrow_mut(), 1);
    assert!(lua.load("return 42").exec().is_err());
    drop(lua);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

struct NativeDrop {
    lua: WeakLua,
    drops: Arc<AtomicUsize>,
    rejected: Arc<AtomicUsize>,
}
impl UserData for NativeDrop {}
impl Drop for NativeDrop {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        if let Some(lua) = self.lua.try_upgrade()
            && lua.load("effect(); while true do end").exec().is_err() {
            self.rejected.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn native_drop() {
    for mode in ["incremental", "generational"] {
        let lua = lua();
        let drops = Arc::new(AtomicUsize::new(0));
        let rejected = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let effect = effects.clone();
        lua.globals().set("effect", lua.create_function(move |_, ()| { effect.fetch_add(1, Ordering::SeqCst); Ok(()) }).unwrap()).unwrap();
        lua.load(format!("collectgarbage('{mode}'); collectgarbage('stop')")).exec().unwrap();
        for i in 1..=32 {
            lua.globals().set(format!("native{i}"), lua.create_userdata(NativeDrop {
                lua: lua.weak(), drops: drops.clone(), rejected: rejected.clone(),
            }).unwrap()).unwrap();
        }
        lua.load("doomed=setmetatable({}, {__gc=function() while true do end end})").exec().unwrap();
        arm(&lua);
        assert!(lua.load("for i=1,32 do _G['native'..i]=nil end; doomed=nil; collectgarbage('collect')").exec().is_err());
        assert_eq!(lua.terminal_reason(), Some(TerminalReason::Deadline));
        assert_eq!(drops.load(Ordering::SeqCst), 32);
        assert_eq!(rejected.load(Ordering::SeqCst), 32);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        drop(lua);
        assert_eq!(drops.load(Ordering::SeqCst), 32);
    }
}

struct AllocationState {
    bytes: AtomicUsize,
    allocations: AtomicUsize,
    fail_at: AtomicUsize,
    fail_growth: AtomicBool,
    forbidden_effects: AtomicUsize,
    retire_in_destructor: bool,
    move_growth: bool,
    moved_bytes: AtomicUsize,
    hook_calls: AtomicUsize,
    hook_failed: AtomicBool,
    deadline: Instant,
}

unsafe extern "C" fn allocator(context: *mut c_void, pointer: *mut c_void, old: usize, size: usize) -> *mut c_void {
    let state = unsafe { &*context.cast::<AllocationState>() };
    if size == 0 {
        if !pointer.is_null() {
            let layout = Layout::from_size_align(old, 16).unwrap();
            unsafe { dealloc(pointer.cast(), layout); }
            state.bytes.fetch_sub(old, Ordering::SeqCst);
        }
        return std::ptr::null_mut();
    }
    let allocation = state.allocations.fetch_add(1, Ordering::SeqCst) + 1;
    let fail_at = state.fail_at.load(Ordering::SeqCst);
    if (state.fail_growth.load(Ordering::SeqCst) || (fail_at != 0 && allocation >= fail_at))
        && (pointer.is_null() || size > old) { return std::ptr::null_mut(); }
    let Ok(new_layout) = Layout::from_size_align(size, 16) else { return std::ptr::null_mut(); };
    let result = unsafe {
        if pointer.is_null() { alloc(new_layout) }
        else {
            let Ok(old_layout) = Layout::from_size_align(old, 16) else { return std::ptr::null_mut(); };
            if state.move_growth && size > old {
                let result = alloc(new_layout);
                if !result.is_null() {
                    std::ptr::copy_nonoverlapping(pointer.cast::<u8>(), result, old);
                    state.moved_bytes.fetch_add(old, Ordering::SeqCst);
                    dealloc(pointer.cast(), old_layout);
                }
                result
            } else { realloc(pointer.cast(), old_layout, size) }
        }
    };
    if !result.is_null() {
        state.bytes.fetch_add(size, Ordering::SeqCst);
        if !pointer.is_null() { state.bytes.fetch_sub(old, Ordering::SeqCst); }
    }
    result.cast()
}

unsafe extern "C" fn poll(context: *mut c_void) -> c_int {
    let state = unsafe { &*context.cast::<AllocationState>() };
    if Instant::now() >= state.deadline { 1 } else { 0 }
}

unsafe extern "C-unwind" fn fail_growth(state: *mut ffi::lua_State) -> c_int {
    let mut context = std::ptr::null_mut();
    unsafe { ffi::lua_getallocf(state, &mut context); }
    unsafe { &*context.cast::<AllocationState>() }.fail_growth.store(true, Ordering::SeqCst);
    0
}

unsafe extern "C-unwind" fn forbidden_finalizer(state: *mut ffi::lua_State) -> c_int {
    let mut context = std::ptr::null_mut();
    unsafe { ffi::lua_getallocf(state, &mut context); }
    unsafe { &*context.cast::<AllocationState>() }.forbidden_effects.fetch_add(1, Ordering::SeqCst);
    0
}

fn allocator_fault() {
    for _ in 0..8 {
        let mut accounting = Box::new(AllocationState { bytes: AtomicUsize::new(0), allocations: AtomicUsize::new(0), fail_at: AtomicUsize::new(0), fail_growth: AtomicBool::new(false), forbidden_effects: AtomicUsize::new(0), retire_in_destructor: false, move_growth: false, moved_bytes: AtomicUsize::new(0), hook_calls: AtomicUsize::new(0), hook_failed: AtomicBool::new(false), deadline: Instant::now() + Duration::from_millis(40) });
        let context = (&mut *accounting as *mut AllocationState).cast();
        unsafe {
            let state = ffi::lua_newstate(allocator, context);
            assert!(!state.is_null());
            ffi::luaL_openlibs(state);
            ffi::lua_newuserdatauv(state, 1, 0);
            ffi::lua_newtable(state);
            ffi::lua_pushcfunction(state, forbidden_finalizer);
            ffi::lua_setfield(state, -2, c"__gc".as_ptr());
            ffi::lua_setmetatable(state, -2);
            ffi::lua_setglobal(state, c"untrusted_native".as_ptr());
            ffi::lua_pushcfunction(state, fail_growth);
            ffi::lua_setglobal(state, c"fail_growth".as_ptr());
            let source = b"local n=0; return ('aa'):gsub('.',function() n=n+1; if n==1 then return string.rep('b',16384) end; fail_growth(); while true do end end)";
            assert_eq!(ffi::luaL_loadbufferx(state, source.as_ptr().cast(), source.len(), c"allocator-probe".as_ptr(), c"t".as_ptr()), ffi::LUA_OK);
            accounting.deadline = Instant::now() + Duration::from_millis(40);
            assert_eq!(ffi::lua_risu_arm(state, poll, context), 1);
            assert_eq!(ffi::lua_pcall(state, 0, 0, 0), ffi::LUA_ERRTERMINAL);
            assert_eq!(ffi::lua_risu_reason(state), 1);
            assert!(accounting.fail_growth.load(Ordering::SeqCst));
            ffi::lua_close(state);
        }
        assert_eq!(accounting.bytes.load(Ordering::SeqCst), 0);
        assert_eq!(accounting.forbidden_effects.load(Ordering::SeqCst), 0);
    }
}

fn churn() {
    let drops = Arc::new(AtomicUsize::new(0));
    for i in 0..64 {
        let lua = lua();
        let resource = CountDrop(drops.clone());
        lua.globals().set("resource", lua.create_function(move |_, ()| { let _ = &resource; Ok(()) }).unwrap()).unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        lua.arm_terminal_budget(Instant::now() + Duration::from_secs(1), cancel).unwrap();
        assert!(lua.load("return resource()").exec().is_err());
        assert_eq!(lua.terminal_reason(), Some(TerminalReason::Cancelled));
        lua.retire_terminal(TerminalReason::Disposed);
        assert_eq!(lua.terminal_reason(), Some(TerminalReason::Cancelled));
        assert!(lua.arm_terminal_budget(Instant::now(), Arc::new(AtomicBool::new(false))).is_err());
        drop(lua);
        assert_eq!(drops.load(Ordering::SeqCst), i + 1);
    }
}


unsafe extern "C" fn expired(_: *mut c_void) -> c_int { 1 }

unsafe extern "C-unwind" fn native_counter(state: *mut ffi::lua_State) -> c_int {
    let mut context = std::ptr::null_mut();
    unsafe {
        ffi::lua_getallocf(state, &mut context);
        std::ptr::drop_in_place(ffi::lua_touserdata(state, 1).cast::<CountDrop>());
        if (&*context.cast::<AllocationState>()).retire_in_destructor {
            ffi::lua_risu_arm(state, expired, context);
        }
    }
    0
}

fn gc_native_transition(after: bool) {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut accounting = Box::new(AllocationState {
        bytes: AtomicUsize::new(0), allocations: AtomicUsize::new(0), fail_at: AtomicUsize::new(0), fail_growth: AtomicBool::new(false),
        forbidden_effects: AtomicUsize::new(0), retire_in_destructor: after, move_growth: false, moved_bytes: AtomicUsize::new(0), hook_calls: AtomicUsize::new(0), hook_failed: AtomicBool::new(false),
        deadline: Instant::now(),
    });
    let context = (&mut *accounting as *mut AllocationState).cast();
    unsafe {
        let state = ffi::lua_newstate(allocator, context);
        assert!(!state.is_null());
        let payload = ffi::lua_newuserdatauv(state, std::mem::size_of::<CountDrop>(), 0).cast::<CountDrop>();
        payload.write(CountDrop(drops.clone()));
        ffi::lua_newtable(state);
        ffi::lua_pushcfunction(state, native_counter);
        ffi::lua_setfield(state, -2, c"__gc".as_ptr());
        ffi::lua_risu_destructor(state, -1, native_counter, 0);
        ffi::lua_setmetatable(state, -2);
        ffi::lua_pop(state, 1);
        if !after { assert_eq!(ffi::lua_risu_arm(state, expired, context), 1); }
        // The GC API itself must return, including when a protected finalizer
        // latches a terminal error before or after the native destructor.
        ffi::lua_gc(state, ffi::LUA_GCCOLLECT, 0);
        assert_eq!(ffi::lua_risu_reason(state), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        ffi::lua_close(state);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(accounting.bytes.load(Ordering::SeqCst), 0);
}

struct ClosedNative(CountDrop);
impl UserData for ClosedNative {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Close, |_, value, _: mlua::Value| { let _ = &value.0; Ok(()) });
    }
}
struct ResetDrop {
    child: Thread,
    replacement: Function,
    raw_child: *mut ffi::lua_State,
    rejected: Arc<AtomicUsize>,
    raw_rejected: Arc<AtomicUsize>,
    child_drops: Arc<AtomicUsize>,
    drops_during_reset: Arc<AtomicUsize>,
}
impl UserData for ResetDrop {}
impl Drop for ResetDrop {
    fn drop(&mut self) {
        if self.child.reset(self.replacement.clone()).is_err() {
            self.rejected.fetch_add(1, Ordering::SeqCst);
        }
        // Exercise the C guard too, without going through the safe mlua guard.
        unsafe {
            let state = self.raw_child;
            if ffi::lua_closethread(state, std::ptr::null_mut()) == ffi::LUA_ERRTERMINAL {
                self.raw_rejected.fetch_add(1, Ordering::SeqCst);
                ffi::lua_pop(state, 1);
            }
        }
        self.drops_during_reset.store(self.child_drops.load(Ordering::SeqCst), Ordering::SeqCst);
    }
}

fn drop_reset() {
    let lua = lua();
    let drops = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let raw_rejected = Arc::new(AtomicUsize::new(0));
    let drops_during_reset = Arc::new(AtomicUsize::new(usize::MAX));
    lua.load("collectgarbage('stop')").exec().unwrap();
    let child = lua.create_thread(lua.load("local value <close> = ...; coroutine.yield(); return true").into_function().unwrap()).unwrap();
    let payload = lua.create_userdata(ClosedNative(CountDrop(drops.clone()))).unwrap();
    child.resume::<()>(payload).unwrap();
    let replacement = lua.load("return true").into_function().unwrap();
    let raw_child: mlua::LightUserData = unsafe { lua.exec_raw(child.clone(), |state| {
        let child = ffi::lua_tothread(state, 1);
        ffi::lua_settop(state, 0);
        ffi::lua_pushlightuserdata(state, child.cast());
    }).unwrap() };
    lua.globals().set("resetter", lua.create_userdata(ResetDrop { child, replacement, raw_child: raw_child.0.cast(), rejected: rejected.clone(), raw_rejected: raw_rejected.clone(), child_drops: drops.clone(), drops_during_reset: drops_during_reset.clone() }).unwrap()).unwrap();
    lua.load("doomed=setmetatable({}, {__gc=function() while true do end end})").exec().unwrap();
    arm(&lua);
    assert!(lua.load("resetter=nil; doomed=nil; collectgarbage('collect')").exec().is_err());
    assert_eq!(rejected.load(Ordering::SeqCst), 1);
    assert_eq!(raw_rejected.load(Ordering::SeqCst), 1);
    assert_eq!(drops_during_reset.load(Ordering::SeqCst), 0, "child destructor ran during rejected reset");
    drop(lua);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn binary_reader() {
    // This is a raw engine probe. Runtime's text-only admission is tested
    // separately and must not turn this native parser loop into a false pass.
    let lua = lua();
    arm(&lua);
    assert!(lua.load(r#"local head=string.dump(function() end):sub(1,32); return load(function() local h=head; head=nil; return h or '\0' end)"#).exec().is_err());
    assert_eq!(lua.terminal_reason(), Some(TerminalReason::Deadline));
    drop(lua);
}

fn allocator_init() {
    let mut count = 0;
    for fault in 0..1024 {
        if fault > 0 && fault > count { break; }
        let mut accounting = Box::new(AllocationState {
            bytes: AtomicUsize::new(0), allocations: AtomicUsize::new(0), fail_at: AtomicUsize::new(fault),
            fail_growth: AtomicBool::new(false), forbidden_effects: AtomicUsize::new(0),
            retire_in_destructor: false, move_growth: false, moved_bytes: AtomicUsize::new(0), hook_calls: AtomicUsize::new(0), hook_failed: AtomicBool::new(false), deadline: Instant::now(),
        });
        unsafe {
            let state = ffi::lua_newstate(allocator, (&mut *accounting as *mut AllocationState).cast());
            if fault == 0 {
                assert!(!state.is_null());
                count = accounting.allocations.load(Ordering::SeqCst);
                assert!(count < 1024);
                ffi::lua_close(state);
            } else {
                assert!(state.is_null(), "initialization admitted after allocator refusal at {fault}");
            }
        }
        assert_eq!(accounting.bytes.load(Ordering::SeqCst), 0, "allocation balance after initialization fault {fault}");
    }
    assert!(count > 0);
}


unsafe extern "C-unwind" fn relocate_hook(state: *mut ffi::lua_State, _: *mut ffi::lua_Debug) {
    let mut context = std::ptr::null_mut();
    unsafe {
        ffi::lua_getallocf(state, &mut context);
        let accounting = &*context.cast::<AllocationState>();
        let call = accounting.hook_calls.fetch_add(1, Ordering::SeqCst);
        let required = 1024 << call.min(8);
        if ffi::lua_checkstack(state, required as c_int) == 0 {
            accounting.hook_failed.store(true, Ordering::SeqCst);
        }
    }
}

fn hook_relocation() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut accounting = Box::new(AllocationState {
        bytes: AtomicUsize::new(0), allocations: AtomicUsize::new(0), fail_at: AtomicUsize::new(0),
        fail_growth: AtomicBool::new(false), forbidden_effects: AtomicUsize::new(0),
        retire_in_destructor: false, move_growth: true, moved_bytes: AtomicUsize::new(0), hook_calls: AtomicUsize::new(0),
        hook_failed: AtomicBool::new(false), deadline: Instant::now(),
    });
    unsafe {
        let state = ffi::lua_newstate(allocator, (&mut *accounting as *mut AllocationState).cast());
        assert!(!state.is_null());
        let payload = ffi::lua_newuserdatauv(state, std::mem::size_of::<CountDrop>(), 0).cast::<CountDrop>();
        payload.write(CountDrop(drops.clone()));
        ffi::lua_newtable(state);
        ffi::lua_pushcfunction(state, native_counter);
        ffi::lua_setfield(state, -2, c"__gc".as_ptr());
        ffi::lua_risu_destructor(state, -1, native_counter, 0);
        ffi::lua_setmetatable(state, -2);
        ffi::lua_setglobal(state, c"resource".as_ptr());
        ffi::lua_pushcfunction(state, native_counter);
        ffi::lua_setglobal(state, c"drop_resource".as_ptr());
        let source = b"drop_resource(resource)";
        assert_eq!(ffi::luaL_loadbufferx(state, source.as_ptr().cast(), source.len(), c"hook-probe".as_ptr(), c"t".as_ptr()), ffi::LUA_OK);
        ffi::lua_sethook(state, Some(relocate_hook), ffi::LUA_MASKCALL, 0);
        assert_eq!(ffi::lua_pcall(state, 0, 0, 0), ffi::LUA_OK);
        assert!(accounting.hook_calls.load(Ordering::SeqCst) >= 2);
        assert!(accounting.moved_bytes.load(Ordering::SeqCst) >= 2048);
        assert!(!accounting.hook_failed.load(Ordering::SeqCst));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        ffi::lua_close(state);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(accounting.bytes.load(Ordering::SeqCst), 0);
}

fn destroy_allocation() {
    let lua = lua();
    let drops = Arc::new(AtomicUsize::new(0));
    lua.gc_stop();
    let value = lua.create_userdata(CountDrop(drops.clone())).unwrap();
    // Reserve nested C call frames without filling the Rust callback error pool.
    lua.load("return string.len('x')").exec().unwrap();
    lua.set_memory_limit(1).unwrap();
    assert!(matches!(value.destroy(), Err(mlua::Error::MemoryError(_))));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    lua.set_memory_limit(0).unwrap();
    drop(value);
    lua.gc_collect().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(lua);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

pub fn probe(name: &str) {
    match name {
        "drop-reentrant" => reentrant(),
        "drop-native" => native_drop(),
        "allocator-fault" => allocator_fault(),
        "allocator-init" => allocator_init(),
        "hook-relocation" => hook_relocation(),
        "destroy-allocation" => destroy_allocation(),
        "retire-churn" => churn(),
        "gc-native-before" => gc_native_transition(false),
        "gc-native-after" => gc_native_transition(true),
        "drop-reset" => drop_reset(),
        "load-binary-reader" => binary_reader(),
        _ => unreachable!(),
    }
}
