import { writable } from "svelte/store"
import { getDatabase } from "./database.svelte"
import localforage from "localforage"
import { alertClear, alertError, alertLogin, alertNormalWait, alertStore } from "../alert"
import { getUncleanablesSync } from "../globalApi.svelte"
import { v4 } from "uuid"
import { language } from "src/lang"
import { fetchProtectedResource } from "../sionyw"
import { completeAccountUnmigration } from "./databaseRestore"
import { getDeviceMarkers } from "./deviceMarkers"
import {
    materializePersistentDatabaseSnapshotWithRevision,
    replacePersistentDatabase,
} from "./persistentDataRuntime.svelte"
import { isTauri } from "../platform"
import { AccountReadError } from './accountReadError'
import { accountReadDeadline, readAccountBody } from './accountReadBody'
export { AccountReadError } from './accountReadError'

export const AccountWarning = writable('')
let risuSession = ''

export function resetAccountStorageSession(): void {
    risuSession = ''
}

let seenWarnings:string[] = []
const accountDatabaseKey = 'database/database.bin'
const maxNativeOfficialWriteAttempts = 3

export type AccountReadResult =
    | { kind: 'value'; bytes: Uint8Array }
    | { kind: 'not-modified'; bytes: Uint8Array }
    | { kind: 'missing' }

export type AccountWriteResult =
    | { kind: 'written'; replacementKey: string }
    | { kind: 'not-modified'; replacementKey: string }
    | { kind: 'auth-warning' }

export interface AccountReadOptions {
    progress?(ratio: number): void
    onBytes?(bytes: number): void
    signal?: AbortSignal
}

export interface AccountWriteOptions {
    signal?: AbortSignal
}

export interface AccountNativeOfficialWriteAttemptContext {
    credential: { kind: 'risu-auth'; token: string }
    session: string | null
    saveDate: string
    signal?: AbortSignal
}

export type AccountNativeOfficialWriteAttemptResult<T> =
    | {
          kind: 'written' | 'not-modified'
          session: string | null
          replacementKey: string
          warning?: string | null
          reloadSession?: boolean
          receipt: T
      }
    | { kind: 'auth-warning'; session: string | null; warning?: string | null }
    | { kind: 'reauthentication-needed'; session: string | null; warning?: string | null }

export type AccountNativeOfficialWriteResult<T> =
    | {
        kind: 'written' | 'not-modified'
        replacementKey: string
        receipt: T
        completeReload(): Promise<void>
    }
    | { kind: 'auth-warning' }

export type AccountNativeOfficialWriteAttempt<T> = (
    context: AccountNativeOfficialWriteAttemptContext,
) => Promise<AccountNativeOfficialWriteAttemptResult<T> | null>

export interface AccountRecoveredOfficialWrite {
    session: string | null
    warning?: string | null
    reloadSession?: boolean
}

/** Nothing caches the account database locally. */
const uncachedDatabase: AccountStorageCache = {
    getItem: async () => null,
    setItem: async () => undefined,
}

export interface AccountStorageCache {
    getItem(key: string): Promise<unknown | null>
    setItem(key: string, value: unknown): Promise<unknown>
}

export interface AccountCredentialRouting {
    getAccountId(): string | null | undefined
    getToken(): string | null | undefined
    reauthenticate(loginResult: string): Promise<void>
}

export interface AccountStorageOptions {
    databaseCache?: AccountStorageCache
    assetCache?: AccountStorageCache
    credentialRouting?: AccountCredentialRouting
}

interface ReauthenticationFlight {
    controller: AbortController
    promise: Promise<void>
    waiters: number
}

const webReauthenticationFlights = new Map<string, ReauthenticationFlight>()
const routedReauthenticationFlights = new WeakMap<AccountCredentialRouting, Map<string, ReauthenticationFlight>>()

function withSignal(options: RequestInit, signal?: AbortSignal): RequestInit {
    return signal ? { ...options, signal } : options
}

function abortReason(signal: AbortSignal): unknown {
    return signal.reason ?? new DOMException('The operation was aborted', 'AbortError')
}

