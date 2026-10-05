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
function recordIdentityOf(field: string | number | undefined): string | undefined {
    if (field === 'characters') return 'chaId'
    if (field === 'plugins') return 'name'
    if (['chats', 'botPresets', 'personas', 'modules', 'loadouts', 'customModels'].includes(String(field))) return 'id'
    return undefined
}

function recordIdentity(path: readonly (string | number)[]): string | undefined {
    return recordIdentityOf(path.at(-1))
}

const wholeRecordFields = ['plugins', 'modules', 'loadouts', 'customModels']
const databaseRootObjects = ['pluginCustomStorage', 'globalChatVariables']

// Baselines keep a 53-bit digest in place of content. Two values share a digest
// exactly when `canonical` renders them alike, barring a hash collision.
let lane1 = 0
let lane2 = 0

function begin(tag: number): void {
    lane1 = 0xdeadbeef ^ tag
    lane2 = 0x41c6ce57 ^ tag
}

function mix(word: number): void {
    lane1 = Math.imul(lane1 ^ word, 2654435761)
    lane2 = Math.imul(lane2 ^ word, 1597334677)
}

function mixText(text: string): void {
    mix(text.length)
    for (let index = 0; index < text.length; index++) mix(text.charCodeAt(index))
}

function mixDigest(digest: number): void {
    mix(digest >>> 0)
    mix(Math.floor(digest / 4294967296))
}

function end(): number {
    let first = Math.imul(lane1 ^ (lane1 >>> 16), 2246822507)
    first ^= Math.imul(lane2 ^ (lane2 >>> 13), 3266489909)
    let second = Math.imul(lane2 ^ (lane2 >>> 16), 2246822507)
    second ^= Math.imul(first ^ (first >>> 13), 3266489909)
    return 4294967296 * (2097151 & second) + (first >>> 0)
}

const STRING = 1
const PRIMITIVE = 2
const OBJECT = 3
const ARRAY = 4

// Children are digested before their parent begins, so the shared lanes never interleave.
function objectDigest(keys: readonly string[], digests: ArrayLike<number>): number {
    const order = keys.map((_, index) => index).sort((a, b) => keys[a] < keys[b] ? -1 : keys[a] > keys[b] ? 1 : 0)
    begin(OBJECT)
    mix(keys.length)
    for (const index of order) {
        mixText(keys[index])
        mixDigest(digests[index])
    }
    return end()
}

function arrayDigest(digests: ArrayLike<number>): number {
    begin(ARRAY)
    mix(digests.length)
    for (let index = 0; index < digests.length; index++) mixDigest(digests[index])
    return end()
}

function digestValue(value: unknown, memo?: WeakMap<object, number>): number {
    if (typeof value === 'string') {
        begin(STRING)
        mixText(value)
        return end()
    }
    if (!value || typeof value !== 'object') {
        begin(PRIMITIVE)
        mixText((value === undefined ? undefined : JSON.stringify(value)) ?? 'undefined')
        return end()
    }
    const known = memo?.get(value)
    if (known !== undefined) return known
    let digest: number
    if (Array.isArray(value)) {
        const digests = new Float64Array(value.length)
        for (let index = 0; index < value.length; index++) digests[index] = digestValue(value[index], memo)
        digest = arrayDigest(digests)
    } else {
        const keys = Object.keys(value)
        const digests = new Float64Array(keys.length)
        for (let index = 0; index < keys.length; index++) digests[index] = digestValue((value as Record<string, unknown>)[keys[index]], memo)
        digest = objectDigest(keys, digests)
    }
    memo?.set(value, digest)
    return digest
}

// A node keeps what the diff walks: field names, record identities, and the
// nodes of the fields it descends into. Everything else is a digest.
class ValueNode {
    constructor(readonly digest: number, readonly identity?: unknown) {}
}

class ObjectNode {
    constructor(
        readonly digest: number,
        readonly keys: readonly string[],
        readonly digests: Float64Array,
        readonly children: ReadonlyMap<string, BaselineNode>,
        readonly identity?: unknown,
    ) {}
}

