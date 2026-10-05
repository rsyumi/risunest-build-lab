import assert from 'node:assert/strict'
import { join, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'
import { localBuild } from '../scripts/buildLocal.mjs'

const main = resolve('fixture-main')
const worktree = join(main, '.worktrees', 'iteration')
const target = join(main, 'src-tauri', 'target')

test('main and worktree local builds reuse the same target without changing the parent environment', () => {
  const env = { CARGO_INCREMENTAL: '0', RUSTFLAGS: '-C debuginfo=1' }
  for (const repository of [main, worktree]) {
    const command = localBuild({ repository, commonGitDirectory: join(main, '.git'), host: 'x86_64-pc-windows-msvc', env })
    assert.equal(command.env.CARGO_TARGET_DIR, target)
    assert.equal(command.env.CARGO_INCREMENTAL, undefined)
    assert.equal(command.env.RUSTFLAGS, env.RUSTFLAGS)
    assert.deepEqual(command.args.slice(1), [
      'build', '--target', 'x86_64-pc-windows-msvc', '--no-bundle', '--', '--locked',
      '--config', 'profile.release.package.risunest.incremental=true',
      '--config', 'profile.release.package.risunest.codegen-units=16',
    ])
  }
  assert.equal(env.CARGO_INCREMENTAL, '0')
})

test('agent builds retain the agent config before the Cargo argument separator on each desktop host', () => {
  for (const host of ['x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-apple-darwin']) {
    const command = localBuild({ repository: worktree, commonGitDirectory: join(main, '.git'), host, agent: true, env: { CARGO_TARGET_DIR: target } })
    const separator = command.args.indexOf('--')
    assert.deepEqual(command.args.slice(separator - 2, separator), ['--config', 'src-tauri/tauri.agent.conf.json'])
    assert.equal(command.args[command.args.indexOf('--target') + 1], host)
    assert.ok(!command.args.includes('--profile'))
  }
})

test('a different target directory and an invalid Git root fail before compiling', () => {
  const args = { repository: worktree, commonGitDirectory: join(main, '.git'), host: 'x86_64-pc-windows-msvc' }
  assert.throws(() => localBuild({ ...args, env: { CARGO_TARGET_DIR: 'src-tauri/target' } }), /main checkout cache/)
  assert.throws(() => localBuild({ ...args, commonGitDirectory: main, env: {} }), /Cannot locate/)
  assert.throws(() => localBuild({ ...args, host: undefined, env: {} }), /Invalid Rust host/)
})

test('help does not run a build and unknown options fail', () => {
  const script = resolve('scripts/buildLocal.mjs')
  const help = spawnSync(process.execPath, [script, '--help'], { encoding: 'utf8' })
  assert.equal(help.status, 0)
  assert.match(help.stdout, /application-only incremental/)
  const invalid = spawnSync(process.execPath, [script, '--debug'], { encoding: 'utf8' })
  assert.equal(invalid.status, 1)
  assert.match(invalid.stderr, /Usage:/)
})
