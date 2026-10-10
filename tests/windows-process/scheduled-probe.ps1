param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null
$native = Join-Path $outputRoot 'native-probe.exe'
& rustc --edition 2021 (Join-Path $PSScriptRoot 'native-probe.rs') -o $native
if ($LASTEXITCODE -ne 0) { throw 'native probe compilation failed' }
$service = New-Object -ComObject 'Schedule.Service'
$service.Connect()
$folder = $service.GetFolder('\')
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$childSource = @'
param([string]$OutputPath)
$ErrorActionPreference='Stop'
[IO.File]::AppendAllText($OutputPath,"entered`n")
$service=New-Object -ComObject 'Schedule.Service'
$service.Connect()
[IO.File]::AppendAllText($OutputPath,"com-ready`n")
[Console]::Out.Write('ready')
[IO.File]::AppendAllText($OutputPath,"complete`n")
'@
$reports = @()
foreach ($mode in @('direct-no-window', 'scheduled-powershell', 'scheduled-no-window', 'scheduled-detached')) {
    $probeId = [guid]::NewGuid().ToString('N')
    $root = Join-Path $outputRoot $probeId
    New-Item -ItemType Directory -Path $root | Out-Null
    [IO.File]::WriteAllText((Join-Path $root 'child.ps1'), $childSource)
    $taskName = "risunest-synthetic-process-$probeId"
    $started = [Diagnostics.Stopwatch]::StartNew()
    $task = $null
    $direct = $null
    try {
        if ($mode -eq 'direct-no-window') {
            $direct = Start-Process -FilePath $native -ArgumentList @(('"' + $root + '"'), '134217728') -WindowStyle Hidden -PassThru
        } else {
            $definition = $service.NewTask(0)
            $definition.RegistrationInfo.Description = 'RisuNest synthetic process probe'
            $definition.Principal.UserId = $sid
            $definition.Principal.LogonType = 3
            $definition.Principal.RunLevel = 0
            $definition.Settings.Enabled = $true
            $definition.Settings.ExecutionTimeLimit = 'PT2M'
            $action = $definition.Actions.Create(0)
            $action.WorkingDirectory = $root
            if ($mode -eq 'scheduled-powershell') {
                $action.Path = 'powershell.exe'
                $action.Arguments = '-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "' + (Join-Path $root 'child.ps1') + '" -OutputPath "' + (Join-Path $root 'child.log') + '"'
            } else {
                $action.Path = $native
                $flags = if ($mode -eq 'scheduled-detached') { 8 } else { 134217728 }
                $action.Arguments = '"' + $root + '" ' + $flags
            }
            $task = $folder.RegisterTaskDefinition($taskName, $definition, 6, $sid, $null, 3, $null)
            $null = $task.Run($null)
        }
        $complete = $false
        while ($started.Elapsed.TotalSeconds -lt 90) {
            $nativeResult = Join-Path $root 'native-result'
            $childLog = Join-Path $root 'child.log'
            $childCompleted = [IO.File]::Exists($childLog) -and [IO.File]::ReadAllText($childLog).Contains('complete')
            if (($mode -eq 'scheduled-powershell' -and $childCompleted) -or [IO.File]::Exists($nativeResult)) {
                $complete = $childCompleted
                break
            }
            Start-Sleep -Milliseconds 200
        }
        $processes = @(Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne $PID -and $_.CommandLine -like "*$probeId*" } | ForEach-Object {
            @{ name=$_.Name; pid=$_.ProcessId; parent=$_.ParentProcessId; command=$_.CommandLine }
        })
        $files = @{}
        foreach ($file in @('native-entered', 'child.log', 'native-result')) {
            $path = Join-Path $root $file
            if ([IO.File]::Exists($path)) { $files[$file] = [IO.File]::ReadAllText($path) }
        }
        $report = @{ mode=$mode; complete=$complete; elapsed_ms=$started.ElapsedMilliseconds; files=$files; processes=$processes; task_state=if($task){[int]$task.State}else{$null}; task_result=if($task){[long]$task.LastTaskResult}else{$null} }
        $reports += $report
        $report | ConvertTo-Json -Depth 8 -Compress | Write-Output
        $reports | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $outputRoot 'results.json')
    } finally {
        if ($task) { try { $task.Stop(0) } catch {}; $folder.DeleteTask($taskName, 0) }
        Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne $PID -and $_.CommandLine -like "*$probeId*" } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }
}
if (@($reports | Where-Object { $_.mode -ne 'scheduled-detached' -and -not $_.complete }).Count -gt 0) { throw 'one or more scheduled process probes did not complete' }
