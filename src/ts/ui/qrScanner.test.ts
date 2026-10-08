import { afterEach, describe, expect, it, vi } from 'vitest'
import { Format } from '@tauri-apps/plugin-barcode-scanner'
import { createQrScanner, isQrScanCancelled, QrScanError, type QrScanApi, type QrScanTone, type QrScanView } from './qrScanner'

function harness() {
    const api = {
        cancel: vi.fn(async () => {}),
        checkPermissions: vi.fn(async (): Promise<string> => 'granted'),
        requestPermissions: vi.fn(async (): Promise<string> => 'granted'),
        scan: vi.fn(async (): Promise<{ format: Format; content: string; bounds: unknown }> => ({ format: Format.QRCode, content: 'synthetic-content', bounds: null })),
    }
    const hide = vi.fn()
    let cancelFromView: (() => void) | undefined
    const view = vi.fn((cancel: () => void, _tone: QrScanTone) => { cancelFromView = cancel; return hide })
    const scanner = createQrScanner(api as unknown as QrScanApi, view as QrScanView)
    return { api, hide, view, scanner, cancelFromView: () => cancelFromView!() }
}
/** Never resolves, like a camera that has not seen a code yet. */
const waiting = () => new Promise<never>(() => {})
const settle = async () => { for (let i = 0; i < 6; i++) await Promise.resolve() }
const order = (mock: { mock: { invocationCallOrder: number[] } }) => mock.mock.invocationCallOrder[0]

afterEach(() => { vi.useRealTimers() })

