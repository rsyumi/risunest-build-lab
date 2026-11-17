import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { brotliCompressSync, constants, gzipSync } from 'node:zlib'

const root = new URL('../../', import.meta.url)
const reference = JSON.parse(await readFile(new URL('tests/fixtures/tokenizer/gemma4.json', root), 'utf8'))
const model = await readFile(new URL(reference.model, root))
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex')
assert.equal(sha256(model), reference.sha256)
assert.equal(JSON.stringify(JSON.parse(model.toString('utf8'))), model.toString('utf8'))

const sizes = {
    bytes: model.byteLength,
    sha256: sha256(model),
    gzip9: gzipSync(model, { level: 9 }).byteLength,
}
for (const quality of [2, 9, 11]) {
    sizes[`brotli${quality}`] = brotliCompressSync(model, {
        params: { [constants.BROTLI_PARAM_QUALITY]: quality },
    }).byteLength
}
console.log(JSON.stringify(sizes))
