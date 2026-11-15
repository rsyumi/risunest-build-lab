import { installCommandWitness } from './commandWitness.mjs'

// This verification entry suspends only startup. All UI code comes from the product entry.
const witness = installCommandWitness(globalThis.__TAURI_INTERNALS__)
let release
const ready = new Promise(resolve => { release = resolve })
Object.defineProperty(globalThis, '__risunestOllamaUi', { value: Object.freeze({
    build: __OLLAMA_ROUTE_BUILD__,
    token: crypto.randomUUID(),
    witness,
    release,
}) })
await ready
await import('../../../src/main.ts')

