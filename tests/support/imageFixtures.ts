import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

/** Synthetic 4x3 color fields, with a different second frame in animated files. */
export function imageFixture(ext: 'png' | 'jpg' | 'gif' | 'webp' | 'avif', animated = false): Uint8Array<ArrayBuffer> {
    return new Uint8Array(readFileSync(resolve(`tests/fixtures/images/${animated ? 'animated' : 'still'}.${ext}`)))
}
