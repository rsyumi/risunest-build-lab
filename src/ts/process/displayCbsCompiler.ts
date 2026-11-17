import { createPureCbsCallbacks, makeArray, parseArray } from '../cbsPure'
import { calculateCbsExpression } from './cbsArithmetic'

type CbsPart = string | CbsCall | CbsBlock
interface CbsCall { kind: 'call', parts: CbsPart[], key?: string }
interface CbsBlock { kind: 'if' | 'if_pure' | 'when', header: CbsPart[], body: CbsPart[] }

export interface DisplayCbsProgram {
    source: string
    node: CbsCall | CbsBlock
    keys: string[]
    nodes: number
}
export interface DisplayCbsLimits { sourceUnits: number, nodes: number, depth: number, variables?: number }
export const DISPLAY_CBS_LIMITS: DisplayCbsLimits = { sourceUnits: 262_144, nodes: 8_192, depth: 128 }
export interface DisplayCbsPlan { programs: DisplayCbsProgram[], keys: string[], estimatedBytes: number }
export interface DisplayCbsContext { variables: Record<string, string> }
export interface DisplayCbsResult { data: string, reads: string[] }

function rejectArithmeticRead(_key: string): string {
    throw new Error('Arithmetic host read requires fallback')
}

const arithmeticVariables = { getChatVar: rejectArithmeticRead, getGlobalChatVar: rejectArithmeticRead }
const calcString = (text: string) => calculateCbsExpression(text, arithmeticVariables)
const callbacks = createPureCbsCallbacks({ parseArray, makeArray, calcString })
const pureCallbacks = { ...callbacks, array: callbacks.makearray, a: callbacks.makearray }

