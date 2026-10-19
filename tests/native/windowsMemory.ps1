param(
    [Parameter(Mandatory=$true)][int]$RootProcessId,
    [Parameter(Mandatory=$true)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
while (Get-Process -Id $RootProcessId -ErrorAction SilentlyContinue) {
    $processes = Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId
    $owned = [Collections.Generic.HashSet[int]]::new()
    [void]$owned.Add($RootProcessId)
    do {
        $added = $false
        foreach ($process in $processes) {
            if ($owned.Contains([int]$process.ParentProcessId) -and $owned.Add([int]$process.ProcessId)) { $added = $true }
        }
    } while ($added)
    $observed = @(Get-Process -Id ([int[]]@($owned)) -ErrorAction SilentlyContinue)
    if ($observed.Count -gt 0) {
        $sample = @{
            timestampMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            privateBytes = ($observed | Measure-Object PrivateMemorySize64 -Sum).Sum
            workingSetBytes = ($observed | Measure-Object WorkingSet64 -Sum).Sum
            processCount = $observed.Count
        }
        $sample | ConvertTo-Json -Compress | Add-Content -LiteralPath $OutputPath -Encoding UTF8
    }
    Start-Sleep -Milliseconds 500
}
