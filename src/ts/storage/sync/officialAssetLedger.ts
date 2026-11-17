const LEDGER_VERSION = 1

export interface OfficialAssetLedger {
    /** The key the account already holds this asset under, or null when it was never published. */
    publishedAs(key: string): string | null
    record(key: string, replacementKey: string): void
    clear(): void
    reset(): void
}

interface LedgerRecord {
    version: number
    assets: Record<string, string>
}

export interface LedgerStorage {
    getItem(key: string): string | null
    setItem(key: string, value: string): void
    removeItem(key: string): void
}

function emptyRecord(): LedgerRecord {
    return { version: LEDGER_VERSION, assets: {} }
}

function parseRecord(raw: string | null): LedgerRecord {
    if (!raw) return emptyRecord()
    try {
        const parsed = JSON.parse(raw) as Partial<LedgerRecord>
        if (parsed?.version !== LEDGER_VERSION) return emptyRecord()
        return {
            version: LEDGER_VERSION,
            assets: { ...parsed.assets },
        }
    } catch {
        return emptyRecord()
    }
}

/**
 * Remembers what an account already holds so an ordinary publish uploads only what changed.
 * Asset keys are content addressed, so a recorded key stays valid.
 */
export function createOfficialAssetLedger(
    storage: LedgerStorage,
    accountId: string,
): OfficialAssetLedger {
    const storageKey = `officialPublishedAssets:${accountId}`
    let record = parseRecord(storage.getItem(storageKey))
    const persist = () => {
        try {
            storage.setItem(storageKey, JSON.stringify(record))
        } catch (error) {
            console.error('Failed to persist the official publication ledger', error)
        }
    }

    return {
        publishedAs: (key) => record.assets[key] ?? null,
        record(key, replacementKey) {
            if (record.assets[key] === replacementKey) return
            record.assets[key] = replacementKey
            persist()
        },
        clear() {
            record = emptyRecord()
            storage.removeItem(storageKey)
        },
        reset() {
            record = emptyRecord()
        },
    }
}

export function createUnrecordedOfficialAssetLedger(): OfficialAssetLedger {
    return {
        publishedAs: () => null,
        record: () => undefined,
        clear: () => undefined,
        reset: () => undefined,
    }
}

/**
 * Resolves the recording ledger from the live account id on every call, so a session that only
 * learns its account identity after the first pull still records replacement keys.
 */
export function createAccountScopedOfficialAssetLedger(
    storage: LedgerStorage,
    resolveAccountId: () => string | undefined,
): OfficialAssetLedger {
    const unrecorded = createUnrecordedOfficialAssetLedger()
    let boundId: string | null = null
    let bound: OfficialAssetLedger = unrecorded
    const resolve = (): OfficialAssetLedger => {
        const accountId = resolveAccountId()
        if (!accountId) {
            boundId = null
            bound = unrecorded
        } else if (accountId !== boundId) {
            boundId = accountId
            bound = createOfficialAssetLedger(storage, accountId)
        }
        return bound
    }
    return {
        publishedAs: (key) => resolve().publishedAs(key),
        record: (key, replacementKey) => resolve().record(key, replacementKey),
        clear: () => resolve().clear(),
        reset() {
            boundId = null
            bound = unrecorded
        },
    }
}

export interface NativeOfficialAssetLedger extends OfficialAssetLedger {
    flush(): Promise<void>
    clearAll(): Promise<void>
}

interface CheckpointEntry {
    key: string
    record: LedgerRecord
    removed: boolean
    generation: number
    persisted: number
    due: number | null
    timer?: ReturnType<typeof setTimeout>
    pending?: Promise<void>
}

