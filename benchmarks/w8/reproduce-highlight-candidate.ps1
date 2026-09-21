$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$patchPath = Join-Path $repositoryRoot 'benchmarks\w8\highlight-candidate.patch'
$expectedHash = 'e610cac9870d2a43cabd664b293ce2bd74fba171af4b1c5c241e1c8d69710a59'
$actualHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) {
    throw "Unexpected Highlight candidate patch hash: $actualHash"
}
$candidateCommit = (git -C $repositoryRoot log -1 --format=%H -- benchmarks/w8/highlight-candidate.patch).Trim()
if ($LASTEXITCODE -ne 0 -or -not $candidateCommit) {
    throw 'Could not resolve the candidate commit.'
}

$gitCommonDirectory = [IO.Path]::GetFullPath((git -C $repositoryRoot rev-parse --path-format=absolute --git-common-dir).Trim())
$worktreesRoot = Join-Path (Split-Path -Parent $gitCommonDirectory) '.worktrees'
if (-not (Test-Path -LiteralPath $worktreesRoot)) {
    New-Item -ItemType Directory -Path $worktreesRoot | Out-Null
}
$worktreesRoot = (Resolve-Path -LiteralPath $worktreesRoot).Path
$candidateRoot = [IO.Path]::GetFullPath((Join-Path $worktreesRoot 'r14-w8-highlight-candidate-repro'))
$candidateParent = [IO.Path]::GetFullPath((Split-Path -Parent $candidateRoot))
if (-not [string]::Equals($candidateParent, $worktreesRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Candidate path is not an immediate child of the worktree directory: $candidateRoot"
}
if (Test-Path -LiteralPath $candidateRoot) {
    throw "Candidate worktree already exists: $candidateRoot"
}

git -C $repositoryRoot worktree add --detach $candidateRoot $candidateCommit
if ($LASTEXITCODE -ne 0) {
    throw "Failed to create candidate worktree: $candidateRoot"
}

try {
    $candidateRoot = (Resolve-Path -LiteralPath $candidateRoot).Path
    $candidatePrefix = $candidateRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $candidatePatch = Join-Path $candidateRoot 'benchmarks\w8\highlight-candidate.patch'

    git -C $candidateRoot apply --check $candidatePatch
    if ($LASTEXITCODE -ne 0) { throw 'Highlight candidate patch check failed.' }
    git -C $candidateRoot apply $candidatePatch
    if ($LASTEXITCODE -ne 0) { throw 'Highlight candidate patch apply failed.' }

    Push-Location $candidateRoot
    try {
        pnpm install --frozen-lockfile
        pnpm exec vite build --manifest .vite/manifest.json --outDir dist --sourcemap
        node benchmarks/w8/bundle-gates.mjs --dist dist
        pnpm exec vite build --config benchmarks/w8/vite.highlight.config.ts --outDir dist-w8-highlight --emptyOutDir
        node benchmarks/w8/highlight-first-use.mjs --dist dist-w8-highlight --samples 20
    } finally {
        Pop-Location
    }
} finally {
    if (Test-Path -LiteralPath $candidateRoot) {
        $candidateRoot = (Resolve-Path -LiteralPath $candidateRoot).Path
        $candidatePrefix = $candidateRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        foreach ($relativePath in @('node_modules', 'dist', 'dist-w8-highlight')) {
            $generatedPath = Join-Path $candidateRoot $relativePath
            if (-not (Test-Path -LiteralPath $generatedPath)) { continue }
            $resolvedGeneratedPath = (Resolve-Path -LiteralPath $generatedPath).Path
            if (-not $resolvedGeneratedPath.StartsWith($candidatePrefix, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Refusing to remove generated path outside the candidate worktree: $resolvedGeneratedPath"
            }
            Remove-Item -LiteralPath $resolvedGeneratedPath -Recurse -Force
        }

        git -C $repositoryRoot worktree remove --force $candidateRoot
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to remove candidate worktree: $candidateRoot"
        }
    }
}
