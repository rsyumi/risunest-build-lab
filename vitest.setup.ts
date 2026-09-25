import { afterEach, vi } from 'vitest'
import rfdc from 'rfdc'
import { testNetwork } from './tests/support/testNetwork'

// Suppress warning
vi.mock(import('katex'), () => ({}))

// Assign directly so unstubAllGlobals restores the guard, not an unrestricted fetch.
const realFetch = globalThis.fetch
const guardedFetch: typeof fetch = (input, init) => {
  testNetwork.check(input)
  return realFetch(input, init)
}
globalThis.fetch = guardedFetch

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

// Mirror the production safeStructuredClone from src/ts/polyfill.ts (structuredClone
// with an rfdc fallback) instead of importing polyfill.ts, which would pull its
// drag-drop/stream/global side effects into every suite. A JSON round-trip must not
// be used here: it silently changes clone semantics (drops undefined/function
// properties, stringifies Dates, mangles typed arrays, throws on cycles) in the
// data-loss-critical storage suites.
const rfdcClone = rfdc({
  circles: false,
})
vi.stubGlobal('safeStructuredClone', <T>(data: T): T => {
  try {
    return structuredClone(data)
  } catch (error) {
    return rfdcClone(data)
  }
})