const operations = new Set(['getvar', 'equal', 'notequal', 'greater', 'less', 'greaterequal', 'lessequal', 'and', 'or', 'not'])
for (const name of Object.keys(pureCallbacks)) operations.add(name)
const opening = /\{[{#]|<(?:user|char|bot)>/ig

function splitExpression(text: string): string[] {
    const colon = text.indexOf(':')
    return text.split(colon !== -1 && text[colon + 1] === ':' ? '::' : ':')
}

function operationName(name: string): string {
    return name.toLocaleLowerCase().replace(/[\s_-]/g, '')
}

// Compilation admits complete source spans. It never freezes evaluated argument separators.
export function compileDisplayCbs(source: string, limits: DisplayCbsLimits = DISPLAY_CBS_LIMITS): DisplayCbsProgram[] | null {
    if (source.length > limits.sourceUnits || source.includes('#}') || /<(?:user|char|bot)>/i.test(source)) return null
    let pointer = 0
    let nodes = 0
    let keys = new Set<string>()
    const programs: DisplayCbsProgram[] = []
    const fail = () => { throw new Error('Unsupported display CBS') }
    function expression(depth: number): { parts: CbsPart[], raw: string } {
        if (depth > limits.depth || ++nodes > limits.nodes) fail()
        const start = pointer
        pointer += 2
        const parts: CbsPart[] = []
        let text = ''
        while (pointer < source.length) {
            if (source.startsWith('}}', pointer)) {
                if (text) parts.push(text)
                pointer += 2
                return { parts, raw: source.slice(start + 2, pointer - 2) }
            }
            if (source.startsWith('{{', pointer)) {
                if (text) parts.push(text)
                text = ''
                const child = expression(depth + 1)
                parts.push(call(child.parts))
            } else {
                if (source.startsWith('{#', pointer)) fail()
                text += source[pointer++]
            }
        }
        return fail()
    }
    function call(parts: CbsPart[]): CbsCall {
        if (typeof parts[0] !== 'string') return fail()
        if (parts[0].startsWith('? ')) {
            if (parts.some(part => typeof part === 'string' && /[$@][a-zA-Z0-9_]+/.test(part))) return fail()
            return { kind: 'call', parts }
        }
        const prefix = splitExpression(parts[0])
        const name = operationName(prefix[0])
        if (!operations.has(name) || (parts.length > 1 && prefix.length === 1)) return fail()
        if (name === 'calc' && parts.some(part => typeof part === 'string' && /[$@][a-zA-Z0-9_]+/.test(part))) return fail()
        if (name === 'getvar') {
            if (parts.length !== 1 || prefix.length < 2) return fail()
            keys.add(prefix[1])
            return { kind: 'call', parts, key: prefix[1] }
        }
        return { kind: 'call', parts }
    }
    function sequence(depth: number, block?: CbsBlock['kind']): CbsPart[] {
        if (depth > limits.depth) return fail()
        const parts: CbsPart[] = []
        while (pointer < source.length) {
            const next = source.indexOf('{{', pointer)
            if (next < 0) {
                if (source.slice(pointer).includes('{#')) fail()
                parts.push(source.slice(pointer))
                pointer = source.length
                break
            }
            if (next > pointer) {
                if (source.slice(pointer, next).includes('{#')) fail()
                parts.push(source.slice(pointer, next))
            }
            pointer = next
            const start = pointer
            const priorNodes = nodes
            const parentKeys = keys
            if (!block) keys = new Set()
            const parsed = expression(depth + 1)
            const prefix = typeof parsed.parts[0] === 'string' ? parsed.parts[0] : ''
            if (prefix.startsWith('/')) {
                if (!block || (parsed.raw !== '/' && parsed.raw !== '/' + block && !(block === 'if_pure' && parsed.raw === '/if'))) return fail()
                return parts
            }
            let node: CbsCall | CbsBlock
            const kind = prefix.startsWith('#if_pure ') ? 'if_pure'
                : prefix.startsWith('#if ') ? 'if'
                : prefix.startsWith('#when ') || prefix.startsWith('#when::') ? 'when' : undefined
            if (kind) {
                node = { kind, header: parsed.parts, body: sequence(depth + 1, kind) }
            } else if (parsed.raw === ':else' && block === 'when') {
                parts.push('{{:else}}')
                continue
            } else node = call(parsed.parts)
            parts.push(node)
            if (!block) {
                programs.push({ source: source.slice(start, pointer), node, keys: [...keys], nodes: nodes - priorNodes })
                keys = parentKeys
            }
        }
        if (block) return fail()
        return parts
    }
    try {
        sequence(0)
        return programs
    } catch { return null }
}

export function createDisplayCbsPlan(sources: readonly string[], limits: DisplayCbsLimits = DISPLAY_CBS_LIMITS): DisplayCbsPlan | undefined {
    const programs = new Map<string, DisplayCbsProgram>()
    const keys = new Set<string>()
    const seenSources = new Set<string>()
    let units = 0
    let nodes = 0
    for (const source of sources) {
        if (source.length > limits.sourceUnits || seenSources.has(source)) continue
        seenSources.add(source)
        for (const program of compileDisplayCbs(source, limits) ?? []) {
            if (programs.has(program.source)) continue
            let additionalKeys = 0
            for (const key of program.keys) if (!keys.has(key)) additionalKeys++
            if (keys.size + additionalKeys > (limits.variables ?? 256)) continue
            if (units + program.source.length > limits.sourceUnits || nodes + program.nodes > limits.nodes) continue
            units += program.source.length
            nodes += program.nodes
            programs.set(program.source, program)
            for (const key of program.keys) keys.add(key)
        }
    }
    return programs.size ? {
        programs: [...programs.values()], keys: [...keys],
        estimatedBytes: units * 4 + nodes * 128,
    } : undefined
}

export function projectDisplayCbsContext(
    keys: readonly string[],
    read: (key: string) => string,
    maxUnits = DISPLAY_CBS_LIMITS.sourceUnits,
): DisplayCbsContext | undefined {
    if (keys.length > 256) return undefined
    const variables: Record<string, string> = Object.create(null)
    let units = 0
    for (const key of keys) {
        const value = read(key)
        if (typeof value !== 'string') return undefined
        units += key.length + value.length
        if (units > maxUnits) return undefined
        variables[key] = value
    }
    return { variables }
}

function evaluateParts(parts: CbsPart[], context: DisplayCbsContext, reads: Set<string>): string | null {
    let result = ''
    for (const part of parts) {
        const value = typeof part === 'string' ? part : evaluateNode(part, context, reads)
        if (value === null || result.length + value.length > DISPLAY_CBS_LIMITS.sourceUnits) return null
        result += value
    }
    return result
}

function evaluateNode(node: CbsCall | CbsBlock, context: DisplayCbsContext, reads: Set<string>): string | null {
    if (node.kind === 'call') {
        if (node.key !== undefined) {
            if (!Object.hasOwn(context.variables, node.key) || typeof context.variables[node.key] !== 'string') return null
            reads.add(node.key)
            return context.variables[node.key]
        }
        const text = evaluateParts(node.parts, context, reads)
        if (text === null) return null
        const segments = splitExpression(text)
        const [rawName, a, b] = segments
        const name = operationName(rawName)
        switch (name) {
            case 'equal': return a === b ? '1' : '0'
            case 'notequal': return a !== b ? '1' : '0'
            case 'greater': return Number(a) > Number(b) ? '1' : '0'
            case 'less': return Number(a) < Number(b) ? '1' : '0'
            case 'greaterequal': return Number(a) >= Number(b) ? '1' : '0'
            case 'lessequal': return Number(a) <= Number(b) ? '1' : '0'
            case 'and': return a === '1' && b === '1' ? '1' : '0'
            case 'or': return a === '1' || b === '1' ? '1' : '0'
            case 'not': return a === '1' ? '0' : '1'
            default: return evaluatePureCallback(text, name, segments.slice(1))
        }
    }
    const header = evaluateParts(node.header, context, reads)
    if (header === null) return null
    let condition: string
    if (header.startsWith('#when::')) {
        const segments = header.split('::')
        if (segments.length !== 2) return null
        condition = segments[1]
    } else condition = header.split(' ', 2)[1]
    const truthy = condition === '1' || condition === 'true'
    if (node.kind !== 'when' && !truthy) return ''
    const body = evaluateParts(node.body, context, reads)
    if (body === null) return null
    if (node.kind === 'if_pure') return body
    if (node.kind === 'if') return body.trim().split('\n').map((line) => line.trimStart()).join('\n').trim()
    const lines = body.split('\n')
    if (lines.length === 1) {
        const index = body.indexOf('{{:else}}')
        return index < 0 ? truthy ? body : ''
            : truthy ? body.substring(0, index) : body.substring(index + 9)
    }
    const index = lines.findIndex((line) => line.trim() === '{{:else}}')
    if (index >= 0) {
        if (truthy) lines.splice(index)
        else lines.splice(0, index + 1)
    } else if (!truthy) return ''
    while (lines.length && lines[0].trim() === '') lines.shift()
    while (lines.length && lines[lines.length - 1].trim() === '') lines.pop()
    return lines.join('\n')
}

function headerKey(source: string, start: number): string {
    const end = source.indexOf('}}', start)
    return end < 0 ? '' : source.slice(start, end + 2)
}

export class DisplayCbsExecutor {
    private readonly programs = new Map<string, DisplayCbsProgram[]>()
    constructor(plan: DisplayCbsPlan) {
        for (const program of plan.programs) {
            const key = headerKey(program.source, 0)
            const candidates = this.programs.get(key) ?? []
            candidates.push(program)
            this.programs.set(key, candidates)
        }
    }

    // A failed checkpoint is handed to the original parser with no writes, random draws or host calls.
    evaluate(source: string, context: DisplayCbsContext): DisplayCbsResult | null {
        let pointer = 0
        let data = ''
        const reads = new Set<string>()
        while (pointer < source.length) {
            opening.lastIndex = pointer
            const match = opening.exec(source)
            if (!match) return { data: data + source.slice(pointer), reads: [...reads] }
            const start = match.index
            if (match[0] !== '{{') return null
            const program = this.programs.get(headerKey(source, start))
                ?.find((candidate) => source.startsWith(candidate.source, start))
            if (!program) return null
            const result = evaluateNode(program.node, context, reads)
            if (result === null) return null
            data += source.slice(pointer, start) + result
            pointer = start + program.source.length
        }
        return { data, reads: [...reads] }
    }
}


function evaluatePureCallback(text: string, name: string, args: string[]): string | null {
    if (text.length > 16_384) return null
    const arithmetic = text.startsWith('? ') ? text.substring(2) : name === 'calc' ? args[0] : undefined
    if (arithmetic !== undefined && (/[$@][a-zA-Z0-9_]+/.test(arithmetic) || arithmetic.length > 4_096)) return null
    if (name === 'replace' && args[0] !== undefined) {
        const input = args[0], pattern = String(args[1]), replacement = String(args[2])
        let matches = pattern.length === 0 ? input.length + 1 : 0
        if (pattern.length !== 0) {
            for (let at = input.indexOf(pattern); at !== -1; at = input.indexOf(pattern, at + pattern.length)) matches++
        }
        const substitutions = ['$&', '$' + String.fromCharCode(96), "$'"].reduce((sum, token) => sum + replacement.split(token).length - 1, 0)
        const possibleUnits = input.length + matches * (replacement.length + substitutions * input.length)
        if (possibleUnits > DISPLAY_CBS_LIMITS.sourceUnits) return null
    }
    // Parsed JSON objects can expand to "[object Object]" during array joining.
    if (name === 'join' && args[0] !== undefined
        && args[0].length * 16 + (args[0].length + 1) * (args[1]?.length ?? 1) > DISPLAY_CBS_LIMITS.sourceUnits) return null
    try {
        const result = text.startsWith('? ') ? calcString(arithmetic!).toString()
            : Object.hasOwn(pureCallbacks, name) ? pureCallbacks[name as keyof typeof pureCallbacks](args) : null
        return typeof result === 'string' && result.length <= DISPLAY_CBS_LIMITS.sourceUnits ? result : null
    } catch { return null }
}