/** Native snapshots are serialized only at a checkpoint or an explicit settlement. */
export async function createNativeOfficialAssetLedger(
    settings: {
        get(key: string): Promise<unknown | null>
        set(key: string, value: unknown | null): Promise<void>
        patch(key: string, entries: Record<string, string | null>): Promise<void>
    },
    storageKey: string,
    resolveAccountId: () => string | undefined,
): Promise<NativeOfficialAssetLedger> {
    const interval = 30_000
    let loaded: Record<string, string> = {}
    let loadFailure: unknown
    try {
        const value = await settings.get(storageKey)
        if (value != null) {
            if (typeof value !== 'object' || Array.isArray(value)
                || Object.values(value).some(entry => typeof entry !== 'string')) {
                throw new Error('Official publication ledger is unreadable')
            }
            loaded = value as Record<string, string>
        }
    } catch (error) { loadFailure = error }
    const entries = new Map<string, CheckpointEntry>()
    const entryFor = (key: string): CheckpointEntry => {
        let entry = entries.get(key)
        if (!entry) {
            entry = { key, record: parseRecord(loaded[key] ?? null), removed: false, generation: 0, persisted: 0, due: null }
            entries.set(key, entry)
            delete loaded[key]
        }
        return entry
    }
    const current = () => {
        const accountId = resolveAccountId()
        return accountId ? entryFor(`officialPublishedAssets:${accountId}`) : null
    }
    const disarm = (entry: CheckpointEntry) => {
        clearTimeout(entry.timer)
        entry.timer = undefined
    }
    const arm = (entry: CheckpointEntry) => {
        if (entry.pending || entry.timer !== undefined || entry.generation === entry.persisted) return
        entry.timer = setTimeout(() => {
            entry.timer = undefined
            void write(entry).catch(() => {})
        }, Math.max(0, (entry.due ?? Date.now() + interval) - Date.now()))
    }
    const write = (entry: CheckpointEntry): Promise<void> => {
        if (entry.pending) return entry.pending
        if (entry.generation === entry.persisted) return Promise.resolve()
        disarm(entry)
        const generation = entry.generation
        const value = entry.removed ? null : JSON.stringify(entry.record)
        entry.due = null
        entry.pending = settings.patch(storageKey, { [entry.key]: value }).then(() => {
            entry.persisted = generation
        }, error => {
            entry.due = Math.max(entry.due ?? 0, Date.now() + interval)
            throw error
        }).finally(() => {
            entry.pending = undefined
            arm(entry)
        })
        return entry.pending
    }
    const changed = (entry: CheckpointEntry, remove = false) => {
        entry.removed = remove
        entry.generation++
        entry.due ??= Date.now() + interval
        arm(entry)
    }
    const clearEntry = (entry: CheckpointEntry) => {
        entry.record = emptyRecord()
        changed(entry, true)
    }
    const flush = async () => {
        const results = await Promise.allSettled([...entries.values()].map(async entry => {
            disarm(entry)
            if (entry.pending) await entry.pending.catch(() => {})
            while (entry.generation !== entry.persisted) await write(entry)
        }))
        const errors = results.flatMap(result => result.status === 'rejected' ? [result.reason] : [])
        if (loadFailure !== undefined) { errors.push(loadFailure); loadFailure = undefined }
        if (errors.length) throw new AggregateError(errors, 'Official publication metadata could not be saved')
    }
    return {
        publishedAs: key => current()?.record.assets[key] ?? null,
        record(key, replacementKey) {
            const entry = current()
            if (!entry || entry.record.assets[key] === replacementKey) return
            entry.record.assets[key] = replacementKey
            changed(entry)
        },
        clear() {
            const entry = current()
            if (entry) clearEntry(entry)
        },
        // Account entries remain owned until explicitly cleared, including dirty generations.
        reset() {},
        flush,
        async clearAll() {
            for (const key of Object.keys(loaded)) entryFor(key)
            for (const entry of entries.values()) clearEntry(entry)
            await Promise.allSettled([...entries.values()].flatMap(entry => entry.pending ? [entry.pending] : []))
            for (const entry of entries.values()) disarm(entry)
            try {
                await settings.set(storageKey, null)
                entries.clear()
                loaded = {}
                loadFailure = undefined
            } finally {
                for (const entry of entries.values()) arm(entry)
            }
        },
    }
}
