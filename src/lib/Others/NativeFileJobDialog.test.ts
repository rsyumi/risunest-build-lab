import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { language } from 'src/lang'
import { nativeFileOperationOutcome } from 'src/ts/storage/nativeFileJobManager'
import NativeFileJobDialog from './NativeFileJobDialog.svelte'
import { syntheticNativeFileJobStatus } from 'src/ts/storage/nativeFileJobs'

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    target?.remove()
    nativeFileOperationOutcome.set(null)
    vi.restoreAllMocks()
})

describe('file operation error details', () => {
    it('offers account asset retry only while the restored library and account still match', async () => {
        target = document.createElement('div')
        document.body.append(target)
        component = mount(NativeFileJobDialog, { target })
        for (const reason of ['service', 'account-changed', 'library-changed'] as const) {
            const status = syntheticNativeFileJobStatus({ kind: 'restore-official-account-snapshot' }, 'finishing-account-restore')
            status.officialAssets = { operationId: 'synthetic-operation', phase: 'finishing', state: 'failed', reason,
                counts: { required: 1, alreadyPresent: 0, installed: 0, missing: 0, failed: 1, notAttempted: 0 } }
            nativeFileOperationOutcome.set({ kind: 'import', format: 'library-backup', state: 'failed', startedAt: 0, finishedAt: 1,
                status, observedStages: ['downloading-account-assets'], warningCodes: [], partialWritesPossible: true })
            await tick()
            const retry = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === language.retry)
            expect(!!retry).toBe(reason === 'service')
        }
    })
    it.each([true, false])('shows and copies diagnostics with raw error available: %s', async rawError => {
        const writeText = vi.fn(async () => {})
        Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
        nativeFileOperationOutcome.set({
            kind: 'import', format: 'content', state: 'failed', startedAt: 0, finishedAt: 1,
            observedStages: [], warningCodes: [], partialWritesPossible: false,
            error: rawError ? { code: 'source-read-failed', message: 'synthetic file failure', recoveryRequired: false } : undefined,
        })
        target = document.createElement('div')
        document.body.append(target)
        component = mount(NativeFileJobDialog, { target })
        await tick()
        const show = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === language.risuNest.importDialog.errorDetails)
        expect(show).toBeDefined()
        show!.click()
        await tick()
        const report = target.querySelector('pre')!.textContent!
        expect(report).toContain('RisuNest Version:')
        expect(report).toContain('\nDevice:')
        expect(report).toContain('\nUser-Agent:')
        expect(report.split('\n\n')[1]).toBeTruthy()
        if (rawError) expect(report).toContain('[source-read-failed] synthetic file failure')
        target.querySelector<HTMLButtonElement>(`button[aria-label="${language.copy}"]`)!.click()
        await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith(report))
    })
})
