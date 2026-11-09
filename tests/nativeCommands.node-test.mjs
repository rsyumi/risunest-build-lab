import assert from 'node:assert/strict'
import { test } from 'node:test'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { appTestPackages, appTestProfile, embeddedCrateTests, harnessManifests, hostManifests, mluaHarnessTests, nativeCommands, packageCrateTests, protocolManifests, sharedCargoTarget, standaloneRustCommands, standaloneRustTests } from '../scripts/testNative.mjs'

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

test('the core workspace runs once more for the tokenizer with its Hugging Face families', () => {
  const base = ['cargo', 'test', '--manifest-path', 'crates/core/Cargo.toml']
  for (const platform of ['win32', 'linux', 'darwin']) {
    assert.deepEqual(nativeCommands('protocol', platform).filter(command => command.includes('crates/core/Cargo.toml')), [
      [...base, '--release', '--locked'],
      [...base, '--package', 'risunest-tokenizer', '--features', 'huggingface', '--release', '--locked'],
    ])
    assert(!nativeCommands('host', platform).some(command => command.includes('crates/core/Cargo.toml')))
  }
})

test('experimental CBS, regex and Lua tests run only through the explicit harness lane', () => {
  const manifests = [
    'crates/cbs-experimental/Cargo.toml',
    'benchmarks/rust-cbs/native/Cargo.toml',
    'crates/regex-compat-experimental/Cargo.toml',
    'benchmarks/rust-regex-compat/native/Cargo.toml',
    'crates/native-lua-experimental/Cargo.toml',
    'crates/regress/Cargo.toml',
  ]
  assert.deepEqual(harnessManifests, manifests)
  assert.deepEqual(mluaHarnessTests, {
    manifest: 'crates/mlua-terminal/Cargo.toml',
    runnerManifest: 'benchmarks/native-lua-containment/Cargo.toml',
    targets: [
      { name: 'upstream-thread', source: 'crates/mlua-terminal/tests/thread.rs' },
      { name: 'upstream-function', source: 'crates/mlua-terminal/tests/function.rs' },
      { name: 'upstream-memory', source: 'crates/mlua-terminal/tests/memory.rs' },
      { name: 'upstream-userdata', source: 'crates/mlua-terminal/tests/userdata.rs' },
      { name: 'upstream-table', source: 'crates/mlua-terminal/tests/table.rs' },
    ],
  })
  for (const platform of ['win32', 'linux', 'darwin']) {
    const commands = nativeCommands('harness', platform)
    for (const manifest of manifests) {
      const expected = manifest === 'crates/native-lua-experimental/Cargo.toml'
        ? [['cargo', 'test', '--manifest-path', manifest, '--locked', '--lib']]
        : manifest === 'crates/regress/Cargo.toml'
          ? [
            ['cargo', 'test', '--manifest-path', manifest, '--locked'],
            ['cargo', 'test', '--manifest-path', manifest, '--locked', '--features', 'utf16'],
          ]
          : [['cargo', 'test', '--manifest-path', manifest, '--release', '--locked']]
      assert.deepEqual(commands.filter(command => command.includes(manifest)), expected)
    }
    assert.deepEqual(commands.filter(command => command.includes(mluaHarnessTests.runnerManifest)), [[
      'cargo', 'test', '--manifest-path', 'benchmarks/native-lua-containment/Cargo.toml', '--locked', '--jobs', '2',
      '--test', 'upstream-thread', '--test', 'upstream-function', '--test', 'upstream-memory',
      '--test', 'upstream-userdata', '--test', 'upstream-table', '--', '--test-threads=1',
    ]])
    assert(!commands.some(command => command.includes(mluaHarnessTests.manifest)))
    for (const group of ['protocol', 'host']) {
      for (const manifest of [...manifests, mluaHarnessTests.manifest, mluaHarnessTests.runnerManifest]) {
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

test('host app selects patched four-coding library and Brotli documentation tests together', () => {
  const codec = packageCrateTests.find(entry => entry.package === 'compression-codecs')
  assert.deepEqual(codec, {
    manifest: 'crates/compression-codecs/Cargo.toml',
    runnerManifest: 'src-tauri/Cargo.toml',
    package: 'compression-codecs',
    platforms: ['win32', 'linux', 'darwin'],
    withApp: true,
    doctests: ['crates/compression-codecs/src/brotli/params.rs'],
  })
  assert.deepEqual(appTestPackages, ['--package', 'risunest', '--package', 'compression-codecs'])
  for (const platform of codec.platforms) {
    const commands = nativeCommands('host', platform)
    const app = commands[0]
    assert.deepEqual(app.slice(-6, -2), appTestPackages)
    assert(!app.includes('--lib'))
    assert(!app.includes('--doc'))
    assert(!commands.slice(1).some(command => command.includes(codec.package)))
    for (const group of ['protocol', 'harness']) {
      assert(!nativeCommands(group, platform).some(command => command.includes(codec.package)))
    }
  }
  for (const [manifest, path] of [
    ['src-tauri/Cargo.toml', '../crates/compression-codecs'],
    ['tests/native/native/Cargo.toml', '../../../crates/compression-codecs'],
    ['benchmarks/macos/native/Cargo.toml', '../../../crates/compression-codecs'],
    ['benchmarks/ios/native/Cargo.toml', '../../../crates/compression-codecs'],
  ]) {
    const source = readFileSync(new URL(`../${manifest}`, import.meta.url), 'utf8')
    const patches = source.split('[patch.crates-io]')[1]?.split(/^\[/m)[0]
    assert(patches?.includes(`compression-codecs = { path = "${path}" }`), manifest)
  }
  const appManifest = readFileSync(new URL('../src-tauri/Cargo.toml', import.meta.url), 'utf8')
  const compression = appManifest.split('\n').find(line => line.startsWith('async-compression = '))
  assert(compression?.includes('default-features = false'))
  assert(compression?.includes('features = ["gzip", "zlib", "brotli", "zstd"]'))
  const example = readFileSync(new URL(`../${codec.doctests[0]}`, import.meta.url), 'utf8')
  assert(example.includes('/// use compression_codecs::brotli;'))
})

test('every local Rust crate with tests has an explicit runner', () => {
  const selected = new Set([...protocolManifests, ...hostManifests, ...harnessManifests, mluaHarnessTests.manifest, ...embeddedCrateTests.map(entry => entry.manifest), ...packageCrateTests.map(entry => entry.manifest)])
  const runnerUrl = new URL(`../${mluaHarnessTests.runnerManifest}`, import.meta.url)
  const runnerManifest = readFileSync(runnerUrl, 'utf8')
  const upstreamTargets = new Map([...runnerManifest.matchAll(/\[\[test\]\]\s*name = "([^"]+)"\s*path = "([^"]+)"/g)].map(match => [match[1], match[2]]))
  for (const target of mluaHarnessTests.targets) {
    assert(upstreamTargets.has(target.name), `Missing mlua harness target: ${target.name}`)
    assert.equal(
      fileURLToPath(new URL(upstreamTargets.get(target.name), runnerUrl)),
      fileURLToPath(new URL(`../${target.source}`, import.meta.url)),
    )
  }
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
  // A virtual workspace has no src of its own; its members run through the workspace manifest.
  const sources = directory => existsSync(join(directory, 'src')) ? [join(directory, 'src')]
    : readdirSync(directory, { withFileTypes: true }).filter(entry => entry.isDirectory() && existsSync(join(directory, entry.name, 'src')))
      .map(entry => join(directory, entry.name, 'src'))
  const missing = []
  for (const crate of readdirSync(new URL('../crates/', import.meta.url), { withFileTypes: true }).filter(entry => entry.isDirectory())) {
    const files = sources(fileURLToPath(new URL(`../crates/${crate.name}/`, import.meta.url))).flatMap(rustFiles)
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
    'src-tauri/src/native_generation/ollama_transport.rs',
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
        // Tests may call transports directly; product entry points remain explicitly reviewed.
        const product = readFileSync(join(root, path), 'utf8').split(/^#\[cfg\((?:all\()?test\b[^\n]*\]\r?\n(?:pub(?:\([^)]*\))? )?mod \w+ \{/m)[0]
        if (nativeNetworkBypasses(product).length) found.push(path)
      }
    }
  }
  for (const directory of ['src-tauri/src', 'crates/network-log/src', 'crates/tauri-plugin-http/src', 'crates/tauri-plugin-updater/src']) scan(directory)
  assert.deepEqual(found.filter(path => !reviewed.includes(path)), [])
  assert(found.includes('crates/network-log/src/reqwest.rs'))
  assert(found.includes('src-tauri/src/native_generation/ollama_transport.rs'))
  assert.equal(nativeNetworkBypasses('client.get(url).send().await?;').length, 1)
  assert.equal(nativeNetworkBypasses('client.get(url).send_logged("test").await?;').length, 0)
})
