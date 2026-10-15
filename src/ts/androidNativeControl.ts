import type { AndroidGenerationKeepAliveBridge } from './androidGenerationKeepAlive'
import type { AndroidBackupCopyProgress, AndroidSafJavascriptBridge } from './storage/androidSafBridge'
import type { NativeFileSelectionBridge } from './storage/nativeFileSelection'

export interface AndroidControlMessagePort {
    postMessage(message: string): void
    onmessage?: ((event: { data: string }) => void) | null
}

interface AndroidLifecycleControl {
    onFlushComplete?(token: string): void
    onFlushHold?(token: string): void
    exitListenerReady?(ready: boolean): void
    acknowledgeOpenedFile?(path: string): void
    requestExit?(): void
    requestRestart?(): void
}

declare global {
    interface Window {
        RisuNestControl?: AndroidControlMessagePort
        RisuNestSafControl?: AndroidControlMessagePort
        RisuLifecycleBridge?: AndroidLifecycleControl
        RisuSafBridge?: AndroidSafJavascriptBridge
        RisuNativeFileSelection?: NativeFileSelectionBridge
        RisuCompletionNotifications?: { enabled(): Promise<boolean>; notify(body: string): Promise<boolean> }
    }
}

export function createAndroidControlClient(port: AndroidControlMessagePort) {
    const pending = new Map<string, {
        resolve(value: unknown): void
        reject(error: Error): void
        timer: ReturnType<typeof setTimeout> | undefined
        progress?(value: unknown): void
    }>()
    function request<T>(method: string, args: string[], onProgress?: (progress: AndroidBackupCopyProgress) => void): Promise<T> {
        if (pending.size >= 32) return Promise.reject(new Error('android-control-busy'))
        const id = crypto.randomUUID()
        const copying = method === 'saf.materializeBackupSource'
        return new Promise<T>((resolve, reject) => {
            const entry = {
                resolve: (value: unknown) => resolve(value as T),
                reject,
                timer: undefined as ReturnType<typeof setTimeout> | undefined,
                progress: undefined as ((value: unknown) => void) | undefined,
            }
            if (copying) {
                let copiedBytes = 0
                entry.progress = value => {
                    if (!value || typeof value !== 'object') return
                    const progress = value as Partial<AndroidBackupCopyProgress>
                    if (typeof progress.copiedBytes !== 'number' || !Number.isSafeInteger(progress.copiedBytes)
                        || progress.copiedBytes <= copiedBytes) return
                    if (progress.totalBytes !== null && (typeof progress.totalBytes !== 'number'
                        || !Number.isSafeInteger(progress.totalBytes) || progress.totalBytes < progress.copiedBytes)) return
                    copiedBytes = progress.copiedBytes
                    onProgress?.({ copiedBytes, totalBytes: progress.totalBytes })
                }
            }
            // A picker waits for the user. A backup copy ends only when native replies, as on iOS, so its
            // copied file is never left without a page to remove it; cancelling the copy stops it.
            if (method !== 'files.pick' && !copying) {
                entry.timer = setTimeout(() => {
                    pending.delete(id)
                    reject(new Error('android-control-timeout'))
                }, 15_000)
            }
            pending.set(id, entry)
            try {
                port.postMessage(JSON.stringify({ id, method, args }))
            } catch (error) {
                pending.delete(id)
                clearTimeout(entry.timer)
                reject(error)
            }
        })
    }
    port.onmessage = (event) => {
        let response: { id?: unknown; result?: unknown; error?: unknown; progress?: unknown }
        try {
            response = JSON.parse(event.data)
        } catch {
            return
        }
        if (!response || typeof response.id !== 'string') return
        const request = pending.get(response.id)
        if (!request) return
        if ('progress' in response) {
            request.progress?.(response.progress)
            return
        }
        pending.delete(response.id)
        clearTimeout(request.timer)
        if (typeof response.error === 'string') request.reject(new Error(response.error))
        else request.resolve(response.result)
    }
    return {
        notify(method: string, ...args: string[]): void {
            port.postMessage(JSON.stringify({ method, args }))
        },
        request<T>(method: string, ...args: string[]): Promise<T> {
            return request<T>(method, args)
        },
        requestWithProgress<T>(method: 'saf.materializeBackupSource', onProgress: ((progress: AndroidBackupCopyProgress) => void) | undefined, ...args: string[]): Promise<T> {
            return request<T>(method, args, onProgress)
        },
    }
}

