import { mount } from 'svelte'
import Settings from './Settings.svelte'
import { fixtureState } from './hostAdapters'
import { loadV3Plugins, getV3PluginInstance } from '../../src/ts/plugins/apiV3/v3.svelte'
import { thoughtDriver } from './thoughtDriver'
import { SandboxHost } from '../../src/ts/plugins/apiV3/factory'
import { createPluginDownloadHandler } from '../../src/ts/plugins/apiV3/pluginDownload'
import { installWebviewBridge, installMainFrameLinkBridge } from '../../src/ts/webviewDownload'

mount(Settings, { target: document.querySelector('#settings')! })
const events: unknown[] = []
const bridgeMessages: any[] = []
window.addEventListener('message', event => {
    if (event.data?.fixture) events.push(event.data)
    if (['CALL_ROOT', 'CALL_INSTANCE', 'EXEC_RESULT', 'CALLBACK_RETURN'].includes(event.data?.type)) bridgeMessages.push(event.data)
})
let callbackResult: unknown = null
let oldHost: ReturnType<typeof getV3PluginInstance>
let downloadHost: SandboxHost | undefined
const downloads = {
    files: [] as { name: string; bytes: number[] }[],
    errors: [] as string[],
    opened: [] as string[],
    mode: 'save' as 'save' | 'cancel' | 'fail',
}
const driver = {
    state: fixtureState, events, bridgeMessages,
    downloads,
    loadDownload(script: string, native = true) {
        downloadHost?.terminate()
        const frame = document.createElement('iframe')
        document.body.append(frame)
        downloadHost = new SandboxHost({
            _getPropertiesForInitialization: () => ({ list: [] }),
            _getAliases: () => ({}),
        }, native ? createPluginDownloadHandler({
            fetch: (url, options) => fetch(url, options),
            save: async (name, bytes) => {
                if (downloads.mode === 'cancel') return false
                if (downloads.mode === 'fail') throw new Error('Synthetic storage failure')
                downloads.files.push({ name, bytes: [...bytes] })
                return true
            },
            reportError: error => { downloads.errors.push(String(error)) },
        }) : undefined)
        downloadHost.run(frame, script, 'synthetic-download')
    },
    installMainBridge() {
        const save = createPluginDownloadHandler({
            fetch: (url, options) => fetch(url, options),
            save: async (name, bytes) => { downloads.files.push({ name, bytes: [...bytes] }); return true },
            reportError: error => { downloads.errors.push(String(error)) },
        })
        installWebviewBridge((name, source) => save(name, source, new AbortController().signal),
            error => { downloads.errors.push(String(error)) }, true)
        installMainFrameLinkBridge(async url => { downloads.opened.push(url) }, error => { downloads.errors.push(String(error)) })
    },
    unloadDownload() { downloadHost?.terminate() },
    get downloadIdle() { return downloadHost?.isIdle() },
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
    forgeGuest(messages: unknown[]) {
        const target = document.querySelector<HTMLIFrameElement>('iframe[data-risu-plugin-frame]')!
        const index = [...document.querySelectorAll('iframe')].indexOf(target)
        const other = document.createElement('iframe')
        other.sandbox.add('allow-scripts')
        other.srcdoc = '<script>for(const message of ' + JSON.stringify(messages) + ')parent.frames[' + index + "].postMessage(message,'*');<\/script>"
        document.body.append(other)
    },
    forge(reqId: string) {
        const other = document.createElement('iframe')
        other.sandbox.add('allow-scripts')
        other.srcdoc = `<script>parent.postMessage({type:'CALLBACK_RETURN',reqId:${JSON.stringify(reqId)},result:'forged'},'*');parent.postMessage({fixture:'forged-sent'},'*')<\/script>`
        document.body.append(other)
    },
}
Object.assign(window, { boundary: driver, thought: thoughtDriver })
export type BrowserDriver = typeof driver
