#Requires -Version 7.0
param(
    [string]$Target,
    [switch]$Release
)
$ErrorActionPreference = 'Stop'
$managerRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$outputRoot = Join-Path $managerRoot ('.test-output/gui-relaunch-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $outputRoot | Out-Null
$probe = Join-Path $outputRoot 'windows-gui-probe.exe'
$rustArgs = @('--edition=2021', (Join-Path $PSScriptRoot 'fixtures/windows-gui-probe.rs'), '-o', $probe)
$cargoArgs = @('test', '--manifest-path', (Join-Path $managerRoot 'Cargo.toml'), '--locked')
if ($Target) {
    $rustArgs += @('--target', $Target)
    $cargoArgs += @('--target', $Target)
}
if ($Release) {
    $rustArgs += '-O'
    $cargoArgs += '--release'
}
$test = 'gui_relaunch_survives_the_launchers_job_without_powershell'
$cargoArgs += @('--test', 'windows_platform', $test, '--', '--ignored', '--exact', '--nocapture')
$previousProbe = $env:RISUNEST_TEST_GUI_PROBE
try {
    & rustc @rustArgs
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic GUI probe compilation failed.' }
    $env:RISUNEST_TEST_GUI_PROBE = $probe
    & cargo @cargoArgs 2>&1 | Tee-Object -FilePath (Join-Path $outputRoot 'test.log') -OutVariable output | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'GUI relaunch lifetime test failed.' }
    if (($output | Out-String) -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored;') {
        throw 'GUI relaunch lifetime test did not execute exactly one passing test.'
    }
} finally {
    $env:RISUNEST_TEST_GUI_PROBE = $previousProbe
    Write-Output "GUI relaunch test artifacts: $outputRoot"
}
