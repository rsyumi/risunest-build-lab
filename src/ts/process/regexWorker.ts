import type {
    RegexWorkerRequest,
    RegexWorkerResponse,
} from './regexWorkerClient'
import { DisplayCbsExecutor } from './displayCbsCompiler'
import { executeRegexOperation } from './regexOperations'
import { hasCbsSyntax } from '../parser/cbsSyntax'

interface CompiledEntry {
    sourceIndex: number
    pattern: string
    replacement: string
    regex?: RegExp
    compileError?: string
    host?: boolean
    conditional?: boolean
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error)
}

export function createRegexWorkerMessageHandler(
    respond: (response: RegexWorkerResponse) => void,
): (request: RegexWorkerRequest) => void {
    let registeredRevision: number | undefined
    let registeredEntries: CompiledEntry[] | undefined
    let retainedInput: string | undefined
    let displayCbs: DisplayCbsExecutor | undefined

    return (request) => {
        if (request.type === 'release') {
            retainedInput = undefined
            return
        }
        if (request.type === 'operation') {
            try {
                respond({ type: 'operationResult', id: request.id, value: executeRegexOperation(request.operation) })
            } catch (error) {
                respond({ type: 'error', id: request.id, message: errorMessage(error) })
            }
            return
        }
        if (request.type === 'testEach') {
            let regex: RegExp
            try {
                regex = new RegExp(request.pattern, request.flags)
            } catch (error) {
                respond({ type: 'error', id: request.id, message: errorMessage(error) })
                return
            }
            const matches = request.inputs.map((input, index) => {
                // Each input starts from a fresh match position, as a separate operation would.
                regex.lastIndex = 0
                const matched = regex.test(input)
                respond({ type: 'progress', id: request.id, index, matched })
                return matched
            })
            respond({ type: 'operationResult', id: request.id, value: matches })
            return
        }
        if (request.type === 'register') {
            retainedInput = undefined
            registeredRevision = request.revision
            displayCbs = request.displayCbs ? new DisplayCbsExecutor(request.displayCbs) : undefined
            registeredEntries = request.entries.map(([
                sourceIndex,
                pattern,
                replacement,
                flags,
                host,
                conditional,
            ]) => {
                const entry: CompiledEntry = { sourceIndex, pattern, replacement, host, conditional }
                if (pattern !== '' && !host) {
                    try {
                        entry.regex = new RegExp(pattern, flags)
                    }
                    catch (error) {
                        entry.compileError = errorMessage(error)
                    }
                }
                return entry
            })
            return
        }

        if (registeredRevision !== request.revision || registeredEntries === undefined) {
            respond({
                type: 'error',
                id: request.id,
                message: `Regex plan revision ${request.revision} is not registered`,
            })
            return
        }

        if (request.reuseInput && retainedInput === undefined) {
            respond({ type: 'error', id: request.id, message: 'Regex continuation input is unavailable' })
            return
        }
        const input = request.reuseInput ? retainedInput! : request.input
        let data = input
        let needsParse = !request.continuation?.skipPlainCbs || hasCbsSyntax(data)
        const errors: [sourceIndex: number, message: string][] = []
        const steps: { index: number, matched: boolean }[] = []
        const cbsReads = new Set<string>()
        let checkpointInput: string | undefined
        let unchangedCbsFallback = false
        let nextIndex = request.continuation?.start ?? 0
        const end = request.continuation?.end ?? registeredEntries.length
        for (; nextIndex < end; nextIndex++) {
            const entry = registeredEntries[nextIndex]
            if (request.continuation && entry.host) break
            if (entry.pattern === '') {
                continue
            }
            if (request.continuation) respond({ type: 'progress', id: request.id, index: nextIndex })
            try {
                if (entry.compileError !== undefined) {
                    throw new Error(entry.compileError)
                }
                if (entry.regex === undefined) {
                    throw new Error('Regex Worker entry was not compiled')
                }
                entry.regex.lastIndex = 0
                // Only conditional rules need a separate match result before replacement.
                const matched = !entry.conditional || entry.regex.test(data)
                entry.regex.lastIndex = 0
                const replaced = matched ? data.replace(entry.regex, entry.replacement) : data
                if (request.continuation && request.cbsContext && displayCbs) {
                    if (entry.conditional && !matched) {
                        data = replaced
                        continue
                    }
                    const parsed = displayCbs.evaluate(replaced, request.cbsContext)
                    if (parsed) {
                        data = parsed.data
                        for (const key of parsed.reads) cbsReads.add(key)
                        continue
                    }
                    // Stop at this exact checkpoint, including unchanged regex output.
                    checkpointInput = data
                    unchangedCbsFallback = replaced === data
                    data = replaced
                    steps.push({ index: nextIndex, matched })
                    nextIndex++
                    break
                }
                if (request.continuation?.skipPlainCbs) {
                    if (replaced !== data) needsParse = hasCbsSyntax(replaced)
                    if (!needsParse) {
                        data = replaced
                        continue
                    }
                }
                steps.push({ index: nextIndex, matched })
                if (request.continuation && replaced !== data) {
                    checkpointInput = data
                    data = replaced
                    nextIndex++
                    break
                }
                data = replaced
            }
            catch (error) {
                errors.push([entry.sourceIndex, errorMessage(error)])
            }
        }

        retainedInput = request.continuation ? data : undefined
        respond({ type: 'result', id: request.id, data, errors,
            ...(request.continuation ? { steps, nextIndex } : {}),
            ...(request.cbsContext ? { cbsReads: [...cbsReads] } : {}),
            ...(checkpointInput !== undefined && checkpointInput !== input ? { checkpointInput } : {}),
            ...(unchangedCbsFallback ? { unchangedCbsFallback: true } : {}),
        })
    }
}

const workerScope = globalThis as typeof globalThis & {
    document?: unknown
    postMessage?: (message: RegexWorkerResponse) => void
    addEventListener?: (type: 'message', listener: EventListener) => void
}

if (workerScope.document === undefined
    && typeof workerScope.postMessage === 'function'
    && typeof workerScope.addEventListener === 'function') {
    const handleRequest = createRegexWorkerMessageHandler((response) => {
        workerScope.postMessage!(response)
    })
    workerScope.postMessage({ type: 'ready' })
    workerScope.addEventListener('message', ((event: MessageEvent<RegexWorkerRequest>) => {
        handleRequest(event.data)
    }) as EventListener)
}