class RecordsNode {
    constructor(readonly digest: number, readonly items: readonly BaselineNode[], readonly identity?: unknown) {}
}

type BaselineNode = ValueNode | ObjectNode | RecordsNode

// An object identity never matches a submitted record, whose identity is a clone.
const identityValue = (value: unknown) => value !== null && (typeof value === 'object' || typeof value === 'function') ? {} : value

const descendsInto = (key: string, databaseRoot: boolean) => recordIdentityOf(key) !== undefined || (databaseRoot && databaseRootObjects.includes(key))

function objectNode(keys: readonly string[], nodes: readonly BaselineNode[], databaseRoot: boolean, identity?: unknown): ObjectNode {
    const digests = new Float64Array(keys.length)
    const children = new Map<string, BaselineNode>()
    keys.forEach((key, index) => {
        digests[index] = nodes[index].digest
        if (descendsInto(key, databaseRoot)) children.set(key, nodes[index])
    })
    return new ObjectNode(objectDigest(keys, digests), keys, digests, children, identity)
}

function recordsNode(items: readonly BaselineNode[], identity?: unknown): RecordsNode {
    return new RecordsNode(arrayDigest(items.map(item => item.digest)), items, identity)
}

function fieldNode(value: unknown, field: string, databaseRoot: boolean): BaselineNode {
    return descendsInto(field, databaseRoot) ? buildNode(value, field, false) : new ValueNode(digestValue(value))
}

// `field` is the last path element, which selects the record identity of an array.
function buildNode(value: unknown, field: string | undefined, databaseRoot: boolean, identity?: unknown): BaselineNode {
    if (Array.isArray(value)) {
        const id = recordIdentityOf(field)
        if (!id) return new ValueNode(digestValue(value), identity)
        const whole = wholeRecordFields.includes(field!)
        const items: BaselineNode[] = []
        for (let index = 0; index < value.length; index++) {
            const item = value[index]
            const itemIdentity = identityValue(item?.[id])
            items.push(whole
                ? new ValueNode(digestValue(item), itemIdentity)
                : buildNode(item, typeof itemIdentity === 'string' ? itemIdentity : undefined, false, itemIdentity))
        }
        return recordsNode(items, identity)
    }
    if (value && typeof value === 'object') {
        const keys = Object.keys(value)
        return objectNode(keys, keys.map(key => fieldNode((value as Record<string, unknown>)[key], key, databaseRoot)), databaseRoot, identity)
    }
    return new ValueNode(digestValue(value), identity)
}

function withRecordField(node: ObjectNode, field: string, value: BaselineNode): ObjectNode {
    const position = node.keys.indexOf(field)
    const keys = position < 0 ? [...node.keys, field] : node.keys
    const digests = new Float64Array(keys.length)
    digests.set(node.digests)
    digests[position < 0 ? keys.length - 1 : position] = value.digest
    return new ObjectNode(objectDigest(keys, digests), keys, digests, new Map(node.children).set(field, value), node.identity)
}

function hasStringIdentities(records: readonly unknown[], id: string): boolean {
    for (let index = 0; index < records.length; index++) {
        if (typeof (records[index] as any)?.[id] !== 'string') return false
    }
    return true
}

const emptyRecords = recordsNode([])
const emptyObject = objectNode([], [], false)

