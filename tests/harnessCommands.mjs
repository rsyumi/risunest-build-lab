import { readdirSync, readFileSync } from 'node:fs'
import { join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

// Harnesses call product and harness commands by name, which type checks cannot see.
const root = resolve(fileURLToPath(new URL('..', import.meta.url)))
const handlerRoots = ['src-tauri/src', 'benchmarks', 'tests/native']
const callerRoots = ['benchmarks', 'tests/native']
const skipped = new Set(['node_modules', 'dist', 'target', 'gen'])

function* files(directory, extensions) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.name.startsWith('.') || skipped.has(entry.name)) continue
    const path = join(directory, entry.name)
    if (entry.isDirectory()) yield* files(path, extensions)
    else if (extensions.some((extension) => entry.name.endsWith(extension))) yield path
  }
}

export function handlerCommands(source) {
  const commands = new Set()
  for (const match of source.matchAll(/generate_handler!\s*\[/g)) {
    let depth = 1
    let end = match.index + match[0].length
    for (; depth > 0; end++) {
      if (end >= source.length) throw new Error('Unterminated generate_handler!')
      if (source[end] === '[') depth++
      else if (source[end] === ']') depth--
    }
    const body = source.slice(match.index + match[0].length, end - 1).replace(/#\[[^\]]*\]/g, '')
    for (const path of body.split(',')) {
      const name = path.trim().split('::').at(-1)
      if (name) commands.add(name)
    }
  }
  return commands
}

export function invokedCommands(source) {
  return [...source.matchAll(/\binvoke\s*(?:<[^>()]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g)].map((match) => match[1])
}

export function missingHarnessCommands(base = root) {
  const registered = new Set()
  for (const directory of handlerRoots) {
    for (const file of files(join(base, directory), ['.rs'])) {
      for (const command of handlerCommands(readFileSync(file, 'utf8'))) registered.add(command)
    }
  }
  const missing = []
  for (const directory of callerRoots) {
    for (const file of files(join(base, directory), ['.ts', '.mjs', '.js', '.svelte'])) {
      for (const command of invokedCommands(readFileSync(file, 'utf8'))) {
        if (!registered.has(command)) missing.push(`${relative(base, file).replaceAll('\\', '/')}: ${command}`)
      }
    }
  }
  return missing
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const missing = missingHarnessCommands()
  if (missing.length) {
    console.error(`Harness commands without a registered handler:\n${missing.join('\n')}`)
    process.exit(1)
  }
  console.log('Every command a harness invokes by name has a registered handler')
}
