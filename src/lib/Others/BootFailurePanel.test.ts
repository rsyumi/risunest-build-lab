// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const f = vi.hoisted(() => ({
    android: false,
    ios: false,
    roots: vi.fn(),
    exportOriginalData: vi.fn(),
}))
vi.mock('src/ts/platform', () => ({
    isTauri: true,
    get isTauriAndroid() { return f.android },
    get isTauriIOS() { return f.ios },
}))
vi.mock('src/ts/storage/nativePaths', () => ({ nativeRoots: f.roots }))
vi.mock('src/ts/storage/rawRecoveryExport', () => ({ exportOriginalData: f.exportOriginalData }))
vi.mock('src/ts/alert', () => ({ alertToast: vi.fn() }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))

import BootFailurePanel from './BootFailurePanel.svelte'
import { languageEnglish } from 'src/lang/en'
import type { BootFailure } from 'src/ts/stores.svelte'

const boot = languageEnglish.risuNest.boot
const schemaFailure: BootFailure = { kind: 'schema-unsupported', message: 'invalid-lww-schema', stage: 'persistent-storage' }
let mounted: ReturnType<typeof mount> | undefined

async function render(failure: BootFailure) {
    mounted = mount(BootFailurePanel, { target: document.body, props: { failure } })
    for (let index = 0; index < 12; index += 1) await tick()
}
const buttons = () => [...document.querySelectorAll('button')].map((button) => button.textContent?.trim())

afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
    f.android = false
    f.ios = false
    vi.clearAllMocks()
})

describe('BootFailurePanel', () => {
    it('explains a store from another version with the data folder this build uses and the three controls', async () => {
        f.roots.mockResolvedValue({ data: 'C:\\Users\\synthetic\\AppData\\Local\\RisuNestData' })
        await render(schemaFailure)
        const text = document.body.textContent ?? ''
        expect(text).toContain(boot.schemaUnsupported)
        expect(text).toContain(boot.dataFolder)
        expect(text).toContain('C:\\Users\\synthetic\\AppData\\Local\\RisuNestData')
        expect(text).not.toContain('%APPDATA%')
        expect(text).not.toContain(boot.dataPathAndroid)
        expect(buttons()).toEqual([languageEnglish.risuNest.recovery.exportAction, boot.restart, boot.copyDetails])
    })

    it('leaves the folder line out when the data folder cannot be resolved', async () => {
        f.roots.mockRejectedValue(new Error('roots unavailable'))
        await render(schemaFailure)
        expect(document.body.textContent).toContain(boot.schemaUnsupported)
        expect(document.body.textContent).not.toContain(boot.dataFolder)
    })

    it.each([
        ['Android', () => { f.android = true }, () => boot.dataPathAndroid],
        ['iOS', () => { f.ios = true }, () => boot.dataPathIos],
    ])('names how to clear the data on %s', async (_platform, select, hint) => {
        select()
        await render(schemaFailure)
        expect(document.body.textContent).toContain(hint())
        expect(document.body.textContent).not.toContain(boot.dataFolder)
        expect(f.roots).not.toHaveBeenCalled()
    })

    it('shows no data folder for a store that could not be opened for another reason', async () => {
        f.roots.mockResolvedValue({ data: 'C:\\Users\\synthetic\\AppData\\Local\\RisuNestData' })
        await render({ kind: 'store-open', message: 'database is locked', stage: 'persistent-database' })
        expect(document.body.textContent).toContain(boot.storeOpen)
        expect(document.body.textContent).not.toContain(boot.dataFolder)
        expect(f.roots).not.toHaveBeenCalled()
    })

    it('keeps the copy for a store from another version in both languages free of upgrade wording', async () => {
        const { languageKorean } = await import('src/lang/ko')
        expect(languageKorean.risuNest.boot.schemaUnsupported).toContain('다른 버전의 RisuNest')
        expect(languageKorean.risuNest.boot.schemaUnsupported).toContain(languageKorean.risuNest.recovery.exportAction)
        expect(languageEnglish.risuNest.boot.schemaUnsupported).toContain('a different version of RisuNest')
        expect(languageEnglish.risuNest.boot.schemaUnsupported).toContain(languageEnglish.risuNest.recovery.exportAction)
        for (const translation of [languageEnglish, languageKorean]) {
            expect(translation.risuNest.boot).not.toHaveProperty('dataPathWindows')
        }
    })
})
