import rfdc from 'rfdc'
import type { ProcessScriptCaptureContext } from './process/scripts'

const clone = rfdc()
const detachedSeeds = new WeakSet<ProcessScriptCaptureContext>()

function isPlainTree(value: unknown): boolean {
    if (value === null || typeof value !== 'object') return true
    if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype) return false
    return Object.values(value).every(isPlainTree)
}

export function createChatParserContextSeed(context: ProcessScriptCaptureContext): ProcessScriptCaptureContext {
    const seed = clone(context)
    if (isPlainTree(seed)) detachedSeeds.add(seed)
    return seed
}

export function captureChatParserContextSeed(seed: ProcessScriptCaptureContext): ProcessScriptCaptureContext {
    const captured = { ...seed, parserContext: { ...seed.parserContext, currentTime: Date.now() } }
    if (detachedSeeds.has(seed)) detachedSeeds.add(captured)
    return captured
}

export function forkChatParserContext(seed: ProcessScriptCaptureContext): ProcessScriptCaptureContext {
    if (!detachedSeeds.has(seed)) return clone(seed)
    // Each state proxy owns its writes, including nested arrays and objects.
    // The detached plain seed is never exposed to a rendering script.
    const context = $state(seed)
    return context
}
