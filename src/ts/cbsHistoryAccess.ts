import type { Message } from './storage/database.svelte'

export interface CbsHistoryChange {
    start: number
    end: number
    countChanged: boolean
}

export function hasCbsHistoryChanges(snapshot: {
    historyVersion?: number
    historyChangesSince?(version: number): readonly CbsHistoryChange[] | null
}, version: number | undefined, dependencies: CbsHistoryDependencies): boolean {
    return snapshot.historyVersion !== version
        && dependencies.affectedBy(version === undefined ? null : snapshot.historyChangesSince?.(version) ?? null)
}

export class CbsHistoryDependencies {
    count = false
    all = false
    private ranges: [number, number][] = []

    range(start: number, end: number) {
        if (end <= start) return
        const retained: [number, number][] = []
        for (const range of this.ranges) {
            if (range[1] < start || range[0] > end) retained.push(range)
            else { start = Math.min(start, range[0]); end = Math.max(end, range[1]) }
        }
        retained.push([start, end])
        this.ranges = retained
    }

    affectedBy(changes: readonly CbsHistoryChange[] | null): boolean {
        if (changes === null) return this.all || this.count || this.ranges.length > 0
        return changes.some((change) => this.all || (this.count && change.countChanged)
            || this.ranges.some(([start, end]) => start < change.end && end > change.start))
    }
}

export class CbsHistoryAccessError extends Error {
    constructor(readonly access: string) {
        super(`CBS history was not prepared: ${access}`)
        this.name = 'CbsHistoryAccessError'
    }
}

export interface CbsHistoryAccess {
    readonly length: number
    readonly dependencies: CbsHistoryDependencies
    peek(index: number): Message | undefined
    preparedMessages(): Iterable<Message>
    preparedRows(): Iterable<[number, Message]>
    row(index: number): Message | undefined
    previous(role: Message['role'], before: number, count: number): readonly Message[]
    all(): readonly Message[]
    assertComplete(): void
}

export function createCbsHistoryAccess(input: {
    length: number
    rows: ReadonlyMap<number, Message>
    searches?: ReadonlyMap<string, readonly Message[]>
    complete?: readonly Message[]
    dependencies?: CbsHistoryDependencies
}): CbsHistoryAccess {
    let missing: CbsHistoryAccessError | undefined
    function miss(access: string): never {
        missing ??= new CbsHistoryAccessError(access)
        throw missing
    }
    return {
        length: input.length,
        dependencies: input.dependencies ?? Object.assign(new CbsHistoryDependencies(), { all: true }),
        peek(index) { return input.rows.get(index) ?? input.complete?.[index] },
        preparedMessages() { return input.complete ?? input.rows.values() },
        preparedRows() { return input.complete?.entries() ?? input.rows.entries() },
        row(index) {
            if (!Number.isInteger(index) || index < 0 || index >= input.length) return undefined
            return input.rows.get(index) ?? input.complete?.[index] ?? miss(`row ${index}`)
        },
        previous(role, before, count) {
            if (before <= 0) return []
            const prepared = input.searches?.get(cbsHistorySearchKey(role, before, count))
            if (prepared) return prepared
            if (input.complete) {
                const result: Message[] = []
                for (let index = Math.min(before, input.length) - 1; index >= 0 && result.length < count; index--) {
                    if (input.complete[index].role === role) result.push(input.complete[index])
                }
                return result
            }
            return miss(`${role} before ${before}, count ${count}`)
        },
        all() { return input.complete ?? miss('complete history') },
        assertComplete() { if (missing) throw missing },
    }
}

export function cbsHistorySearchKey(role: Message['role'], before: number, count: number): string {
    return `${role}:${before}:${count}`
}
