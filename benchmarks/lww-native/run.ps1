param(
    [ValidateSet('test', 'compile', 'plan', 'generate')][string]$Action = 'test',
    [string]$Scale = 'small',
    [string]$Output,
    [switch]$Bodies
)
$ErrorActionPreference = 'Stop'
$repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$commonGitDirectory = (& git -C $repository rev-parse --path-format=absolute --git-common-dir).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve shared target directory.' }
$mainCheckout = Split-Path $commonGitDirectory -Parent
$savedEnvironment = @{}
foreach ($key in 'RUSTUP_TOOLCHAIN', 'RUSTC', 'CARGO_TARGET_DIR') {
    $savedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    $env:RUSTUP_TOOLCHAIN = '1.97.1'
    $env:RUSTC = $null
    $env:CARGO_TARGET_DIR = Join-Path $mainCheckout 'src-tauri/target'
    $cargo = 'cargo'
    $manifest = Join-Path $PSScriptRoot 'Cargo.toml'
    switch ($Action) {
        # The self-tests are ignored so the app's test run, which compiles the same modules, skips them.
        'test' { & $cargo test --locked --manifest-path $manifest -- --include-ignored }
        'compile' { & $cargo test --locked --no-run --manifest-path $manifest }
        'plan' { & $cargo run --locked --manifest-path $manifest -- plan }
        'generate' {
            if (!$Output) { throw 'Generate requires a new output directory.' }
            $arguments = @('run', '--locked', '--manifest-path', $manifest, '--', 'generate', $Scale, $Output)
            if ($Bodies) { $arguments += '--bodies' }
            & $cargo @arguments
        }
    }
    if ($LASTEXITCODE -ne 0) { throw "Harness exited with code $LASTEXITCODE." }
}
finally {
    foreach ($key in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($key, $savedEnvironment[$key], 'Process')
    }
}
