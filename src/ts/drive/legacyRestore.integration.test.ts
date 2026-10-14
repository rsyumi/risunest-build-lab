import 'fake-indexeddb/auto'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createCipheriv, createHash, webcrypto } from 'node:crypto'
import type { BlobMetadata, BlobWriteMetadata } from 'src/ts/storage/blobStore'
import type { Database } from 'src/ts/storage/database.svelte'
import type { CommittedApplyOutcome, PersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime'
import { File } from 'node:buffer'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { gzipSync } from 'fflate'
import { pack } from 'msgpackr'
import { encodeRisuSaveLegacy } from 'src/ts/storage/risuSave'
import { risuSaveFixtureDatabase } from 'src/ts/storage/tests/risuSaveFixtures'
import { encodeBackupInlayEntry, getBackupInlayName } from 'src/ts/drive/backupAssets'
import { backupArchive, backupEntry } from 'src/ts/drive/backupRestore.testSupport'

const state = vi.hoisted(() => ({
    native: false,
    nativeFetch: vi.fn<typeof fetch>(),
    blobs: new Map<string, Uint8Array>(),
    metadata: new Map<string, BlobMetadata>(),
    afterPut: vi.fn(),
    replace: vi.fn(async (_database: Database): Promise<CommittedApplyOutcome> => ({
        kind: 'committed', revision: 2, projection: 'applied',
    } as const)),
}))
vi.mock('src/ts/alert', async () => (await import('src/ts/drive/backupRestore.testSupport')).alertModule())
vi.mock('src/ts/globalApi.svelte', () => ({
    forageStorage: { Init: vi.fn(async () => undefined), isAccount: false },
    getUncleanables: vi.fn(async () => []),
    LocalWriter: class {},
}))
vi.mock('src/ts/storage/platformBlobStore', async () => (await import('src/ts/drive/backupRestore.testSupport')).blobStoreModule(() => ({
    put: async (key: string, bytes: Uint8Array, input: BlobWriteMetadata) => {
        const metadata = { ...input, key, size: bytes.length } as BlobMetadata
        state.blobs.set(key, bytes.slice())
        state.metadata.set(key, metadata)
        state.afterPut(key)
        return metadata
    },
    read: async (key: string) => state.blobs.get(key) ?? null,
    stat: async (key: string) => state.metadata.get(key) ?? null,
    list: async () => [...state.metadata.values()],
    remove: async (key: string) => { state.blobs.delete(key); state.metadata.delete(key) },
    resolveUrl: async () => null,
})))
vi.mock('src/ts/storage/database.svelte', async () => (await import('src/ts/drive/backupRestore.testSupport')).databaseModule(() => ({}) as Database))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async () => (await import('src/ts/drive/backupRestore.testSupport')).persistentRuntimeModule({
    getPersistentDataRuntime: () => ({}) as PersistentDataRuntime,
    replacePersistentDatabase: (database: Database) => state.replace(database),
}))
vi.mock('src/ts/process/coldstorage.svelte', async () => (await import('src/ts/drive/backupRestore.testSupport')).coldStorageModule())
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => ({ runtime: state.native ? 'tauri' : 'web', os: 'windows' })))
vi.mock('@tauri-apps/plugin-http', () => ({ fetch: state.nativeFetch } satisfies Partial<typeof import('@tauri-apps/plugin-http')>))
vi.mock('../desktopRelaunch', async () => (await import('src/ts/drive/backupRestore.testSupport')).relaunchModule())
vi.mock('src/ts/util', async () => (await import('src/ts/drive/backupRestore.testSupport')).utilModule())
vi.mock('src/ts/characterCards', async () => (await import('src/ts/drive/backupRestore.testSupport')).characterCardsModule())

function databaseEntry() {
    return backupEntry('database.risudat', encodeRisuSaveLegacy(structuredClone(risuSaveFixtureDatabase), 'compression'))
}

