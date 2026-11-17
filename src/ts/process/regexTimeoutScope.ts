export interface RegexTimeoutScope {
    skipped: Set<string>
    noticeShown: boolean
}

export function createRegexTimeoutScope(): RegexTimeoutScope {
    return { skipped: new Set(), noticeShown: false }
}

const generations = new WeakMap<object, RegexTimeoutScope>()

export function generationRegexTimeoutScope(generation: object): RegexTimeoutScope {
    let scope = generations.get(generation)
    if (!scope) {
        scope = createRegexTimeoutScope()
        generations.set(generation, scope)
    }
    return scope
}