async function waitForSignal<T>(operation: Promise<T>, signal?: AbortSignal): Promise<T> {
    if (!signal) return await operation
    if (signal.aborted) throw abortReason(signal)
    return await new Promise<T>((resolve, reject) => {
        const removeAbortListener = () => signal.removeEventListener('abort', onAbort)
        const onAbort = () => {
            removeAbortListener()
            reject(abortReason(signal))
        }
        signal.addEventListener('abort', onAbort, { once: true })
        operation.then(
            (value) => {
                removeAbortListener()
                resolve(value)
            },
            (error) => {
                removeAbortListener()
                reject(error)
            },
        )
    })
}

function isJsonResponse(response: Response): boolean {
    return /^\s*application\/json\s*(?:;|$)/i.test(response.headers.get('content-type') ?? '')
}

async function discardResponseBody(response: Response): Promise<void> {
    try {
        await response.body?.cancel()
    } catch (error) {}
}

function waitForever(): Promise<never> {
    return new Promise(() => {})
}

function publishAccountWarning(warning: string | null | undefined): void {
    if (typeof warning !== 'string' || !warning || seenWarnings.includes(warning)) return
    seenWarnings.push(warning)
    AccountWarning.set(warning)
}

async function applyAccountReload(reloadSession: boolean): Promise<void> {
    if (!reloadSession) return
    void alertNormalWait(language.activeTabChange).then(() => {
        location.reload()
    })
    await waitForever()
}

async function cacheDatabaseWrite(
    cache: AccountStorageCache,
    key:string,
    value:Uint8Array,
    saveDate:string,
):Promise<void> {
    if(key !== accountDatabaseKey){
        return
    }
    await cache.setItem(key, value)
    await cache.setItem(key + '__date', saveDate)
}

export class AccountStorage{
    auth:string
    usingSync:boolean
    private readonly databaseCache: AccountStorageCache
    private readonly assetCache: AccountStorageCache
    private readonly credentialRouting?: AccountCredentialRouting

    constructor(options: AccountStorageOptions = {}) {
        this.databaseCache = options.databaseCache ?? uncachedDatabase
        this.assetCache = options.assetCache ?? localforage
        this.credentialRouting = options.credentialRouting
    }

    async setItem(key:string, value:Uint8Array) {
        const result = await this.writeItem(key, value)
        if(result.kind === 'auth-warning'){
            return undefined
        }
        return result.replacementKey
    }

    async writeItem(
        key:string,
        value:Uint8Array,
        options:AccountWriteOptions = {},
    ):Promise<AccountWriteResult> {
        this.checkAuth()
        const expectedAccountId = this.getAccountId()
        let expectedToken = this.auth
        let da:Response|undefined

        while((!da) || da.status === 403){

            const saveDate = Date.now().toFixed(0)

            if(risuSession === ''){
                da = await fetchProtectedResource('/api/account/getsessionnumber', withSignal({
                    method: "GET"
                }, options.signal))

                const json = await da.json()
                risuSession = `${json.sessionNumber}`
            }

            da = await fetchProtectedResource('/api/account/write', withSignal({
                method: "POST",
                body: value as any,
                headers: {
                    'content-type': 'application/octet-stream',
                    'x-risu-key': key,
                    'X-Format': 'nocheck',
                    'x-risu-session': risuSession,
                    'x-risu-save-date': saveDate
                }
            }, options.signal))
            let daText:string|undefined = undefined
            const getDaText = async () => {
                if(daText === undefined){
                    daText = await da!.text()
                }
                return daText
            }

            if(da.status === 304){
                await discardResponseBody(da)
                await cacheDatabaseWrite(this.databaseCache, key, value, saveDate)
                return { kind: 'not-modified', replacementKey: key }
            }
            if (da.status === 403) {
                if (isJsonResponse(da)) {
                    // A malformed warning must not change the authentication outcome.
                    const text = await getDaText()
                    try {
                        publishAccountWarning(JSON.parse(text)?.warning)
                    } catch {}
                } else {
                    await discardResponseBody(da)
                }
                if (da.headers.get('x-risu-status') === 'warn') {
                    return { kind: 'auth-warning' }
                }
                await this.reauthenticate(options.signal, expectedAccountId, expectedToken)
                expectedToken = this.auth
                continue
            }
            if(da.status < 200 || da.status >= 300){
                throw await getDaText()
            }

            if(isJsonResponse(da)){
                const json = JSON.parse(await getDaText())
                publishAccountWarning(json?.warning)
                await applyAccountReload(Boolean(json?.reloadSession))
            }

            const responseText = await getDaText()
            const replacementKey = key === 'database/database.bin' && !isJsonResponse(da)
                && responseText.length === 0 ? key : responseText
            if(key.startsWith('assets/')){
                await this.assetCache.setItem(key, new Uint8Array(value).buffer)
            }
            await cacheDatabaseWrite(this.databaseCache, key, value, saveDate)
            return { kind: 'written', replacementKey }
        }

        throw new Error('Account write did not complete')
    }

