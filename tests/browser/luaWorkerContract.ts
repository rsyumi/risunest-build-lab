import { LuaWorkerExecutor } from '../../src/ts/process/luaWorkerExecutor'
import { luaCodeWrapper } from '../../src/ts/process/luaCodeWrapper'
import { createLuaFactory } from '../../src/ts/process/luaRuntime'

export async function runLuaWorkerContract() {
    const check = (value: unknown, expected: unknown) => {
        if (value !== expected) throw new Error(`Lua runtime contract: ${String(value)} != ${String(expected)}`)
    }
    const response = await fetch('/lua/json.lua')
    if (!response.ok) throw new Error('Lua JSON asset unavailable')
    const json = await response.text()
    const first = new LuaWorkerExecutor(5_000)
    const second = new LuaWorkerExecutor(5_000)
    const controller = new AbortController()
    first.register('echo', value => value)
    first.register('wait', async () => 'ready')
    first.register('cancel', () => controller.abort(new Error('superseded render')))
    first.register('getFullChatMain', () => JSON.stringify([{data: 'synthetic '.repeat(512)}]))
    try {
        await first.initialize(luaCodeWrapper(`count=0
            function run() count=count+1; return ('xx'):gsub('x',function() return echo('ok') end) end
            onOutput=async(function() return wait():await() end)
            function views()
                local a=getFullChat('id'); a[1].data='edited'
                return getFullChat('id')[1].data
            end
            function superseded() count=count+1; cancel() end
            function read() return count end
            function compute() local n=0; for i=1,3000000 do n=n+i end; return n end
        `), json)
        await second.initialize('function read() return _VERSION end', json)
        check(await first.invoke('run', []), 'okok')
        check(await first.invoke('onOutput', []), 'ready')
        check(await first.invoke('views', []), 'synthetic '.repeat(512))
        let cancelled = false
        try { await first.invoke('superseded', [], controller.signal) }
        catch (error) { cancelled = error instanceof Error && error.message === 'superseded render' }
        check(cancelled, true)
        check(await first.invoke('read', []), 2)
        check(await second.invoke('read', []), 'Lua 5.4')
        let timerRan = false
        const timer = new Promise<void>(resolve => setTimeout(() => { timerRan = true; resolve() }, 0))
        check(await first.invoke('compute', []), 4500001500000)
        check(timerRan, true)
        await timer
        const factory = await createLuaFactory()
        const fallback = await factory.createEngine({ injectObjects: true, enableProxy: false })
        try { check(await fallback.doString('return _VERSION'), 'Lua 5.4') }
        finally { fallback.global.close() }
        return { passed: true, worker: true, independentEngines: true, cancellation: true, fallbackAsset: true }
    } finally { first.close(); second.close() }
}
