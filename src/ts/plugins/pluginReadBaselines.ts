export type PluginReadKind = 'database' | 'character' | 'conversation' | 'preset'

export interface PluginReadProvenance {
    path: Array<string | number>
    token: string
}

const provenance = new WeakMap<object, string>()

export function attachPluginReadProvenance(value: unknown, entries: readonly PluginReadProvenance[]): void {
    for (const entry of entries) {
        let object: any = value
        for (const key of entry.path) object = object?.[key]
        if (object && typeof object === 'object') provenance.set(object, entry.token)
    }
}

export function collectPluginReadProvenance(value: unknown): PluginReadProvenance[] {
    const entries: PluginReadProvenance[] = []
    const seen = new WeakSet<object>()
    const visit = (object: unknown, path: Array<string | number>) => {
        if (!object || typeof object !== 'object' || seen.has(object)) return
        seen.add(object)
        const token = provenance.get(object)
        if (token) entries.push({ path, token })
        for (const [key, child] of Object.entries(object)) {
            visit(child, [...path, Array.isArray(object) ? Number(key) : key])
        }
    }
    visit(value, [])
    return entries
}

function canonical(value: unknown): string {
    if (value === undefined) return 'undefined'
    if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']'
    if (value && typeof value === 'object') {
        return '{' + Object.keys(value).sort().map(key => JSON.stringify(key) + ':' + canonical((value as any)[key])).join(',') + '}'
    }
    return JSON.stringify(value)
}

export interface PluginFieldIntent {
    path: Array<string | number>
    type: 'set' | 'delete'
    value?: unknown
}

// Record arrays merge by identity; other arrays remain a single field intent.
function recordIdentity(path: readonly (string | number)[]): string | undefined {
    const field = path.at(-1)
    if (field === 'characters') return 'chaId'
    if (field === 'plugins') return 'name'
    if (['chats', 'botPresets', 'personas', 'modules', 'loadouts', 'customModels'].includes(String(field))) return 'id'
    return undefined
}

export function diffPluginReadBaseline(baseline: any, submitted: any, kind: PluginReadKind): PluginFieldIntent[] {
    const intents: PluginFieldIntent[] = []
    const visit = (before: any, after: any, path: Array<string | number>, partial = false) => {
        if (canonical(before) === canonical(after)) return
        const id = recordIdentity(path)
        if (id && Array.isArray(before) && Array.isArray(after) && [...before, ...after].every(record => typeof record?.[id] === 'string')) {
            const oldRecords = new Map(before.map(record => [record[id], record]))
            const newRecords = new Map(after.map(record => [record[id], record]))
            for (const key of new Set([...oldRecords.keys(), ...newRecords.keys()])) {
                const recordPath = [...path, String(key)]
                if (!newRecords.has(key)) intents.push({ path: recordPath, type: 'delete' })
                else if (!oldRecords.has(key)) intents.push({ path: recordPath, type: 'set', value: structuredClone(newRecords.get(key)) })
                else if (['plugins', 'modules', 'loadouts', 'customModels'].includes(String(path.at(-1)))) {
                    if (canonical(oldRecords.get(key)) !== canonical(newRecords.get(key))) intents.push({ path: recordPath, type: 'set', value: structuredClone(newRecords.get(key)) })
                } else visit(oldRecords.get(key), newRecords.get(key), recordPath)
            }
            if (canonical(before.map(record => record[id])) !== canonical(after.map(record => record[id]))) {
                intents.push({ path: [...path, '$order'], type: 'set', value: after.map(record => record[id]) })
            }
            return
        }
        if (before && after && !Array.isArray(before) && !Array.isArray(after) && typeof before === 'object' && typeof after === 'object') {
            for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) {
                if (!Object.hasOwn(after, key)) {
                    if (!partial) {
                        if (recordIdentity([...path, key]) && Array.isArray(before[key])) visit(before[key], [], [...path, key])
                        else intents.push({ path: [...path, key], type: 'delete' })
                    }
                } else if (!Object.hasOwn(before, key)) {
                    if (recordIdentity([...path, key]) && Array.isArray(after[key])) visit([], after[key], [...path, key])
                    else if (kind === 'database' && path.length === 0 && ['pluginCustomStorage', 'globalChatVariables'].includes(key)) visit({}, after[key], [...path, key])
                    else intents.push({ path: [...path, key], type: 'set', value: structuredClone(after[key]) })
                } else {
                    // Fields, metadata values and message lists are atomic units.
                    if (recordIdentity([...path, key]) || (kind === 'database' && path.length === 0 && ['pluginCustomStorage', 'globalChatVariables'].includes(key))) visit(before[key], after[key], [...path, key])
                    else if (canonical(before[key]) !== canonical(after[key])) intents.push({ path: [...path, key], type: 'set', value: structuredClone(after[key]) })
                }
            }
            return
        }
        intents.push({ path, type: 'set', value: structuredClone(after) })
    }
    visit(baseline, submitted, [], kind === 'database')
    return intents.sort((a, b) => canonical(a.path) < canonical(b.path) ? -1 : canonical(a.path) > canonical(b.path) ? 1 : 0)
}