function diffBaselineNode(baseline: BaselineNode, submitted: unknown, kind: PluginReadKind): PluginFieldIntent[] {
    const intents: PluginFieldIntent[] = []
    const memo = new WeakMap<object, number>()
    const digestOf = (value: unknown) => digestValue(value, memo)
    const visit = (before: BaselineNode, after: any, path: Array<string | number>, partial = false) => {
        if (before.digest === digestOf(after)) return
        const field = path.at(-1)
        const id = recordIdentityOf(field)
        if (id && before instanceof RecordsNode && Array.isArray(after)
            && before.items.every(record => typeof record.identity === 'string') && hasStringIdentities(after, id)) {
            const oldRecords = new Map<string, BaselineNode>()
            for (const record of before.items) oldRecords.set(record.identity as string, record)
            const newRecords = new Map<string, unknown>(after.map(record => [record[id], record]))
            for (const key of new Set([...oldRecords.keys(), ...newRecords.keys()])) {
                const recordPath = [...path, key]
                if (!newRecords.has(key)) intents.push({ path: recordPath, type: 'delete' })
                else if (!oldRecords.has(key)) intents.push({ path: recordPath, type: 'set', value: structuredClone(newRecords.get(key)) })
                else if (wholeRecordFields.includes(String(field))) {
                    if (oldRecords.get(key)!.digest !== digestOf(newRecords.get(key))) intents.push({ path: recordPath, type: 'set', value: structuredClone(newRecords.get(key)) })
                } else visit(oldRecords.get(key)!, newRecords.get(key), recordPath)
            }
            const order: string[] = after.map(record => record[id])
            if (order.length !== before.items.length || order.some((key, index) => key !== before.items[index].identity)) {
                intents.push({ path: [...path, '$order'], type: 'set', value: order })
            }
            return
        }
        if (before instanceof ObjectNode && after && typeof after === 'object' && !Array.isArray(after)) {
            const databaseRoot = kind === 'database' && path.length === 0
            const positions = new Map(before.keys.map((key, index) => [key, index]))
            for (const key of new Set([...before.keys, ...Object.keys(after)])) {
                const position = positions.get(key)
                if (!Object.hasOwn(after, key)) {
                    if (!partial) {
                        const child = before.children.get(key)
                        if (recordIdentityOf(key) && child instanceof RecordsNode) visit(child, [], [...path, key])
                        else intents.push({ path: [...path, key], type: 'delete' })
                    }
                } else if (position === undefined) {
                    if (recordIdentityOf(key) && Array.isArray(after[key])) visit(emptyRecords, after[key], [...path, key])
                    else if (databaseRoot && databaseRootObjects.includes(key)) visit(emptyObject, after[key], [...path, key])
                    else intents.push({ path: [...path, key], type: 'set', value: structuredClone(after[key]) })
                } else {
                    // Fields, metadata values and message lists are atomic units.
                    const child = before.children.get(key)
                    if (child) visit(child, after[key], [...path, key])
                    else if (before.digests[position] !== digestOf(after[key])) intents.push({ path: [...path, key], type: 'set', value: structuredClone(after[key]) })
                }
            }
            return
        }
        intents.push({ path, type: 'set', value: structuredClone(after) })
    }
    visit(baseline, submitted, [], kind === 'database')
    return intents.sort((a, b) => canonical(a.path) < canonical(b.path) ? -1 : canonical(a.path) > canonical(b.path) ? 1 : 0)
}

export function diffPluginReadBaseline(baseline: any, submitted: any, kind: PluginReadKind): PluginFieldIntent[] {
    return diffBaselineNode(buildNode(baseline, undefined, kind === 'database'), submitted, kind)
}

