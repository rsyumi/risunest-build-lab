import assert from 'node:assert/strict'
import { test } from 'node:test'
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { assertInventory, assertListedTestsExist, discoverTestEntries, extendedAppTests, nodeAppTests, ownerOf, selectNodeTests } from './suiteOwnership.mjs'
import { parseVitestInventory, webExportInventory } from './testInventory.mjs'

const ordinary = 'src/ts/storage/newRegression.test.ts'
const extended = 'src/ts/storage/selectedConversationEvictionCorpus.test.ts'
const nodeOnly = nodeAppTests[0]

test('classifies ordinary, extended, harness, Node and independent entries', () => {
  for (const [path, expected] of [
    [ordinary, 'app'], [extended, 'app-extended'], [nodeOnly, 'app-node'],
    ['scripts/example.spec.mjs', 'app'],
    ['src-tauri/src/newRegression.test.ts', 'app'],
    ['benchmarks/tokenizer/nativeTokenizerBenchmark.test.ts', 'harness'],
    ['src/ts/process/regexSafePlan.profile.test.ts', 'harness'],
    ['tests/release/contract.node-test.mjs', 'node'],
    ['benchmarks/sync-server/transfer-comparison.test.mjs', 'harness-node'],
    ['benchmarks/startup/startup.node-test.mjs', 'harness-node'],
    ['crates/sync-wire/tests/golden.test.mjs', 'node'],
    ['server/manager/gui/tests/App.test.ts', 'manager'],
    ['server/endpoint-registry/tests/registry.worker.ts', 'registry'],
    ['tools/web-risu-export/tests/core.test.mjs', 'web-export'],
    ['tests/browser/settings.browser.spec.ts', 'browser'],
    ['src/lib/Others/BookmarkDisplayState.test.svelte.ts', null],
    ['src/ts/storage/nativeCommitEncoder.worker.ts', null],
  ]) assert.equal(ownerOf(path), expected, path)
  assert.equal(ownerOf(ordinary.replaceAll('/', '\\')), 'app')
  assert.throws(() => ownerOf('new-package/missed.test.ts'), /Unowned/)
})

test('detects exclusions, duplicate ownership and missing runner owners', () => {
  assert.doesNotThrow(() => assertInventory([ordinary, extended], { app: [ordinary], 'app-extended': [extended] }))
  assert.throws(() => assertInventory([ordinary, extended], { app: [ordinary], 'app-extended': [] }), /omitted/)
  assert.throws(() => assertInventory([ordinary], { app: [ordinary, ordinary] }), /Duplicate/)
  assert.throws(() => assertInventory([ordinary], { app: [ordinary], harness: [ordinary] }), /Duplicate/)
  assert.throws(() => assertInventory([extended], { app: [] }), /Missing runner/)
  assert.throws(() => assertInventory([extended], { app: [extended] }), /Unexpected/)
})

test('every listed extended and node entry exists', () => {
  const listed = [...extendedAppTests, ...nodeAppTests]
  assert.doesNotThrow(() => assertListedTestsExist(listed))
  assert.throws(() => assertListedTestsExist(listed.filter(path => path !== nodeOnly)), new RegExp(nodeOnly))
})

test('accepts the installed CLI JSON shape and rejects unexpected output', () => {
  const root = join(tmpdir(), 'inventory')
  const listed = [{ file: join(root, ordinary), projectName: 'app' }, { file: join(root, nodeOnly), projectName: 'app-node' }]
  assert.deepEqual(parseVitestInventory(JSON.stringify(listed), root), {
    app: [ordinary], 'app-node': [nodeOnly], 'app-extended': [], harness: [],
  })
  for (const invalid of ['warning\n[]', '{}', '[]', '[{}]', JSON.stringify([{ file: join(root, ordinary), projectName: 'unknown' }])]) {
    assert.throws(() => parseVitestInventory(invalid, root))
  }
  assert.throws(() => parseVitestInventory(JSON.stringify([{ file: join(root, '..', 'outside.test.ts'), projectName: 'app' }]), root), /outside/)
})

test('the web export inventory matches only what its own test script runs', () => {
  const top = 'tools/web-risu-export/tests/core.test.mjs'
  const nested = 'tools/web-risu-export/tests/nested/deep.test.mjs'
  const files = [ordinary, top, nested, 'tools/web-risu-export/tests/browser.mjs']
  const selected = webExportInventory(files, 'node --test tests/*.test.mjs')
  assert.deepEqual(selected, [top])
  assert.throws(() => assertInventory([top, nested], { 'web-export': selected }), /omitted/)
  assert.throws(() => webExportInventory(files, 'node --test tests/**/*.test.mjs'), /Unexpected web export test script/)
})

test('Node selection is bounded, deterministic and never selects measurement scripts', () => {
  const files = [ordinary, 'tests/externalStorageWasm.mjs', 'tests/b.node-test.mjs', 'tests/a.node-test.mjs', 'benchmarks/startup/startup.node-test.mjs']
  assert.deepEqual(selectNodeTests(files), ['tests/a.node-test.mjs', 'tests/b.node-test.mjs'])
  assert.deepEqual(selectNodeTests(files, 'harness-node'), ['benchmarks/startup/startup.node-test.mjs'])
  assert.throws(() => selectNodeTests([ordinary]), /No Node/)
  assert.throws(() => selectNodeTests(['tests/a.node-test.mjs', 'tests/a.node-test.mjs']), /Duplicate/)
})

test('filesystem discovery includes untracked unknown roots, but not helpers, builds or symlinks', async () => {
  const root = await mkdtemp(join(tmpdir(), 'risunest-test-inventory-'))
  try {
    for (const directory of ['src', 'new-package', 'target', 'node_modules']) await mkdir(join(root, directory))
    for (const file of ['src/new.test.ts', 'src/helper.test.svelte.ts', 'new-package/unknown.test.ts', 'target/generated.test.ts', 'node_modules/vendor.test.ts']) {
      await writeFile(join(root, file), '')
    }
    await symlink(join(root, 'src'), join(root, 'linked'), 'junction')
    assert.deepEqual(await discoverTestEntries(root), ['new-package/unknown.test.ts', 'src/new.test.ts'])
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
