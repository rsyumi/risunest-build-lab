import { spawnSync } from 'node:child_process'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

export const protocolManifests = [
  'crates/sync-wire/Cargo.toml',
  'crates/sync-connect/Cargo.toml',
  'crates/external-storage-format/Cargo.toml',
  'crates/release-update/Cargo.toml',
  'crates/small-object-store/Cargo.toml',
]
export const hostManifests = [
  'src-tauri/Cargo.toml',
  'server/sync/Cargo.toml',
  'server/manager/Cargo.toml',
  'server/manager/gui/src-tauri/Cargo.toml',
]

export const packageCrateTests = [
  {
    manifest: 'crates/tauri-plugin-window-state/Cargo.toml',
    runnerManifest: 'src-tauri/Cargo.toml',
    package: 'tauri-plugin-window-state',
    platforms: ['win32', 'linux', 'darwin'],
  },
  {
    manifest: 'crates/wry/Cargo.toml',
    runnerManifest: 'crates/wry/Cargo.toml',
    package: 'wry',
    platforms: ['win32', 'linux', 'darwin'],
  },
  {
    manifest: 'crates/tauri-plugin-updater/Cargo.toml',
    runnerManifest: 'src-tauri/Cargo.toml',
    package: 'tauri-plugin-updater',
    platforms: ['win32', 'linux', 'darwin'],
  },
]

export const embeddedCrateTests = [
  {
    manifest: 'crates/tauri-plugin-ios-native/Cargo.toml',
    runnerManifest: 'src-tauri/Cargo.toml',
    integrationTarget: 'ios_native_contracts',
    source: 'crates/tauri-plugin-ios-native/src/lib.rs',
    platforms: ['win32', 'linux', 'darwin'],
  },
]

export function sharedCargoTarget(commonGitDirectory, configuredTarget, root) {
  const common = resolve(root, commonGitDirectory.trim())
  if (basename(common) !== '.git') throw new Error('Cannot locate the main checkout shared Cargo target')
  const target = join(dirname(common), 'src-tauri', 'target')
  if (configuredTarget && resolve(root, configuredTarget) !== target) {
    throw new Error(`CARGO_TARGET_DIR must use the main checkout cache: ${target}`)
  }
  return target
}

export function nativeCommands(group, platform = process.platform) {
  if (!['protocol', 'host'].includes(group)) throw new Error('Expected protocol or host')
  const commands = []
  for (const manifest of group === 'protocol' ? protocolManifests : hostManifests) {
    if (manifest === 'src-tauri/Cargo.toml' && platform === 'linux') {
      commands.push(['dbus-run-session', '--', 'bash', 'scripts/linux-native-tests.sh', '--release'])
    } else if (manifest === 'server/sync/Cargo.toml') {
      commands.push(['cargo', 'test', '--manifest-path', manifest, '--release', '--locked', '--', '--skip', 'source_observer'])
    } else {
      commands.push(['cargo', 'test', '--manifest-path', manifest, '--release', '--locked'])
    }
  }
  if (group === 'host') {
    const server = ['cargo', 'test', '--manifest-path', 'server/sync/Cargo.toml', '--release', '--locked', '--lib']
    commands.push([...server, 'source_observer', '--', '--skip', 'source_observer::small_object_store::tests'])
    commands.push([...server, 'source_observer::small_object_store::tests'])
    for (const entry of packageCrateTests.filter(entry => entry.platforms.includes(platform))) {
      commands.push([
        'cargo', 'test', '--manifest-path', entry.runnerManifest,
        '--package', entry.package, '--release', '--locked', '--lib',
      ])
    }
  }
  return commands
}

export function runNative(group) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  const git = spawnSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root, encoding: 'utf8', windowsHide: true })
  if (git.error || git.status !== 0) throw git.error ?? new Error(git.stderr)
  const env = { ...process.env, CARGO_TARGET_DIR: sharedCargoTarget(git.stdout, process.env.CARGO_TARGET_DIR, root) }
  for (const [command, ...args] of nativeCommands(group)) {
    console.log(`> ${command} ${args.join(' ')}`)
    const result = spawnSync(command, args, { cwd: root, env, stdio: 'inherit', windowsHide: true })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`${command} failed: ${result.signal ?? result.status}`)
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    if (process.argv[2] === '--help') {
      console.log('Usage: node scripts/testNative.mjs protocol|host\nRequires the checked-in Rust toolchain and native build dependencies. Linux host tests require dbus-run-session and gnome-keyring-daemon. Uses only the main checkout src-tauri/target; never installs toolchains or skips missing prerequisites.')
    } else {
      if (process.argv.length !== 3) throw new Error('Usage: node scripts/testNative.mjs protocol|host')
      runNative(process.argv[2])
    }
  } catch (error) {
    console.error(error)
    process.exitCode = 1
  }
}
