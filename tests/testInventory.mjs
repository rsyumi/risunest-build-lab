import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { assertInventory, assertListedTestsExist, discoverTestEntries, normalizePath, selectNodeTests } from './suiteOwnership.mjs'

export function parseVitestInventory(output, root, owner) {
  const entries = JSON.parse(output)
  if (!Array.isArray(entries) || entries.length === 0) throw new Error(`Empty or invalid ${owner ?? 'root'} Vitest inventory`)
  const result = owner ? { [owner]: [] } : { app: [], 'app-node': [], 'app-extended': [], 'app-lua-worker': [], harness: [] }
  for (const entry of entries) {
    if (!entry || typeof entry.file !== 'string') throw new Error('Unexpected Vitest list output')
    const name = owner ?? entry.projectName
    if (!Object.hasOwn(result, name)) throw new Error(`Unknown Vitest project: ${name}`)
    const path = normalizePath(relative(root, entry.file))
    if (path.startsWith('../') || path.includes(':')) throw new Error(`Test outside repository: ${entry.file}`)
    result[name].push(path)
  }
  return result
}

export function vitestInventory(root, directory = '.', owner) {
  const cwd = join(root, directory)
  const require = createRequire(pathToFileURL(join(cwd, 'package.json')))
  const cli = join(dirname(require.resolve('vitest/package.json')), 'vitest.mjs')
  const directoryRoot = mkdtempSync(join(tmpdir(), 'risunest-vitest-inventory-'))
  try {
    const output = join(directoryRoot, 'inventory.json')
    const result = spawnSync(process.execPath, [cli, 'list', '--filesOnly', `--json=${output}`], {
      cwd, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, windowsHide: true,
    })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`Vitest inventory failed (${owner ?? 'root'}): ${result.signal ?? result.status}\n${result.stdout}\n${result.stderr}`)
    return parseVitestInventory(readFileSync(output, 'utf8'), root, owner)
  } finally {
    rmSync(directoryRoot, { recursive: true, force: true })
  }
}


export function browserInventory(root) {
  const require = createRequire(pathToFileURL(join(root, 'package.json')))
  const cli = join(dirname(require.resolve('@playwright/test/package.json')), 'cli.js')
  const result = spawnSync(process.execPath, [cli, 'test', '--config', 'tests/browser/playwright.config.ts', '--list', '--reporter=json'], {
    cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, windowsHide: true,
  })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`Browser inventory failed: ${result.stderr}`)
  const report = JSON.parse(result.stdout)
  if (report.errors?.length) throw new Error('Browser test discovery failed')
  const files = new Set()
  function visit(suite) {
    for (const spec of suite.specs ?? []) files.add(normalizePath(relative(root, resolve(report.config.rootDir, spec.file))))
    for (const child of suite.suites ?? []) visit(child)
  }
  for (const suite of report.suites) visit(suite)
  if (files.size === 0) throw new Error('Empty browser inventory')
  return [...files].sort()
}

// The exporter runs `node --test tests/*.test.mjs` from its own package, so only top-level entries run.
export function webExportInventory(files, testScript) {
  if (testScript !== 'node --test tests/*.test.mjs') throw new Error(`Unexpected web export test script: ${testScript}`)
  return files.map(normalizePath).filter(path => /^tools\/web-risu-export\/tests\/[^/]+\.test\.mjs$/.test(path)).sort()
}

export async function main() {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  const files = await discoverTestEntries(root)
  assertListedTestsExist(files)
  const inventories = {
    ...vitestInventory(root),
    ...vitestInventory(root, 'server/manager/gui', 'manager'),
    ...vitestInventory(root, 'server/endpoint-registry', 'registry'),
    node: selectNodeTests(files),
    'harness-node': selectNodeTests(files, 'harness-node'),
    browser: browserInventory(root),
    'web-export': webExportInventory(files, JSON.parse(readFileSync(join(root, 'tools/web-risu-export/package.json'), 'utf8')).scripts?.test),
  }
  assertInventory(files, inventories)
  for (const [owner, selected] of Object.entries(inventories)) console.log(`${owner}: ${selected.length} files`)
  console.log('Every discovered test entry has exactly one runner owner.')
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch(error => { console.error(error); process.exitCode = 1 })
}
