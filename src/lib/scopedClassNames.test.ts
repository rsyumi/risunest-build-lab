import { readFileSync } from 'node:fs'
import { execSync } from 'node:child_process'
import { describe, expect, it } from 'vitest'

// Tailwind generates single-word utilities such as `.fixed` and `.hidden`. A component
// that styles its own `.fixed` modifier also picks up `position: fixed`, so scoped
// class names must not reuse those words. `ConnectionForm` selects the utility on purpose.
const utilityWords = [
    'absolute', 'antialiased', 'block', 'border', 'capitalize', 'collapse', 'container',
    'contents', 'fixed', 'filter', 'flex', 'grid', 'grow', 'hidden', 'inline', 'invisible',
    'isolate', 'italic', 'lowercase', 'outline', 'overline', 'relative', 'resize', 'ring',
    'rounded', 'shadow', 'shrink', 'sr-only', 'static', 'sticky', 'table', 'transform',
    'transition', 'truncate', 'underline', 'uppercase', 'visible',
]
const allowed: Record<string, string[]> = {
    'src/lib/Setting/ExternalStorage/ConnectionForm.svelte': ['contents'],
}

describe('scoped class names', () => {
    it('do not reuse Tailwind utility words', () => {
        const files = execSync('git ls-files "src/lib/**/*.svelte"', { encoding: 'utf8' })
            .split(/\r?\n/)
            .filter(Boolean)
        const collisions: string[] = []
        for (const file of files) {
            const style = readFileSync(file, 'utf8').match(/<style[^>]*>([\s\S]*?)<\/style>/)?.[1]
            if (!style) continue
            const names = new Set([...style.matchAll(/\.([a-z][a-z0-9-]*)/g)].map((match) => match[1]))
            for (const name of names) {
                if (utilityWords.includes(name) && !allowed[file]?.includes(name)) collisions.push(`${file}: .${name}`)
            }
        }
        expect(collisions).toEqual([])
    })
})
