export interface RegexOperationResults {
    test: boolean
    replace: string
    match: RegExpMatchArray | null
    exec: RegExpExecArray | null
    matches: (RegExpMatchArray | null)[]
    split: string[]
    replacementMatches: RegexReplacementMatch[]
}

export interface RegexReplacementMatch {
    match: string
    captures: (string | undefined)[]
    offset: number
    groups?: Record<string, string>
}

export interface RegexOperation<K extends keyof RegexOperationResults = keyof RegexOperationResults> {
    kind: K
    pattern: string
    flags: string
    input: string
    replacement?: string
}

/** Also used by hosts without Worker support. */
export function executeRegexOperation<K extends keyof RegexOperationResults>(operation: RegexOperation<K>): RegexOperationResults[K] {
    const regex = new RegExp(operation.pattern, operation.flags)
    const input = operation.input
    let result: RegexOperationResults[keyof RegexOperationResults]
    switch (operation.kind) {
        case 'test': result = regex.test(input); break
        case 'replace': result = input.replace(regex, operation.replacement ?? ''); break
        case 'match': result = input.match(regex); break
        case 'exec': result = regex.exec(input); break
        case 'matches': result = regex.global ? [...input.matchAll(regex)] : [input.match(regex)]; break
        case 'split': result = input.split(regex); break
        case 'replacementMatches': {
            const matches: RegexReplacementMatch[] = []
            input.replace(regex, (...args) => {
                const named = typeof args.at(-1) === 'object'
                matches.push({
                    match: args[0],
                    captures: args.slice(1, named ? -3 : -2),
                    offset: args.at(named ? -3 : -2),
                    ...(named ? { groups: args.at(-1) } : {}),
                })
                return args[0]
            })
            result = matches
            break
        }
    }
    return result as RegexOperationResults[K]
}

export function unmatchedRegexOperation<K extends keyof RegexOperationResults>(operation: RegexOperation<K>): RegexOperationResults[K] {
    const results: RegexOperationResults = {
        test: false, replace: operation.input, match: null, exec: null,
        matches: [], split: [operation.input], replacementMatches: [],
    }
    return results[operation.kind]
}

export function replaceRegexMatches(input: string, matches: RegexReplacementMatch[], replace: (...args: any[]) => string): string {
    let end = 0
    const parts: string[] = []
    for (const match of matches) {
        parts.push(input.slice(end, match.offset), replace(
            match.match, ...match.captures, match.offset, input,
            ...(match.groups ? [match.groups] : []),
        ))
        end = match.offset + match.match.length
    }
    parts.push(input.slice(end))
    return parts.join('')
}
