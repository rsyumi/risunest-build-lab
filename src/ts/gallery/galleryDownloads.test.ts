import {describe, expect, it, vi} from 'vitest'

const nativePlatform = vi.hoisted(() => ({android: false}))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() =>
    nativePlatform.android ? {runtime: 'tauri', os: 'android'} : {runtime: 'web', os: 'windows'}))

import {get} from 'svelte/store'
import {downloadGallery, galleryArchiveDate, galleryDownloadName, type GalleryDownloadDependencies} from './galleryDownloads'
import {isLibraryFileOperationReserved, registerLibraryFileOperationGate} from '../storage/libraryFileOperation'
import {nativeFileOperation} from '../storage/nativeFileJobManager'
import {RevisionConflictError} from '../storage/persistentDataStore'
import type {NativeFileJobResult, NativeGalleryExportInput} from '../storage/nativeFileJobs'

const result: NativeFileJobResult = {revision: 4, sourceBytes: 3, sourceSha256: 'a'.repeat(64),
    sourceFingerprintKind: 'whole-file-sha256', characterCount: 0, presetCount: 0, warningCodes: []}
function dependencies(runExport: GalleryDownloadDependencies['runExport']): GalleryDownloadDependencies {
    return {isDesktop: () => true, isAndroid: () => false, isIOS: () => false,
        runtime: () => ({revision: 4, flushPendingData: async () => {}}),
        chooseDestination: async () => '/synthetic/chosen.zip', runExport,
        downloadWeb: async () => {throw new Error('Native downloads must not read WebView bytes')}}
}
describe('gallery downloads', () => {
    it('uses the media suffix once and sanitizes names', () => {
        expect(galleryDownloadName('photo.PNG', 'id', 'webp')).toBe('photo.webp')
        expect(galleryDownloadName('gemini-audio', 'id', 'wav')).toBe('gemini-audio.wav')
        expect(galleryDownloadName('theme.CSS', 'id', 'css')).toBe('theme.css')
        expect(galleryDownloadName('', 'id', 'mp4')).toBe('id.mp4')
        expect(galleryDownloadName('../bad:name. ', 'id', 'png')).toBe('.._bad_name.png')
        expect(galleryDownloadName('x'.repeat(130), 'id', 'png')).toBe(`${'x'.repeat(120)}.png`)
        for (const [name, extension, expected] of [
            ['한'.repeat(120), 'png', `${'한'.repeat(83)}.png`],
            ['😀'.repeat(120), 'png', `${'😀'.repeat(62)}.png`],
            ['한'.repeat(120), '한'.repeat(120), `${'한'.repeat(44)}.${'한'.repeat(40)}`],
            ['CON', 'png', '_CON.png'],
            ['aux.other', 'png', '_aux.other.png'],
            ['LPT¹', 'png', '_LPT¹.png'],
            ['COM10', 'png', 'COM10.png'],
        ]) {
            const actual = galleryDownloadName(name, 'id', extension)
            expect(actual).toBe(expected)
            expect(new TextEncoder().encode(actual).length).toBeLessThanOrEqual(255)
        }
        expect(galleryArchiveDate(new Date(2026, 9, 20, 0, 30))).toBe('2026-10-20')
        expect(galleryArchiveDate(new Date(2026, 0, 5, 23, 59))).toBe('2026-01-05')
    })
    it('captures asset occurrences after picker flush and retries a stale revision once', async () => {
        let revision = 3
        let captured = 0
        const requests: NativeGalleryExportInput[] = []
        const deps = dependencies(async request => {
            requests.push(request)
            if (requests.length === 1) throw {code: 'revision-conflict'}
            return {...result, warningCodes: ['gallery-skipped:2']}
        })
        deps.runtime = () => ({get revision() {return revision}, flushPendingData: async () => {revision++}})
        const answer = await downloadGallery({kind: 'asset', archive: true, suggestedName: 'owner.zip',
            captureEntries: async at => { captured = at; return [{key: 'assets/shared.png', name: `at-${at}`, section: 'icons'}] }}, {}, deps)
        expect(answer).toEqual({cancelled: false, skipped: 2})
        expect(captured).toBe(5)
        expect(requests.map(request => request.expectedRevision)).toEqual([4, 5])
        expect(requests[1].selection).toEqual({kind: 'asset', entries: [{key: 'assets/shared.png', name: 'at-5', section: 'icons'}]})
    })
    it('picks the Android destination once and keeps it for the stale-revision retry', async () => {
        const events: unknown[] = []
        const reservation = {requestId: 'synthetic-request', bind: async () => {}, release: async () => {events.push('release')}}
        const requests: NativeGalleryExportInput[] = []
        const deps = dependencies(async request => {
            requests.push(request)
            if (requests.length === 1) throw {code: 'revision-conflict'}
            return result
        })
        deps.isDesktop = () => false
        deps.isAndroid = () => true
        deps.reserveAndroidDestination = async (name, kind) => { events.push(['pick', name, kind]); return reservation }
        await expect(downloadGallery({kind: 'inlay', keys: ['first'], archive: true, suggestedName: 'attachments.zip'}, {}, deps))
            .resolves.toEqual({cancelled: false, skipped: 0})
        expect(requests.map(request => request.destination))
            .toEqual(Array(2).fill({type: 'androidSaf', suggestedName: 'attachments.zip', reservation}))
        expect(events).toEqual([['pick', 'attachments.zip', 'download'], 'release'])
    })
    it('retries a caller snapshot lease conflict before starting any native job', async () => {
        let captures = 0
        let exports = 0
        const answer = await downloadGallery({kind: 'asset', archive: true, suggestedName: 'owner.zip',
            captureEntries: async () => {
                if (++captures === 1) throw new RevisionConflictError(3, 4)
                return [{key: 'assets/photo.png', name: 'photo', section: 'icons'}]
            }}, {}, dependencies(async () => { exports++; return result }))
        expect(answer.cancelled).toBe(false)
        expect(captures).toBe(2)
        expect(exports).toBe(1)
    })
    it('passes selected inlay keys without loading metadata or bodies', async () => {
        let sent: NativeGalleryExportInput | undefined
        await downloadGallery({kind: 'inlay', keys: ['first', 'second'], archive: true, suggestedName: 'attachments.zip'}, {},
            dependencies(async input => {sent = input; return result}))
        expect(sent?.selection).toEqual({kind: 'inlay', keys: ['first', 'second']})
    })
    it('does not export when the system picker is cancelled', async () => {
        const deps = dependencies(async () => {throw new Error('unexpected export')})
        deps.chooseDestination = async () => null
        expect(await downloadGallery({kind: 'inlay', keys: ['first'], archive: false, suggestedName: 'photo.png'}, {}, deps))
            .toEqual({cancelled: true, skipped: 0})
    })
    it('holds the Android background task from the picker return through the export', async () => {
        const events: string[] = []
        nativePlatform.android = true
        window.RisuBackgroundTasks = {
            begin: async (_id, kind) => { events.push(`hold ${kind}`); return 'active' },
            progress: async () => undefined,
            end: async () => { events.push('end hold') },
        }
        try {
            const deps = dependencies(async () => { events.push('export'); return result })
            deps.isDesktop = () => false
            deps.isAndroid = () => true
            deps.reserveAndroidDestination = async () => {
                events.push('pick')
                return {requestId: 'synthetic-request', bind: async () => {}, release: async () => { events.push('release') }}
            }
            await downloadGallery({kind: 'inlay', keys: ['first'], archive: true, suggestedName: 'attachments.zip'}, {}, deps)
        } finally {
            nativePlatform.android = false
            delete window.RisuBackgroundTasks
        }
        expect(events).toEqual(['pick', 'hold export', 'export', 'release', 'end hold'])
    })
    it('waits for sync and backups to settle before the picker, and its cancel ends the wait', async () => {
        const picks: string[] = []
        let settle!: () => void
        const unregister = registerLibraryFileOperationGate(() => new Promise<void>(resolve => { settle = resolve }))
        try {
            const deps = dependencies(async () => result)
            deps.chooseDestination = async name => { picks.push(name); return '/synthetic/chosen.zip' }
            const input = {kind: 'inlay' as const, keys: ['first'], archive: true, suggestedName: 'attachments.zip'}
            const exporting = downloadGallery(input, {}, deps)
            await vi.waitFor(() => expect(get(nativeFileOperation)?.waitingForSync).toBe(true))
            expect(picks).toEqual([])
            settle()
            expect(await exporting).toEqual({cancelled: false, skipped: 0})
            expect(picks).toEqual(['attachments.zip'])

            const controller = new AbortController()
            const waiting = downloadGallery(input, {signal: controller.signal}, deps)
            await vi.waitFor(() => expect(get(nativeFileOperation)?.waitingForSync).toBe(true))
            controller.abort()
            expect(await waiting).toEqual({cancelled: true, skipped: 0})
            expect(picks).toEqual(['attachments.zip'])
            expect(isLibraryFileOperationReserved()).toBe(false)
        } finally { unregister() }
    })
    it('propagates a second stale revision failure', async () => {
        await expect(downloadGallery({kind: 'inlay', keys: ['first'], archive: false, suggestedName: 'photo.png'}, {},
            dependencies(async () => {throw {code: 'revision-conflict'}}))).rejects.toEqual({code: 'revision-conflict'})
    })
})
