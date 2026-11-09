//! The main-thread Wasmoon engine profile: its injected globals, the JS string crossing and the edit listener entry.
use mlua::{ffi, Function, Lua, MultiValue, TerminalReason, Value};
use std::{cell::RefCell, ffi::c_int, rc::Rc};

const PRELUDE: &str = include_str!("wasmoon_prelude.lua");
const LABEL_BYTES: usize = 96;

// luaL_newmetatable gives the table its `__name`, as Wasmoon's type extensions do.
unsafe extern "C-unwind" fn make_metatable(state: *mut ffi::lua_State) -> c_int {
    let name = ffi::luaL_checklstring(state, 1, std::ptr::null_mut());
    ffi::luaL_newmetatable(state, name);
    ffi::lua_pushstring(state, c"protected metatable".as_ptr());
    ffi::lua_setfield(state, -2, c"__metatable".as_ptr());
    1
}

unsafe extern "C-unwind" fn new_userdata(state: *mut ffi::lua_State) -> c_int {
    let name = ffi::luaL_checklstring(state, 1, std::ptr::null_mut());
    ffi::lua_newuserdatauv(state, 0, 0);
    ffi::luaL_setmetatable(state, name);
    1
}

// `new_array(items, n)`: lua_createtable(n, 0) and t[i] = items[i], as Wasmoon pushes a JS array.
unsafe extern "C-unwind" fn new_array(state: *mut ffi::lua_State) -> c_int {
    let count = ffi::luaL_checkinteger(state, 2).clamp(0, c_int::MAX as ffi::lua_Integer);
    ffi::lua_createtable(state, count as c_int, 0);
    for index in 1..=count {
        ffi::lua_geti(state, 1, index);
        ffi::lua_seti(state, -2, index);
    }
    1
}

/// The bytes Wasmoon's `lua_pushstring` receives for a JS string: Emscripten 3.1 `stringToUTF8Array`, which pairs
/// any surrogate unit with the unit after it (0 past the end), then the C string ends at the first NUL byte.
pub fn js_string_bytes(units: &[u16]) -> Vec<u8> {
    let mut output = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let mut point = units[index] as u32;
        if (0xd800..=0xdfff).contains(&point) {
            index += 1;
            let next = units.get(index).copied().unwrap_or(0) as u32;
            point = 0x10000 + ((point & 0x3ff) << 10) + (next & 0x3ff);
        }
        index += 1;
        if point == 0 { break; }
        if point < 0x80 { output.push(point as u8); }
        else if point < 0x800 { output.extend([0xc0 | (point >> 6) as u8, 0x80 | (point & 63) as u8]); }
        else if point < 0x10000 {
            output.extend([0xe0 | (point >> 12) as u8, 0x80 | (point >> 6 & 63) as u8, 0x80 | (point & 63) as u8]);
        } else {
            output.extend([0xf0 | (point >> 18) as u8, 0x80 | (point >> 12 & 63) as u8, 0x80 | (point >> 6 & 63) as u8,
                0x80 | (point & 63) as u8]);
        }
    }
    output
}

/// The JS string Wasmoon reads from a Lua string's bytes, or `None` where Wasmoon would read past them.
pub fn js_string_units(bytes: &[u8]) -> Option<Vec<u16>> { super::value_input::wasmoon_text(bytes).ok() }

/// A host argument as the gate of an unsupported host function sees it.
#[derive(Debug, PartialEq)]
pub enum GateArgument { Nil, Boolean(bool), Number(f64), String(Vec<u8>), Other }

pub(super) struct Profile {
    pub listen: Function,
    pub fulfilled_nil: Function,
}

/// Records the first decline and retires the VM, so a protected call cannot continue past it.
pub(super) fn decline_error(lua: &Lua, slot: &Rc<RefCell<Option<String>>>, label: &[u8]) -> mlua::Error {
    let label = String::from_utf8_lossy(&label[..label.len().min(LABEL_BYTES)]).into_owned();
    let mut current = slot.borrow_mut();
    if current.is_none() { *current = Some(label.clone()); }
    drop(current);
    lua.retire_terminal(TerminalReason::Cancelled);
    mlua::Error::runtime(format!("unsupported: {label}"))
}

/// An allocation failure under the VM memory limit, as a Rust callback or a resume reports it.
pub(super) fn is_memory_error(error: &mlua::Error) -> bool {
    match error {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } | mlua::Error::WithContext { cause, .. } | mlua::Error::BadArgument { cause, .. } =>
            is_memory_error(cause),
        _ => false,
    }
}

pub(super) fn install(lua: &Lua, slot: &Rc<RefCell<Option<String>>>, entry: &Rc<std::cell::Cell<usize>>,
    modules: &[(&str, &str)]) -> mlua::Result<Profile> {
    let decline_slot = slot.clone();
    let decline = lua.create_function(move |lua, label: mlua::LuaString| -> mlua::Result<()> {
        Err(decline_error(lua, &decline_slot, &label.as_bytes()))
    })?;
    let entry = entry.clone();
    let is_entry = lua.create_function(move |lua, ()| Ok(lua.current_thread().to_pointer() as usize == entry.get()))?;
    let (make, create, array) = unsafe {
        (lua.create_c_function(make_metatable)?, lua.create_c_function(new_userdata)?, lua.create_c_function(new_array)?)
    };
    // A Lua string after a trip through a JS string.
    let text_slot = slot.clone();
    let js_text = lua.create_function(move |lua, text: mlua::LuaString| match super::value_input::wasmoon_text(&text.as_bytes()) {
        Ok(units) => lua.create_string(js_string_bytes(&units)),
        Err(_) => Err(decline_error(lua, &text_slot, b"lua-promise-text")),
    })?;
    // Lua raises an allocation failure as its preallocated message, a Rust callback as an error value.
    let memory = lua.create_function(|_, value: Value| Ok(match &value {
        Value::String(text) => &*text.as_bytes() == b"not enough memory".as_slice(),
        Value::Error(error) => is_memory_error(error),
        _ => false,
    }))?;
    let loaders = lua.create_table()?;
    for (name, source) in modules { loaders.raw_set(*name, lua.load(*source).set_name(format!("@{name}.lua")).into_function()?)?; }
    let (listen, fulfilled_nil) = lua.load(PRELUDE).set_name("wasmoon")
        .call::<(Function, Function)>((make, create, array, js_text, decline, is_entry, memory, loaders))?;
    Ok(Profile { listen, fulfilled_nil })
}

pub(super) fn gate_arguments(arguments: &MultiValue) -> Vec<GateArgument> {
    arguments.iter().map(|value| match value {
        Value::Nil => GateArgument::Nil,
        Value::Boolean(value) => GateArgument::Boolean(*value),
        Value::Integer(value) => GateArgument::Number(*value as f64),
        Value::Number(value) => GateArgument::Number(*value),
        Value::String(text) => GateArgument::String(text.as_bytes().to_vec()),
        _ => GateArgument::Other,
    }).collect()
}