// Parents keep only child targets; a child's content lives once, in its own newest read.
type StoredValue =
    | { kind: 'conversation' | 'preset'; value: unknown }
    | { kind: 'character'; detail: unknown; chats?: string[] }
    | { kind: 'database'; fields: Map<string, { children: PluginReadKind; targets: string[] } | { value: unknown }> }

interface Baseline {
    token: string
    authority: number
    stored: StoredValue
}

const nestedRecords = [['characters', 'character', 'chaId'], ['botPresets', 'preset', 'id']] as const

export class PluginReadBaselineError extends Error {
    constructor(reason: 'stale' | 'target' | 'closed') {
        super(`Plugin whole-object write rejected: ${reason} read baseline`)
        this.name = 'PluginReadBaselineError'
    }
}

/**
 * Keeps the newest completed read of each target. Database reads keep each
 * top-level key separately, so a read of fewer keys replaces only those keys.
 */
export class PluginReadBaselines {
    private readonly baselines = new Map<string, Baseline>()
    private readonly tokens = new Map<string, { kind: PluginReadKind; target: string }>()
    private readonly history = new Set<string>()
    private closed = false

    constructor(private readonly owner: string, private readonly authority: () => number) {}

    get retainedBaselineCount(): number { return this.baselines.size }

    expire(): void { this.baselines.clear(); this.tokens.clear() }
    close(): void { this.closed = true; this.expire() }

    assertOpen(): void {
        if (this.closed) throw new PluginReadBaselineError('closed')
    }

    hasRead(kind: PluginReadKind, target: string): boolean {
        return this.history.has(JSON.stringify([this.owner, kind, target]))
    }

    track<T>(value: T, kind: PluginReadKind, target: string, _revision: number, expectedAuthority = this.authority()): T {
        if (this.closed) throw new PluginReadBaselineError('closed')
        if (expectedAuthority !== this.authority()) throw new PluginReadBaselineError('stale')
        this.store(value, kind, target, expectedAuthority)
        return value
    }

    private store(value: unknown, kind: PluginReadKind, target: string, authority: number): void {
        const key = JSON.stringify([kind, target])
        let baseline = this.baselines.get(key)
        if (!baseline || baseline.authority !== authority) {
            if (baseline) this.tokens.delete(baseline.token)
            baseline = { token: crypto.randomUUID(), authority, stored: { kind: 'database', fields: new Map() } }
            this.baselines.set(key, baseline)
            this.tokens.set(baseline.token, { kind, target })
        }
        this.history.add(JSON.stringify([this.owner, kind, target]))
        if (value && typeof value === 'object') provenance.set(value, baseline.token)
        const record = value as any
        if (kind === 'database') {
            if (baseline.stored.kind !== 'database') baseline.stored = { kind, fields: new Map() }
            const fields = baseline.stored.fields
            for (const field of record && typeof record === 'object' ? Object.keys(record) : []) {
                const nested = nestedRecords.find(([name]) => name === field)
                if (nested && Array.isArray(record[field])) {
                    const [, childKind, id] = nested
                    for (const child of record[field]) this.store(child, childKind, child?.[id], authority)
                    fields.set(field, { children: childKind, targets: record[field].map((child: any) => child?.[id]) })
                } else fields.set(field, { value: structuredClone(record[field]) })
            }
        } else if (kind === 'character' && record && typeof record === 'object' && Array.isArray(record.chats)) {
            const { chats, ...detail } = record
            const targets = chats.map((chat: any) => JSON.stringify([target, chat?.id]))
            chats.forEach((chat: unknown, index: number) => this.store(chat, 'conversation', targets[index], authority))
            baseline.stored = { kind, detail: structuredClone(detail), chats: targets }
        } else if (kind === 'character') baseline.stored = { kind, detail: structuredClone(value) }
        else baseline.stored = { kind: kind as 'conversation' | 'preset', value: structuredClone(value) }
    }

    private current(kind: PluginReadKind, target: string): Baseline | undefined {
        const baseline = this.baselines.get(JSON.stringify([kind, target]))
        return baseline?.authority === this.authority() ? baseline : undefined
    }

