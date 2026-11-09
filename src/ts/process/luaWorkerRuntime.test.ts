// @vitest-environment node

import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { afterEach, expect, test } from 'vitest'
import { LuaFactory } from 'wasmoon'
import { LuaAsyncifyRuntime } from './luaAsyncifyRuntime'
import { luaCodeWrapper } from './luaCodeWrapper'

const runtimes: LuaAsyncifyRuntime[] = []
afterEach(() => { for (const runtime of runtimes.splice(0)) runtime.abandon() })

async function create(source: string, callbacks: Record<string, (...args: any[]) => unknown> = {},
    options: { timeout?: number; now?: () => number } = {}) {
    const factory = new LuaFactory(resolve('src/ts/process/lua/worker.wasm'))
    await factory.mountFile('json.lua', await readFile('public/lua/json.lua', 'utf8'))
    const engine = await factory.createEngine({ injectObjects: true, enableProxy: false })
    const promises = new Map<number, Promise<unknown>>()
    let sequence = 0
    const runtime = new LuaAsyncifyRuntime(engine, options.timeout ?? 1_000, async (name, args) => {
        // A host reply arrives outside the native entry in the real Worker.
        await Promise.resolve()
        const result = callbacks[name](...structuredClone(args))
        if (result instanceof Promise) {
            const id = ++sequence
            promises.set(id, result)
            void result.catch(() => {})
            return { promise: id }
        }
        return { value: structuredClone(result) }
    }, id => promises.get(id)!, { now: options.now ?? (() => performance.now()), run: run => run() })
    runtimes.push(runtime)
    for (const name of Object.keys(callbacks)) runtime.register(name)
    await runtime.initialize(luaCodeWrapper(source))
    return runtime
}

test('bounds synchronous reply latency without discarding Lua globals', async () => {
    let now = 0
    const runtime = await create(`
        count=0
        function run()
            count=count+1
            for i=1,10 do ping() end
            return 'completed'
        end
        function read() return count end
    `, { ping: () => { now += 150; return 'pong' } }, { timeout: 100, now: () => now })
    await expect(runtime.invoke('run', [])).rejects.toThrow('thread timeout exceeded')
    expect(runtime.isTainted()).toBe(false)
    expect(await runtime.invoke('read', [])).toBe(1)
})

test('keeps synchronous host calls inside C callbacks, metamethods and nested coroutines', async () => {
    const runtime = await create(`
        local initialized = host('initialized')
        function run()
            local text, count = ('xx'):gsub('x', function() return host('ok') end)
            local values = {3, 1, 2}
            table.sort(values, function(a, b) return host(a) < host(b) end)
            local t = setmetatable({}, {__index = function() return host('meta') end})
            local co = coroutine.create(function() return host('nested') end)
            local ok, nested = coroutine.resume(co)
            return table.concat({initialized, text, count, table.concat(values, ','), t.key, tostring(ok), nested}, '|')
        end
    `, { host: value => value })
    expect(await runtime.invoke('run', [])).toBe('initialized|okok|2|1,2,3|meta|true|nested')
})

test('keeps globals and genuine Promise APIs, and does not replay effects after a caught failure', async () => {
    const writes: string[] = []
    const runtime = await create(`
        count = 0
        onOutput = async(function()
            count = count + 1
            write('before')
            local ok, message = pcall(fail)
            local value = wait():await()
            write('after')
            return tostring(count)..':'..tostring(ok)..':'..tostring(message)..':'..value
        end)
    `, {
        write: value => { writes.push(value) },
        fail: () => { throw new Error('host failure') },
        wait: async () => 'resolved',
    })
    expect(await runtime.invoke('onOutput', [])).toBe('1:false:host failure:resolved')
    expect(await runtime.invoke('onOutput', [])).toBe('2:false:host failure:resolved')
    expect(writes).toEqual(['before', 'after', 'before', 'after'])
})

test('preserves async rejection handling and parallel promise completion', async () => {
    const runtime = await create(`
        onOutput = async(function()
            local rejected = failAsync():catch(function() return 'handled' end):await()
            local values = Promise.all({wait('a'), wait('b')}):await()
            return rejected..':'..values[1]..values[2]
        end)
    `, { failAsync: async () => { throw new Error('rejected') }, wait: async value => value })
    expect(await runtime.invoke('onOutput', [])).toBe('handled:ab')
})

test('returns independent decoded views and observes changed JSON and overridden decoders', async () => {
    let text = '[{"data":"original","nested":[1,2]}]'
    const runtime = await create(`
        function read()
            local a = getFullChat('id')
            a[1].data = 'local'
            a[1].nested[1] = 99
            local b = getFullChat('id')
            return b[1].data..':'..b[1].nested[1]
        end
        function override()
            local original = json.decode
            json.decode = function(_) return {{data='override', nested={3}}} end
            local result = read()
            json.decode = original
            return result
        end
    `, { getFullChatMain: () => text })
    expect(await runtime.invoke('read', [])).toBe('original:1')
    text = '[{"data":"changed","nested":[2]}]'
    expect(await runtime.invoke('read', [])).toBe('changed:2')
    expect(await runtime.invoke('override', [])).toBe('override:3')
    expect(await runtime.invoke('read', [])).toBe('changed:2')
})