function encryptedArchive(metadata: unknown = { type: 'account', time: 123 }, key = 'synthetic-key') {
    vi.stubGlobal('crypto', webcrypto)
    const encoded = encodeRisuSaveLegacy(structuredClone(risuSaveFixtureDatabase), 'compression')
    const cipher = createCipheriv('aes-256-gcm', createHash('sha256').update(key).digest(), Buffer.alloc(12))
    const encrypted = Buffer.concat([cipher.update(encoded), cipher.final(), cipher.getAuthTag()])
    return backupArchive(
        backupEntry('synthetic.png', new Uint8Array([1, 2, 3])),
        backupEntry('encryption.risudat', new TextEncoder().encode(JSON.stringify(metadata))),
        backupEntry('database.risudat', encrypted),
    )
}

async function restore(bytes: Uint8Array, controller = new AbortController(), lifecycle: { beforeActivation?(): Promise<void>; onCommitted?(): void } = {}) {
    const { importLegacyBackupWithWebView } = await import('src/ts/drive/backuplocal')
    const input = document.createElement('input')
    Object.defineProperty(input, 'files', { value: [new File([Buffer.from(bytes)], 'synthetic.bin')] })
    const click = vi.spyOn(input, 'click').mockImplementation(() => {})
    vi.spyOn(document, 'createElement').mockReturnValueOnce(input)
    const context = {
        signal: controller.signal,
        onStatus: vi.fn(), setSource: vi.fn(), setPartialWritesPossible: vi.fn(),
    }
    const pending = importLegacyBackupWithWebView(context, lifecycle)
    await vi.waitFor(() => expect(click).toHaveBeenCalledOnce())
    input.dispatchEvent(new Event('change'))
    return pending
}

afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    state.blobs.clear()
    state.native = false
    state.nativeFetch.mockReset()
    state.metadata.clear()
    state.afterPut.mockReset()
    state.replace.mockReset().mockResolvedValue({ kind: 'committed', revision: 2, projection: 'applied' })
})

