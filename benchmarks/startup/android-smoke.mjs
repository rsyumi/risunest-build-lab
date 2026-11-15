import { runnerOptions } from '../support/androidTarget.mjs'

// Keep argument validation, but perform no device or application actions.
runnerOptions(process.argv.slice(2))
console.error(JSON.stringify({
    phase: 'preflight',
    profile: 'legacy-api21-mutation',
    success: false,
    reason: 'unsupported-legacy-profile',
}))
process.exitCode = 1
