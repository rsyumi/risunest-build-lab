import { beforeAll, afterAll, vi } from 'vitest'
import { nodeLuaWorkers } from './luaWorkerNode'

declare module 'vitest' {
    export interface ProvidedContext { luaWorker?: boolean }
}

let workers: Awaited<ReturnType<typeof nodeLuaWorkers>>
beforeAll(async () => {
    workers = await nodeLuaWorkers()
    vi.stubGlobal('Worker', workers.Worker)
})
afterAll(async () => { await workers.close() })