test('does not reuse decoded output after a decoder dependency changes', async () => {
    const runtime = await create(`
        function run()
            local first=getState('id','key')
            local original=string.char
            string.char=function() return 'changed' end
            local second=getState('id','key')
            string.char=original
            local originalConcat=table.concat
            table.concat=function() return 'joined' end
            local joined=getState('id','key')
            table.concat=originalConcat
            return first..':'..second..':'..joined..':'..getState('id','key')
        end
    `, { getChatVar: () => '"\\u0061"' })
    expect(await runtime.invoke('run', [])).toBe('a:changed:joined:a')
})

test('runs Lua finalizers with host APIs when new invocation arguments trigger collection', async () => {
    let finalized = 0
    const runtime = await create(`
        function prepare()
            collectgarbage('stop')
            held={}
            for i=1,200 do held[i]=setmetatable({}, {__gc=function() host() end}) end
            held=nil
            collectgarbage('incremental', 1, 1000)
            collectgarbage('restart')
        end
        function run(value)
            collectgarbage('collect')
            return #value
        end
    `, { host: () => { finalized++ } })
    await runtime.invoke('prepare', [])
    expect(await runtime.invoke('run', ['x'.repeat(1024 * 1024)])).toBe(1024 * 1024)
    expect(finalized).toBe(200)
})

test('native values helper asset matches selected current product wrapper sections', async () => {
    const wrapper = luaCodeWrapper('').replaceAll('\r\n', '\n')
    const section = (start: string, end: string) => {
        const begin = wrapper.indexOf(start), finish = wrapper.indexOf(end, begin)
        expect(begin).toBeGreaterThanOrEqual(0)
        expect(finish).toBeGreaterThan(begin)
        expect(wrapper.indexOf(start, begin + start.length)).toBe(-1)
        expect(wrapper.indexOf(end, finish + end.length)).toBe(-1)
        return wrapper.slice(begin, finish)
    }
    const helpers = section("json = require 'json'", 'function setFullChat(')
        + section('function getState(', 'function async(')
    expect((await readFile('src-tauri/src/native_script/lua_value_helpers.lua', 'utf8')).replaceAll('\r\n', '\n')).toBe(helpers)
})

test('native values lexical decoder replacement is visible from actual product helpers', async () => {
    const runtime = await create(`decodeJSON=function(_) return 'lexical' end
        function run() return getState('id','x')..':'..getChat('id',0) end`, {
        getChatVar: () => 'null', getChatMain: () => '{"data":"original"}',
    })
    expect(await runtime.invoke('run', [])).toBe('lexical:lexical')
})

test('native values clone normalization uses final own payload and includes ignored arguments', async () => {
    const observed: string[] = []
    const runtime = await create(`function run()
        assert(not pcall(history,function() end,2))
        assert(not pcall(history,nil,2,function() end))
        assert(not pcall(history,nil,{valueOf=function() end}))
        assert(history(nil,{__proto__=function() end})=='NaN')
        assert(history(nil,{__proto__={toString=false}})=='NaN')
        assert(history(nil,{toString=false})=='coercion-error')
        assert(history(nil,{valueOf=false})=='NaN')
        local cyclic={}; cyclic[1]=cyclic; assert(history(nil,cyclic)=='0')
        return true
    end`, { history: (_id, value) => {
        let result: string
        try { result = String(Math.max(0, Math.floor(value || 0))) }
        catch { result = 'coercion-error' }
        observed.push(result)
        return result
    } })
    expect(await runtime.invoke('run', [])).toBe(true)
    expect(observed).toEqual(['NaN', 'NaN', 'coercion-error', 'NaN', '0'])
})

test('native values known byte windows match pinned Wasmoon without reading unknown tails', async () => {
    const runtime = await create(`function run()
        assert(echo('a'..string.char(0)..'b')=='a')
        assert(echo(string.char(239,187,191)..'x')==utf8.char(65279)..'x')
        assert(echo(string.char(239,187,191)..string.rep('x',17))==string.rep('x',17))
        assert(echo(string.char(194))==utf8.char(128))
        assert(echo(string.char(226,130))==utf8.char(8320))
        assert(echo(string.char(226,0,130))==utf8.char(8194))
        assert(echo(string.rep('x',17)..string.char(255))==string.rep('x',17)..utf8.char(65533))
        return numeric(string.char(237,160,128))
    end`, { echo: value => value, numeric: value => String(Number(value)) })
    expect(await runtime.invoke('run', [])).toBe('NaN')
})
