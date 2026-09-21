import { copyFileSync, existsSync, mkdirSync, readdirSync } from 'node:fs'
import { basename, dirname, join, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath, pathToFileURL } from 'node:url'

const targets = new Set(['aarch64', 'armv7', 'i686', 'x86_64'])

function generatedDirectory(root) {
  return join(
    root,
    'src-tauri',
    'gen',
    'android',
    'app',
    'src',
    'main',
    'java',
    'io',
    'github',
    'rsyumi',
    'risunest',
    'generated',
  )
}

export function androidGeneratedCopies(target, existingFiles, sharedFiles) {
  if (!targets.has(target)) throw new Error(`Unsupported Android target: ${target}`)
  return [...sharedFiles].filter(file => !existingFiles.has(file)).sort()
}

export function prepareAndroidGenerated(target) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  const git = spawnSync(
    'git',
    ['rev-parse', '--path-format=absolute', '--git-common-dir'],
    { cwd: root, encoding: 'utf8' },
  )
  if (git.error || git.status !== 0) throw git.error ?? new Error(git.stderr)
  const common = resolve(root, git.stdout.trim())
  if (basename(common) !== '.git') throw new Error('Cannot locate the main checkout Android bindings')

  const mainRoot = dirname(common)
  if (mainRoot === root) return
  const generated = generatedDirectory(root)
  const sharedGenerated = generatedDirectory(mainRoot)
  if (!existsSync(sharedGenerated)) return

  const files = directory => new Set(
    existsSync(directory)
      ? readdirSync(directory, { withFileTypes: true }).filter(entry => entry.isFile()).map(entry => entry.name)
      : [],
  )
  const copies = androidGeneratedCopies(target, files(generated), files(sharedGenerated))
  if (!copies.length) return

  mkdirSync(generated, { recursive: true })
  for (const file of copies) copyFileSync(join(sharedGenerated, file), join(generated, file))
  console.log(`Restored ${copies.length} cached Android binding file(s) from the main checkout.`)
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: node scripts/prepareAndroidGenerated.mjs <target>')
    prepareAndroidGenerated(process.argv[2])
  } catch (error) {
    console.error(error)
    process.exitCode = 1
  }
}
