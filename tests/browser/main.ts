import { mount } from 'svelte'
import Settings from './Settings.svelte'
import { fixtureState } from './hostAdapters'
import { loadV3Plugins, getV3PluginInstance } from '../../src/ts/plugins/apiV3/v3.svelte'

mount(Settings, { target: document.querySelector('#settings')! })
const events: unknown[] = []
window.addEventListener('message', event => {
    if (event.data?.fixture) events.push(event.data)
})
let callbackResult: unknown = null
let oldHost: ReturnType<typeof getV3PluginInstance>
const driver = {
    state: fixtureState, events,
    async load(script: string) {
        fixtureState.database.plugins = [{ name: 'boundary', script, arguments: {}, realArg: {}, version: '3.0', customLink: [], argMeta: {}, enabled: true }]
        await loadV3Plugins(fixtureState.database.plugins)
    },
    async unload() {
        oldHost = getV3PluginInstance('boundary')
        await loadV3Plugins([])
    },
    call() {
        callbackResult = null
        const callback = [...fixtureState.callbacks][0]
        if (!callback) throw new Error('Plugin callback missing')
        void callback({ text: 'synthetic' }).then(value => { callbackResult = { value } }, error => { callbackResult = { error: String(error) } })
    },
    get callbackResult() { return callbackResult },
    get released() {
        const host = oldHost?.host as unknown as { pendingCallbacks: Map<unknown, unknown>; messageHandlerRef: unknown }
        return !!host && host.pendingCallbacks.size === 0 && host.messageHandlerRef === null && fixtureState.callbacks.size === 0
    },
    get oldCallsFinished() {
        return (oldHost?.host as unknown as { pendingHostCalls: number })?.pendingHostCalls === 0
    },
    releaseHeld() { fixtureState.releaseHeld?.() },
    release() { document.querySelector<HTMLIFrameElement>('iframe[data-risu-plugin-frame]')?.contentWindow?.postMessage({ fixtureRelease: true }, '*') },
    forge(reqId: string) {
        const other = document.createElement('iframe')
        other.sandbox.add('allow-scripts')
        other.srcdoc = `<script>parent.postMessage({type:'CALLBACK_RETURN',reqId:${JSON.stringify(reqId)},result:'forged'},'*');parent.postMessage({fixture:'forged-sent'},'*')<\/script>`
        document.body.append(other)
    },
}
Object.assign(window, { boundary: driver })
export type BrowserDriver = typeof driver