function installAndroidNativeControl(): void {
    if (typeof window === 'undefined' || !window.RisuNestControl) return
    const control = createAndroidControlClient(window.RisuNestControl)
    window.RisuNativeFileSelection = {
        pick: multiple => control.request('files.pick', String(multiple)),
        discard: path => control.request('files.discard', path),
    }
    window.RisuLifecycleBridge = {
        onFlushComplete: (token) => control.notify('lifecycle.onFlushComplete', token),
        onFlushHold: (token) => control.notify('lifecycle.onFlushHold', token),
        exitListenerReady: (ready) => control.notify('lifecycle.exitListenerReady', String(ready)),
        acknowledgeOpenedFile: (path) => control.notify('lifecycle.acknowledgeOpenedFile', path),
        requestExit: () => control.notify('lifecycle.requestExit'),
        requestRestart: () => control.notify('lifecycle.requestRestart'),
    }
    const generation: AndroidGenerationKeepAliveBridge = {
        begin: () => control.request<boolean>('generation.begin'),
        end: () => control.request<boolean>('generation.end'),
        notificationsEnabled: () => control.request<boolean>('generation.notificationsEnabled'),
        requestNotifications: () => control.request<void>('generation.requestNotifications'),
        openNotificationSettings: () => control.request<boolean>('generation.openNotificationSettings'),
        webViewVersion: () => control.request<string>('generation.webViewVersion'),
    }
    window.RisuGenerationKeepAlive = generation
    window.RisuCompletionNotifications = {
        enabled: () => control.request<boolean>('notification.enabled'),
        notify: body => control.request<boolean>('notification.completion', body.slice(0, 4096)),
    }
    window.RisuBackgroundTasks = {
        begin: kind => control.request<string | null>('background.begin', kind),
        progress: (id, percent) => control.request('background.progress', id, String(percent)),
        end: id => control.request('background.end', id),
    }
    if (!window.RisuNestSafControl) {
        control.notify('lifecycle.onFrontendReady')
        return
    }
    const saf = createAndroidControlClient(window.RisuNestSafControl)
    window.RisuSafBridge = {
        copyExport: (...args) => saf.notify('saf.copyExport', ...args),
        cancelExport: (id) => saf.request<boolean>('saf.cancelExport', id),
        cancelSource: (id) => saf.notify('saf.cancelSource', id),
        pickBackupSource: (id) => saf.notify('saf.pickBackupSource', id),
        pickPortableBackupSource: (id) => saf.notify('saf.pickPortableBackupSource', id),
        discardPortableSource: (token) => saf.request<boolean>('saf.discardPortableSource', token),
        materializeBackupSource: (token, onProgress) => saf.requestWithProgress<string>('saf.materializeBackupSource', onProgress, token),
        pickContentSource: (id, destination) => saf.notify('saf.pickContentSource', id, destination),
        pickLegacyBackupSource: (id) => saf.notify('saf.pickLegacyBackupSource', id),
        discardSource: (token) => saf.request<boolean>('saf.discardSource', token),
        getActiveSourceRequestIds: () => saf.request<string>('saf.getActiveSourceRequestIds'),
        getExportStatus: () => saf.request<string | null>('saf.getExportStatus'),
        getExportSourceId: () => saf.request<string | null>('saf.getExportSourceId'),
        markExportPublicationReady: (id) => saf.request<boolean>('saf.markExportPublicationReady', id),
        acknowledgeExport: (id) => saf.request<boolean>('saf.acknowledgeExport', id),
    }
    control.notify('lifecycle.onFrontendReady')
}

installAndroidNativeControl()
