import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { appTestProfile, embeddedCrateTests, harnessManifests, hostManifests, nativeCommands, packageCrateTests, protocolManifests, sharedCargoTarget, standaloneRustCommands, standaloneRustTests } from '../scripts/testNative.mjs'

test('the Sync server runs as one plain cargo test and its source observer only through --harness', () => {
  const base = ['cargo', 'test', '--manifest-path', 'server/sync/Cargo.toml', '--release', '--locked']
  for (const platform of ['win32', 'linux', 'darwin']) {
    for (const group of ['protocol', 'host']) {
      const server = nativeCommands(group, platform).filter(command => command.includes('server/sync/Cargo.toml'))
      assert.deepEqual(server, group === 'host' ? [base] : [])
    }
  }
  assert.deepEqual(nativeCommands('harness').filter(command => command.includes('server/sync/Cargo.toml')), [
    [...base, '--lib', '--features', 'source-observer', '--', '--skip', 'source_observer::small_object_store::tests'],
  ])
})

test('experimental CBS tests run only through the explicit harness lane', () => {
  const manifests = ['crates/cbs-experimental/Cargo.toml', 'benchmarks/rust-cbs/native/Cargo.toml']
  assert.deepEqual(harnessManifests, manifests)
  for (const platform of ['win32', 'linux', 'darwin']) {
    for (const manifest of manifests) {
      assert.deepEqual(nativeCommands('harness', platform).filter(command => command.includes(manifest)), [
        ['cargo', 'test', '--manifest-path', manifest, '--release', '--locked'],
      ])
      for (const group of ['protocol', 'host']) {
        assert(!nativeCommands(group, platform).some(command => command.includes(manifest)))
      }
    }
  }
})

test('host runs compile only the app crate under test unoptimized', () => {
  assert.deepEqual(appTestProfile, ['--config', 'profile.release.package.risunest.opt-level=0'])
  for (const platform of ['win32', 'linux', 'darwin']) {
    const [app, ...others] = nativeCommands('host', platform)
    assert(app.includes(platform === 'linux' ? 'scripts/linux-native-tests.sh' : 'src-tauri/Cargo.toml'))
    assert.deepEqual(app.slice(-2), appTestProfile)
    for (const command of [...others, ...nativeCommands('protocol', platform)]) assert(!command.includes('--config'), command.join(' '))
  }
})

test('host mode compiles and runs the std-only Android response body tests as their own test binary', () => {
  assert.deepEqual(standaloneRustTests.map(entry => entry.source), ['crates/wry/src/android/response_bodies.rs'])
  const directory = join(tmpdir(), 'risunest-standalone-rust')
  for (const platform of ['win32', 'linux', 'darwin']) {
    const binary = join(directory, `wry_android_response_bodies${platform === 'win32' ? '.exe' : ''}`)
    assert.deepEqual(standaloneRustCommands(directory, platform), [
      ['rustc', '--edition', '2021', '--test', 'crates/wry/src/android/response_bodies.rs', '-o', binary],
      [binary],
    ])
  }
  for (const entry of standaloneRustTests) {
    const source = readFileSync(new URL(`../${entry.source}`, import.meta.url), 'utf8')
    assert(source.includes('#[cfg(test)]'))
    // Only std and the module itself resolve when the file is compiled as its own crate.
    assert.doesNotMatch(source, /\bcrate::|^\s*use (?!std::|super::)/m)
  }
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
  const selected = new Set([...protocolManifests, ...hostManifests, ...harnessManifests, ...embeddedCrateTests.map(entry => entry.manifest), ...packageCrateTests.map(entry => entry.manifest)])
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


function nativeNetworkBypasses(source) {
  return [...source.matchAll(/\.send\s*\(\s*\)|\.execute\s*\(\s*request\s*\)|\bconnect_async(?:_tls_with_config|_with_config)?\s*\(/g)].map(match => match[0].replace(/\s+/g, ''))
}

test('native HTTP and WebSocket raw entry points stay in the reviewed adapters', () => {
  const root = fileURLToPath(new URL('../', import.meta.url))
  const reviewed = [
    'src-tauri/src/server_sync/client.rs',
    'src-tauri/src/server_sync/notification.rs',
    'crates/network-log/src/reqwest.rs',
    'crates/tauri-plugin-updater/src/network_log.rs',
  ]
  const found = []
  function scan(directory) {
    for (const entry of readdirSync(join(root, directory), { withFileTypes: true })) {
      const path = `${directory}/${entry.name}`
      if (entry.isDirectory()) scan(path)
      else if (entry.name.endsWith('.rs') && !/(?:^|[/_])tests?\.rs$/.test(path)) {
        // Tests may call the transport directly; product code goes through the logged adapters.
        const product = readFileSync(join(root, path), 'utf8').split(/^#\[cfg\((?:all\()?test\b[^\n]*\]\r?\n(?:pub(?:\([^)]*\))? )?mod \w+ \{/m)[0]
        if (nativeNetworkBypasses(product).length) found.push(path)
      }
    }
  }
  for (const directory of ['src-tauri/src', 'crates/network-log/src', 'crates/tauri-plugin-http/src', 'crates/tauri-plugin-updater/src']) scan(directory)
  assert.deepEqual(found.filter(path => !reviewed.includes(path)), [])
  assert(found.includes('crates/network-log/src/reqwest.rs'))
  assert.equal(nativeNetworkBypasses('client.get(url).send().await?;').length, 1)
  assert.equal(nativeNetworkBypasses('client.get(url).send_logged("test").await?;').length, 0)
})
