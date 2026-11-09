import assert from 'node:assert/strict'

export const nativeTestEntry = 'ollama_ui_verification::run_app'
export const nativeProfileEnvironment = 'RISUNEST_OLLAMA_UI_PROFILE'

export function launchSpecification(mode, profile, environment = {}) {
    assert.ok(['binary', 'test-executable'].includes(mode), 'Known native UI launch mode')
    assert.ok(profile.length <= 32 && /^agent-ollama-[0-9a-f]{12}-[0-9]+$/.test(profile), 'Owned synthetic profile')
    const env = { ...environment }
    delete env[nativeProfileEnvironment]
    if (mode === 'test-executable') {
        env[nativeProfileEnvironment] = profile
        return { args: ['--ignored', '--exact', nativeTestEntry, '--test-threads=1'], env }
    }
    return { args: [`--profile=${profile}`], env }
}

export function ownedCommandLinePattern(executable, mode, profile) {
    const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    const file = escape(executable)
    const args = launchSpecification(mode, profile).args.map(escape).join(' ')
    return `^(?:"${file}"|${file}) ${args}$`
}

export const ownedProcessScript = [
    '$ErrorActionPreference = "Stop"',
    '$owned = Get-CimInstance Win32_Process -Filter ("ProcessId = " + $env:RISUNEST_UI_PID)',
    'if (-not $owned) { throw "owned process no longer exists" }',
    'if (-not [StringComparer]::OrdinalIgnoreCase.Equals($owned.ExecutablePath, $env:RISUNEST_UI_EXE)) { throw "process executable mismatch" }',
    'if ($env:RISUNEST_UI_MODE -eq "test-executable") {',
    '    if ($owned.CommandLine -cnotmatch $env:RISUNEST_UI_COMMAND_PATTERN) { throw "process launch arguments mismatch" }',
    '} elseif ($env:RISUNEST_UI_MODE -eq "binary") {',
    '    $profilePattern = "(?:^|\\s)" + [regex]::Escape("--profile=" + $env:RISUNEST_UI_PROFILE) + "(?:\\s|$)"',
    '    if ($owned.CommandLine -notmatch $profilePattern) { throw "process profile mismatch" }',
    '} else { throw "unknown owned process mode" }',
    '$process = Get-Process -Id ([int]$env:RISUNEST_UI_PID)',
    '$created = $process.StartTime.ToUniversalTime().ToString("o")',
    'if ($env:RISUNEST_UI_CREATED -and $created -cne $env:RISUNEST_UI_CREATED) { throw "process creation identity mismatch" }',
    'if ($env:RISUNEST_UI_ACTION -eq "close") { if (-not $process.CloseMainWindow()) { throw "owned window did not accept graceful close" } }',
    'elseif ($env:RISUNEST_UI_ACTION -eq "crash") { $process.Kill() }',
    'elseif ($env:RISUNEST_UI_ACTION -ne "identity") { throw "unknown owned process action" }',
    '@{created=$created} | ConvertTo-Json -Compress',
].join('\n')
