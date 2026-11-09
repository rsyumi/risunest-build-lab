-- The globals a main-thread Wasmoon 1.16 engine created with injectObjects adds (Promise, Error, null), print,
-- `require` for the mounted files, and stand-ins for the standard libraries this VM does not open.
--
-- Promise values are held as JS holds them: undefined is nil, null is the null userdata, and a Lua value stays as it
-- entered JS until it reaches Lua again, where `into_lua` converts it as Wasmoon's pushValue would. A Lua function
-- JS calls runs here as a protected call on the calling thread; a yield inside one fails as it does across
-- Wasmoon's pcall.
local make_metatable, new_userdata, new_array, js_text, decline, is_entry, is_memory, loaders = ...
local type, rawequal, rawget, select, tostring, error, next, setmetatable =
    type, rawequal, rawget, select, tostring, error, next, setmetatable
local math_type, tointeger, floor, huge = math.type, math.tointeger, math.floor, math.huge
local running, yield = coroutine.running, coroutine.yield
local sub = string.sub
local weak = { __mode = 'k' }

-- Wasmoon's heap grows where this VM meets its memory limit, so an allocation failure is declined wherever Lua code
-- could catch it.
local function caught(ok, ...)
    if not ok and is_memory((...)) then decline('lua-memory') end
    return ok, ...
end
local function loaded(chunk, ...)
    if chunk == nil and is_memory((...)) then decline('lua-memory') end
    return chunk, ...
end
-- Each wrapped function is called through a local of its own name, which an argument error names.
local pcall = (function(pcall) return function(...) return caught(pcall(...)) end end)(pcall)
_G.pcall = pcall
xpcall = (function(xpcall) return function(...) return caught(xpcall(...)) end end)(xpcall)
coroutine.resume = (function(resume) return function(...) return caught(resume(...)) end end)(coroutine.resume)
load = (function(load) return function(...) return loaded(load(...)) end end)(load)

local null_meta = make_metatable('js_null')
local null_value = new_userdata('js_null')
null_meta.__index = function() return null_value end
null_meta.__tostring = function() return 'null' end
null_meta.__eq = function(left, right) return rawequal(left, right) end
null = null_value

-- An argument as JS receives it: a missing one is undefined, a nil one is null.
local function to_js(present, value)
    if not present then return nil end
    if value == nil then return null_value end
    return value
end

local function falsy(value)
    if value == nil or value == false or rawequal(value, null_value) then return true end
    local kind = type(value)
    if kind == 'number' then return value == 0 or value ~= value end
    if kind == 'string' then return js_text(value) == '' end
    return false
end

local into_lua

local error_meta = make_metatable('js_error')
-- Each Error's message, as it entered JS.
local messages = setmetatable({}, weak)
error_meta.__index = function(self, key)
    if key == 'message' then return into_lua(messages[self]) end
    return null_value
end
error_meta.__tostring = function(self) return into_lua(messages[self]) end
local function make_error(message)
    local value = new_userdata('js_error')
    messages[value] = message
    return value
end
-- A JS function that throws raises its Error in Lua.
local function throw(message) error(make_error(message), 0) end

Error = {
    create = function(...)
        local message = to_js(select('#', ...) > 0, (...))
        if not falsy(message) and type(message) ~= 'string' then throw('message must be a string') end
        -- `new Error(message)`: String(message), and '' for undefined.
        if message == nil then return make_error('') end
        if type(message) == 'string' then return make_error(message) end
        if rawequal(message, null_value) then return make_error('null') end
        if message == false then return make_error('false') end
        if message ~= message then return make_error('NaN') end
        return make_error('0')
    end,
}

local promise_meta = make_metatable('js_promise')
-- Each crossing into Lua makes a new userdata; equality compares the promise it refers to.
local records = setmetatable({}, weak)
-- Arrays JS built, with their length.
local js_arrays = setmetatable({}, weak)
local function wrap(record)
    local value = new_userdata('js_promise')
    records[value] = record
    return value
end
local function record_of(value) return type(value) == 'userdata' and records[value] or nil end

-- lua_tonumberx, then lua_pushinteger(BigInt(value)) when Number.isInteger(value).
local function number_into_lua(value)
    if math_type(value) == 'integer' then value = value + 0.0 end
    if value ~= value or value == huge or value == -huge or value ~= floor(value) then return value end
    if value >= -2 ^ 63 and value < 2 ^ 63 then return tointeger(value) end
    decline('lua-promise-number')
end

function into_lua(value)
    local kind = type(value)
    if kind == 'nil' or kind == 'boolean' then return value end
    if kind == 'number' then return number_into_lua(value) end
    if kind == 'string' then return js_text(value) end
    if kind == 'userdata' then
        if rawequal(value, null_value) then return value end
        local record = records[value]
        if record then return wrap(record) end
        local message = messages[value]
        if message ~= nil then return make_error(message) end
    elseif kind == 'table' then
        local count = js_arrays[value]
        if count then
            local items = {}
            for index = 1, count do items[index] = into_lua(value[index]) end
            return new_array(items, count)
        end
    end
    -- Wasmoon copies a table or wraps a function on the way through JS.
    decline('lua-promise-value:' .. kind)