describe('QR scan session', () => {
    it('shows the scanning view only after permission, right before the camera starts', async () => {
        const h = harness()
        let grant!: (value: string) => void
        h.api.checkPermissions.mockReturnValue(new Promise(resolve => { grant = resolve }))
        h.api.scan.mockImplementation(waiting)
        const pending = h.scanner.scan('onboarding')
        await settle()
        expect(h.view).not.toHaveBeenCalled()
        grant('granted')
        await settle()
        expect(h.view).toHaveBeenCalledOnce()
        expect(h.view.mock.calls[0][1]).toBe('onboarding')
        expect(order(h.view)).toBeLessThan(order(h.api.scan))
        expect(h.api.scan).toHaveBeenCalledWith({ formats: [Format.QRCode], windowed: true, cameraDirection: 'back' })
        expect(h.hide).not.toHaveBeenCalled()
        h.scanner.cancel()
        await expect(pending).rejects.toThrow('qr-scan-cancelled')
    })

    it('returns the content, then removes the view before closing the camera', async () => {
        const h = harness()
        expect(await h.scanner.scan()).toBe('synthetic-content')
        expect(h.view.mock.calls[0][1]).toBe('settings')
        expect(h.hide).toHaveBeenCalledOnce()
        expect(h.api.cancel).toHaveBeenCalledOnce()
        expect(order(h.hide)).toBeLessThan(order(h.api.cancel))
    })

    it.each([
        ['the view cancel button', (h: ReturnType<typeof harness>) => h.cancelFromView()],
        ['the caller', (h: ReturnType<typeof harness>) => h.scanner.cancel()],
    ])('ends on a cancel from %s and restores the page', async (_, cancel) => {
        const h = harness()
        h.api.scan.mockImplementation(waiting)
        const pending = h.scanner.scan()
        await settle()
        cancel(h)
        const error = await pending.catch((reason: unknown) => reason)
        expect(isQrScanCancelled(error)).toBe(true)
        expect(h.hide).toHaveBeenCalledOnce()
        expect(h.api.cancel).toHaveBeenCalledOnce()
    })

    it('ends on a timeout and restores the page', async () => {
        vi.useFakeTimers()
        const h = harness()
        h.api.scan.mockImplementation(waiting)
        const pending = h.scanner.scan()
        const result = expect(pending).rejects.toThrow('qr-scan-timeout')
        await vi.advanceTimersByTimeAsync(60_000)
        await result
        expect(h.hide).toHaveBeenCalledOnce()
        expect(h.api.cancel).toHaveBeenCalledOnce()
    })

    it.each([
        // Android Back outside the overlay, or the app moving to the background, ends the native scan.
        [{ message: 'qr-scan-cancelled' }, 'qr-scan-cancelled'],
        [{ message: 'qr-camera-unavailable' }, 'qr-camera-unavailable'],
        [new Error('bridge failure'), 'qr-camera-unavailable'],
    ])('ends on a native rejection %o as %s and restores the page', async (rejection, code) => {
        const h = harness()
        h.api.scan.mockRejectedValue(rejection)
        const error = await h.scanner.scan().catch((reason: unknown) => reason)
        expect(error).toBeInstanceOf(QrScanError)
        expect((error as QrScanError).code).toBe(code)
        expect(h.hide).toHaveBeenCalledOnce()
        expect(h.api.cancel).toHaveBeenCalledOnce()
    })

    it.each([
        ['denied', 'denied', 'qr-camera-permission-blocked'],
        ['prompt', 'denied', 'qr-camera-permission-denied'],
        ['prompt-with-rationale', 'denied', 'qr-camera-permission-denied'],
    ])('never shows the view or starts the camera when permission is %s then %s', async (checked, requested, code) => {
        const h = harness()
        h.api.checkPermissions.mockResolvedValue(checked)
        h.api.requestPermissions.mockResolvedValue(requested)
        await expect(h.scanner.scan()).rejects.toThrow(code)
        expect(h.api.requestPermissions).toHaveBeenCalledTimes(checked === 'denied' ? 0 : 1)
        expect(h.view).not.toHaveBeenCalled()
        expect(h.api.scan).not.toHaveBeenCalled()
    })

    it('asks again after a rationale prompt and scans once granted', async () => {
        const h = harness()
        h.api.checkPermissions.mockResolvedValue('prompt-with-rationale')
        expect(await h.scanner.scan()).toBe('synthetic-content')
        expect(h.api.requestPermissions).toHaveBeenCalledOnce()
    })

    it('cancelled during the permission request, it never opens the view or the camera', async () => {
        const h = harness()
        let grant!: (value: string) => void
        h.api.checkPermissions.mockReturnValue(new Promise(resolve => { grant = resolve }))
        const pending = h.scanner.scan()
        h.scanner.cancel()
        await expect(pending).rejects.toThrow('qr-scan-cancelled')
        grant('granted')
        await settle()
        expect(h.view).not.toHaveBeenCalled()
        expect(h.api.scan).not.toHaveBeenCalled()
    })

    it('removes a view that finished loading after the scan was cancelled', async () => {
        const h = harness()
        let loaded!: (hide: () => void) => void
        h.view.mockImplementation(((): Promise<() => void> => new Promise(resolve => { loaded = resolve })) as never)
        const pending = h.scanner.scan()
        await settle()
        h.scanner.cancel()
        await expect(pending).rejects.toThrow('qr-scan-cancelled')
        loaded(h.hide)
        await settle()
        expect(h.hide).toHaveBeenCalledOnce()
        expect(h.api.scan).not.toHaveBeenCalled()
    })

    it('runs one scan at a time across scanners, and a cancel only ends its own scan', async () => {
        const first = harness()
        const second = harness()
        first.api.scan.mockImplementation(waiting)
        const pending = first.scanner.scan()
        await settle()
        await expect(second.scanner.scan()).rejects.toThrow('qr-scan-busy')
        second.scanner.cancel()
        await settle()
        expect(second.view).not.toHaveBeenCalled()
        expect(second.api.scan).not.toHaveBeenCalled()
        expect(first.hide).not.toHaveBeenCalled()
        first.scanner.cancel()
        await expect(pending).rejects.toThrow('qr-scan-cancelled')
        expect(await second.scanner.scan()).toBe('synthetic-content')
    })

    it('reports a non-QR result as unreadable', async () => {
        const h = harness()
        h.api.scan.mockResolvedValue({ format: Format.EAN13, content: '0000000000000', bounds: null })
        await expect(h.scanner.scan()).rejects.toThrow('qr-camera-unavailable')
        expect(h.hide).toHaveBeenCalledOnce()
    })
})
