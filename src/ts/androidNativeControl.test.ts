import { afterEach, describe, expect, it, vi } from 'vitest'
import { createAndroidControlClient, type AndroidControlMessagePort } from './androidNativeControl'
import progressGolden from './storage/tests/fixtures/androidBackupCopyProgressV1Golden.json'

function fixture() {
    const sent: { id?: string; method: string; args: string[] }[] = []
    const port: AndroidControlMessagePort = {
        postMessage: (text) => { sent.push(JSON.parse(text)) },
    }
    const client = createAndroidControlClient(port)
    const reply = (value: unknown) => port.onmessage?.({ data: JSON.stringify(value) })
    return { sent, port, client, reply }
}

afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
    delete window.RisuNestControl
    delete window.RisuNestSafControl
    delete window.RisuLifecycleBridge
    delete window.RisuGenerationKeepAlive
    delete window.RisuCompletionNotifications
    delete window.RisuSafBridge
    delete window.RisuNativeFileSelection
})

describe('scoped Android control transport', () => {
    it.each([false, true])('announces the real app document after installing controls (SAF %s)', async (safEnabled) => {
        vi.resetModules()
        const ready = vi.fn((text: string) => {
            expect(window.RisuLifecycleBridge?.onFlushComplete).toBeTypeOf('function')
            expect(window.RisuGenerationKeepAlive?.begin).toBeTypeOf('function')
            expect(Boolean(window.RisuSafBridge)).toBe(safEnabled)
            expect(JSON.parse(text)).toEqual({ method: 'lifecycle.onFrontendReady', args: [] })
        })
        window.RisuNestControl = { postMessage: ready }
        if (safEnabled) window.RisuNestSafControl = { postMessage: vi.fn() }
        await import('./androidNativeControl')
        expect(ready).toHaveBeenCalledOnce()
    })

    it('bounds completion notification content before crossing the native transport', async () => {
        vi.resetModules()
        const sent: { id?: string; method: string; args: string[] }[] = []
        const port: AndroidControlMessagePort = { postMessage: text => sent.push(JSON.parse(text)) }
        window.RisuNestControl = port
        await import('./androidNativeControl')
        const delivered = window.RisuCompletionNotifications!.notify('a'.repeat(100_000))
        const message = sent.find(item => item.method === 'notification.completion')!
        expect(message.args).toEqual(['a'.repeat(4096)])
        port.onmessage?.({ data: JSON.stringify({ id: message.id, result: true }) })
        await expect(delivered).resolves.toBe(true)
    })

    it('matches delayed responses without treating promises as successful booleans', async () => {
        const { client, sent, reply } = fixture()
        const first = client.request<boolean>('saf.acknowledgeExport', 'first')
        const second = client.request<boolean>('saf.acknowledgeExport', 'second')
        expect(sent.map(({ method, args }) => ({ method, args }))).toEqual([
            { method: 'saf.acknowledgeExport', args: ['first'] },
            { method: 'saf.acknowledgeExport', args: ['second'] },
        ])
        reply({ id: sent[1].id, result: false })
        reply({ id: sent[0].id, result: true })
        expect(await first).toBe(true)
        expect(await second).toBe(false)
    })

    it('keeps exit holds synchronous to send and independent of pending requests', () => {
        const { client, sent } = fixture()
        client.notify('lifecycle.onFlushHold', 'exit-1')
        expect(sent).toEqual([{ method: 'lifecycle.onFlushHold', args: ['exit-1'] }])
    })

    it('rejects native errors and ignores unrelated or malformed replies', async () => {
        const { client, sent, reply, port } = fixture()
        const pending = client.request<string>('saf.getExportStatus')
        const rejected = expect(pending).rejects.toThrow('android-control-failed')
        port.onmessage?.({ data: '{' })
        reply(null)
        reply({ id: 'unrelated', result: 'ignored' })
        reply({ id: sent[0].id, error: 'android-control-failed' })
        await rejected
    })

    it('bounds pending requests and never retries an uncertain native action', async () => {
        vi.useFakeTimers()
        const { client, sent } = fixture()
        const pending = Array.from({ length: 32 }, () =>
            client.request<boolean>('saf.acknowledgeExport', 'receipt').catch((error: Error) => error.message),
        )
        await expect(client.request('saf.getExportStatus')).rejects.toThrow('android-control-busy')
        await vi.advanceTimersByTimeAsync(15_000)
        expect(await Promise.all(pending)).toEqual(Array(32).fill('android-control-timeout'))
        expect(sent).toHaveLength(32)
        expect(vi.getTimerCount()).toBe(0)
    })

    it('keeps the file picker request alive while the user chooses a document', async () => {
        vi.useFakeTimers()
        const { client, sent, reply } = fixture()
        const selected = client.request('files.pick', 'true')
        await vi.advanceTimersByTimeAsync(60_000)
        expect(sent).toHaveLength(1)
        reply({ id: sent[0].id, result: { cancelled: true } })
        await expect(selected).resolves.toEqual({ cancelled: true })
        expect(vi.getTimerCount()).toBe(0)
    })

    it('waits for a backup copy reply however long it stalls, as on iOS', async () => {
        vi.useFakeTimers()
        const { client, sent, reply } = fixture()
        const onProgress = vi.fn()
        let settled = false
        const outcome = client.requestWithProgress<string>('saf.materializeBackupSource', onProgress, 'source')
            .finally(() => { settled = true })
        expect(vi.getTimerCount()).toBe(0)
        await vi.advanceTimersByTimeAsync(10 * 60_000)
        reply({ id: sent[0].id, progress: { copiedBytes: 200, totalBytes: 200 } })
        // Verifying and publishing the finished copy reports nothing more.
        await vi.advanceTimersByTimeAsync(10 * 60_000)
        expect(settled).toBe(false)
        reply({ id: sent[0].id, result: 'copied-source' })
        await expect(outcome).resolves.toBe('copied-source')
        expect(onProgress.mock.calls).toEqual([[{ copiedBytes: 200, totalBytes: 200 }]])
    })

    it('reports backup copy progress only when copied bytes advance within the total', () => {
        const { client, sent, reply } = fixture()
        const onProgress = vi.fn()
        void client.requestWithProgress('saf.materializeBackupSource', onProgress, 'source')
        for (const progress of [
            { copiedBytes: 100, totalBytes: 200 },
            { copiedBytes: 100, totalBytes: 200 },
            { copiedBytes: 50, totalBytes: 200 },
            { copiedBytes: 201, totalBytes: 200 },
            { copiedBytes: '150', totalBytes: 200 },
            { copiedBytes: 150, totalBytes: '200' },
            null,
        ]) reply({ id: sent[0].id, progress })
        reply({ id: 'another-request', progress: { copiedBytes: 150, totalBytes: 200 } })
        reply({ id: sent[0].id, progress: { copiedBytes: 150, totalBytes: null } })
        expect(onProgress.mock.calls).toEqual([
            [{ copiedBytes: 100, totalBytes: 200 }],
            [{ copiedBytes: 150, totalBytes: null }],
        ])
    })

    it('routes the installed file selection bridge to the native file commands', async () => {
        vi.resetModules()
        const sent: { id?: string; method: string; args: string[] }[] = []
        const port: AndroidControlMessagePort = { postMessage: text => sent.push(JSON.parse(text)) }
        window.RisuNestControl = port
        await import('./androidNativeControl')
        const selected = window.RisuNativeFileSelection!.pick(true)
        const message = sent.find(item => item.method === 'files.pick')!
        expect(message.args).toEqual(['true'])
        port.onmessage?.({ data: JSON.stringify({ id: message.id, result: { cancelled: true } }) })
        await expect(selected).resolves.toEqual({ cancelled: true })
        const discarded = window.RisuNativeFileSelection!.discard('/owned/source')
        const cleanup = sent.find(item => item.method === 'files.discard')!
        expect(cleanup.args).toEqual(['/owned/source'])
        port.onmessage?.({ data: JSON.stringify({ id: cleanup.id, result: null }) })
        await discarded
    })

    it('delivers the native copy progress wire format through the installed SAF bridge', async () => {
        vi.resetModules()
        // AndroidControlBridgeTest checks that native writes these exact messages.
        vi.spyOn(crypto, 'randomUUID').mockReturnValue(progressGolden.id as ReturnType<Crypto['randomUUID']>)
        const sent: { id?: string; method: string; args: string[] }[] = []
        const port: AndroidControlMessagePort = { postMessage: text => sent.push(JSON.parse(text)) }
        window.RisuNestControl = { postMessage() {} }
        window.RisuNestSafControl = port
        await import('./androidNativeControl')
        const onProgress = vi.fn()
        const copied = window.RisuSafBridge!.materializeBackupSource!('source', onProgress)
        expect(sent.find(item => item.id === progressGolden.id)).toEqual({
            id: progressGolden.id, method: progressGolden.method, args: ['source'],
        })
        for (const { wire } of progressGolden.messages) port.onmessage?.({ data: wire })
        expect(onProgress.mock.calls).toEqual(progressGolden.messages.map(({ copiedBytes, totalBytes }) => [
            { copiedBytes, totalBytes },
        ]))
        port.onmessage?.({ data: JSON.stringify({ id: progressGolden.id, result: 'copy-result' }) })
        await expect(copied).resolves.toBe('copy-result')
    })

    it('releases pending state when sending fails', async () => {
        vi.useFakeTimers()
        const port: AndroidControlMessagePort = { postMessage: () => { throw new Error('closed') } }
        const client = createAndroidControlClient(port)
        await expect(client.request('generation.begin')).rejects.toThrow('closed')
        expect(vi.getTimerCount()).toBe(0)
    })
})
