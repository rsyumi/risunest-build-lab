import { describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { configurePluginStartup, createPluginStartupGate, delayPluginLoadingUntilSync, PLUGIN_SYNC_SETTING,
    pluginStartupGate, retryPluginStartup } from './pluginStartupGate'
import { notifySyncBindingChanged } from '../storage/sync/bindingChanges'
import { prepareBoundLibraryReplacement, registerSyncBindingTransport } from '../storage/sync/bindingRegistry'
import type { SyncBindingState } from '../storage/sync/bindingFlow'

const native = vi.hoisted(() => ({ unavailable: false, state: { target: { kind: 'none' }, libraryId: 'synthetic', targetAuthority: '1', selectionEpoch: '1', progress: null } as SyncBindingState }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({runtime:'tauri',os:'windows'}))
vi.mock('../storage/sync/bindingNative', () => ({ createNativeSyncBindingBridge: () => ({
    state: async () => {if(native.unavailable)throw new Error('native state unavailable');return native.state}, assertAuthority: async () => {},
}) }))

function deferred() {
    let resolve!: () => void
    let reject!: (error: unknown) => void
    const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
    return { promise, resolve, reject }
}
function harness() {
    const receive = deferred()
    const h = { enabled: true, delay: true, excluded: false, pending: false, bound: true, held: false, identity: 'first', starts: 0, reads: 0, received: false }
    const gate = createPluginStartupGate({
        enabled: () => h.enabled, delay: () => h.delay,
        syncExcluded: () => h.excluded,
        replacementPending: () => h.pending,
        async context() {
            h.reads++
            const identity = h.identity
            return { identity, bound: h.bound, heldByRestore: h.held, replacementPending: h.pending,
                async receive(signal) { await receive.promise; signal.throwIfAborted(); h.received = true },
                async assertCurrent() { if (identity !== h.identity) throw new Error('Sync binding changed') },
            }
        },
        async start() { if (await gate.ensure()) h.starts++ },
    })
    return { h, gate, receive }
}

describe('plugin initial receive gate', () => {
    it.each([false,true])('uses the real replacement resume notification to release the production gate after a server hold (delay disabled=%s)', async disabled => {
        native.state.target = {kind:'server',connectionId:'synthetic-startup'}
        let active = true, held = false, starts = 0
        const unsupported = async (): Promise<never> => {throw new Error('Unexpected binding switch')}
        const unregister = registerSyncBindingTransport(native.state.target, {
            inspectTarget:unsupported,pullAvailableState:unsupported,replaceFromTarget:unsupported,publishInitialSharedState:unsupported,
            heldForCommittedRestore:()=>held,
            receiveAvailableChanges:async()=>{if(!active)throw new Error('Sync binding changed')},
            fenceOldJobs:async()=>{active=false},
            resumeBinding:async context=>{held=context.holdAutomaticSync===true;active=true},
        })
        configurePluginStartup(()=>true,async()=>{if(await pluginStartupGate.ensure())starts++})
        try {
            const replacement=await prepareBoundLibraryReplacement()
            await replacement.fence()
            if(disabled){localStorage.setItem(PLUGIN_SYNC_SETTING,'{"delayPluginLoadingUntilSync":false}');native.unavailable=true}
            expect(await pluginStartupGate.ensure()).toBe(false)
            expect(get(pluginStartupGate.state).phase).toBe('waiting')
            await replacement.resume({holdAutomaticSync:true})
            await vi.waitFor(()=>expect(starts).toBe(1))
            expect(get(pluginStartupGate.state).phase).toBe('released')
        } finally {
            unregister();native.state.target={kind:'none'};native.unavailable=false;localStorage.removeItem(PLUGIN_SYNC_SETTING)
            configurePluginStartup(()=>false,async()=>{})
            pluginStartupGate.invalidate();pluginStartupGate.state.set({phase:'released',since:0})
        }
    })
    it('handles failed plugin loads after a binding change and an explicit retry', async () => {
        const failure = new Error('synthetic plugin load failure')
        const log = vi.spyOn(console, 'error').mockImplementation(() => {})
        configurePluginStartup(() => true, async () => { throw failure })
        pluginStartupGate.state.set({ phase: 'waiting', since: 1 })
        try {
            notifySyncBindingChanged()
            await vi.waitFor(() => expect(log).toHaveBeenCalledWith('Plugin startup failed', failure))
            log.mockClear()
            await expect(retryPluginStartup()).resolves.toBeUndefined()
            expect(log).toHaveBeenCalledWith('Plugin startup failed', failure)
        } finally {
            configurePluginStartup(() => false, async () => {})
            pluginStartupGate.invalidate()
            pluginStartupGate.state.set({ phase: 'released', since: 0 })
            log.mockRestore()
        }
    })
    it('returns control to the shell while receive is pending and executes only after completion', async () => {
        const { h, gate, receive } = harness()
        const checking = gate.ensure()
        expect(gate.refuse()).toBe(false)
        expect(await checking).toBe(false)
        expect(get(gate.state).phase).toBe('waiting')
        expect(h.starts).toBe(0)
        expect(gate.refuse()).toBe(true)
        expect(gate.refusalCount).toBe(1)
        receive.resolve()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        expect(h.received).toBe(true)
        expect(gate.refuse()).toBe(false)
        expect(await gate.ensure()).toBe(true)
    })
    it.each(['bound', 'enabled', 'delay'] as const)('does not wait without %s', async field => {
        const { h, gate } = harness()
        h[field] = false
        expect(await gate.ensure()).toBe(true)
        expect(get(gate.state).phase).toBe('released')
    })
    it('never shows a wait or refuses input while checking an unbound device again', async () => {
        const { h, gate } = harness()
        h.bound = false
        const phases: string[] = []
        const stop = gate.state.subscribe(value => phases.push(value.phase))
        for (let index = 0; index < 3; index++) {
            const pending = gate.ensure()
            expect(gate.refuse()).toBe(false)
            expect(await pending).toBe(true)
        }
        expect(phases).toEqual(['released'])
        stop()
    })
    it('waits before installing defaults with no enabled plugins and retains that wait across loads', async () => {
        const { h, gate, receive } = harness()
        h.enabled = false
        expect(await gate.ensure({ installingDefaults: true })).toBe(false)
        expect(await gate.ensure()).toBe(false)
        expect(gate.refuse()).toBe(false)
        receive.resolve()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        expect(h.received).toBe(true)
        expect(await gate.ensure({ installingDefaults: true })).toBe(true)
    })
    it('bypasses only a committed server restore hold', async () => {
        const { h, gate } = harness()
        h.held = true
        expect(await gate.ensure()).toBe(true)
        h.held = false
        expect(await gate.ensure()).toBe(false)
    })
    it('keeps a failed receive closed without a timeout, and accepts a one-time load', async () => {
        const { h, gate, receive } = harness()
        await gate.ensure()
        receive.reject(new Error('offline'))
        await vi.waitFor(() => expect(get(gate.state).phase).toBe('failed'))
        expect(h.starts).toBe(0)
        gate.loadOnce()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        expect(h.delay).toBe(true)
        expect(await harness().gate.ensure()).toBe(false)
    })
    it('ignores a completion from the previous binding or cancelled startup attempt', async () => {
        const { h, gate, receive } = harness()
        await gate.ensure()
        gate.invalidate()
        h.identity = 'replacement'
        receive.resolve()
        await Promise.resolve(); await Promise.resolve()
        expect(h.starts).toBe(0)
        expect(get(gate.state).phase).toBe('waiting')
    })
    it('waits for a notification when authority changes, without an immediate receive loop', async () => {
        const { h, gate, receive } = harness()
        await gate.ensure()
        h.identity = 'replacement'
        receive.resolve()
        await vi.waitFor(() => expect(h.received).toBe(true))
        expect(get(gate.state).phase).toBe('waiting')
        expect(get(gate.state).error).toBeUndefined()
        for (let index = 0; index < 3; index++) expect(await gate.ensure()).toBe(false)
        expect(h.reads).toBe(1)
        expect(h.starts).toBe(0)
        gate.retry()
        await vi.waitFor(() => expect(h.starts).toBe(1))
    })
    it('also waits for notification when the context lookup reports a changed binding', async () => {
        let reads = 0
        const gate = createPluginStartupGate({enabled:()=>true,delay:()=>true,start:async()=>{},
            context:async()=>{reads++;throw new Error('Sync binding changed')},
        })
        expect(await gate.ensure()).toBe(false)
        expect(get(gate.state).phase).toBe('waiting')
        expect(get(gate.state).error).toBeUndefined()
        expect(await gate.ensure()).toBe(false)
        expect(reads).toBe(1)
        gate.invalidate()
        expect(await gate.ensure()).toBe(false)
        expect(reads).toBe(2)
    })
    it('starts without receiving excluded sync and retains the stored preference', async () => {
        const { h, gate } = harness()
        h.excluded = true
        expect(await gate.ensure()).toBe(true)
        expect(await gate.ensure({ installingDefaults: true })).toBe(true)
        expect(h.reads).toBe(0)
        expect(h.received).toBe(false)
        expect(h.delay).toBe(true)
        expect(gate.refuse()).toBe(false)
        expect(get(gate.state).phase).toBe('released')
    })
    it.each(['delay', 'excluded', 'once'] as const)('does not bypass an operation-owned hold with %s', async policy => {
        const { h, gate } = harness()
        h.pending = true
        if (policy === 'delay') h.delay = false
        if (policy === 'excluded') h.excluded = true
        if (policy === 'once') gate.loadOnce()
        expect(await gate.ensure()).toBe(false)
        expect(h.starts).toBe(0)
        expect(get(gate.state).phase).toBe('waiting')
        h.pending = false
        gate.retry()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        expect(h.received).toBe(false)
    })
    it.each(['delay', 'excluded', 'once'] as const)('retains an operation-owned hold before a failing context lookup with %s', async policy => {
        let pending = true, reads = 0, starts = 0
        const gate = createPluginStartupGate({enabled:()=>true,delay:()=>policy!=='delay',syncExcluded:()=>policy==='excluded',
            replacementPending:()=>pending,
            context:async()=>{reads++;throw new Error('native state unavailable')},
            start:async()=>{if(await gate.ensure())starts++},
        })
        if(policy==='once')gate.loadOnce()
        expect(await gate.ensure()).toBe(false)
        expect(get(gate.state).phase).toBe('waiting')
        expect(reads).toBe(0)
        expect(starts).toBe(0)
        pending=false
        gate.retry()
        await vi.waitFor(()=>expect(starts).toBe(1))
    })
    it('keeps an operation-fenced binding pending even if it had received before the fence', async () => {
        const { h, gate, receive } = harness()
        await gate.ensure()
        receive.resolve()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        h.pending = true
        expect(await gate.ensure()).toBe(false)
        expect(get(gate.state).phase).toBe('waiting')
        h.pending = false
        h.held = true
        gate.retry()
        await vi.waitFor(() => expect(h.starts).toBe(2))
    })
    it('keeps an existing receive waiting when a replacement fences it before plugin preparation', async () => {
        const { h, gate, receive } = harness()
        const phases: string[] = []
        const stop=gate.state.subscribe(value=>phases.push(value.phase))
        await gate.ensure()
        h.pending=true
        receive.reject(new Error('Sync is stopped'))
        await new Promise<void>(resolve=>queueMicrotask(resolve))
        await new Promise<void>(resolve=>queueMicrotask(resolve))
        expect(phases).not.toContain('failed')
        expect(get(gate.state).phase).toBe('waiting')
        expect(get(gate.state).error).toBeUndefined()
        stop()
    })
    it('releases only this wait when the preference is unchecked', async () => {
        const { h, gate, receive } = harness()
        await gate.ensure()
        h.delay = false
        gate.preferenceChanged()
        await vi.waitFor(() => expect(h.starts).toBe(1))
        receive.resolve()
        await Promise.resolve(); await Promise.resolve()
        expect(h.starts).toBe(1)
        h.delay = true
        gate.preferenceChanged()
        expect(h.starts).toBe(1)
    })
    it.each([null, 'invalid', '{}', '{"delayPluginLoadingUntilSync":"false"}', '{"delayPluginLoadingUntilSync":true}'])('defaults on for %s', value => {
        localStorage.removeItem(PLUGIN_SYNC_SETTING)
        if (value !== null) localStorage.setItem(PLUGIN_SYNC_SETTING, value)
        expect(delayPluginLoadingUntilSync()).toBe(true)
    })
    it('preserves an explicit false without altering existing preferences', () => {
        localStorage.setItem('risuNestDeviceSettings', 'synthetic-existing-preferences')
        localStorage.setItem(PLUGIN_SYNC_SETTING, '{"delayPluginLoadingUntilSync":false}')
        expect(delayPluginLoadingUntilSync()).toBe(false)
        expect(localStorage.getItem('risuNestDeviceSettings')).toBe('synthetic-existing-preferences')
        localStorage.clear()
    })
})