    async writeOfficialDatabaseFromNative<T>(
        attempt: AccountNativeOfficialWriteAttempt<T>,
        options: AccountWriteOptions = {},
    ): Promise<AccountNativeOfficialWriteResult<T> | null> {
        for (let attemptNumber = 1; attemptNumber <= maxNativeOfficialWriteAttempts; attemptNumber += 1) {
            this.checkAuth()
            if (getDeviceMarkers().getItem('ignoreRisuAuth') === 'true' || !this.auth) return null
            const expectedAccountId = this.getAccountId()
            const expectedToken = this.auth

            const result = await attempt({
                credential: { kind: 'risu-auth', token: this.auth },
                session: risuSession || null,
                saveDate: Date.now().toFixed(0),
                signal: options.signal,
            })
            if (result === null) return null
            if (result.session !== null) risuSession = result.session
            publishAccountWarning(result.warning)
            if (result.kind === 'reauthentication-needed') {
                if (attemptNumber === maxNativeOfficialWriteAttempts) {
                    throw new Error('Official account reauthentication was rejected too many times')
                }
                await this.reauthenticate(options.signal, expectedAccountId, expectedToken)
                continue
            }
            if (result.kind === 'auth-warning') return { kind: 'auth-warning' }
            return {
                kind: result.kind,
                replacementKey: result.replacementKey,
                receipt: result.receipt,
                completeReload: () => applyAccountReload(result.reloadSession ?? false),
            }
        }
    }

    adoptRecoveredOfficialWrite(result: AccountRecoveredOfficialWrite): {
        completeReload(): Promise<void>
    } {
        if (result.session !== null) risuSession = result.session
        publishAccountWarning(result.warning)
        return {
            completeReload: () => applyAccountReload(result.reloadSession ?? false),
        }
    }

    async getItem(key:string, callback?:(status:number) => void):Promise<Buffer|null> {
        const result = await this.readItem(key, { progress: callback })
        if(result.kind === 'missing'){
            return null
        }
        return Buffer.from(result.bytes)
    }

