/* Private terminal extension. See Copyright Notice in lua.h. */
#ifndef lrisu_h
#define lrisu_h
#include "lstate.h"
LUAI_FUNC void luaR_cleanup (lua_State *L, const TValue *value);
LUAI_FUNC int luaR_checkc (lua_State *L, StkId func, lua_CFunction f);
#endif
