import { appendFileSync } from 'node:fs'

const targets = new Set([
    'src/ts/storage/presetWorkingSetOperations.test.ts',
    'src/ts/storage/nativePngCardAdapter.test.ts',
    'src/ts/storage/accountStorage.test.ts',
    'benchmarks/streaming/rerollCapture.test.ts',
])

export default class LoadFailureReporter {
    record(entity, phase, details = {}) {
        const module = entity.type === 'module' ? entity : entity.module
        const path = module.relativeModuleId.replaceAll('\\', '/')
        if (!targets.has(path) || !process.env.RISUNEST_TEST_PHASE_OUTPUT) return
        appendFileSync(process.env.RISUNEST_TEST_PHASE_OUTPUT, JSON.stringify({
            at: new Date().toISOString(), module: path,
            test: entity.type === 'test' ? entity.fullName : null,
            phase, ...details,
        }) + '\n')
    }
    onTestModuleQueued(module) { this.record(module, 'queued') }
    onTestModuleCollected(module) { this.record(module, 'collected') }
    onTestModuleStart(module) { this.record(module, 'module-start') }
    onTestModuleEnd(module) {
        const { environmentSetupDuration, prepareDuration, collectDuration, setupDuration, duration } = module.diagnostic()
        this.record(module, 'module-end', {
            state: module.state(),
            timing: { environmentSetupDuration, prepareDuration, collectDuration, setupDuration, duration },
        })
    }
    onTestCaseReady(test) { this.record(test, 'test-ready') }
    onTestCaseResult(test) {
        this.record(test, 'test-result', { state: test.result().state, duration: test.diagnostic()?.duration })
    }
    onHookStart(hook) { this.record(hook.entity, `${hook.name}-start`) }
    onHookEnd(hook) { this.record(hook.entity, `${hook.name}-end`) }
}