describe('legacy WebView restore integrity', () => {
    it.each(['v1.10.0', 'v1.12.0', 'v1.14.0'])('preserves Voyage Context 4 settings from a PocketRisu %s backup', async (version) => {
        const archive = readFileSync(resolve(`src-tauri/src/native_file_jobs/legacy_backup/fixtures/pocket-risu-${version}.bin`))
        let restored: Database | undefined
        state.replace.mockImplementationOnce(async (database) => {
            restored = database
            return { kind: 'committed', revision: 2, projection: 'applied' }
        })
        await restore(archive)
        expect(restored).toMatchObject({ hypaModel: 'voyageContext4', voyageApiKey: 'synthetic-voyage-key' })
    })

    it('restores an extensionless PocketRisu JSON inlay after the replacement handshake', async () => {
        const payload = new TextEncoder().encode(JSON.stringify({
            name: 'old.png', data: 'data:image/png;base64,CQgH', ext: 'png', type: 'image', width: 3, height: 1,
        }))
        const events: string[] = []
        state.replace.mockImplementationOnce(async () => {
            events.push('commit')
            return { kind: 'committed', revision: 2, projection: 'applied' }
        })
        await restore(backupArchive(backupEntry('inlay/legacy-image', payload), databaseEntry()), new AbortController(), {
            beforeActivation: async () => { expect(state.blobs.size).toBe(0); events.push('handshake') },
            onCommitted: () => { events.push('hold-sync') },
        })
        expect(events).toEqual(['handshake', 'commit', 'hold-sync'])
        expect(state.blobs.get('legacy-image')).toEqual(new Uint8Array([9, 8, 7]))
    })

    it.each([false, true])('decrypts an account backup through the correct transport, native=%s', async native => {
        state.native = native
        const serveKey: typeof fetch = async (url, init) => {
            expect(url).toBe('https://sv.risuai.xyz/cryptokey?key=123')
            expect(new Headers(init?.headers).get('Sec-Fetch-Site')).toBe(native ? 'same-site' : null)
            return new Response(JSON.stringify({ key: 'synthetic-key' }))
        }
        vi.stubGlobal('fetch', native ? vi.fn(() => { throw new Error('Unexpected browser request') }) : serveKey)
        state.nativeFetch.mockImplementation(serveKey)
        await restore(encryptedArchive())
        expect(state.replace.mock.calls[0][0].characters).toEqual(risuSaveFixtureDatabase.characters)
        expect(state.blobs.get('assets/synthetic.png')).toEqual(new Uint8Array([1, 2, 3]))
    })

    it.each([
        ['network', 'backup-key-request-failed', /DNS lookup failed/],
        ['plugin', 'backup-key-request-failed', /Synthetic HTTP plugin failure/],
        ['json', 'backup-key-invalid', /JSON/],
        ['missing-key', 'backup-key-invalid', /no valid backup key/],
        ['empty-key', 'backup-key-invalid', /no valid backup key/],
        ['wrong-key', 'backup-decryption-failed', /operation-specific reason/],
    ] as const)('classifies %s failures and preserves their details without replacing the library or publishing attachments', async (failure, code, detail) => {
        state.native = failure === 'plugin'
        const failRequest: typeof fetch = async () => {
            if (failure === 'network') throw new TypeError('DNS lookup failed')
            if (failure === 'plugin') throw 'Synthetic HTTP plugin failure'
            if (failure === 'json') return new Response('not JSON')
            return new Response(JSON.stringify(failure === 'missing-key' ? {} : { key: failure === 'empty-key' ? '' : 'wrong-key' }))
        }
        vi.stubGlobal('fetch', failRequest)
        state.nativeFetch.mockImplementation(failRequest)
        await expect(restore(encryptedArchive())).rejects.toMatchObject({ code, message: expect.stringMatching(detail) })
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
    })

    it.each([
        [false, 403], [true, 403], [false, 503], [true, 503],
    ] as const)('distinguishes HTTP rejection from connectivity failure, native=%s status=%s', async (native, status) => {
        state.native = native
        const rejectRequest: typeof fetch = async () => new Response('Request rejected', { status })
        vi.stubGlobal('fetch', rejectRequest)
        state.nativeFetch.mockImplementation(rejectRequest)
        await expect(restore(encryptedArchive())).rejects.toMatchObject({
            code: 'backup-key-http-error', message: expect.stringContaining(`HTTP ${status}`),
        })
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
    })

    it.each([{ type: 'account' }, { type: 'account', time: '123' }, { type: 'account', time: -1 }])('rejects invalid account encryption metadata %j before decoding', async metadata => {
        vi.stubGlobal('fetch', async () => { throw new Error('Unexpected key request') })
        await expect(restore(encryptedArchive(metadata))).rejects.toMatchObject({ code: 'backup-encryption-invalid' })
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
    })

    it.each([false, true])('preserves cancellation while requesting an account backup key, native=%s', async native => {
        state.native = native
        const controller = new AbortController()
        const cancelRequest: typeof fetch = async (_url, init) => {
            controller.abort()
            init?.signal?.throwIfAborted()
            throw new TypeError('Failed to fetch')
        }
        vi.stubGlobal('fetch', cancelRequest)
        state.nativeFetch.mockImplementation(cancelRequest)
        await expect(restore(encryptedArchive(), controller)).rejects.toMatchObject({ name: 'AbortError' })
        expect(state.replace).not.toHaveBeenCalled()
    })

    // The native importer treats every readable metadata except the account type as unencrypted.
    it.each(['{"type":"none"}', '{}', 'null', '{"type":"synthetic"}'])('restores a backup with non-account encryption metadata %s unencrypted', async metadata => {
        vi.stubGlobal('fetch', async () => { throw new Error('Unexpected key request') })
        await restore(backupArchive(
            backupEntry('encryption.risudat', new TextEncoder().encode(metadata)),
            databaseEntry(),
        ))
        expect(state.replace.mock.calls[0][0].characters).toEqual(risuSaveFixtureDatabase.characters)
    })

    it('classifies an undecodable database separately from account decryption', async () => {
        await expect(restore(backupEntry('database.risudat', Uint8Array.of(7, 7, 7, 7))))
            .rejects.toMatchObject({ code: 'backup-database-invalid', message: expect.stringContaining('invalid block type') })
        expect(state.replace).not.toHaveBeenCalled()
    })

    it('keeps decoded content of a corrupted database out of the error detail', async () => {
        const decoded = pack({ note: 'Synthetic private note' })
        const corrupted = gzipSync(Uint8Array.of(...decoded, 0xc0), { mtime: 0 })
        await expect(restore(backupEntry('database.risudat', corrupted))).rejects.toMatchObject({
            code: 'backup-database-invalid',
            message: 'Could not decode the backup database: Data read, but end of buffer not reached',
        })
        expect(state.replace).not.toHaveBeenCalled()
    })

    it('preserves the parse error for malformed encryption metadata', async () => {
        const invalid = '{"type":'
        let detail = ''
        try { JSON.parse(invalid) } catch (error) { detail = (error as Error).message }
        await expect(restore(backupArchive(
            backupEntry('encryption.risudat', new TextEncoder().encode(invalid)), databaseEntry(),
        ))).rejects.toMatchObject({ code: 'backup-encryption-invalid', message: expect.stringContaining(detail) })
        expect(state.replace).not.toHaveBeenCalled()
    })

    it('does not activate attachments or replace the library when the replacement handshake rejects', async () => {
        const onCommitted = vi.fn()
        await expect(restore(backupArchive(backupEntry('synthetic.png', new Uint8Array([1])), databaseEntry()), new AbortController(), {
            beforeActivation: async () => { throw new Error('pending sync operation') }, onCommitted,
        })).rejects.toThrow('pending sync operation')
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
        expect(onCommitted).not.toHaveBeenCalled()
    })

    it.each([1, 3, 6, 12, 15, 17])('rejects a trailing frame truncated at byte %s', async (length) => {
        const damaged = backupEntry('file.png', new Uint8Array([1, 2, 3]))
        await expect(restore(backupArchive(databaseEntry(), damaged.subarray(0, length))))
            .rejects.toThrow('truncated entry')
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
    })

    it('rejects a truncated database after a complete attachment', async () => {
        await expect(restore(backupArchive(backupEntry('file.png', new Uint8Array([1])), databaseEntry().subarray(0, -1))))
            .rejects.toThrow('truncated entry')
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.size).toBe(0)
    })

    it.each(['database.risudat', 'encryption.risudat'])('rejects duplicate %s records', async (name) => {
        const data = name === 'database.risudat'
            ? databaseEntry()
            : backupEntry(name, new TextEncoder().encode('{"type":"account","time":1}'))
        await expect(restore(backupArchive(data, data))).rejects.toThrow('Duplicate')
        expect(state.replace).not.toHaveBeenCalled()
    })

    it.each(['asset', 'inlay', 'pocket'] as const)('preserves existing %s bytes and metadata on every pre-commit failure', async (kind) => {
        const key = kind === 'asset' ? 'assets/existing.png' : 'existing'
        const metadata = {
            key, kind: kind === 'asset' ? 'asset' : 'inlay', inlayType: 'image',
            mime: 'image/png', name: 'original.png', ext: 'png', size: 3, width: 2, height: 2,
        } as BlobMetadata
        const oldBytes = new Uint8Array([9, 8, 7])
        const newBytes = new Uint8Array([1, 2, 3])
        const attachment = kind === 'asset' ? backupEntry('existing.png', newBytes)
            : kind === 'pocket' ? backupEntry('inlay/existing.png', newBytes)
            : backupEntry(getBackupInlayName(key), encodeBackupInlayEntry({ ...metadata, kind: 'inlay', inlayType: 'image' }, newBytes))
        for (const failure of ['missing', 'invalid', 'truncated', 'cancel', 'activation'] as const) {
            state.blobs.set(key, oldBytes.slice())
            state.metadata.set(key, structuredClone(metadata))
            state.replace.mockClear()
            const controller = new AbortController()
            let bytes = backupArchive(attachment, databaseEntry())
            if (failure === 'missing') bytes = attachment
            if (failure === 'invalid') bytes = backupArchive(attachment, backupEntry('database.risudat', new Uint8Array([0xff, 0xff, 0xff, 0xff])))
            if (failure === 'truncated') bytes = bytes.subarray(0, -1)
            if (failure === 'activation') state.replace.mockRejectedValueOnce(new Error('activation rejected'))
            const error = await restore(bytes, controller, {
                beforeActivation: async () => { if (failure === 'cancel') controller.abort() },
            }).then(() => null, (error: unknown) => error)
            expect(error, failure).not.toBeNull()
            expect(state.blobs.get(key), failure).toEqual(oldBytes)
            expect(state.metadata.get(key), failure).toEqual(metadata)
            if (failure !== 'activation') expect(state.replace, failure).not.toHaveBeenCalled()
        }
        await restore(backupArchive(attachment, databaseEntry()))
        expect(state.blobs.get(key)).toEqual(newBytes)
        expect(state.replace).toHaveBeenCalled()
    })

    it('removes newly published attachments when activation is rejected', async () => {
        state.replace.mockRejectedValueOnce(new Error('activation rejected'))
        await expect(restore(backupArchive(backupEntry('new.png', new Uint8Array([1])), databaseEntry()))).rejects.toThrow('activation rejected')
        expect(state.blobs.size).toBe(0)
        expect(state.metadata.size).toBe(0)
    })

    it('reports the committed revision without adopting bodies when projection needs recovery', async () => {
        state.replace.mockResolvedValueOnce({ kind: 'committed', revision: 2, projection: 'refresh-required' })
        await expect(restore(backupArchive(backupEntry('new.png', new Uint8Array([1])), databaseEntry())))
            .rejects.toMatchObject({ committedRevision: 2, cause: {message: 'Committed backup projection requires recovery'} })
        expect(state.replace).toHaveBeenCalledOnce()
        expect(state.blobs.size).toBe(0)
        expect(state.afterPut).not.toHaveBeenCalled()
    })

    it('retains the committed database and completed physical writes if a later body write fails', async () => {
        state.afterPut.mockImplementationOnce(() => undefined)
            .mockImplementationOnce(() => { throw new Error('second write failed') })
        await expect(restore(backupArchive(
            backupEntry('first.png', new Uint8Array([1])),
            backupEntry('second.png', new Uint8Array([2])),
            databaseEntry(),
        ))).rejects.toMatchObject({ committedRevision: 2, cause: {message: 'second write failed'} })
        expect(state.blobs.get('assets/first.png')).toEqual(new Uint8Array([1]))
        expect(state.blobs.get('assets/second.png')).toEqual(new Uint8Array([2]))
        expect(state.metadata.get('assets/first.png')).toMatchObject({key: 'assets/first.png', size: 1})
        expect(state.replace).toHaveBeenCalledOnce()
    })

    it('does not overwrite an existing inlay when the archive has no database', async () => {
        const original = new Uint8Array([9, 9, 9])
        state.blobs.set('synthetic-existing-inlay', original.slice())
        const metadata = {
            key: 'synthetic-existing-inlay', kind: 'inlay', inlayType: 'image',
            mime: 'image/png', name: 'synthetic.png', ext: 'png', size: 3,
        } as const
        const replacement = new Uint8Array([1, 2, 3])
        const bytes = backupEntry(getBackupInlayName(metadata.key), encodeBackupInlayEntry(metadata, replacement))
        await expect(restore(bytes)).rejects.toThrow('no database entry')
        expect(state.replace).not.toHaveBeenCalled()
        expect(state.blobs.get(metadata.key)).toEqual(original)
    })
})
