import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { androidGeneratedCopies } from '../scripts/prepareAndroidGenerated.mjs'

test('android worktree preparation only copies generated bindings that are missing', () => {
  const shared = new Set(['TauriActivity.kt', 'WryActivity.kt'])
  assert.deepEqual(androidGeneratedCopies('x86_64', shared, shared), [])
  assert.deepEqual(
    androidGeneratedCopies('x86_64', new Set(['WryActivity.kt']), shared),
    ['TauriActivity.kt'],
  )
  assert.deepEqual(androidGeneratedCopies('aarch64', new Set(), shared), [
    'TauriActivity.kt',
    'WryActivity.kt',
  ])
})

test('android worktree preparation rejects unknown target aliases', () => {
  assert.throws(() => androidGeneratedCopies('mips', new Set(), new Set()), /Unsupported Android target/)
})

test('every Android build entry prepares worktree bindings first', () => {
  const packageJson = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
  const androidBuilds = Object.entries(packageJson.scripts)
    .filter(([name]) => name.startsWith('android:build:'))

  assert.ok(androidBuilds.length > 0)
  for (const [name, command] of androidBuilds) {
    assert.match(command, /^node scripts\/prepareAndroidGenerated\.mjs (?:aarch64|x86_64) && /, name)
  }
})