    async readItem(
        key:string,
        options:AccountReadOptions = {},
    ):Promise<AccountReadResult> {
        this.checkAuth()
        const expectedAccountId = this.getAccountId()
        let expectedToken = this.auth
        const assetRead = /^assets[/\\]/.test(key)
        const nativeAssetRead = assetRead && !!this.credentialRouting
        const checkReadOwner = () => {
            if (options.signal?.aborted) throw abortReason(options.signal)
            if (nativeAssetRead && this.getAccountId() !== expectedAccountId) throw new AccountReadError('account-changed')
        }
        const cacheOperation = async <T>(pending: Promise<T>): Promise<T> => {
            try { return await pending }
            catch (error) {
                checkReadOwner()
                if (nativeAssetRead) throw new AccountReadError('local')
                throw error
            }
        }
        checkReadOwner()
        if(assetRead){
            const cached = await cacheOperation(this.assetCache.getItem(key))
            if(cached instanceof ArrayBuffer || ArrayBuffer.isView(cached)){
                checkReadOwner()
                return { kind: 'value', bytes: new Uint8Array(
                    cached instanceof ArrayBuffer
                        ? cached
                        : cached.buffer.slice(cached.byteOffset, cached.byteOffset + cached.byteLength),
                ) }
            }
        }
        let da:Response|undefined
        let deadline: ReturnType<typeof accountReadDeadline> | undefined
        let reauthenticated = false
        const saveDate = await cacheOperation(this.databaseCache.getItem(key + '__date')) as number|string|undefined
        try {
        while((!da) || da.status === 403 || (nativeAssetRead && da.status === 401)){
            checkReadOwner()
            deadline?.dispose()
            deadline = nativeAssetRead ? accountReadDeadline(options.signal) : undefined
            try {
                da = await waitForSignal(fetchProtectedResource('/api/account/read/' + Buffer.from(key ,'utf-8').toString('hex') +
                (key === accountDatabaseKey ? ('|' + v4()) : ''), withSignal({
                method: "GET",
                headers: {
                    'x-risu-key': key,
                    'x-risu-save-date': (saveDate || 0).toString()
                }
                }, deadline?.signal ?? options.signal)), deadline?.signal ?? options.signal)
            } catch (error) {
                checkReadOwner()
                if (deadline?.signal.aborted) throw deadline.signal.reason
                if (nativeAssetRead) throw new AccountReadError('transport')
                throw error
            }
            try { checkReadOwner() }
            catch (error) { await discardResponseBody(da); throw error }
            if(da.status === 403 || (nativeAssetRead && da.status === 401)){
                await discardResponseBody(da)
                deadline?.dispose()
                if (nativeAssetRead && reauthenticated) throw new AccountReadError('http', da.status)
                try { await this.reauthenticate(options.signal, expectedAccountId, expectedToken) }
                catch (error) {
                    checkReadOwner()
                    if (nativeAssetRead) throw new AccountReadError('http', da.status)
                    throw error
                }
                reauthenticated = true
                expectedToken = this.auth
            }
        }
        if(da.status === 303){
            let data: { match?: unknown }
            try {
                data = deadline ? JSON.parse(new TextDecoder().decode(await readAccountBody(da, deadline.signal, deadline.reset, options.onBytes))) : await da.json()
                if (nativeAssetRead && (!data || typeof data.match !== 'boolean')) throw new Error('Invalid account cache response')
            } catch (error) {
                checkReadOwner()
                if (deadline?.signal.aborted) throw deadline.signal.reason
                if (nativeAssetRead) throw new AccountReadError('malformed', 303)
                throw error
            }
            checkReadOwner()
            if(data.match){
                const cached = await cacheOperation(this.databaseCache.getItem(key)) as ArrayBuffer|Uint8Array|null
                checkReadOwner()
                if(!cached){
                    if (nativeAssetRead) throw new AccountReadError('malformed', 303)
                    throw new Error(`Cached account bytes are missing for ${key}`)
                }
                return { kind: 'not-modified', bytes: new Uint8Array(cached) }
            }
            else{
                return { kind: 'missing' }
            }
        }

        if(da.status < 200 || da.status >= 300){
            if (nativeAssetRead) {
                await discardResponseBody(da)
                throw new AccountReadError('http', da.status, da.headers.get('retry-after') ?? undefined)
            }
            throw await da.text()
        }
        if(da.status === 204){
            return { kind: 'missing' }
        }
        if(assetRead){
            let ab: ArrayBuffer
            try { ab = deadline ? (await readAccountBody(da, deadline.signal, deadline.reset, options.onBytes)).buffer as ArrayBuffer : await da.arrayBuffer() }
            catch (error) {
                checkReadOwner()
                if (deadline?.signal.aborted) throw deadline.signal.reason
                if (error instanceof AccountReadError) throw error
                if (nativeAssetRead) throw new AccountReadError('transport')
                throw error
            }
            checkReadOwner()
            await cacheOperation(this.assetCache.setItem(key, ab))
            checkReadOwner()
            return { kind: 'value', bytes: new Uint8Array(ab) }
        }
        if(!options.progress){
            const ab = await da.arrayBuffer()
            return { kind: 'value', bytes: new Uint8Array(ab) }
        }
        const size = parseInt(da.headers.get('x-body-size'))
        const appendable = new Uint8Array(size)
        const reader = da.body.getReader()

        let i = 0
        while(true){
            const {done, value} = await reader.read()
            if(done){
                break
            }
            appendable.set(value, i)
            i += value.length
            options.progress(i/size)
        }

        return { kind: 'value', bytes: appendable }
        } finally { deadline?.dispose() }
    }
    keys():string[]{
        let db = getDatabase()
        return getUncleanablesSync(db, 'pure')
    }
    removeItem(key:string){
        throw "Error: You cannot remove data in account. report this to dev if you found this."
    }

