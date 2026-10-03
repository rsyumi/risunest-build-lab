param(
    [string[]]$Rungs = @('S1'),
    [string[]]$Groups = @('server', 'backup', 'maintenance', 'serverless'),
    [string]$Output,
    [string]$Results,
    [string]$Executable,
    [string]$TempDirectory,
    [int]$Repetitions = 5,
    [int]$Warmups = 1,
    [int]$SampleMs = 10,
    [int]$PollMs = 250,
    [string[]]$CargoArgs = @(),
    [switch]$ConstructOnly,
    [switch]$KeepWork
)
$ErrorActionPreference = 'Stop'
# pwsh -File passes a comma list as one string.
$Rungs = @($Rungs | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$Groups = @($Groups | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
foreach ($group in $Groups) {
    if ($group -notin 'server', 'backup', 'maintenance', 'serverless') { throw "Unknown group '$group'." }
}
$repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$commonGitDirectory = (& git -C $repository rev-parse --path-format=absolute --git-common-dir).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared target directory.' }
$mainCheckout = Split-Path $commonGitDirectory -Parent
if (!$Output) { $Output = Join-Path $repository '.tmp/lww-ladder' }
$Output = [IO.Path]::GetFullPath($Output)
if (!$Results) { $Results = Join-Path $Output 'results.jsonl' }
$Results = [IO.Path]::GetFullPath($Results)
$logs = Join-Path $Output 'logs'
New-Item -ItemType Directory -Force -Path $Output, $logs, (Split-Path $Results -Parent) | Out-Null

# name = assets, assets per owner, approximate active record bytes
$named = @{
    'S1' = @(1000, 1000, 5000000); 'S2' = @(2000, 1000, 5000000)
    'A1k' = @(1000, 10000, 20000000); 'A10k' = @(10000, 10000, 20000000)
    'A30k' = @(30000, 10000, 20000000); 'A100k' = @(100000, 10000, 20000000)
    'R10' = @(5000, 1000, 10000000); 'R50' = @(5000, 1000, 50000000)
    'R200' = @(5000, 1000, 200000000); 'R1G' = @(5000, 1000, 1000000000)
}
function Resolve-Rung([string]$spec) {
    if ($named.ContainsKey($spec)) {
        $values = $named[$spec]
        return [pscustomobject]@{ Name = $spec; Assets = $values[0]; PerOwner = $values[1]; RecordBytes = $values[2] }
    }
    $parts = $spec -split ':'
    if ($parts.Count -ne 4 -or $parts[0] -notmatch '^[A-Za-z0-9_-]+$') {
        throw "Rung '$spec' is neither a named rung nor name:assets:assetsPerOwner:recordBytes."
    }
    return [pscustomobject]@{ Name = $parts[0]; Assets = [int64]$parts[1]; PerOwner = [int64]$parts[2]; RecordBytes = [int64]$parts[3] }
}

Add-Type -Namespace LwwLadder -Name Native -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct Counters {
    public uint cb; public uint PageFaultCount; public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
    public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage; public UIntPtr QuotaPeakNonPagedPoolUsage;
    public UIntPtr QuotaNonPagedPoolUsage; public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage; public UIntPtr PrivateUsage;
}
[DllImport("kernel32.dll")]
private static extern bool K32GetProcessMemoryInfo(IntPtr process, out Counters counters, uint size);
public static ulong[] Peaks(IntPtr process) {
    Counters counters;
    if (!K32GetProcessMemoryInfo(process, out counters, (uint)Marshal.SizeOf(typeof(Counters)))) { return null; }
    return new ulong[] { counters.PeakWorkingSetSize.ToUInt64(), counters.PeakPagefileUsage.ToUInt64() };
}
'@

function Invoke-Cargo([string[]]$arguments) {
    $saved = @{}
    foreach ($key in 'RUSTUP_TOOLCHAIN', 'RUSTC', 'CARGO_TARGET_DIR') { $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process') }
    try {
        $env:RUSTUP_TOOLCHAIN = '1.97.1'
        $env:RUSTC = $null
        $env:CARGO_TARGET_DIR = Join-Path $mainCheckout 'src-tauri/target'
        Push-Location (Join-Path $repository 'src-tauri')
        try { & cargo @arguments } finally { Pop-Location }
    }
    finally {
        foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
    }
}

if (!$Executable) {
    $messages = Invoke-Cargo (@('test', '--release', '--lib', '--no-run', '--message-format=json') + $CargoArgs)
    if ($LASTEXITCODE -ne 0) { throw "Release test build failed with code $LASTEXITCODE." }
    $Executable = $messages | Where-Object { $_.StartsWith('{') } | ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.executable -and $_.profile.test -and $_.target.name -eq 'risunest_lib' } |
        Select-Object -Last 1 -ExpandProperty executable
    if (!$Executable) { throw 'The release build reported no library test executable.' }
}
$Executable = (Resolve-Path $Executable).Path
Write-Host "Executable: $Executable"

function Add-ResultLine($line) {
    $text = ($line | ConvertTo-Json -Compress -Depth 8) + "`n"
    [IO.File]::AppendAllText($Results, $text, [Text.UTF8Encoding]::new($false))
}

function Invoke-Child([string]$test, [hashtable]$environment, [string]$rung, [string]$group) {
    $stamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $log = Join-Path $logs "$rung-$group-$stamp.log"
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    foreach ($argument in @("persistent_store::benchmark_lww::ladder::$test", '--ignored', '--exact', '--nocapture', '--test-threads=1')) {
        $start.ArgumentList.Add($argument)
    }
    $start.WorkingDirectory = Join-Path $repository 'src-tauri'
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($key in $environment.Keys) { $start.Environment[$key] = [string]$environment[$key] }
    if ($TempDirectory) { $start.Environment['RISUNEST_LADDER_TEMP_DIR'] = $TempDirectory }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    $standardOutput = $process.StandardOutput.ReadToEndAsync()
    $standardError = $process.StandardError.ReadToEndAsync()
    $polled = @(0, 0)
    while (!$process.WaitForExit($PollMs)) {
        try {
            $process.Refresh()
            $polled = @([math]::Max($polled[0], $process.PeakWorkingSet64), [math]::Max($polled[1], $process.PeakPagedMemorySize64))
        }
        catch { }
    }
    $process.WaitForExit()
    $wall = $clock.Elapsed.TotalMilliseconds
    $final = [LwwLadder.Native]::Peaks($process.Handle)
    [IO.File]::WriteAllText($log, $standardOutput.Result + $standardError.Result)
    $line = [ordered]@{
        schema = 'risunest.lww-ladder-process/v1'; rung = $rung; group = $group; test = $test; pid = $process.Id
        exitCode = $process.ExitCode; wallMs = $wall
        peakWorkingSet = if ($final) { $final[0] } else { $polled[0] }
        peakPrivateBytes = if ($final) { $final[1] } else { $polled[1] }
        polledPeakWorkingSet = $polled[0]; polledPeakPrivateBytes = $polled[1]; pollMs = $PollMs
        log = $log; executable = $Executable; recordedAtMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    }
    $process.Dispose()
    Add-ResultLine $line
    Write-Host ("{0} {1}: exit {2}, {3:N0} ms, peak working set {4:N0} MB" -f $rung, $group, $line.exitCode, $wall, ($line.peakWorkingSet / 1MB))
    return $line
}

$children = @()
foreach ($spec in $Rungs) {
    $rung = Resolve-Rung $spec
    $directory = Join-Path $Output "rungs/$($rung.Name)"
    $manifest = Join-Path $directory 'rung.json'
    if (Test-Path $manifest) {
        $existing = Get-Content -Raw $manifest | ConvertFrom-Json
        if ($existing.params.assets -ne $rung.Assets -or $existing.params.assetsPerOwner -ne $rung.PerOwner -or $existing.params.recordBytesTarget -ne $rung.RecordBytes) {
            throw "Rung $($rung.Name) at $directory was built with different parameters."
        }
    }
    else {
        if ((Test-Path $directory) -and (Get-ChildItem -Force $directory | Select-Object -First 1)) {
            throw "Rung directory $directory exists without rung.json; remove it before construction."
        }
        $construction = Invoke-Child 'ladder_construct_rung' @{
            RISUNEST_LADDER_RUNG_DIR = $directory; RISUNEST_LADDER_NAME = $rung.Name; RISUNEST_LADDER_ASSETS = $rung.Assets
            RISUNEST_LADDER_ASSETS_PER_OWNER = $rung.PerOwner; RISUNEST_LADDER_RECORD_BYTES = $rung.RecordBytes
        } $rung.Name 'construct'
        $children += $construction
        if ($construction.exitCode -ne 0) { Write-Warning "Construction of $($rung.Name) failed; see $($construction.log)."; continue }
    }
    if ($ConstructOnly) { continue }
    foreach ($group in $Groups) {
        $environment = @{
            RISUNEST_LADDER_RUNG_DIR = $directory; RISUNEST_LADDER_GROUP = $group; RISUNEST_LADDER_OUTPUT_DIR = $Output
            RISUNEST_LADDER_RESULTS = $Results; RISUNEST_LADDER_REPETITIONS = $Repetitions; RISUNEST_LADDER_WARMUPS = $Warmups
            RISUNEST_LADDER_SAMPLE_MS = $SampleMs
        }
        if ($KeepWork) { $environment['RISUNEST_LADDER_KEEP_WORK'] = '1' }
        $child = Invoke-Child 'ladder_run_group' $environment $rung.Name $group
        $children += $child
        if (!$KeepWork) {
            Get-ChildItem -Force -Directory (Join-Path $Output 'work') -Filter "$($rung.Name)-$group-*" -ErrorAction SilentlyContinue |
                Remove-Item -Recurse -Force
        }
    }
}

$pids = $children | ForEach-Object { $_.pid }
$rows = Get-Content $Results | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.schema -eq 'risunest.lww-ladder/v1' -and $pids -contains $_.pid }
$rows | ForEach-Object {
    [pscustomobject]@{
        Rung = $_.rung.name; Group = $_.group; Phase = $_.phase; Status = $_.status
        ElapsedMs = if ($null -ne $_.elapsedMs) { [math]::Round([double]$_.elapsedMs, 1) } else { $null }
        PeakWorkingSetMB = if ($_.memory.peakWorkingSet) { [math]::Round($_.memory.peakWorkingSet / 1MB, 1) } else { $null }
    }
} | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
