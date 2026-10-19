export type WebviewDownloadHandler = (name: string, source: Blob | string, signal: AbortSignal) => Promise<boolean>

interface WebviewDownloadDependencies {
    fetch: (url: string, options: { signal: AbortSignal }) => Promise<Response>
    save: (name: string, bytes: Uint8Array) => Promise<boolean>
    reportError: (error: unknown) => void | Promise<void>
}

// All WebView downloads share the native picker, which accepts one request at a time.
let pendingDownload: Promise<unknown> = Promise.resolve()

export function createWebviewDownloadHandler(dependencies: WebviewDownloadDependencies): WebviewDownloadHandler {
    return (name, source, signal) => {
        const task = pendingDownload.then(async () => {
            try {
                signal.throwIfAborted()
                if (typeof name !== 'string') throw new Error('Invalid download filename')
                let blob: Blob
                if (source instanceof Blob) {
                    blob = source
                    name ||= source instanceof File ? source.name : 'download'
                } else {
                    if (typeof source !== 'string') throw new Error('Invalid download source')
                    const url = new URL(source)
                    if (!['data:', 'http:', 'https:'].includes(url.protocol)) throw new Error('Unsupported download URL')
                    const response = url.protocol === 'data:'
                        ? await fetch(source, { signal })
                        : await dependencies.fetch(source, { signal })
                    if (!response.ok) throw new Error(`Download failed: HTTP ${response.status}`)
                    blob = await response.blob()
                    if (!name) {
                        try { name = decodeURIComponent(url.pathname.split('/').pop() ?? '') } catch { /* Use a default filename. */ }
                        if (url.protocol === 'data:') name = ''
                        name ||= 'download'
                    }
                }
                const bytes = new Uint8Array(await blob.arrayBuffer())
                signal.throwIfAborted()
                return await dependencies.save(name, bytes)
            } catch (error) {
                if (signal.aborted || (error as { name?: unknown })?.name === 'AbortError') return false
                await dependencies.reportError(error)
                throw error
            }
        })
        pendingDownload = task.catch(() => {})
        return task
    }
}

// Also serialized into opaque plugin frames; keep this function self-contained.
export function installWebviewBridge(
    save: (name: string, source: Blob | string) => Promise<unknown>,
    reportError: (error: unknown) => void,
    mainDocument = false,
) {
    const objects = new Map<string, Blob>()
    const create = URL.createObjectURL.bind(URL)
    const revoke = URL.revokeObjectURL.bind(URL)
    URL.createObjectURL = object => {
        const url = create(object)
        if (object instanceof Blob) objects.set(url, object)
        return url
    }
    URL.revokeObjectURL = url => {
        objects.delete(url)
        revoke(url)
    }

    const handle = (event: MouseEvent) => {
        if (event.defaultPrevented || event.button !== 0) return
        const anchor = event.composedPath().find(node => node instanceof HTMLAnchorElement) as HTMLAnchorElement | undefined
        if (!anchor?.hasAttribute('download') || !anchor.hasAttribute('href')) return
        const href = anchor.href
        const blob = objects.get(href)
        if (!blob && !/^(?:data:|https?:)/i.test(href)) return
        event.preventDefault()
        // Capture the Blob before the plugin can immediately revoke its URL.
        void save(anchor.download, blob ?? href).catch(reportError)
    }
    const clickTarget = mainDocument ? document : window
    clickTarget.addEventListener('click', handle as EventListener)

    const click = HTMLAnchorElement.prototype.click
    HTMLAnchorElement.prototype.click = function () {
        if (this.isConnected) { click.call(this); return }
        // Detached anchors never bubble to window, but still dispatch their click handlers.
        this.addEventListener('click', handle)
        try { click.call(this) } finally { this.removeEventListener('click', handle) }
    }
    const dispatch = HTMLAnchorElement.prototype.dispatchEvent
    HTMLAnchorElement.prototype.dispatchEvent = function (event) {
        if (event.type === 'click' && event instanceof MouseEvent && !event.cancelable
            && this.hasAttribute('download') && /^(?:blob:|data:|https?:)/i.test(this.href)) {
            // FileSaver dispatches non-cancelable clicks. Suppress the engine's duplicate download.
            event = new MouseEvent('click', {
                bubbles: event.bubbles, cancelable: true, composed: event.composed,
                view: event.view, detail: event.detail,
                screenX: event.screenX, screenY: event.screenY, clientX: event.clientX, clientY: event.clientY,
                ctrlKey: event.ctrlKey, shiftKey: event.shiftKey, altKey: event.altKey, metaKey: event.metaKey,
                button: event.button, buttons: event.buttons, relatedTarget: event.relatedTarget,
            })
        }
        if (event.type !== 'click' || !(event instanceof MouseEvent) || (this.isConnected && event.bubbles)) {
            return dispatch.call(this, event)
        }
        this.addEventListener('click', handle)
        try { return dispatch.call(this, event) } finally { this.removeEventListener('click', handle) }
    }
}

export function installMainFrameLinkBridge(openExternal: (url: string) => Promise<unknown>, reportError: (error: unknown) => void) {
    document.addEventListener('click', event => {
        if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.shiftKey || event.metaKey || event.altKey) return
        const anchor = event.composedPath().find(node => node instanceof HTMLAnchorElement) as HTMLAnchorElement | undefined
        if (!anchor?.hasAttribute('href') || anchor.hasAttribute('download')) return
        if (!['', '_self', '_top', '_parent'].includes(anchor.target)) return
        let url: URL
        try { url = new URL(anchor.href) } catch { return }
        if (!['http:', 'https:', 'mailto:', 'tel:'].includes(url.protocol)) return
        const current = new URL(location.href)
        if (url.protocol === current.protocol && url.host === current.host) return
        event.preventDefault()
        void openExternal(url.href).catch(reportError)
    })
}
