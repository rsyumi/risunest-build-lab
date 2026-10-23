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
            set: async (_key: string, _value: unknown) => { for (const key of Object.keys(values)) delete values[key] },
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

    it.each(['success', 'failed-once', 'failed-again'] as const)('flushes a later generation after an overlapping checkpoint: %s', async checkpoint => {
        vi.useFakeTimers()
        const h = device()
        const write = h.storage.patch
        let release!: () => void
        let active = 0
        let failWrites = checkpoint === 'failed-again'
        h.storage.patch = async (key, entries) => {
            if (++active !== 1) throw new Error('Overlapping ledger writes')
            try {
                if (!release) {
                    await new Promise<void>(resolve => { release = resolve })
                    if (checkpoint !== 'success') throw new Error('checkpoint write failed')
                }
                if (failWrites) throw new Error('settlement write failed')
                await write(key, entries)
            } finally { active-- }
        }
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => 'a')
        ledger.record('first', 'one')
        await vi.advanceTimersByTimeAsync(30_000)
        ledger.record('second', 'two')
        const settled = ledger.flush()
        const checked = checkpoint === 'failed-again' ? expect(settled).rejects.toThrow() : expect(settled).resolves.toBeUndefined()
        release()
        await checked
        if (checkpoint === 'failed-again') {
            expect(h.reader('a').publishedAs('second')).toBeNull()
            failWrites = false
            await ledger.flush()
        }
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

    it('accepts a durable flush after a timer checkpoint failed', async () => {
        vi.useFakeTimers()
        const h = device()
        const write = h.storage.patch
        h.storage.patch = vi.fn().mockRejectedValueOnce(new Error('disk full')).mockImplementation(write)
        const ledger = await createNativeOfficialAssetLedger(h.storage, 'ledger', () => 'a')
        ledger.record('asset', 'remote')
        await vi.advanceTimersByTimeAsync(30_000)
        expect(h.reader('a').publishedAs('asset')).toBeNull()
        await expect(ledger.flush()).resolves.toBeUndefined()
        expect(h.reader('a').publishedAs('asset')).toBe('remote')
        await ledger.clearAll()
    })

    it('clears unreadable persisted metadata on logout', async () => {
        let stored: unknown = ['unreadable']
        const storage = {
            get: async () => stored,
            set: async (_key: string, value: unknown) => { stored = value },
            patch: async () => { throw new Error('unreadable metadata cannot be patched') },
        }
        const ledger = await createNativeOfficialAssetLedger(storage, 'ledger', () => 'a')
        await expect(ledger.clearAll()).resolves.toBeUndefined()
        expect(stored).toBeNull()
        const reopened = await createNativeOfficialAssetLedger(storage, 'ledger', () => 'a')
        await expect(reopened.flush()).resolves.toBeUndefined()
    })

    it.each(['account', 'all'] as const)('orders %s clearing after an older in-flight snapshot so it cannot reappear', async scope => {
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
        if (scope === 'account') ledger.clear()
        const cleared = scope === 'account' ? ledger.flush() : ledger.clearAll()
        release()
        await cleared
        await vi.advanceTimersByTimeAsync(60_000)
        expect(h.reader('a').publishedAs('asset')).toBeNull()
        expect(Object.keys(h.values)).toEqual([])
    })
})
