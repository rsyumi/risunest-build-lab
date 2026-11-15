import { resolve } from 'node:path'
import { defineConfig, mergeConfig } from 'vite'
import productConfig from '../../../vite.config'
import { executionProfileProbe, repositoryRoot } from './probe.mjs'

export default defineConfig(async environment => {
    if (environment.command !== 'build' || environment.mode !== 'agent') {
        throw new Error('execution-profile:agent-build-required')
    }
    const base = await (typeof productConfig === 'function' ? productConfig(environment) : productConfig)
    return mergeConfig(base, {
        root: repositoryRoot,
        plugins: [executionProfileProbe()],
        build: { outDir: resolve(repositoryRoot, '.tmp/test-results/execution-profile/dist'), emptyOutDir: true, sourcemap: true },
    })
})
