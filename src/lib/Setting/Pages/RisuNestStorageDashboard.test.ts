// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const maintenance = vi.hoisted(() => ({
    getNativePersistentStorageStats: vi.fn(),
    listNativePersistentSnapshots: vi.fn(),
    previewNativePersistentAssetGc: vi.fn(),
    executeNativePersistentAssetGc: vi.fn(),
    deleteNativePersistentSnapshot: vi.fn(),
    createNativePersistentSnapshot: vi.fn(),
    restoreNativePersistentSnapshot: vi.fn(),
}))
const server = vi.hoisted(() => ({
    getServerSyncBackupInventory: vi.fn(),
    getServerSyncCacheUsage: vi.fn(),
    cleanupServerSyncCache: vi.fn(),
    deleteServerSyncBackup: vi.fn(),
    exportServerSyncBackup: vi.fn(),
    restoreServerSyncBackup: vi.fn(),
}))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => server)
const backups = vi.hoisted(() => ({ list: vi.fn(), remove: vi.fn() }))
const alerts = vi.hoisted(() => ({
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertNormal: vi.fn(),
}))

vi.mock('src/ts/storage/nativePersistentMaintenance', () => maintenance)
vi.mock('src/ts/storage/sync/syncConflictBackup', () => ({ getSyncConflictBackupStore: () => backups }))
vi.mock('src/ts/alert', () => alerts)
vi.mock('src/ts/platform', () => ({ isTauri: true }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))

import RisuNestStorageDashboard from './RisuNestStorageDashboard.svelte'
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'

const syncText = languageEnglish.risuNest.serverSync
const stats = {
    snapshotBytes: 2 * 1024 * 1024,
    databaseBytes: 1024 * 1024,
    assetObjects: { count: 2, bytes: 2 * 1024 * 1024 }, assetAliases: [{ kind: 'inlay', inlayType: null, count: 1, bytes: 1024 * 1024 }], pluginStorage: { count: 1, bytes: 1024 },
    characters: { active: { count: 2, bytes: 0 }, trashedCount: 1 }, conversations: { count: 3, messageCount: 4 }, assetObjectDeletions: [],
}

