import { spawnSync } from 'node:child_process'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

export function localBuild({ repository, commonGitDirectory, host, agent = false, env = process.env }) {
  const common = resolve(repository, commonGitDirectory.trim())
  if (basename(common) !== '.git') throw new Error('Cannot locate the main checkout shared Cargo target')
  const target = join(dirname(common), 'src-tauri', 'target')
  if (env.CARGO_TARGET_DIR && resolve(repository, env.CARGO_TARGET_DIR) !== target) {
    throw new Error(`CARGO_TARGET_DIR must use the main checkout cache: ${target}`)
  }
  if (!/^[a-z0-9_]+(?:-[a-z0-9_]+)+$/.test(host)) throw new Error('Invalid Rust host target')
  const childEnv = { ...env, CARGO_TARGET_DIR: target }
  // The global override would also change incremental compilation for dependencies.
  delete childEnv.CARGO_INCREMENTAL
  return {
    args: [
      join(repository, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'),
      'build', '--target', host, '--no-bundle',
      ...(agent ? ['--config', 'src-tauri/tauri.agent.conf.json'] : []),
      '--', '--locked',
      '--config', 'profile.release.package.risunest.incremental=true',
      '--config', 'profile.release.package.risunest.codegen-units=16',
    ],
    env: childEnv,
  }
}

function capture(command, args) {
  const result = spawnSync(command, args, { cwd: root, encoding: 'utf8', windowsHide: true })
  if (result.error || result.status !== 0) throw result.error ?? new Error(result.stderr || `${command} failed`)
  return result.stdout.trim()
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    const args = process.argv.slice(2)
    if (args.length > 1 || (args.length && !['--agent', '--help'].includes(args[0]))) {
      throw new Error('Usage: node scripts/buildLocal.mjs [--agent | --help]')
    }
    if (args[0] === '--help') {
      console.log('Build an optimized host executable without installers, using the shared Cargo target and application-only incremental compilation. The first build prepares the cache; subsequent Rust edits can reuse it. Use the normal platform build command for distribution.')
    } else {
      const host = capture('rustc', ['-vV']).split(/\r?\n/).find(line => line.startsWith('host: '))?.slice(6)
      const command = localBuild({
        repository: root,
        commonGitDirectory: capture('git', ['rev-parse', '--path-format=absolute', '--git-common-dir']),
        host,
        agent: args[0] === '--agent',
      })
      const result = spawnSync(process.execPath, command.args, { cwd: root, env: command.env, stdio: 'inherit', windowsHide: true })
      if (result.error) throw result.error
      process.exitCode = result.status ?? 1
    }
  } catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
