export const MAX_NATIVE_VALUE_DEPTH = 100

const persistenceAreas: Record<string, string> = {
    root: 'root', rootMutations: 'root', character: 'character', characters: 'character',
    characterDetails: 'character', replaceCharacter: 'character', addCharacter: 'character',
    conversations: 'conversation', pluginStorage: 'plugin storage', pluginStorageValues: 'plugin storage',
    assetAliases: 'asset aliases', botPresets: 'presets', replacePresets: 'presets',
}

export class UnsaveableValueError extends Error {
    readonly code = 'unsaveable-value'
    constructor(readonly area: string, readonly reason: string, readonly recordId?: string) {
        super(`Cannot save ${area}: ${reason}`)
        this.name = 'UnsaveableValueError'
    }
}

/** Native JSON stores Unicode scalar values; preserve every valid code point exactly. */
export function prepareNativePersistenceValue<T>(input: T, area = 'persistent data'): T {
    let replacements = 0
    const wellFormed = (value: string): string => value.replace(
        /[\uD800-\uDBFF][\uDC00-\uDFFF]|[\uD800-\uDFFF]/g,
        (part) => {
            if (part.length === 2) return part
            replacements++
            return '\uFFFD'
        },
    )
    const ancestors = new Set<object>()
    const visit = (value: unknown, depth: number, location = area, recordId?: string): unknown => {
        if (depth > MAX_NATIVE_VALUE_DEPTH) {
            throw new UnsaveableValueError(location, `JSON nesting exceeds ${MAX_NATIVE_VALUE_DEPTH}`, recordId)
        }
        if (typeof value === 'string') return wellFormed(value)
        if (value === null || typeof value !== 'object') return value
        if (ancestors.has(value)) throw new UnsaveableValueError(location, 'cyclic value', recordId)
        const record = value as Record<string, unknown>
        if (depth <= 4 && !recordId) {
            const id = location === 'character' ? record.chaId
                : location === 'conversation' ? record.conversationId
                : location === 'plugin storage' ? record.key
                : location === 'presets' ? record.id : undefined
            if (typeof id === 'string') recordId = id
        }
        ancestors.add(value)
        try {
            const toJSON = (value as { toJSON?: unknown }).toJSON
            if (typeof toJSON === 'function') {
                const converted = toJSON.call(value)
                if (converted !== value) return visit(converted, depth, location, recordId)
            }
            if (Array.isArray(value)) return value.map((entry) => visit(entry, depth + 1, location, recordId))
            const result: Record<string, unknown> = Object.create(null)
            for (const key of Object.keys(value)) {
                const nextKey = wellFormed(key)
                if (Object.hasOwn(result, nextKey)) {
                    throw new UnsaveableValueError(location, 'Unicode replacement would merge object keys', recordId)
                }
                result[nextKey] = visit(record[key], depth + 1, depth <= 2 && Object.hasOwn(persistenceAreas, key) ? persistenceAreas[key] : location, recordId)
            }
            return result
        } finally {
            ancestors.delete(value)
        }
    }
    const result = visit(input, 0) as T
    if (replacements) console.warn('Native persistence replaced invalid Unicode', { area, replacements })
    return result
}
