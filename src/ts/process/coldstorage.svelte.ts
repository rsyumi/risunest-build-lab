import { DBState } from "../stores.svelte"
import { Decompress } from "fflate"
import { fetchProtectedResource } from "../sionyw"
import { alertConfirm } from "../alert"
import { language } from "src/lang"
import type { Database } from "../storage/database.svelte"
import { getColdStorageAffectedCharacters, listColdDataKeysFromDb } from "./coldstorageData"
import type { ColdItemRead } from "./coldPayloadExpansion"
import { MAX_NATIVE_REQUEST_BYTES } from "../storage/nativePersistenceValue"

export {
    coldStorageHeader,
    getColdStorageBackupKey,
    getColdStorageBackupName,
    isColdStorageBackupData,
    replaceColdStoragePayloadResources,
    listColdDataKeysFromDb
} from "./coldstorageData"

/**
 * Decompresses a response while neither the body nor its decoded bytes exceed
 * `maxBytes`, and returns null once either does.
 */
async function decompressBounded(response:Response, maxBytes:number):Promise<Uint8Array|null> {
    const chunks:Uint8Array[] = []
    let decoded = 0
    const inflater = new Decompress((chunk) => {
        decoded += chunk.length
        if (decoded <= maxBytes) chunks.push(chunk)
    })
    const reader = response.body?.getReader()
    let received = 0
    const push = (chunk:Uint8Array, final:boolean) => {
        received += chunk.length
        if (received > maxBytes) return false
        inflater.push(chunk, final)
        return decoded <= maxBytes
    }
    if (!reader) {
        if (!push(new Uint8Array(await response.arrayBuffer()), true)) return null
    } else {
        while (true) {
            const { done, value } = await reader.read()
            if (done) break
            if (!push(value, false)) {
                await reader.cancel().catch(() => {})
                return null
            }
        }
        if (!push(new Uint8Array(0), true)) return null
    }
    const bytes = new Uint8Array(decoded)
    let offset = 0
    for (const chunk of chunks) {
        bytes.set(chunk, offset)
        offset += chunk.length
    }
    return bytes
}

async function discardResponseBody(response:Response):Promise<void> {
    try {
        await response.body?.cancel()
    } catch (error) {}
}

/**
 * Reads an upstream account cold item. Import paths expand it into a record
 * body or a plugin value; nothing publishes a cold item back to the account.
 * 200 is present whatever JSON it holds, 404 is missing, and anything else is unreadable.
 * An item above the size native storage accepts for one record is too large.
 */
export async function getAccountColdStorageItem(
    key:string,
    signal?:AbortSignal,
    maxBytes = MAX_NATIVE_REQUEST_BYTES,
):Promise<ColdItemRead> {
    const d = await fetchProtectedResource('/hub/account/coldstorage', {
        method: 'GET',
        headers: {
            'x-risu-key': key,
        },
        ...(signal ? { signal } : {}),
    })

    if(d.status !== 200){
        await discardResponseBody(d)
        return { kind: d.status === 404 ? 'missing' : 'unreadable' }
    }
    try {
        const bytes = await decompressBounded(d, maxBytes)
        if (!bytes) return { kind: 'too-large' }
        return { kind: 'present', value: JSON.parse(new TextDecoder().decode(bytes)) }
    } catch (error) {
        if ((error as { name?: string })?.name === 'AbortError') throw error
        console.error(`Failed to decode the account cold storage item ${key}:`, error)
        return { kind: 'unreadable' }
    }
}

export async function listColdDataKeys(db: Pick<Database, 'characters'> = DBState.db): Promise<string[]> {
    return listColdDataKeysFromDb(db)
}

export async function confirmIncompleteColdStorageRestore(
    db: Pick<Database, 'characters'>,
    unavailableKeys: Iterable<string>,
): Promise<boolean> {
    const uniqueUnavailableKeys = Array.from(new Set(unavailableKeys))
    if (uniqueUnavailableKeys.length === 0) {
        return true
    }

    const affected = getColdStorageAffectedCharacters(db, uniqueUnavailableKeys)
    return await alertConfirm(language.errors.coldStorageIncompleteRestoreConfirm(
        affected.characterNames.join(', '),
        uniqueUnavailableKeys.length,
        affected.unresolvedKeys.length,
    ))
}
