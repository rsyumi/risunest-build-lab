import { afterEach, vi } from 'vitest'
import { testNetwork } from './tests/support/testNetwork'
import { safeStructuredClone } from './src/ts/safeStructuredClone'

// Suppress warning
vi.mock(import('katex'), () => ({}))

// Assign directly so unstubAllGlobals restores the guard, not an unrestricted fetch.
const realFetch = globalThis.fetch
const guardedFetch: typeof fetch = (input, init) => {
  testNetwork.check(input)
  return realFetch(input, init)
}
globalThis.fetch = guardedFetch

const RealWebSocket = globalThis.WebSocket
if (RealWebSocket) {
    globalThis.WebSocket = class extends RealWebSocket {
        constructor(url: string | URL, protocols?: string | string[]) {
            testNetwork.check(url)
            super(url, protocols)
        }
    }
}

type GuardRequest = { request: { url: string } }
const happyDOM = (globalThis as { happyDOM?: { settings?: { fetch?: { interceptor?: unknown } } } }).happyDOM
if (happyDOM?.settings?.fetch) {
  happyDOM.settings.fetch.interceptor = {
    beforeAsyncRequest: async ({ request }: GuardRequest) => { testNetwork.check(request.url) },
    beforeSyncRequest: ({ request }: GuardRequest) => { testNetwork.check(request.url) },
  }
}

// A caught transport error still fails the owning test.
afterEach(() => testNetwork.finish())

// The global polyfill.ts installs, without its drag-drop, stream and Buffer side effects.
vi.stubGlobal('safeStructuredClone', safeStructuredClone)

// Most gated waits settle within a few microtasks, so poll every 2 ms instead of every 50 ms.
// A call that passes its own interval keeps it.
const defaultWaitFor = vi.waitFor.bind(vi)
vi.waitFor = ((callback: Parameters<typeof vi.waitFor>[0], options?: Parameters<typeof vi.waitFor>[1]) => {
  const resolved = typeof options === 'number' ? { timeout: options } : (options ?? {})
  return defaultWaitFor(callback, { interval: 2, ...resolved })
}) as typeof vi.waitFor