    private checkAuth(){
        if (this.credentialRouting) {
            this.auth = this.credentialRouting.getToken() ?? ''
            return
        }
        const db = getDatabase()
        this.auth = db?.account?.token
        if(!this.auth){
            try {
                db.account = JSON.parse(localStorage.getItem("fallbackRisuToken"))
                this.auth = db?.account?.token
                db.account.useSync = true
            } catch (error) {}
        }
    }

    private getAccountId() {
        return this.credentialRouting ? this.credentialRouting.getAccountId() : getDatabase().account?.id
    }

    private getToken() {
        return this.credentialRouting ? this.credentialRouting.getToken() : getDatabase().account?.token
    }

    private async reauthenticate(
        signal?: AbortSignal,
        expectedAccountId = this.getAccountId(),
        expectedToken = this.getToken(),
    ): Promise<void> {
        if (signal?.aborted) throw abortReason(signal)
        if (this.getAccountId() !== expectedAccountId) {
            throw new Error('Official account session changed during reauthentication')
        }
        const currentToken = this.getToken()
        if (expectedAccountId && currentToken && currentToken !== expectedToken) {
            this.checkAuth()
            return
        }
        let flights = webReauthenticationFlights
        if (this.credentialRouting) {
            flights = routedReauthenticationFlights.get(this.credentialRouting)
            if (!flights) {
                flights = new Map()
                routedReauthenticationFlights.set(this.credentialRouting, flights)
            }
        }
        const key = JSON.stringify([expectedAccountId, expectedToken])
        let flight = flights.get(key)
        if (!flight) {
            const controller = new AbortController()
            flight = {
                controller,
                waiters: 0,
                promise: this.performReauthentication(controller.signal, expectedAccountId, expectedToken)
                    .finally(() => {
                        if (flights.get(key) === flight) flights.delete(key)
                    }),
            }
            flights.set(key, flight)
        }
        flight.waiters += 1
        try {
            await waitForSignal(flight.promise, signal)
            if (expectedAccountId && this.getAccountId() !== expectedAccountId) {
                throw new Error('Official account session changed during reauthentication')
            }
            this.checkAuth()
        } finally {
            flight.waiters -= 1
            if (flight.waiters === 0) {
                if (flights.get(key) === flight) flights.delete(key)
                flight.controller.abort()
            }
        }
    }

    private async performReauthentication(
        signal: AbortSignal,
        expectedAccountId: string | null | undefined,
        expectedToken: string | null | undefined,
    ): Promise<void> {
        const loginResult = await waitForSignal(alertLogin(), signal)
        if (signal?.aborted) throw abortReason(signal)
        if (this.getAccountId() !== expectedAccountId || this.getToken() !== expectedToken) {
            throw new Error('Official account session changed during reauthentication')
        }
        if (this.credentialRouting) {
            await this.credentialRouting.reauthenticate(loginResult)
        } else {
            const database = getDatabase()
            let parsed: Partial<NonNullable<typeof database.account>> | null
            try {
                parsed = JSON.parse(loginResult)
            } catch {
                throw new Error('Invalid official account credential')
            }
            if (!parsed || typeof parsed.id !== 'string' || !parsed.id
                || typeof parsed.token !== 'string' || !parsed.token) {
                throw new Error('Invalid official account credential')
            }
            if (expectedAccountId && parsed.id !== expectedAccountId) {
                throw new Error('Official account changed during reauthentication')
            }
            const credential = {
                ...database.account,
                id: parsed.id,
                token: parsed.token,
                data: parsed.data && typeof parsed.data === 'object' ? parsed.data : {},
            }
            localStorage.setItem("fallbackRisuToken", JSON.stringify(credential))
            database.account = credential
        }
    }


    listItem = this.keys
}

export const accountUnmigrationBusy = writable(false)
let accountUnmigration: Promise<void> | null = null

