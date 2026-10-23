import { defineConfig, mergeConfig } from 'vitest/config'
import { sharedVitestConfig } from '../../tests/vitestShared'
import { generationSendProbe } from './probe'

export default mergeConfig(sharedVitestConfig(), defineConfig({
    plugins: [generationSendProbe()],
    test: { include: ['benchmarks/generation-send/send.test.ts'], maxWorkers: 1, fileParallelism: false },
}))
