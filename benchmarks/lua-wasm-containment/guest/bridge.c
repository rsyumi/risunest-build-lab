#include <stdint.h>
#include <errno.h>
#include <emscripten/syscalls.h>
#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"

#define EXPORT __attribute__((visibility("default")))
#define IMPORT(name) __attribute__((import_module("risunest_probe"), import_name(name)))

IMPORT("get") extern int32_t host_get(void);
IMPORT("commit") extern void host_commit(int32_t value);
IMPORT("acquire") extern int32_t host_acquire(void);
IMPORT("release") extern int32_t host_release(int32_t handle);
IMPORT("reenter") extern int32_t host_reenter(int32_t value);
IMPORT("signal") extern void host_signal(void);
IMPORT("compiling") extern void host_compiling(void);

static lua_State *root;
static lua_State *entry;
static int entry_ref = LUA_NOREF;
static int started;
static int pending;
static int sequence;
static int result;
static unsigned char input[1024 * 1024];

/* Retained libc stdio code must not import descriptor duplication. */
int __syscall_dup3(int oldfd, int newfd, int flags) {
    (void)oldfd;
    (void)newfd;
    (void)flags;
    return -ENOSYS;
}

static int get(lua_State *L) {
    lua_pushinteger(L, host_get());
    return 1;
}

static int commit(lua_State *L) {
    host_commit((int32_t)luaL_checkinteger(L, 1));
    return 0;
}

static int acquire(lua_State *L) {
    lua_pushinteger(L, host_acquire());
    return 1;
}

static int release(lua_State *L) {
    lua_pushboolean(L, host_release((int32_t)luaL_checkinteger(L, 1)));
    return 1;
}

static int reenter(lua_State *L) {
    lua_pushinteger(L, host_reenter((int32_t)luaL_checkinteger(L, 1)));
    return 1;
}

static int signal_host(lua_State *L) {
    (void)L;
    host_signal();
    return 0;
}

static int await_continuation(lua_State *L, int status, lua_KContext context) {
    (void)status;
    (void)context;
    if (!lua_toboolean(L, -2)) return luaL_error(L, "synthetic host rejection");
    lua_pushvalue(L, -1);
    return 1;
}

static int await_host(lua_State *L) {
    if (L != entry || !lua_isyieldable(L)) {
        return luaL_error(L, "hostAwait requires a yieldable entry coroutine");
    }
    if (sequence == INT32_MAX) return luaL_error(L, "request id exhausted");
    pending = ++sequence;
    lua_pushinteger(L, pending);
    return lua_yieldk(L, 1, 0, await_continuation);
}

EXPORT uintptr_t probe_input_ptr(void) { return (uintptr_t)input; }
EXPORT int probe_input_capacity(void) { return sizeof(input); }
EXPORT int probe_result(void) { return result; }
EXPORT int probe_pending(void) { return pending; }

EXPORT int probe_init(void) {
    if (root) return -1;
    root = luaL_newstate();
    if (!root) return -2;
    const luaL_Reg libraries[] = {
        {LUA_GNAME, luaopen_base}, {LUA_COLIBNAME, luaopen_coroutine},
        {LUA_TABLIBNAME, luaopen_table}, {LUA_STRLIBNAME, luaopen_string},
        {LUA_MATHLIBNAME, luaopen_math}, {LUA_UTF8LIBNAME, luaopen_utf8},
        {NULL, NULL}
    };
    for (const luaL_Reg *lib = libraries; lib->func; lib++) {
        luaL_requiref(root, lib->name, lib->func, 1);
        lua_pop(root, 1);
    }
    const char *disabled[] = {"dofile", "loadfile", "print", NULL};
    for (const char **name = disabled; *name; name++) {
        lua_pushnil(root);
        lua_setglobal(root, *name);
    }
    const luaL_Reg functions[] = {
        {"hostGet", get}, {"hostCommit", commit}, {"hostAcquire", acquire},
        {"hostRelease", release}, {"hostReenter", reenter},
        {"hostSignal", signal_host}, {"hostAwait", await_host}, {NULL, NULL}
    };
    for (const luaL_Reg *function = functions; function->func; function++) {
        lua_pushcfunction(root, function->func);
        lua_setglobal(root, function->name);
    }
    return 0;
}

EXPORT int probe_load(int length) {
    if (!root || length < 0 || (unsigned)length > sizeof(input) || pending) return -1;
    if (entry_ref != LUA_NOREF) luaL_unref(root, LUA_REGISTRYINDEX, entry_ref);
    entry = lua_newthread(root);
    entry_ref = luaL_ref(root, LUA_REGISTRYINDEX);
    started = 0;
    result = 0;
    host_compiling();
    return luaL_loadbufferx(entry, (const char *)input, length, "synthetic", "t");
}

/* Results: complete=0, ordinary yield=1, host request=2, Lua error=3. */
EXPORT int probe_resume(int reply_id, int accepted, int value) {
    if (!entry) return -1;
    int arguments = 0;
    if (started) {
        if (pending && pending != reply_id) return -2;
        lua_settop(entry, 0);
        if (pending) {
            lua_pushboolean(entry, accepted);
            lua_pushinteger(entry, value);
            arguments = 2;
        } else {
            lua_pushinteger(entry, value);
            arguments = 1;
        }
    }
    pending = 0;
    started = 1;
    int results = 0;
    int status = lua_resume(entry, NULL, arguments, &results);
    if (status == LUA_YIELD) return pending ? 2 : 1;
    if (status != LUA_OK) return 3;
    result = results ? (int)lua_tointeger(entry, -results) : 0;
    return 0;
}

/* Called reentrantly by a Rust import while entry is executing. */
EXPORT int probe_callback(int value) {
    if (!root) return -1;
    lua_getglobal(root, "callback");
    lua_pushinteger(root, value);
    if (lua_pcall(root, 1, 1, 0) != LUA_OK) {
        lua_pop(root, 1);
        return -2;
    }
    int answer = (int)lua_tointeger(root, -1);
    lua_pop(root, 1);
    return answer;
}

/* No lua_close export: Store disposal never enters guest finalizers. */