end

-- A Lua error reaching JS: an Error with the error value's text as its message.
local function error_value(reason)
    local ok, message = pcall(tostring, reason)
    if not ok or type(message) ~= 'string' then decline('lua-promise-error') end
    return make_error(message)
end

local js_calls = setmetatable({}, weak)
-- A Lua function called from JS with arguments already converted for Lua: its first result as JS reads it, or the
-- Error JS receives when it throws.
local function call_from_js(callback, ...)
    local thread = running()
    js_calls[thread] = (js_calls[thread] or 0) + 1
    local ok, result = pcall(callback, ...)
    js_calls[thread] = js_calls[thread] - 1
    if ok then return true, to_js(true, result) end
    return false, error_value(result)
end

local queue, head, tail = {}, 1, 0
-- Jobs TS runs only after the invocation revoked its access key; user handlers among them are declined.
local trailing = false
local function enqueue(job) tail = tail + 1; queue[tail] = job end
local function new_record() return { state = 'pending', reactions = {} } end

local function settle(record, state, value)
    local reactions = record.reactions
    record.state, record.value, record.reactions = state, value, nil
    for index = 1, #reactions do
        local reaction = reactions[index]
        enqueue(function() reaction(state, value) end)
    end
end

local function subscribe(record, reaction)
    if record.state == 'pending' then
        record.reactions[#record.reactions + 1] = reaction
    else
        local state, value = record.state, record.value
        enqueue(function() reaction(state, value) end)
    end
end

local resolving
local function adopt(record, value)
    local other = record_of(value)
    if other == record then
        settle(record, 'rejected', make_error('Chaining cycle detected for promise #<Promise>'))
    elseif other then
        -- NewPromiseResolveThenableJob, then the reaction it registers.
        enqueue(function()
            local resolve, reject = resolving(record)
            subscribe(other, function(state, result)
                if state == 'fulfilled' then resolve(result) else reject(result) end
            end)
        end)
    else
        -- The object JS made of a table would be a thenable.
        if type(value) == 'table' and not js_arrays[value] and rawget(value, 'then') ~= nil then decline('lua-promise-thenable') end
        settle(record, 'fulfilled', value)
    end
end

function resolving(record)
    local done = false
    return function(value)
        if done then return end
        done = true
        adopt(record, value)
    end, function(reason)
        if done then return end
        done = true
        settle(record, 'rejected', reason)
    end
end

-- The resolving function as Lua code calls it.
local function js_function(callback)
    return function(...) callback(to_js(select('#', ...) > 0, (...))) end
end

-- A 'user' handler is Lua code JS calls; 'prelude' and 'internal' handlers belong to this file and take and return
-- values as JS holds them. Only 'internal' handlers run no user code.
local function chain(record, on_fulfilled, on_rejected, mode)
    local derived = new_record()
    local resolve, reject = resolving(derived)
    subscribe(record, function(state, value)
        local handler
        if state == 'fulfilled' then handler = on_fulfilled else handler = on_rejected end
        if type(handler) ~= 'function' then
            if state == 'fulfilled' then resolve(value) else reject(value) end
            return
        end
        if trailing and mode ~= 'internal' then decline('lua-edit-trailing-microtasks') end
        local ok, result
        if mode == 'user' then ok, result = call_from_js(handler, into_lua(value))
        else ok, result = pcall(handler, value) end
        if ok then resolve(result) else reject(result) end
    end)
    return derived
end

-- PromiseResolve.
local function resolved(value)
    local record = record_of(value)
    if record then return record end
    record = new_record()
    adopt(record, value)
    return record
end

local function finally(record, on_finally)
    if type(on_finally) ~= 'function' then return chain(record, on_finally, on_finally, 'user') end
    local function after(value, rejected)
        local ok, result = call_from_js(on_finally)
        if not ok then error(result, 0) end
        return wrap(chain(resolved(result), function()
            if rejected then error(value, 0) end
            return value
        end, nil, 'internal'))
    end
    return chain(record, function(value) return after(value, false) end, function(reason) return after(reason, true) end, 'prelude')
end

local function self_record(self)
    local record = record_of(self)
    if record then return record end
    if type(self) == 'table' and rawget(self, 'then') ~= nil then decline('lua-promise-thenable') end
    throw('promise method called without self instance')
end

promise_meta.__index = {
    next = function(self, on_fulfilled, on_rejected) return wrap(chain(self_record(self), on_fulfilled, on_rejected, 'user')) end,
    catch = function(self, on_rejected) return wrap(chain(self_record(self), nil, on_rejected, 'user')) end,
    finally = function(self, on_finally) return wrap(finally(self_record(self), on_finally)) end,
    await = function(self)
        local record = self_record(self)
        local state, value
        local awaiting = chain(chain(record, function(result) state, value = 'fulfilled', result end, nil, 'internal'), nil,
            function(reason) state, value = 'rejected', reason end, 'internal')
        if (js_calls[running()] or 0) > 0 then error('attempt to yield across a C-call boundary', 0) end
        -- Wasmoon awaits a promise yielded by top-level source; this VM has no scheduler for that.
        if is_entry() then decline('lua-await-top-level') end
        yield(wrap(awaiting))
        while state == nil do yield() end
        if state == 'rejected' then
            if falsy(value) then value = make_error('promise rejected with no error') end
            error(into_lua(value), 0)
        end
        return into_lua(value)
    end,
}
promise_meta.__eq = function(left, right)
    local record = records[left]
    return record ~= nil and record == records[right]
end

Promise = {
    create = function(...)
        local callback = to_js(select('#', ...) > 0, (...))
        -- V8 names the value in its TypeError.
        if type(callback) ~= 'function' then decline('lua-promise-resolver') end
        local record = new_record()
        local resolve, reject = resolving(record)
        local ok, reason = call_from_js(callback, js_function(resolve), js_function(reject))
        if not ok then reject(reason) end
        return wrap(record)
    end,
    all = function(...)
        local list = to_js(select('#', ...) > 0, (...))
        -- Wasmoon converts a table to an Array when its keys enumerate as 1..n.
        if type(list) ~= 'table' then throw('argument must be an array of promises') end
        local items, count = {}, 0
        for key, value in next, list do
            count = count + 1
            if tostring(key) ~= tostring(count) then throw('argument must be an array of promises') end
            items[count] = value
        end
        if count == 0 then throw('argument must be an array of promises') end
        local result = new_record()
        local resolve, reject = resolving(result)
        local values, remaining = {}, count
        js_arrays[values] = count
        for index = 1, count do
            subscribe(resolved(items[index]), function(state, value)
                if state ~= 'fulfilled' then return reject(value) end
                values[index] = value
                remaining = remaining - 1
                if remaining == 0 then resolve(values) end
            end)
        end
        return wrap(result)
    end,
    resolve = function(...) return wrap(resolved(to_js(select('#', ...) > 0, (...)))) end,
}

-- Unsupported declined the invocation, so a bounded label is enough.
local function label(key)
    if type(key) == 'string' then return sub(key, 1, 48) end
    return type(key)
end
local function unsupported(name)
    return setmetatable({}, {
        __index = function(_, key) decline('lua-std:' .. name .. '.' .. label(key)) end,
        __newindex = function(_, key) decline('lua-std:' .. name .. '.' .. label(key)) end,
        __pairs = function() decline('lua-std:' .. name) end,
    })
end
os, io, debug, package = unsupported('os'), unsupported('io'), unsupported('debug'), unsupported('package')
dofile = function() decline('lua-std:dofile') end
loadfile = function() decline('lua-std:loadfile') end
-- Text-only chunks: dump output can never be loaded again here, unlike Wasmoon.
string.dump = function() decline('lua-std:string.dump') end
-- Only a full collection returns the same here as in Wasmoon's heap.
local collect = collectgarbage
collectgarbage = function(option, ...)
    if option ~= nil and option ~= 'collect' then decline('lua-std:collectgarbage') end
    return collect('collect')
end

-- The files Wasmoon mounts; the standard `require` would search the file system for any other name.
local loaded = {}
require = function(name)
    local loader = type(name) == 'string' and loaders[name]
    if not loader then decline('lua-std:require') end
    if loaded[name] == nil then
        local module = loader(name)
        if loaded[name] == nil then loaded[name] = module == nil or module end
    end
    return loaded[name]
end

print = function(...)
    for index = 1, select('#', ...) do tostring((select(index, ...))) end
end

local function fulfilled_nil() return wrap(resolved(nil)) end

local function run_job()
    local job = queue[head]
    queue[head] = nil
    head = head + 1
    job()
end

local function drain_trailing()
    trailing = true
    while head <= tail do run_job() end
    trailing = false
    queue, head, tail = {}, 1, 0
end

-- TS `await func(mode, key, value, meta)` for the four edit modes: the await reaction joins the job queue at the
-- call's return, and the invocation ends when it runs.
local function listen(mode, key, value, meta)
    drain_trailing()
    -- `getFunction` reads the global into JS and calls it when truthy.
    local listener = callListenMain
    if falsy(listener) then return nil end
    if type(listener) ~= 'function' then error('callListenMain is not a function') end
    local ok, result = call_from_js(listener, mode, key, value, meta)
    local done, state, outcome = false
    if ok then
        local record = record_of(result)
        if record then
            subscribe(record, function(s, v) done, state, outcome = true, s, v end)
        else
            enqueue(function() done, state, outcome = true, 'fulfilled', result end)
        end
        while not done do
            if head > tail then decline('lua-edit-pending-promise') end
            run_job()
        end
    end
    drain_trailing()
    if not ok then error(result, 0) end
    if state == 'rejected' then error(outcome, 0) end
    if outcome == nil or rawequal(outcome, null_value) then return nil end
    if type(outcome) == 'string' then return outcome end
    decline('lua-edit-result:' .. type(outcome))
end

return listen, fulfilled_nil
