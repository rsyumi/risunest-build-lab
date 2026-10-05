import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const app = readFileSync('src/App.svelte', 'utf8')

function effectStarting(marker: string): string {
    const at = app.indexOf(marker)
    expect(at).toBeGreaterThan(-1)
    const start = app.lastIndexOf('$effect(', at)
    const end = app.indexOf('\n    })', at)
    expect(start).toBeGreaterThan(-1)
    expect(end).toBeGreaterThan(at)
    return app.slice(start, end)
}

describe('startup update check', () => {
    it('is gated only by the update exclusion', () => {
        const effect = effectStarting('startAppUpdateChecks')
        const exclusions = [...effect.matchAll(/isStartupExcluded\('([^']+)'/g)].map((match) => match[1])
        expect(exclusions).toEqual(['autoUpdate'])
    })
})
