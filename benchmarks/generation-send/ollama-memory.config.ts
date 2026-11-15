import { defineConfig, mergeConfig } from 'vitest/config'
import { sharedVitestConfig } from '../../tests/vitestShared'
export default mergeConfig(sharedVitestConfig(), defineConfig({ test: {
    include: ['benchmarks/generation-send/ollama-memory-oracle.test.ts'], maxWorkers: 1, fileParallelism: false,
} }))
