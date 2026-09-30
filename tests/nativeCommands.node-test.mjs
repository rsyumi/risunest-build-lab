import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { embeddedCrateTests, hostManifests, nativeCommands, packageCrateTests, protocolManifests, sharedCargoTarget } from '../scripts/testNative.mjs'

test('native groups explicitly own dependency packages and full app integration targets', () => {
  const protocol = nativeCommands('protocol', 'win32')
  assert.deepEqual(protocol.map(command => command[3]), protocolManifests)
  assert.equal(protocol.length, 5)
  const host = nativeCommands('host', 'win32')
  assert.deepEqual(host.slice(0, 4).map(command => command[3]), hostManifests)
  for (const command of [...protocol, ...host]) {
    assert(command.includes('--locked'))
    assert(command.includes('--release'))
    assert(!command.includes('--ignored'))
  }
  assert(!host[0].includes('--lib'))
  assert.deepEqual(packageCrateTests.map(entry => entry.package), ['tauri-plugin-window-state', 'wry', 'tauri-plugin-updater'])
  for (const platform of ['win32', 'linux', 'darwin']) {
    assert.deepEqual(nativeCommands('host', platform).slice(hostManifests.length), packageCrateTests.map(entry => [
      'cargo', 'test', '--manifest-path', entry.runnerManifest,
      '--package', entry.package, '--release', '--locked', '--lib',
    ]))
  }
  assert(host.at(-1).includes('tauri-plugin-updater'))
  assert(host.at(-1).includes('--lib'))
  assert.deepEqual(nativeCommands('host', 'linux')[0], ['dbus-run-session', '--', 'bash', 'scripts/linux-native-tests.sh', '--release'])
  assert.throws(() => nativeCommands('everything'), /Expected/)
})

test('main checkout and worktrees share one target and reject an unrelated cache', () => {
  const main = join(tmpdir(), 'risunest-main')
  const worktree = join(tmpdir(), 'risunest-worktree')
  const target = join(main, 'src-tauri', 'target')
  assert.equal(sharedCargoTarget(join(main, '.git'), undefined, main), target)
  assert.equal(sharedCargoTarget(join(main, '.git'), target, worktree), target)
  assert.throws(() => sharedCargoTarget(join(main, '.git'), join(worktree, 'target'), worktree), /main checkout cache/)
  assert.throws(() => sharedCargoTarget(join(main, 'unknown'), undefined, worktree), /Cannot locate/)
})

test('every local Rust crate with tests has an explicit runner', () => {
  const selected = new Set([...protocolManifests, ...hostManifests, ...embeddedCrateTests.map(entry => entry.manifest), ...packageCrateTests.map(entry => entry.manifest)])
  for (const entry of packageCrateTests) {
    assert(entry.runnerManifest === entry.manifest || hostManifests.includes(entry.runnerManifest))
    assert.deepEqual(entry.platforms, ['win32', 'linux', 'darwin'])
    const manifest = readFileSync(new URL(`../${entry.manifest}`, import.meta.url), 'utf8')
    assert(manifest.includes(`name = "${entry.package}"`))
  }
  for (const entry of embeddedCrateTests) {
    assert(hostManifests.includes(entry.runnerManifest))
    assert.deepEqual(entry.platforms, ['win32', 'linux', 'darwin'])
    const harness = readFileSync(new URL(`../src-tauri/tests/${entry.integrationTarget}.rs`, import.meta.url), 'utf8')
    assert(harness.includes('#![cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]'))
    assert(harness.includes(`#[path = "../../${entry.source}"]`))
    assert(readFileSync(new URL(`../${entry.source}`, import.meta.url), 'utf8').includes('#[cfg(test)]'))
    for (const platform of entry.platforms) {
      const command = nativeCommands('host', platform)[0]
      if (platform === 'linux') {
        const wrapper = readFileSync(new URL('../scripts/linux-native-tests.sh', import.meta.url), 'utf8')
        assert(wrapper.includes('cargo test --manifest-path src-tauri/Cargo.toml --locked "$@"'))
        assert(!wrapper.includes('--lib'))
      } else {
        assert(command.includes(entry.runnerManifest))
        assert(!command.includes('--lib'))
      }
    }
  }
  const rustFiles = directory => readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    if (['target', 'node_modules', '.git'].includes(entry.name)) return []
    const path = join(directory, entry.name)
    return entry.isDirectory() ? rustFiles(path) : entry.name.endsWith('.rs') ? [path] : []
  })
  const missing = []
  for (const crate of readdirSync(new URL('../crates/', import.meta.url), { withFileTypes: true }).filter(entry => entry.isDirectory())) {
    const directory = new URL(`../crates/${crate.name}/src/`, import.meta.url)
    const files = rustFiles(fileURLToPath(directory))
    if (files.some(path => /#\[(?:tokio::)?test(?:\([^\]]*\))?\]/.test(readFileSync(path, 'utf8')))) {
      if (!selected.has(`crates/${crate.name}/Cargo.toml`)) missing.push(crate.name)
    }
  }
  assert.deepEqual(missing, [], `Missing native test runners: ${missing.join(', ')}`)
  assert(protocolManifests.includes('crates/small-object-store/Cargo.toml'))
})
