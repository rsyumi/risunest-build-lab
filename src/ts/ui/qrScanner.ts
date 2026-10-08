import {
    cancel,
    checkPermissions,
    requestPermissions,
    scan,
    Format,
} from '@tauri-apps/plugin-barcode-scanner'

export type QrScanTone = 'settings' | 'onboarding'
export type QrScanApi = {
    cancel: typeof cancel
    checkPermissions: typeof checkPermissions
    requestPermissions: typeof requestPermissions
    scan: typeof scan
}
/** Shows the scanning screen over the camera preview and returns the function that removes it. */
export type QrScanView = (cancel: () => void, tone: QrScanTone) => (() => void) | Promise<() => void>

export class QrScanError extends Error {
    constructor(readonly code: string) {
        super(code)
        this.name = 'QrScanError'
    }
}

export const isQrScanCancelled = (error: unknown) => error instanceof QrScanError && error.code === 'qr-scan-cancelled'

const SCAN_TIMEOUT_MS = 60_000
const pluginApi: QrScanApi = { cancel, checkPermissions, requestPermissions, scan }
const overlayView: QrScanView = async (cancelScan, tone) => (await import('./qrScanOverlay')).showQrScanOverlay(cancelScan, tone)

// The native plugin rejects with `{ message: 'qr-…' }`.
function scanError(cause: unknown): QrScanError {
    if (cause instanceof QrScanError) return cause
    const code = typeof cause === 'string' ? cause
        : typeof cause === 'object' && cause && typeof (cause as { message?: unknown }).message === 'string' ? (cause as { message: string }).message
            : ''
    return new QrScanError(/^qr-[a-z-]+$/.test(code) ? code : 'qr-camera-unavailable')
}

// One camera preview exists, so one scan runs at a time across every caller.
let active: object | undefined

/**
 * Scans one QR code with the camera shown behind the page. The page is replaced by the scanning
 * view only while the camera runs, and every ending (result, cancel, timeout, failure) removes it.
 */
export function createQrScanner(api: QrScanApi = pluginApi, view: QrScanView = overlayView) {
    let current: { stopped: boolean; stop(): void } | undefined
    return {
        async scan(tone: QrScanTone = 'settings'): Promise<string> {
            if (active) throw new QrScanError('qr-scan-busy')
            let reject!: (error: QrScanError) => void
            const stopped = new Promise<never>((_, fail) => { reject = fail })
            const session = {
                stopped: false,
                stop() {
                    session.stopped = true
                    reject(new QrScanError('qr-scan-cancelled'))
                },
            }
            active = session
            current = session
            let timer: ReturnType<typeof setTimeout> | undefined
            let hide: (() => void) | undefined
            const work = async () => {
                let permission: string = await api.checkPermissions()
                if (session.stopped) throw new QrScanError('qr-scan-cancelled')
                if (permission === 'denied') throw new QrScanError('qr-camera-permission-blocked')
                if (permission === 'prompt' || permission === 'prompt-with-rationale')
                    permission = await api.requestPermissions()
                if (session.stopped) throw new QrScanError('qr-scan-cancelled')
                if (permission !== 'granted') throw new QrScanError('qr-camera-permission-denied')
                const shown = await view(() => session.stop(), tone)
                if (session.stopped) {
                    shown()
                    throw new QrScanError('qr-scan-cancelled')
                }
                hide = shown
                const result = await api.scan({ formats: [Format.QRCode], windowed: true, cameraDirection: 'back' })
                if (session.stopped) throw new QrScanError('qr-scan-cancelled')
                if (result.format !== Format.QRCode) throw new QrScanError('qr-camera-unavailable')
                return result.content
            }
            try {
                const timeout = new Promise<never>((_, fail) => {
                    timer = setTimeout(() => fail(new QrScanError('qr-scan-timeout')), SCAN_TIMEOUT_MS)
                })
                return await Promise.race([work(), stopped, timeout])
            } catch (cause) {
                throw scanError(cause)
            } finally {
                session.stopped = true
                if (timer) clearTimeout(timer)
                // The page covers the camera again before the native preview goes away.
                hide?.()
                await api.cancel().catch(() => undefined)
                if (active === session) active = undefined
                if (current === session) current = undefined
            }
        },
        cancel(): void {
            if (current && !current.stopped) current.stop()
        },
    }
}