    // Composes a read value by reference from current baselines; callers never mutate it.
    private compose(kind: PluginReadKind, target: string): unknown {
        const stored = this.current(kind, target)?.stored
        if (!stored) return undefined
        if (stored.kind === 'character') {
            if (!stored.chats) return stored.detail
            return { ...stored.detail as object, chats: stored.chats.map(chat => this.compose('conversation', chat)).filter(chat => chat !== undefined) }
        }
        if (stored.kind !== 'database') return stored.value
        const result: Record<string, unknown> = {}
        for (const [field, entry] of stored.fields) {
            const value = 'children' in entry
                ? entry.targets.map(child => this.compose(entry.children, child)).filter(child => child !== undefined)
                : entry.value
            Object.defineProperty(result, field, { value, enumerable: true, configurable: true, writable: true })
        }
        return result
    }

    // Resolves the baseline of a submitted value: its token's target, or the newest read of the target.
    private resolve(value: unknown, kind: PluginReadKind, target: string, fallback: unknown, staleWithoutRead: boolean): unknown {
        const token = value && typeof value === 'object' ? provenance.get(value) : undefined
        if (token) {
            const read = this.tokens.get(token)
            const baseline = read && this.current(read.kind, read.target)
            if (!read || baseline?.token !== token) throw new PluginReadBaselineError('stale')
            if (read.kind !== kind || read.target !== target) throw new PluginReadBaselineError('target')
            return this.compose(kind, target)
        }
        if (this.current(kind, target)) return this.compose(kind, target)
        if (staleWithoutRead && this.hasRead(kind, target)) throw new PluginReadBaselineError('stale')
        return fallback
    }

    private withNestedBaselines(baseline: unknown, submitted: any, kind: PluginReadKind, target: string): unknown {
        if (!baseline || typeof baseline !== 'object' || !submitted || typeof submitted !== 'object') return baseline
        const overlay = (records: unknown, children: unknown, childKind: PluginReadKind, id: string, childTarget: (child: any) => string) => {
            if (!Array.isArray(children)) return records
            let result = Array.isArray(records) ? records : undefined
            const positions = new Map<unknown, number>()
            result?.forEach((record: any, index) => { if (!positions.has(record?.[id])) positions.set(record?.[id], index) })
            for (const child of children) {
                const index = positions.get(child?.[id]) ?? -1
                const previous = index >= 0 ? result![index] : undefined
                const target = childTarget(child)
                const next = this.withNestedBaselines(this.resolve(child, childKind, target, previous, true), child, childKind, target)
                if (next === undefined || next === previous) continue
                if (result === records) result = [...(result ?? [])]
                if (index >= 0) result![index] = next
                else positions.set(child?.[id], result!.push(next) - 1)
            }
            return result
        }
        let result = baseline as Record<string, unknown>
        const replace = (field: string, value: unknown) => {
            if (value === result[field]) return
            if (result === baseline) result = { ...result }
            Object.defineProperty(result, field, { value, enumerable: true, configurable: true, writable: true })
        }
        if (kind === 'database') {
            for (const [field, childKind, id] of nestedRecords) {
                replace(field, overlay(result[field], submitted[field], childKind, id, child => child?.[id]))
            }
        } else if (kind === 'character') {
            replace('chats', overlay(result.chats, submitted.chats, 'conversation', 'id', chat => JSON.stringify([target, chat?.id])))
        }
        return result
    }

    intent(value: unknown, kind: PluginReadKind, target: string, admission: unknown): PluginFieldIntent[] {
        if (this.closed) throw new PluginReadBaselineError('closed')
        const baseline = this.resolve(value, kind, target, admission, true)
        return diffPluginReadBaseline(this.withNestedBaselines(baseline, value, kind, target), value, kind)
    }
}

export function rebasePluginFieldIntents<T>(latest: T, intents: readonly PluginFieldIntent[]): T {
    const result: any = structuredClone(latest)
    for (const intent of intents) {
        let parent = result
        for (let index = 0; index < intent.path.length - 1; index++) {
            const key = intent.path[index]
            if (Array.isArray(parent)) {
                const identity = recordIdentity(intent.path.slice(0, index))
                parent = parent.find(record => record?.[identity!] === key)
            } else parent = parent?.[key]
            if (!parent) throw new PluginReadBaselineError('target')
        }
        const key = intent.path.at(-1)!
        if (Array.isArray(parent)) {
            const identity = recordIdentity(intent.path.slice(0, -1))!
            if (key === '$order') {
                const order = intent.value as string[]
                const ordered = order.map(id => parent.find(record => record[identity] === id)).filter(Boolean)
                parent.splice(0, parent.length, ...ordered, ...parent.filter(record => !order.includes(record[identity])))
            } else {
                const index = parent.findIndex(record => record[identity] === key)
                if (intent.type === 'delete') { if (index >= 0) parent.splice(index, 1) }
                else if (index >= 0) parent[index] = structuredClone(intent.value)
                else parent.push(structuredClone(intent.value))
            }
        } else if (intent.type === 'delete') delete parent[key]
        else Object.defineProperty(parent, key, { value: structuredClone(intent.value), enumerable: true, configurable: true, writable: true })
    }
    return result
}
