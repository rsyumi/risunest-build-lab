import { defineConfig } from 'vite'
import { resolve } from 'node:path'
import boundaryConfig from '../vite.config'

export default defineConfig((environment) => {
    const config = typeof boundaryConfig === 'function' ? boundaryConfig(environment) : boundaryConfig
    return {
        ...config,
        root: resolve(import.meta.dirname),
        build: {
            outDir: resolve(import.meta.dirname, '../../../.tmp/test-results/renderer-loss/dist'),
            emptyOutDir: true,
        },
    }
})