describe('RisuNestStorageDashboard', () => {
    let mounted: ReturnType<typeof mount> | undefined

    afterEach(async () => {
        if (mounted) await unmount(mounted)
        mounted = undefined
        document.body.replaceChildren()
        vi.clearAllMocks()
    })

    const button = (target: HTMLElement, text: string) =>
        [...target.querySelectorAll<HTMLButtonElement>('button')].find(
            (candidate) => candidate.textContent?.trim().startsWith(text),
        )
    const exact = (target: HTMLElement, text: string) =>
        [...target.querySelectorAll<HTMLButtonElement>('button')].find(
            (candidate) => candidate.textContent?.trim() === text,
        )

    it.each(['temp'] as const)('keeps snapshot restore usable when the first %s inventory fails', async source => {
        server.getServerSyncCacheUsage.mockRejectedValueOnce(new Error('cache'))
        const target = setup()
        await vi.waitFor(() => expect(target.querySelector('[data-storage-backup-list="snapshots"]')).not.toBeNull())
        expect(target.querySelector('[data-storage-summary]')).toBeNull()
        expect(exact(target, languageEnglish.risuNest.storage.restoreSnapshot)?.disabled).toBe(false)
        expect(target.textContent).toContain(languageEnglish.risuNest.storage.loadFailed)
    })

    it('localizes snapshot reasons without exposing unknown tokens', async () => {
        maintenance.listNativePersistentSnapshots.mockResolvedValueOnce(['manual', 'periodic', 'pre-restore', 'data-health-repair', 'synthetic-unknown'].map((reason, index) => ({ id: `snapshot-${index}`, reason, bytes: 1, reclaimableBytes: 0, modifiedAt: 1 })))
        const target = setup()
        await vi.waitFor(() => expect(target.querySelectorAll('[data-storage-backup-list="snapshots"] [data-storage-backup-row]')).toHaveLength(5))
        expect(target.textContent).toContain(languageEnglish.risuNest.storage.snapshotReasons.dataHealthRepair)
        expect(target.textContent).not.toContain('synthetic-unknown')
        expect(target.textContent).not.toContain('data-health-repair')
    })

    function setup(
        statsPromise: Promise<typeof stats> = Promise.resolve(stats),
    ): HTMLElement {
        maintenance.getNativePersistentStorageStats.mockImplementation(
            () => statsPromise,
        )
        maintenance.listNativePersistentSnapshots.mockResolvedValue([
            {
                id: 'snapshot.db',
                reason: 'manual',
                reclaimableBytes: 0,
                bytes: 1024,
                modifiedAt: 1,
            },
        ])
        server.getServerSyncCacheUsage.mockResolvedValue({
            totalBytes: 1024 + 2048,
            cacheBytes: 1024,
            protectedBytes: 512,
            reclaimableBytes: 512,
            ledgerBytes: 2048,
            databaseBytes: 0,
            blockedReason: null,
        })
        backups.list.mockResolvedValue([
            {
                id: 'conflict',
                createdAt: 3,
                side: 'local',
                characterCount: 2,
                byteLength: 4096,
                scope: 'database-only',
            },
        ])
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(RisuNestStorageDashboard, { target })
        return target
    }

    it('renders the total with its seven-part breakdown, count summary, and backup rows with complete server storage totals', async () => {
        const target = setup()
        await vi.waitFor(() =>
            expect(target.textContent).toContain('Total data'),
        )

        const legend = [...target.querySelectorAll<HTMLElement>('[data-storage-legend] > li')]
        expect(legend).toHaveLength(6)
        const text = (label: string) =>
            legend.find((item) => item.textContent?.includes(label))?.textContent?.replace(/\s+/g, ' ').trim()
        expect(text(syncText.management.cache)).toBe('Temporary files 1.0 KiB')
        expect(text(syncText.management.ledger)).toBe('Asset storage records 2.0 KiB')
        expect(target.textContent).toContain('5.0 MiB')
        expect(target.textContent).toContain('Database')
        expect(target.textContent).toContain(
            'Chat attachments 1.0 MiB (included in images & media) · Plugin data 1.0 KiB',
        )
        expect(target.textContent).toContain(
            '2 characters · 3 chats · 4 messages',
        )
        expect(target.textContent).toContain('(1 in trash)')
        expect(target.textContent).toContain(new Date(1).toLocaleString())
        expect(target.textContent).not.toContain('snapshot.db')
        expect(
            target.querySelector('[role="status"][aria-live="polite"]'),
        ).not.toBeNull()
    })

    it('places the loading placeholder where the total, bar and legend appear', async () => {
        let resolveStats: ((value: typeof stats) => void) | undefined
        const target = setup(new Promise((resolve) => { resolveStats = resolve }))

        await vi.waitFor(() => expect(target.querySelector('[data-storage-summary-placeholder]')).not.toBeNull())
        const placeholder = target.querySelector<HTMLElement>('[data-storage-summary-placeholder]')!
        const blocks = [...placeholder.querySelectorAll<HTMLElement>('.animate-pulse')]
        expect(blocks.length).toBeGreaterThan(0)
        expect(blocks.every((block) => block.classList.contains('bg-darkbutton'))).toBe(true)
        expect(placeholder.innerHTML).not.toContain('sm:')

        resolveStats?.(stats)
        await vi.waitFor(() => expect(target.textContent).toContain('Total data'))
        expect(target.querySelector('[data-storage-summary-placeholder]')).toBeNull()
    })

    it('summarizes each backup list with its count and size and keeps delete beside the row text', async () => {
        const target = setup()
        await vi.waitFor(() => expect(target.textContent).toContain('Snapshots'))

        const summaries = [...target.querySelectorAll<HTMLElement>('[data-storage-backup-list] > summary')]
        expect(summaries.map((summary) => summary.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
            'Snapshots 1 items · 2.0 MiB',
            'Conflict backups 1 items · 4.0 KiB',
            'Temporary files 1.0 KiB',
        ])
        const row = target.querySelector<HTMLElement>('[data-storage-backup-list] [data-storage-backup-row]')
        expect(row?.className).not.toContain('justify-between')
        expect(row?.textContent).toContain('1.0 KiB')
        expect(row?.textContent).toContain('Created manually')
        expect(target.textContent).toContain('The total counts shared storage once.')
        expect(target.textContent).toContain('2 characters · 3 chats · 4 messages')
    })

    it('previews and confirms unused-image GC independently of server cache', async () => {
        const target = setup()
        maintenance.previewNativePersistentAssetGc.mockResolvedValue({
            candidateCount: 2,
            candidateBytes: 2048,
            deletedCount: 0,
            deletedBytes: 0,
            blockers: [],
        })
        maintenance.executeNativePersistentAssetGc.mockResolvedValue({
            candidateCount: 2,
            candidateBytes: 2048,
            deletedCount: 2,
            deletedBytes: 2048,
            blockers: [],
        })
        alerts.alertConfirm.mockResolvedValue(true)
        const button = (text: string) =>
            [...target.querySelectorAll<HTMLButtonElement>('button')].find(
                (candidate) => candidate.textContent?.trim() === text,
            )
        await vi.waitFor(() => expect(button('Find')).toBeDefined())
        button('Find')!.click()
        await vi.waitFor(() =>
            expect(target.textContent).toContain(
                'Removable: 2 items (2.0 KiB)',
            ),
        )
        button('Delete now')!.click()
        await vi.waitFor(() =>
            expect(
                maintenance.executeNativePersistentAssetGc,
            ).toHaveBeenCalledOnce(),
        )
        expect(alerts.alertConfirm).toHaveBeenCalledWith(
            'This will delete 2 unused files (2.0 KiB). Continue?',
        )
        expect(server.cleanupServerSyncCache).not.toHaveBeenCalled()
    })
    it('lists what the cleanup found and why each file stayed', async () => {
        const target = setup()
        maintenance.previewNativePersistentAssetGc.mockResolvedValue({
            candidateCount: 1,
            candidateBytes: 1024,
            deletedCount: 0,
            deletedBytes: 0,
            blockers: [],
            candidates: [
                { objectHash: 'a'.repeat(64), bytes: 1024, createdAtMs: 0, state: 'deletable', holders: [] },
                { objectHash: 'b'.repeat(64), bytes: 2048, createdAtMs: 0, state: 'held', holders: ['repair'] },
                { objectHash: 'c'.repeat(64), bytes: 4096, createdAtMs: 0, state: 'held', holders: [] },
                { objectHash: 'd'.repeat(64), bytes: 512, createdAtMs: 0, state: 'recent', holders: [] },
            ],
            omitted: 7,
        })
        const find = () =>
            [...target.querySelectorAll<HTMLButtonElement>('button')].find(
                (candidate) => candidate.textContent?.trim() === 'Find',
            )
        await vi.waitFor(() => expect(find()).toBeDefined())
        find()!.click()
        await vi.waitFor(() =>
            expect(target.querySelectorAll('[data-storage-gc-row]')).toHaveLength(4),
        )
        // The files to delete are listed first; the kept ones sit behind their own fold.
        const deletable = [...target.querySelectorAll('[data-storage-gc-list] [data-storage-gc-row]')].map(
            (row) => row.textContent ?? '',
        )
        expect(deletable).toHaveLength(1)
        expect(deletable[0]).toContain('Can be deleted')
        expect(target.querySelector('[data-storage-gc-list]')?.textContent).toContain(
            'and 7 more',
        )
        const kept = target.querySelector('[data-storage-gc-kept]')
        expect(kept?.textContent).toContain('3 files kept')
        expect(kept?.hasAttribute('open')).toBe(false)
        const keptRows = [...kept!.querySelectorAll('[data-storage-gc-row]')].map(
            (row) => row.textContent ?? '',
        )
        expect(keptRows[0]).toContain('kept so a fix can be undone')
        expect(keptRows[1]).toContain('in use')
        expect(keptRows[2]).toContain('Too new to delete yet')
    })

    it('shows the cleanup blocker instead of offering deletion', async () => {
        const target = setup()
        maintenance.previewNativePersistentAssetGc.mockResolvedValue({ candidateCount: 0, candidateBytes: 0, deletedCount: 0, deletedBytes: 0, blockers: ['plugin-storage-opaque'] })
        await vi.waitFor(() => expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === 'Find')).toBe(true))
        ;[...target.querySelectorAll('button')].find(button => button.textContent?.trim() === 'Find')!.click()
        await vi.waitFor(() => expect(target.textContent).toContain('Plugin data may reference these files.'))
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === 'Delete now')).toBe(false)
        expect(maintenance.executeNativePersistentAssetGc).not.toHaveBeenCalled()
    })

    it('shows the search as a progress panel while the cleanup preview runs', async () => {
        const target = setup()
        let release: (value: unknown) => void = () => {}
        maintenance.previewNativePersistentAssetGc.mockImplementation(
            () => new Promise((resolve) => { release = resolve }),
        )
        const find = () =>
            [...target.querySelectorAll<HTMLButtonElement>('button')].find(
                (candidate) => candidate.textContent?.trim() === 'Find',
            )
        await vi.waitFor(() => expect(find()).toBeDefined())
        find()!.click()
        await vi.waitFor(() =>
            expect(target.querySelector('[data-storage-gc-progress]')?.textContent).toContain(
                'Looking for unused files',
            ),
        )
        release({ candidateCount: 0, candidateBytes: 0, deletedCount: 0, deletedBytes: 0, blockers: [], candidates: [] })
        await vi.waitFor(() =>
            expect(target.querySelector('[data-storage-gc-progress]')).toBeNull(),
        )
    })

    it('says that clearing a link does not delete the file', async () => {
        const target = setup()
        await vi.waitFor(() => expect(target.textContent).toContain('Unused files'))
        expect(target.textContent).toContain(
            'Clearing a broken link in the data check does not delete the file.',
        )
    })

    it('formats large counts with locale separators', async () => {
        const target = setup(Promise.resolve({ ...stats, conversations: { count: 1200, messageCount: 15231 } }))
        await vi.waitFor(() => expect(target.textContent).toContain('Total data'))

        expect(target.textContent).toContain(`${(1200).toLocaleString()} chats · ${(15231).toLocaleString()} messages`)
    })


    it('uses a dedicated localized confirmation before deleting a conflict backup', async () => {
        const target = setup()
        alerts.alertConfirm.mockResolvedValue(false)
        await vi.waitFor(() => expect(target.textContent).toContain('Conflict backups'))

        const conflictRow = target.querySelectorAll<HTMLElement>('[data-storage-backup-list]')[1]
        conflictRow?.querySelector<HTMLButtonElement>('button')?.click()

        await vi.waitFor(() => expect(alerts.alertConfirm).toHaveBeenCalledWith(
            'Delete this conflict backup? This conflict backup cannot be recovered after deletion.',
        ))
        expect(backups.remove).not.toHaveBeenCalled()
        expect(languageKorean.risuNest.storage.deleteConflictBackupConfirm)
            .toBe('이 충돌 백업을 삭제하시겠습니까? 삭제한 충돌 백업은 복구할 수 없습니다.')
    })

    it('orders all backup lists before the storage action rows', async () => {
        const target = setup()
        await vi.waitFor(() => expect(target.textContent).toContain('Create now'))

        const actionRow = target.querySelector<HTMLElement>('[data-storage-action="snapshot"]')
        const lists = [...target.querySelectorAll<HTMLElement>('[data-storage-backup-list]')]
        expect(actionRow).not.toBeNull()
        expect(lists).toHaveLength(3)
        expect(lists.every((list) => Boolean(list.compareDocumentPosition(actionRow!) & Node.DOCUMENT_POSITION_FOLLOWING))).toBe(true)
    })

    it('keeps stale totals visible and leaves one reload control after a post-snapshot reload fails', async () => {
        const target = setup()
        maintenance.createNativePersistentSnapshot.mockResolvedValue({ path: 'new.db', bytes: 1, modifiedAt: 4 })
        await vi.waitFor(() => expect(target.textContent).toContain('Total data'))
        maintenance.getNativePersistentStorageStats.mockRejectedValueOnce(new Error('reload failed'))

        ;[...target.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent?.trim() === 'Create now')?.click()

        await vi.waitFor(() => expect(target.textContent).toContain('Storage totals may be out of date.'))
        expect(target.textContent).toContain('Total data')
        // The header refresh is the only control that reloads the page.
        expect(target.textContent).not.toContain('Retry')
        expect([...target.querySelectorAll<HTMLButtonElement>('button')]
            .filter((button) => button.textContent?.trim() === 'Refresh')).toHaveLength(1)
    })

    it.each([true, false])('delegates the selected snapshot row to the live guarded restore (accepted=%s)', async accepted => {
        const target = setup()
        const selected: string[] = []
        maintenance.restoreNativePersistentSnapshot.mockImplementation(async (actions) => {
            const chosen = await actions.choose([{ id: 'snapshot.db', reason: 'manual', reclaimableBytes: 0, bytes: 1024, modifiedAt: 1 }])
            expect(chosen).toBe('snapshot.db')
            expect(actions).not.toHaveProperty('confirm')
            expect(actions).not.toHaveProperty('restart')
            selected.push(chosen)
            return accepted
        })
        await vi.waitFor(() => expect(exact(target, 'Restore')).toBeDefined())

        exact(target, 'Restore')!.click()

        await vi.waitFor(() => expect(selected).toEqual(['snapshot.db']))
        await vi.waitFor(() => expect(exact(target, 'Restore')?.disabled).toBe(false))
        expect(maintenance.restoreNativePersistentSnapshot).toHaveBeenCalledOnce()
        expect(alerts.alertConfirm).not.toHaveBeenCalled()
        expect(alerts.alertError).not.toHaveBeenCalled()
    })

    it('marks a running action busy on its button instead of swapping the label', async () => {
        const target = setup()
        let finish: ((value: { id: string; revision: number; bytes: number; durationMs: number }) => void) | undefined
        maintenance.createNativePersistentSnapshot.mockImplementation(() => new Promise((resolve) => { finish = resolve }))
        await vi.waitFor(() => expect(button(target, 'Create now')).toBeDefined())
        const create = button(target, 'Create now')!

        create.click()

        await vi.waitFor(() => expect(create.getAttribute('aria-busy')).toBe('true'))
        expect(create.disabled).toBe(true)
        expect(create.textContent).toContain('Create now')
        expect(create.textContent).not.toContain('Loading...')
        finish?.({ id: 'new.db', revision: 1, bytes: 1, durationMs: 1 })
        await vi.waitFor(() => expect(create.getAttribute('aria-busy')).toBeNull())
    })
})
