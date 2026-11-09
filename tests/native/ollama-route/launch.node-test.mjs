import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { test } from 'node:test'
import { launchSpecification, nativeProfileEnvironment, nativeTestEntry, ownedCommandLinePattern, ownedProcessScript } from './launch.mjs'

const profile = 'agent-ollama-012345abcdef-0'
const executable = 'C:\\Synthetic UI\\RisuNest-test.exe'

test('normal and ignored-test launches keep separate arguments and profile inputs', () => {
    const environment = { SYNTHETIC: 'retained', [nativeProfileEnvironment]: 'personal' }
    assert.deepEqual(launchSpecification('binary', profile, environment), {
        args: [`--profile=${profile}`], env: { SYNTHETIC: 'retained' },
    })
    const first = launchSpecification('test-executable', profile, environment)
    assert.deepEqual(first, {
        args: ['--ignored', '--exact', 'ollama_ui_verification::run_app', '--test-threads=1'],
        env: { SYNTHETIC: 'retained', [nativeProfileEnvironment]: profile },
    })
    assert.deepEqual(launchSpecification('test-executable', profile, environment), first, 'Restart selects the same owned profile and only one test')
    assert.equal(environment[nativeProfileEnvironment], 'personal', 'Parent environment is not mutated')
    for (const value of ['', 'personal', 'agent-other-012345abcdef-0', 'agent-ollama-012345abcdeg-0',
        'agent-ollama-012345abcdef-', 'agent-ollama-012345abcdef-x', 'agent-ollama-012345abcdef-0/../personal',
        'agent-ollama-012345abcdef-000000000']) {
        assert.throws(() => launchSpecification('test-executable', value), /Owned synthetic profile/)
    }
    assert.throws(() => launchSpecification('unknown', profile), /Known native UI launch mode/)
})

test('test executable ownership requires the exact ignored test command line', () => {
    const pattern = new RegExp(ownedCommandLinePattern(executable, 'test-executable', profile))
    const args = `--ignored --exact ${nativeTestEntry} --test-threads=1`
    assert.ok(pattern.test(`"${executable}" ${args}`))
    assert.ok(pattern.test(`${executable} ${args}`))
    for (const value of [
        `"${executable}" --profile=${profile}`,
        `"${executable}" --ignored ${nativeTestEntry} --test-threads=1`,
        `"${executable}" --ignored --exact other_test --test-threads=1`,
        `"${executable}" ${args} --include-ignored`,
        `"${executable}" ${args} synthetic.risum`,
        `"${executable.replace('test.exe', 'other.exe')}" ${args}`,
    ]) assert.equal(pattern.test(value), false, value)
})

// Only OS queries/actions are substituted; the actual ownership and cleanup script runs.
const processBoundary = `
function Get-CimInstance {
    param($Filter)
    if ($Filter -cne ('ProcessId = ' + $env:RISUNEST_UI_PID)) { throw 'wrong PID query' }
    if ($env:TEST_MISSING -eq 'yes') { return $null }
    [pscustomobject]@{ ExecutablePath=$env:TEST_EXE; CommandLine=$env:TEST_COMMAND }
}
function Get-Process {
    param($Id)
    if ($Id -ne [int]$env:RISUNEST_UI_PID) { throw 'wrong PID lookup' }
    $result = [pscustomobject]@{ StartTime=[datetime]::Parse($env:TEST_CREATED) }
    $result | Add-Member ScriptMethod CloseMainWindow { [Console]::WriteLine('action:close'); return $true }
    $result | Add-Member ScriptMethod Kill { [Console]::WriteLine('action:crash') }
    return $result
}
`

test('owned process cleanup refuses wrong executable, argv, PID lifetime and missing process', { skip: process.platform !== 'win32' }, () => {
    const args = launchSpecification('test-executable', profile).args.join(' ')
    const created = '2026-01-01T00:00:00.0000000Z'
    const base = {
        ...process.env, RISUNEST_UI_PID: '12345', RISUNEST_UI_EXE: executable,
        RISUNEST_UI_PROFILE: profile, RISUNEST_UI_MODE: 'test-executable',
        RISUNEST_UI_COMMAND_PATTERN: ownedCommandLinePattern(executable, 'test-executable', profile),
        RISUNEST_UI_CREATED: created, RISUNEST_UI_ACTION: 'identity',
        TEST_EXE: executable, TEST_COMMAND: `"${executable}" ${args}`, TEST_CREATED: created, TEST_MISSING: '',
    }
    const execute = changes => execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', processBoundary + ownedProcessScript], {
        env: { ...base, ...changes }, encoding: 'utf8', windowsHide: true, timeout: 10_000, stdio: ['ignore', 'pipe', 'pipe'],
    })
    assert.equal(JSON.parse(execute({})).created, created)
    for (const changes of [
        { TEST_EXE: 'C:\\Synthetic UI\\another.exe' },
        { TEST_COMMAND: `"${executable}" --profile=${profile}` },
        { TEST_COMMAND: `"${executable}" ${args} --include-ignored` },
        { TEST_CREATED: '2026-01-02T00:00:00.0000000Z' },
        { TEST_MISSING: 'yes' },
    ]) {
        assert.throws(() => execute({ ...changes, RISUNEST_UI_ACTION: 'close' }), error => {
            assert.doesNotMatch(String(error.stdout), /action:/)
            return true
        })
    }
    assert.match(execute({ RISUNEST_UI_ACTION: 'crash' }), /action:crash/)
    assert.match(execute({ RISUNEST_UI_ACTION: 'close' }), /action:close/)
    const normal = { RISUNEST_UI_MODE: 'binary', TEST_COMMAND: `"${executable}" --profile=${profile}` }
    assert.equal(JSON.parse(execute(normal)).created, created)
    assert.throws(() => execute({ ...normal, TEST_COMMAND: `"${executable}" --profile=personal` }))
})
