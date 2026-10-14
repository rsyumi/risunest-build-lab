import assert from 'node:assert/strict'
import { test } from 'node:test'
import { checkProductCommands, readProductRouter, readProductSources } from './productCommands.mjs'

const result = checkProductCommands(readProductRouter(), readProductSources())

test('the product builder installs the product command router', () => {
    assert.ok(result.router, 'src-tauri/src/lib.rs has no invoke_handler() with generate_handler!')
    assert.ok(result.linked, 'the app builder does not call .invoke_handler(invoke_handler())')
})

test('the product router registers each command once', () => {
    assert.deepEqual(result.duplicates, [])
})

test('every command a product caller names is registered in the product router', () => {
    assert.deepEqual(result.missing.map(({ command, file, line }) => `${file}:${line}: ${command}`), [])
    // A caller shape that no longer matches anything means its commands went unchecked.
    assert.deepEqual(result.unmatchedPatterns, [])
})
