// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

vi.mock('src/lang', async () => ({
    language: (await import('src/lang/en')).languageEnglish,
}))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: { plugins: [] } } }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({
    pluginStorageStore: { invalidate: vi.fn() },
}))
vi.mock('src/ts/storage/persistentDataStoreFactory', () => ({
    getPersistentDataStore: () => ({ listPluginStorage: async () => [] }),
}))
vi.mock('src/ts/storage/localDataSections', () => ({
    readLocalDataParticipation: async () => [],
}))

import PluginDataManager from './PluginDataManager.svelte'
import { UNOWNED_PLUGIN_OWNER } from 'src/ts/plugins/pluginOwner'
import type { PluginDataItem } from 'src/ts/plugins/pluginDataInventory'

const staged: PluginDataItem[] = ['pm_store', 'pm_keys', 'yt_glossary'].map(
    (key) => ({
        owner: UNOWNED_PLUGIN_OWNER,
        key,
        valueType: 'json',
        byteSize: 24,
        automatic: false,
    }),
)

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined

async function openImportStage(
    initialAssignments: { owner: string; keys: string[] }[],
    pluginNames = ['provider-manager', 'yumi-translator'],
    values = staged,
): Promise<{ owner: string; keys: string[] }[][]> {
    const published: { owner: string; keys: string[] }[][] = []
    target = document.createElement('div')
    document.body.append(target)
    component = mount(PluginDataManager, {
        target,
        props: {
            place: 'import',
            staged: values,
            pluginNames,
            initialAssignments,
            onselectionchange: (assignments) => {
                published.push(
                    assignments.map((assignment) => ({
                        owner: assignment.owner,
                        keys: assignment.items.map((item) => item.key),
                    })),
                )
            },
        },
    })
    await tick()
    return published
}

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    target?.remove()
    target = undefined
})

describe('plugin data manager import stage', () => {
    it('keeps all eight individual assignments reachable when grouping changes', async () => {
        const keys = ['pm_1', 'pm_2', 'pm_3', 'pm_4', 'pm_5', 'pm_6', 'single', 'other']
        const values = keys.map(key => ({ ...staged[0], key }))
        const published = await openImportStage([], undefined, values)
        expect(target!.querySelectorAll('[data-plugin-data-assignment]')).toHaveLength(8)
        const row = target!.querySelector('[data-plugin-data-assignment="pm_6"]')!
        row.querySelector<HTMLInputElement>('input')!.click()
        const owner = row.querySelector('select')!
        owner.value = 'yumi-translator'
        owner.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        const grouping = target!.querySelector<HTMLInputElement>('input[type="checkbox"]')!
        grouping.click()
        await tick()
        expect(target!.querySelectorAll('[data-plugin-data-group]')).toHaveLength(0)
        expect(target!.querySelectorAll('[data-plugin-data-assignment]')).toHaveLength(8)
        expect(published.at(-1)).toEqual([{ owner: 'yumi-translator', keys: ['pm_6'] }])
        grouping.click()
        await tick()
        expect(target!.querySelectorAll('[data-plugin-data-assignment]')).toHaveLength(8)
        expect(target!.querySelector<HTMLSelectElement>('[data-plugin-data-assignment="pm_6"] select')!.value).toBe('yumi-translator')
        expect(published.at(-1)).toEqual([{ owner: 'yumi-translator', keys: ['pm_6'] }])
    })

    it('opens on the answers a cancelled import kept', async () => {
        const published = await openImportStage([
            { owner: 'provider-manager', keys: ['pm_store', 'pm_keys'] },
            { owner: 'yumi-translator', keys: ['yt_glossary'] },
        ])

        expect(published.at(-1)).toEqual([
            { owner: 'provider-manager', keys: ['pm_store', 'pm_keys'] },
            { owner: 'yumi-translator', keys: ['yt_glossary'] },
        ])
        const chosen = target?.querySelector<HTMLSelectElement>(
            '[data-plugin-data-group="pm_"] select',
        )
        expect(chosen?.value).toBe('provider-manager')
    })

    it('leaves the answers out when the save no longer carries the plugin', async () => {
        const published = await openImportStage(
            [{ owner: 'uninstalled-plugin', keys: ['pm_store', 'pm_keys'] }],
            ['provider-manager'],
        )

        expect(published.at(-1)).toEqual([])
        const chosen = target?.querySelector<HTMLSelectElement>(
            '[data-plugin-data-group="pm_"] select',
        )
        expect(chosen?.value).toBe('')
    })

    it('opens on nothing when no answers were kept', async () => {
        const published = await openImportStage([])

        expect(published).toEqual([])
        const chosen = target?.querySelector<HTMLSelectElement>(
            '[data-plugin-data-group="pm_"] select',
        )
        expect(chosen?.value).toBe('')
    })
})
