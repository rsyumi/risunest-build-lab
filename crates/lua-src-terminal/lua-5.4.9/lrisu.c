/* Private terminal extension. See Copyright Notice in lua.h. */
#define lrisu_c
#define LUA_CORE
#include "lprefix.h"
#include "lua.h"
#include "ldo.h"
#include "lgc.h"
#include "lrisu.h"

LUA_API int lua_risu_arm (lua_State *L, lua_RisuPoll poll, void *context) {
  global_State *g = G(L);
  if (g->risu_reason || g->risu_poll != NULL) return 0;
  g->risu_context = context;
  g->risu_countdown = 1;
  g->risu_poll = poll;
  return 1;
}

LUA_API void lua_risu_disarm (lua_State *L) {
  G(L)->risu_poll = NULL;
  G(L)->risu_context = NULL;
}

LUA_API void lua_risu_retire (lua_State *L, int reason) {
  global_State *g = G(L);
  if (!g->risu_reason) g->risu_reason = reason;
  lua_risu_disarm(L);
}

LUA_API void lua_risu_text_only (lua_State *L) {
  G(L)->risu_text_only = 1;
}

LUA_API int lua_risu_reason (lua_State *L) {
  return G(L)->risu_reason;
}

LUA_API void lua_risu_checkpoint (lua_State *L) {
  global_State *g = G(L);
  if (g->risu_reason) luaD_throw(L, LUA_ERRTERMINAL);
  if (g->risu_poll != NULL && --g->risu_countdown == 0) {
    int reason;
    g->risu_countdown = 1024;
    reason = g->risu_poll(g->risu_context);
    /* The Rust callback has returned before any longjmp. */
    if (reason) {
      lua_risu_retire(L, reason);
      luaD_throw(L, LUA_ERRTERMINAL);
    }
  }
}

LUA_API void lua_risu_pollnow (lua_State *L) {
  G(L)->risu_countdown = 1;
  lua_risu_checkpoint(L);
}

/* The permit is exact, full-userdata-only, and consumed before native code.
** A destructor's nested Lua call cannot inherit permission to execute. */
int luaR_checkc (lua_State *L, StkId func, lua_CFunction f) {
  global_State *g = G(L);
  if (g->risu_reason && g->risu_permit == f &&
      L->top.p > func + 1 && ttisfulluserdata(s2v(func + 1)) &&
      uvalue(s2v(func + 1)) == g->risu_permit_ud) {
    g->risu_permit = NULL;
    g->risu_permit_ud = NULL;
    return 1;
  }
  lua_risu_pollnow(L);
  return 0;
}

static void cleanupcall (lua_State *L, void *ud) {
  UNUSED(ud);
  luaD_callnoyield(L, L->top.p - 2, 0);
}

void luaR_cleanup (lua_State *L, const TValue *value) {
  Udata *u;
  lua_CFunction f;
  global_State *g = G(L);
  ptrdiff_t top;
  int oldgcstp;
  lu_byte oldah;
  if (!ttisfulluserdata(value)) return;
  u = uvalue(value);
  if (u->risu_destructed || u->metatable == NULL) return;
  f = u->metatable->risu_destructor;
  if (f == NULL) return;
  /* Only exact native destructors execute here. Recursive native cleanup is
  ** outside their contract; ordinary Lua calls are rejected by the latch. */
  lua_assert(!g->risu_cleaning && g->risu_cleanup != NULL);
  L = g->risu_cleanup;
  g->risu_cleaning = 1;
  top = savestack(L, L->top.p);
  oldgcstp = g->gcstp;
  oldah = L->allowhook;
  g->gcstp |= GCSTPGC;
  L->allowhook = 0;
  /* The dedicated stack and first CallInfo were reserved at VM creation. */
  setfvalue(s2v(L->top.p++), f);
  setuvalue(L, s2v(L->top.p++), u);
  g->risu_permit = f;
  g->risu_permit_ud = u;
  luaD_pcall(L, cleanupcall, NULL, top, 0);
  g->risu_permit = NULL;
  g->risu_permit_ud = NULL;
  L->allowhook = oldah;
  g->gcstp = oldgcstp;
  L->top.p = restorestack(L, top);
  g->risu_cleaning = 0;
}