// Parents keep only child targets; a child's digests live once, in its own newest read.
type StoredValue =
    | { kind: 'conversation' | 'preset'; node: BaselineNode }
    | { kind: 'character'; detail: BaselineNode; chats?: string[] }
    | { kind: 'database'; fields: Map<string, { childKind: PluginReadKind; targets: unknown[] } | BaselineNode> }

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
 * Keeps the newest completed read of each target as digests. Database reads keep
 * each top-level key separately, so a read of fewer keys replaces only those keys.
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
                    fields.set(field, { childKind, targets: record[field].map((child: any) => child?.[id]) })
                } else fields.set(field, fieldNode(record[field], field, true))
            }
        } else if (kind === 'character' && record && typeof record === 'object' && Array.isArray(record.chats)) {
            const { chats, ...detail } = record
            const targets = chats.map((chat: any) => JSON.stringify([target, chat?.id]))
            chats.forEach((chat: unknown, index: number) => this.store(chat, 'conversation', targets[index], authority))
            baseline.stored = { kind, detail: buildNode(detail, undefined, false, identityValue(record.chaId)), chats: targets }
        } else if (kind === 'character') baseline.stored = { kind, detail: buildNode(value, undefined, false, identityValue(record?.chaId)) }
        else baseline.stored = { kind: kind as 'conversation' | 'preset', node: buildNode(value, undefined, false, identityValue(record?.id)) }
    }

    private current(kind: PluginReadKind, target: string): Baseline | undefined {
        const baseline = this.baselines.get(JSON.stringify([kind, target]))
        return baseline?.authority === this.authority() ? baseline : undefined
    }

    // Composes a read baseline from the current baselines of its parts.
    private compose(kind: PluginReadKind, target: string): BaselineNode | undefined {
        const stored = this.current(kind, target)?.stored
        if (!stored) return undefined
        const present = (node: BaselineNode | undefined): node is BaselineNode => node !== undefined
        if (stored.kind === 'character') {
            if (!stored.chats) return stored.detail
            return withRecordField(stored.detail as ObjectNode, 'chats', recordsNode(stored.chats.map(chat => this.compose('conversation', chat)).filter(present)))
        }
        if (stored.kind !== 'database') return stored.node
        const nodes = [...stored.fields.values()].map(entry => 'targets' in entry
            ? recordsNode(entry.targets.map(child => this.compose(entry.childKind, child as string)).filter(present))
            : entry)
        return objectNode([...stored.fields.keys()], nodes, true)
    }

    // Resolves the baseline of a submitted value: its token's target, or the newest read of the target.
    private resolve(value: unknown, kind: PluginReadKind, target: string, fallback: () => BaselineNode | undefined, staleWithoutRead: boolean): BaselineNode | undefined {
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
        return fallback()
    }

    private withNestedBaselines(baseline: BaselineNode | undefined, submitted: any, kind: PluginReadKind, target: string): BaselineNode | undefined {
        if (!(baseline instanceof ObjectNode) || !submitted || typeof submitted !== 'object') return baseline
        const overlay = (records: BaselineNode | undefined, children: unknown, childKind: PluginReadKind, id: string, childTarget: (child: any) => string) => {
            if (!Array.isArray(children)) return records
            const original = records instanceof RecordsNode ? records.items : undefined
            let items = original
            const positions = new Map<unknown, number>()
            items?.forEach((record, index) => { if (!positions.has(record.identity)) positions.set(record.identity, index) })
            for (const child of children) {
                const index = positions.get(child?.[id]) ?? -1
                const previous = index >= 0 ? items![index] : undefined
                const target = childTarget(child)
                const next = this.withNestedBaselines(this.resolve(child, childKind, target, () => previous, true), child, childKind, target)
                if (next === undefined || next === previous) continue
                if (items === original) items = [...(items ?? [])]
                if (index >= 0) (items as BaselineNode[])[index] = next
                else positions.set(child?.[id], (items as BaselineNode[]).push(next) - 1)
            }
            return items === original ? records : recordsNode(items!, records?.identity)
        }
        let result = baseline
        const replace = (field: string, value: BaselineNode | undefined) => {
            if (value === undefined || value === result.children.get(field)) return
            result = withRecordField(result, field, value)
        }
        if (kind === 'database') {
            for (const [field, childKind, id] of nestedRecords) {
                replace(field, overlay(result.children.get(field), submitted[field], childKind, id, child => child?.[id]))
            }
        } else if (kind === 'character') {
            replace('chats', overlay(result.children.get('chats'), submitted.chats, 'conversation', 'id', chat => JSON.stringify([target, chat?.id])))
        }
        return result
    }

    intent(value: unknown, kind: PluginReadKind, target: string, admission: unknown): PluginFieldIntent[] {
        if (this.closed) throw new PluginReadBaselineError('closed')
        const baseline = this.resolve(value, kind, target, () => buildNode(admission, undefined, kind === 'database'), true)
        return diffBaselineNode(this.withNestedBaselines(baseline, value, kind, target) ?? buildNode(undefined, undefined, false), value, kind)
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