export function unMigrationAccount(): Promise<void> {
    if (isTauri) {
        return Promise.reject(new Error('Account unmigration is only available on the web'))
    }
    if (accountUnmigration) return accountUnmigration
    accountUnmigration = Promise.resolve()
        .then(performAccountUnmigration)
        .finally(() => {
            accountUnmigration = null
            accountUnmigrationBusy.set(false)
            alertClear()
        })
    accountUnmigrationBusy.set(true)
    alertStore.set({ type: 'wait', msg: language.accountUnmigration.preparing })
    return accountUnmigration
}

async function performAccountUnmigration(): Promise<void> {
    const snapshot = await materializePersistentDatabaseSnapshotWithRevision(
        'account-unmigration',
        { includePluginStorageValues: true },
    )
    const db = snapshot.database
    const expectedRevision = snapshot.revision
    const expectedMutationGeneration = snapshot.mutationGeneration
    const { materializeAccountUnmigrationResources } = await import("./databaseRestore")
    const { resolveBlobStore } = await import("./platformBlobStore")
    const { selectLegacyBackupAssetKeys } = await import("../drive/backupAssets")
    const { storeActiveAsset } = await import("./accountAssetAccess")
    const {
        getAccountColdStorageItem,
        isColdStorageBackupData,
        listColdDataKeys,
    } = await import("../process/coldstorage.svelte")
    const { expandColdPayloads } = await import("../process/coldPayloadExpansion")
    const blobStore = await resolveBlobStore()
    const accountStorage = new AccountStorage()
    const coldKeys = await listColdDataKeys(db)

    await completeAccountUnmigration(db, {
        onPostCommitError: (error) => {
            console.error('Committed account unmigration follow-up failed', error)
            alertError(language.risuNest.persistentData.followupFailed)
        },
        prepareResources: async (candidate) => {
            const selectedCold = await materializeAccountUnmigrationResources({
                coldKeys,
                onProgress: (stage, completed, total) => {
                    alertStore.set({
                        type: 'wait',
                        msg: `${language.accountUnmigration[stage]} (${completed}/${total})`,
                    })
                },
                collectAssetKeys: (selectedCold) => {
                    const chars = db.characters.map((character) => {
                        if (!character.coldstorage) return character
                        const selected = selectedCold.get(character.coldstorage) as
                            | {
                                  character?: typeof character
                              }
                            | undefined
                        return selected?.character?.chaId === character.chaId
                            ? selected.character
                            : character
                    })
                    return selectLegacyBackupAssetKeys(getUncleanablesSync(db, 'pure', { chars }))
                },
                isValidCold: isColdStorageBackupData,
                readLocalAsset: (key) => blobStore.read(key),
                readRemoteAsset: async (key) => {
                    const result = await accountStorage.readItem(key)
                    return result.kind === 'missing' ? null : result.bytes
                },
                writeLocalAsset: async (key, bytes) => {
                    const name = key.replace(/\\/g, '/').split('/').pop() ?? key
                    await storeActiveAsset(blobStore, key, bytes, {
                        kind: 'asset',
                        mime: '',
                        name,
                        ext: name.split('.').pop() ?? '',
                    })
                },
                readRemoteCold: async (key) => {
                    const value = await getAccountColdStorageItem(key)
                    if (value !== null && !isColdStorageBackupData(value)) {
                        throw new Error(`Invalid account cold payload: ${key}`)
                    }
                    return value
                },
            })
            await expandColdPayloads(candidate, async (key) => selectedCold.get(key) ?? null)
        },
        replaceDatabase: (database, reason, options) => {
            alertStore.set({ type: 'wait', msg: language.accountUnmigration.finishing })
            // Keep the snapshot guards: concurrent edits must survive a failed transition.
            return replacePersistentDatabase(database, reason, {
                ...options,
                authoritative: true,
                expectedRevision,
                expectedMutationGeneration,
                pluginStorageValues: snapshot.pluginStorageValues,
            })
        },
        finalize: async () => {
            alertClear()
            const markers = getDeviceMarkers()
            markers.setItem('dosync', 'avoid')
            markers.removeItem('accountst')
            await markers.flush()
            localStorage.removeItem('fallbackRisuToken')
            location.reload()
        },
    })
}
