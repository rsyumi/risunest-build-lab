import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { language } from 'src/lang'
import { nativeFileOperationOutcome } from 'src/ts/storage/nativeFileJobManager'
import NativeFileJobDialog from './NativeFileJobDialog.svelte'

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
