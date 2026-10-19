import { afterEach, describe, expect, it, vi } from 'vitest'
import {
    createAccountScopedOfficialAssetLedger,
    createOfficialAssetLedger,
    createNativeOfficialAssetLedger,
    type LedgerStorage,
} from './officialAssetLedger'

function memoryStorage(): LedgerStorage {
    const values = new Map<string, string>()
    return {
        getItem: (key) => values.get(key) ?? null,
        setItem: (key, value) => void values.set(key, value),
        removeItem: (key) => void values.delete(key),
    }
}

describe('createAccountScopedOfficialAssetLedger', () => {
    it('starts unrecorded and begins recording once the account id is known', () => {
        const storage = memoryStorage()
        let accountId: string | undefined
        const ledger = createAccountScopedOfficialAssetLedger(storage, () => accountId)

        ledger.record('assets/a.png', 'remote/assets/a.png')
        expect(ledger.publishedAs('assets/a.png')).toBeNull()

        accountId = 'acct-1'
        ledger.record('assets/a.png', 'remote/assets/a.png')
        expect(ledger.publishedAs('assets/a.png')).toBe('remote/assets/a.png')
        expect(createOfficialAssetLedger(storage, 'acct-1').publishedAs('assets/a.png'))
            .toBe('remote/assets/a.png')
    })

    it('keeps recorded keys separated per account id', () => {
        const storage = memoryStorage()
        let accountId: string | undefined = 'acct-1'
        const ledger = createAccountScopedOfficialAssetLedger(storage, () => accountId)
        ledger.record('assets/a.png', 'remote/a')

        accountId = 'acct-2'
        expect(ledger.publishedAs('assets/a.png')).toBeNull()

        accountId = 'acct-1'
        expect(ledger.publishedAs('assets/a.png')).toBe('remote/a')
    })
})

describe('native official publication checkpoints', () => {
    afterEach(() => vi.useRealTimers())

    function device() {
        const values: Record<string, string> = {}
        const storage = {
            get: async () => ({ ...values }),
            patch: async (_key: string, entries: Record<string, string | null>) => {
                for (const [key, value] of Object.entries(entries)) {
                    if (value === null) delete values[key]
                    else values[key] = value
                }
            },
        }
        const reader = (account: string) => createOfficialAssetLedger({
            getItem: key => values[key] ?? null, setItem() {}, removeItem() {},
        }, account)
        return { values, storage, reader }
    }

    it('keeps live records without serializing or persisting until the first dirty checkpoint', async () => {
        vi.useFakeTimers()
        const h = device()
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => 'a')
        const stringify = vi.spyOn(JSON, 'stringify')
        try {
            for (let index = 0; index < 1000; index++) ledger.record(`asset-${index}`, `remote-${index}`)
            expect(stringify).not.toHaveBeenCalled()
            expect(ledger.publishedAs('asset-999')).toBe('remote-999')
            expect(h.reader('a').publishedAs('asset-999')).toBeNull()
            await vi.advanceTimersByTimeAsync(29_999)
            ledger.record('late', 'remote-late')
            await vi.advanceTimersByTimeAsync(1)
            expect(h.reader('a').publishedAs('asset-999')).toBe('remote-999')
            expect(h.reader('a').publishedAs('late')).toBe('remote-late')
        } finally { stringify.mockRestore() }
        await ledger.clearAll()
    })

    it('flushes a later generation after an overlapping checkpoint without concurrent snapshots', async () => {
        vi.useFakeTimers()
        const h = device()
        const write = h.storage.patch
        let release!: () => void
        let active = 0
        h.storage.patch = async (key, entries) => {
            if (++active !== 1) throw new Error('Overlapping ledger writes')
            try {
                if (!release) await new Promise<void>(resolve => { release = resolve })
                await write(key, entries)
            } finally { active-- }
        }
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => 'a')
        ledger.record('first', 'one')
        await vi.advanceTimersByTimeAsync(30_000)
        ledger.record('second', 'two')
        const settled = ledger.flush()
        release()
        await settled
        expect(h.reader('a').publishedAs('first')).toBe('one')
        expect(h.reader('a').publishedAs('second')).toBe('two')
        await ledger.clearAll()
    })

    it('retains dirty entries across account changes and reset and retries a failed durable write', async () => {
        const h = device()
        let account = 'a'
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => account)
        ledger.record('asset', 'remote-a')
        account = 'b'
        ledger.record('asset', 'remote-b')
        ledger.reset()
        account = 'a'
        expect(ledger.publishedAs('asset')).toBe('remote-a')
        const write = h.storage.patch
        h.storage.patch = async (key, entries) => {
            if ('officialPublishedAssets:a' in entries) throw new Error('disk full')
            return write(key, entries)
        }
        await expect(ledger.flush()).rejects.toThrow()
        expect(h.reader('b').publishedAs('asset')).toBe('remote-b')
        h.storage.patch = write
        await ledger.flush()
        expect(h.reader('a').publishedAs('asset')).toBe('remote-a')
        await ledger.clearAll()
    })

    it('orders clearing after an older in-flight snapshot so it cannot reappear', async () => {
        vi.useFakeTimers()
        const h = device()
        const write = h.storage.patch
        let release!: () => void
        h.storage.patch = async (key, entries) => {
            if (!release) await new Promise<void>(resolve => { release = resolve })
            await write(key, entries)
        }
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => 'a')
        ledger.record('asset', 'remote')
        await vi.advanceTimersByTimeAsync(30_000)
        ledger.clear()
        const cleared = ledger.flush()
        release()
        await cleared
        await vi.advanceTimersByTimeAsync(60_000)
        expect(h.reader('a').publishedAs('asset')).toBeNull()
        expect(Object.keys(h.values)).toEqual([])
    })
})
