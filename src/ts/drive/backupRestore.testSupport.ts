import { vi } from 'vitest'
import * as coldStorageData from '../process/coldstorageData'
import type { ColdItemRead } from '../process/coldPayloadExpansion'
import { decryptBuffer, encryptBuffer } from '../bufferCrypto'
import type { BlobStore } from '../storage/blobStore'
import type { Database } from '../storage/database.svelte'

// Module stubs shared by the backup restore tests. Each one cuts an import graph that cannot load
// in the test environment; the type check keeps their names and signatures in step with the product.

type Alert = typeof import('../alert')
type PersistentRuntime = typeof import('../storage/persistentDataRuntime.svelte')

/** Alert calls without UI; every confirmation answers yes. */
export function alertModule(overrides: Partial<Alert> = {}) {
    return {
        alertConfirm: vi.fn(async () => true),
        alertCheckboxConfirm: vi.fn(async () => ({ confirmed: true, checked: true })),
        alertActionConfirm: vi.fn(async () => true),
        alertSelect: vi.fn(async () => ''),
        alertError: vi.fn(),
        alertMd: vi.fn(),
        alertNormal: vi.fn(),
        alertWait: vi.fn(),
        alertStore: { set: vi.fn() },
        ...overrides,
    } satisfies Partial<Alert>
}

export function databaseModule(getDatabase: () => Database) {
    return { getDatabase } satisfies Partial<typeof import('../storage/database.svelte')>
}

export function persistentRuntimeModule(overrides: Partial<PersistentRuntime> = {}) {
    return {
        publishCurrentOfficialRevision: vi.fn(async () => undefined),
        replacePersistentDatabase: vi.fn(),
        ...overrides,
    } satisfies Partial<PersistentRuntime>
}

export function blobStoreModule(resolve: () => BlobStore) {
    return { resolveBlobStore: async () => resolve() } satisfies Partial<typeof import('../storage/platformBlobStore')>
}

export function libraryCharactersModule(hasLibraryCharacters: () => Promise<boolean>) {
    return { hasLibraryCharacters } satisfies typeof import('../storage/libraryCharacters')
}

/** The cold storage module over its real data helpers, without account access or prompts. */
export function coldStorageModule() {
    return {
        coldStorageHeader: coldStorageData.coldStorageHeader,
        getColdStorageBackupKey: coldStorageData.getColdStorageBackupKey,
        getColdStorageBackupName: coldStorageData.getColdStorageBackupName,
        isColdStorageBackupData: coldStorageData.isColdStorageBackupData,
        replaceColdStoragePayloadResources: coldStorageData.replaceColdStoragePayloadResources,
        listColdDataKeysFromDb: coldStorageData.listColdDataKeysFromDb,
        listColdDataKeys: async (db?: Pick<Database, 'characters'>) => coldStorageData.listColdDataKeysFromDb(db),
        getAccountColdStorageItem: vi.fn(async (): Promise<ColdItemRead> => ({ kind: 'missing' })),
        confirmIncompleteColdStorageRestore: vi.fn(async () => true),
    } satisfies typeof import('../process/coldstorage.svelte')
}

export function relaunchModule() {
    return { relaunch: vi.fn(async () => {}) } satisfies typeof import('../desktopRelaunch')
}

export function utilModule() {
    return {
        decryptBuffer,
        encryptBuffer,
        sleep: vi.fn(async () => undefined),
    } satisfies Partial<typeof import('../util')>
}

export function characterCardsModule() {
    return { hubURL: 'https://hub.invalid' } satisfies Partial<typeof import('../characterCards')>
}

/** One entry of a legacy local backup: name length, name, data length and data, little endian. */
export function backupEntry(name: string, data: Uint8Array): Uint8Array {
    const encodedName = new TextEncoder().encode(name)
    const entry = new Uint8Array(8 + encodedName.byteLength + data.byteLength)
    const view = new DataView(entry.buffer)
    view.setUint32(0, encodedName.byteLength, true)
    entry.set(encodedName, 4)
    view.setUint32(4 + encodedName.byteLength, data.byteLength, true)
    entry.set(data, 8 + encodedName.byteLength)
    return entry
}

export function backupArchive(...entries: Uint8Array[]): Uint8Array {
    const archive = new Uint8Array(entries.reduce((size, entry) => size + entry.byteLength, 0))
    let offset = 0
    for (const entry of entries) {
        archive.set(entry, offset)
        offset += entry.byteLength
    }
    return archive
}

/** A picked backup file over `archive`; `beforeRead` runs before each slice returns its bytes. */
export function backupFile(name: string, archive: Uint8Array, beforeRead?: (start: number, end: number) => void): File {
    return {
        name,
        size: archive.byteLength,
        slice: (start: number, end: number) => ({
            arrayBuffer: async () => {
                beforeRead?.(start, end)
                return archive.slice(start, end).buffer
            },
        }),
    } as unknown as File
}
