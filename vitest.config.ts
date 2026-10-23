import { defineConfig, mergeConfig } from 'vitest/config'
import { sharedVitestConfig } from './tests/vitestShared'
import { appTestIncludes, extendedAppTests, harnessVitestTests, luaWorkerTests, nodeAppTests, separateRunnerPaths } from './tests/suiteOwnership.mjs'

export default mergeConfig(sharedVitestConfig(), defineConfig({
  test: {
    coverage: {
      provider: 'v8',
      enabled: false,
      include: ['src/**/*.{ts,svelte}'],
      exclude: [
        'src/**/*.d.ts',
        'src/**/*.{test,spec}.?(c|m)[jt]s?(x)',
        'src/**/*.test.svelte',
        'src/**/*.test.svelte.ts',
        'src/**/*.bench.ts',
        'src/**/tests/**',
        'src/**/{__tests__,__fixtures__}/**',
        'src/**/test-fixtures/**',
        'src/**/*.testSupport.ts',
        'src/**/*.testUtils.ts',
        'src/ts/realmEndpoints.blocked.ts',
      ],
      reporter: ['text-summary', 'json-summary', 'lcov', 'html'],
      reportsDirectory: '.tmp/test-results/coverage',
      reportOnFailure: true,
    },
    projects: [
      {
        extends: true,
        optimizeDeps: {
          // Exercise the tokenizer's WebView WASM path in happy-dom.
          rolldownOptions: {
            transform: { define: { 'process.versions.node': 'undefined' } },
          },
        },
        test: {
          name: 'app',
          deps: {
            optimizer: {
              // Bundle the UMD tokenizer so its named exports are available.
              client: {
                enabled: true,
                include: ['@mlc-ai/web-tokenizers'],
              },
            },
          },
          include: appTestIncludes,
          exclude: [...extendedAppTests, ...nodeAppTests, ...harnessVitestTests, ...separateRunnerPaths],
        },
      },
      {
        extends: true,
        test: { name: 'app-node', include: nodeAppTests, environment: 'node' },
      },
      {
        extends: true,
        test: {
          name: 'app-lua-worker', include: luaWorkerTests, environment: 'node',
          setupFiles: ['src/ts/process/tests/luaWorkerSetup.ts'],
          provide: { luaWorker: true },
          testTimeout: 15_000,
        },
      },
      {
        extends: true,
        test: { name: 'app-extended', include: extendedAppTests },
      },
      {
        extends: true,
        test: { name: 'harness', include: harnessVitestTests, exclude: separateRunnerPaths },
      },
    ],
  },
}))
