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

interface Baseline {
    token: string
    kind: PluginReadKind
    target: string
    revision: number
    authority: number
    value: unknown
}

export class PluginReadBaselineError extends Error {
    constructor(reason: 'stale' | 'ambiguous' | 'target' | 'closed') {
        super(`Plugin whole-object write rejected: ${reason} read baseline`)
        this.name = 'PluginReadBaselineError'
    }
}

export class PluginReadBaselines {
    private readonly baselines = new Map<string, Baseline>()
    private readonly history = new Set<string>()
    private closed = false

    constructor(private readonly owner: string, private readonly authority: () => number) {}

    expire(): void { this.baselines.clear() }
    close(): void { this.closed = true; this.expire() }

    assertOpen(): void {
        if (this.closed) throw new PluginReadBaselineError('closed')
    }

    hasRead(kind: PluginReadKind, target: string): boolean {
        return this.history.has(JSON.stringify([this.owner, kind, target]))
    }

    track<T>(value: T, kind: PluginReadKind, target: string, revision: number, expectedAuthority = this.authority()): T {
        if (this.closed) throw new PluginReadBaselineError('closed')
        if (expectedAuthority !== this.authority()) throw new PluginReadBaselineError('stale')
        const token = crypto.randomUUID()
        const baseline: Baseline = { token, kind, target, revision, authority: expectedAuthority, value: structuredClone(value) }
        this.baselines.set(token, baseline)
        this.history.add(JSON.stringify([this.owner, kind, target]))
        if (value && typeof value === 'object') provenance.set(value, token)
        if (kind === 'database') {
            const database = value as any
            for (const character of database.characters ?? []) this.track(character, 'character', character.chaId, revision, expectedAuthority)
            for (const preset of database.botPresets ?? []) this.track(preset, 'preset', preset.id, revision, expectedAuthority)
        }
        if (kind === 'character') {
            for (const chat of (value as any).chats ?? []) this.track(chat, 'conversation', JSON.stringify([target, chat.id]), revision, expectedAuthority)
        }
        return value
    }

    private withNestedBaselines(baseline: unknown, submitted: any, kind: PluginReadKind, target: string): unknown {
        const result: any = structuredClone(baseline)
        const overlay = (before: any, after: any, childKind: PluginReadKind, childTarget: string) => {
            const token = after && typeof after === 'object' ? provenance.get(after) : undefined
            if (token) {
                const read = this.baselines.get(token)
                if (!read || read.authority !== this.authority()) throw new PluginReadBaselineError('stale')
                if (read.kind !== childKind || read.target !== childTarget) throw new PluginReadBaselineError('target')
                before = structuredClone(read.value)
            } else if (this.hasRead(childKind, childTarget)) {
                const compatible = [...this.baselines.values()].filter(read => read.kind === childKind && read.target === childTarget && read.authority === this.authority())
                if (!compatible.length) throw new PluginReadBaselineError('stale')
                const candidates = compatible.map(read => diffPluginReadBaseline(this.withNestedBaselines(read.value, after, childKind, childTarget), after, childKind))
                if (candidates.some(candidate => canonical(candidate) !== canonical(candidates[0]))) throw new PluginReadBaselineError('ambiguous')
                before = structuredClone(compatible[0].value)
            }
            return this.withNestedBaselines(before, after, childKind, childTarget)
        }
        if (kind === 'database' && result && submitted) {
            for (const [field, childKind, id] of [['characters', 'character', 'chaId'], ['botPresets', 'preset', 'id']] as const) {
                if (!Array.isArray(submitted[field])) continue
                for (const child of submitted[field]) {
                    const index = result[field]?.findIndex((record: any) => record[id] === child[id]) ?? -1
                    const previous = index >= 0 ? result[field][index] : undefined
                    const next = overlay(previous, child, childKind, child[id])
                    if (next !== undefined) {
                        result[field] ??= []
                        if (index >= 0) result[field][index] = next
                        else result[field].push(next)
                    }
                }
            }
        } else if (kind === 'character' && result && submitted) {
            for (const chat of submitted.chats ?? []) {
                const index = result.chats?.findIndex((record: any) => record.id === chat.id) ?? -1
                const next = overlay(index >= 0 ? result.chats[index] : undefined, chat, 'conversation', JSON.stringify([target, chat.id]))
                if (next !== undefined) {
                    result.chats ??= []
                    if (index >= 0) result.chats[index] = next
                    else result.chats.push(next)
                }
            }
        }
        return result
    }

    intent(value: unknown, kind: PluginReadKind, target: string, admission: unknown): PluginFieldIntent[] {
        if (this.closed) throw new PluginReadBaselineError('closed')
        const token = value && typeof value === 'object' ? provenance.get(value) : undefined
        if (token) {
            const baseline = this.baselines.get(token)
            if (!baseline || baseline.authority !== this.authority()) throw new PluginReadBaselineError('stale')
            if (baseline.kind !== kind || baseline.target !== target) throw new PluginReadBaselineError('target')
            return diffPluginReadBaseline(this.withNestedBaselines(baseline.value, value, kind, target), value, kind)
        }
        const compatible = [...this.baselines.values()].filter(baseline => baseline.kind === kind && baseline.target === target && baseline.authority === this.authority())
        if (!compatible.length) {
            if (this.history.has(JSON.stringify([this.owner, kind, target]))) throw new PluginReadBaselineError('stale')
            return diffPluginReadBaseline(this.withNestedBaselines(admission, value, kind, target), value, kind)
        }
        const candidates = compatible.map(baseline => diffPluginReadBaseline(this.withNestedBaselines(baseline.value, value, kind, target), value, kind))
        if (candidates.some(candidate => canonical(candidate) !== canonical(candidates[0]))) throw new PluginReadBaselineError('ambiguous')
        return candidates[0]
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
