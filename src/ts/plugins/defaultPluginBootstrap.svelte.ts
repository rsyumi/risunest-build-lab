import { pluginStartupState, pluginStartupGate } from './pluginStartupGate'
import { fromStore } from 'svelte/store'
import { onboardingHold } from '../../lib/Others/Onboarding/onboardingGate'
import { isTauri } from '../platform'
import { DBState } from '../stores.svelte'
import { getDeviceMarkers } from '../storage/deviceMarkers'
import { getStartupExclusions } from '../storage/deviceSettings'
import { isStartupExcluded } from '../storage/recoveryMode.svelte'
import {
    assertPersistentMutationAllowed,
    flushPendingDataLocally,
    getPersistentStorageAuthorityEpoch,
    markPersistentDataDirty,
    persistentWorkingSetInputBlocked,
} from '../storage/persistentDataRuntime.svelte'
import { yieldToUi } from '../ui/yieldToUi'
import { importPlugin, loadPlugins } from './plugins.svelte'

const ATTEMPT_KEY = 'risuNestDefaultPluginsAttempted'
const DEFAULT_PLUGINS = [
    ['provider-manager', 'https://update.rsyumi.workers.dev/provider-manager.js'],
    ['yumi-translator', 'https://update.rsyumi.workers.dev/yumi-translator.js'],
] as const

let started = false

function defaultPluginAttempts() {
    const markers = getDeviceMarkers()
    const attempted: unknown = JSON.parse(markers.getItem(ATTEMPT_KEY) ?? '[]')
    if (!Array.isArray(attempted) || attempted.some(name => typeof name !== 'string')) {
        throw new Error('Invalid default plugin attempt record')
    }
    const pending = DEFAULT_PLUGINS.filter(([name]) => !attempted.includes(name))
    return { markers, attempted, pending }
}

async function installDefaultPlugins(): Promise<void> {
    const { markers, attempted, pending } = defaultPluginAttempts()
    if (!pending.length) return
    const epoch = getPersistentStorageAuthorityEpoch()
    assertPersistentMutationAllowed(epoch)
    markers.setItem(ATTEMPT_KEY, JSON.stringify([...attempted, ...pending.map(([name]) => name)]))
    await markers.flush()

    let installed = false
    for (const [name, url] of pending) {
        assertPersistentMutationAllowed(epoch)
        if (DBState.db.plugins.some(plugin => plugin.name === name)) continue
        const controller = new AbortController()
        const timer = setTimeout(() => controller.abort(), 30_000)
        try {
            const response = await fetch(url, { cache: 'no-store', signal: controller.signal })
            if (!response.ok) throw new Error(`Plugin download failed: ${response.status}`)
            const code = await response.text()
            assertPersistentMutationAllowed(epoch)
            // Empty input selects a local file in the manual importer.
            if (!code.trim()) throw new Error('Plugin download was empty')
            installed = await importPlugin(code, { installIfMissing: name, deferReload: true }) || installed
        } catch (error) {
            console.error(`Default plugin installation failed: ${name}`, error)
        } finally {
            clearTimeout(timer)
        }
    }
    if (!installed) return
    assertPersistentMutationAllowed(epoch)
    markPersistentDataDirty(0)
    await flushPendingDataLocally('default-plugin-install')
    await loadPlugins(true, () => getPersistentStorageAuthorityEpoch() === epoch)
}

/** Called after bootstrap and legal acceptance, before or after onboarding finishes. */
export function startDefaultPluginBootstrap(): void {
    if (started || !isTauri || isStartupExcluded('plugins', getStartupExclusions())) return
    started = true
    const onboarding = fromStore(onboardingHold)
    const blocked = fromStore(persistentWorkingSetInputBlocked)
    const startup = fromStore(pluginStartupState)
    const stop = $effect.root(() => {
        $effect(() => {
            const held = onboarding.current
            const replacing = blocked.current
            const waiting = startup.current.phase !== 'released'
            if (!DBState.db.didFirstSetup || held || replacing || waiting) return
            let cancelled = false
            void yieldToUi().then(async () => {
                if (cancelled) return
                if (!defaultPluginAttempts().pending.length) { stop(); return }
                if (!await pluginStartupGate.ensure({ installingDefaults: true }) || cancelled) return
                stop()
                await installDefaultPlugins()
            }).catch(error => console.error('Default plugin bootstrap failed', error))
            return () => { cancelled = true }
        })
    })
}
