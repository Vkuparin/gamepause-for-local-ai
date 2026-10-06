param(
    [Parameter(Mandatory=$true)][string]$Candidate,
    [Parameter(Mandatory=$true)][string]$Baseline,
    [int]$Repeats = 3,
    [double]$Seconds = 12
)
$ErrorActionPreference = 'Stop'
if ($Repeats -lt 2 -or $Seconds -lt 10) { throw 'Use repeated samples of at least ten seconds' }
$taskRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$taskOutput = Join-Path $taskRoot ('build\observation-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $taskOutput | Out-Null
$taskCores = [Environment]::ProcessorCount
$taskResults = @()
foreach ($taskRepeat in 1..$Repeats) {
    foreach ($taskVariant in @('baseline-one','candidate-one','candidate-none','candidate-two')) {
        $taskFolder = Join-Path $taskOutput "$taskVariant-$taskRepeat"
        New-Item -ItemType Directory -Path $taskFolder | Out-Null
        $taskCommon = [ordered]@{mode='observe';poll_seconds=2;discovery_seconds=30;restore_delay_seconds=30;retry_seconds=30;automation_enabled=$false;ignored_games=@();steam_roots=@();epic_manifest_dirs=@();game_roots=@();extra_games=@();excluded_executables=@();excluded_paths=@()}
        if ($taskVariant.StartsWith('baseline')) {
            $taskExe = (Resolve-Path -LiteralPath $Baseline).Path
            $taskCommon.settings_version = 2
            $taskCommon.lms_path = Join-Path $taskFolder 'absent-fixture-lms.exe'
            $taskCommon.api_host = '127.0.0.1:61991'
            $taskCommon.stop_server_during_gaming = $true
        } else {
            $taskExe = (Resolve-Path -LiteralPath $Candidate).Path
            $taskCommon.settings_version = 4
            $taskCommon.providers = @(
                @{kind='lmstudio';id='lmstudio-main';enabled=($taskVariant -ne 'candidate-none');connection=@{endpoint='127.0.0.1:61991';lms_path=(Join-Path $taskFolder 'absent-fixture-lms.exe');stop_server_during_gaming=$true}},
                @{kind='ollama';id='ollama-main';enabled=($taskVariant -eq 'candidate-two');endpoint='127.0.0.1:61992'}
            )
            $taskCommon.notifications_enabled = $false
            $taskCommon.sound_enabled = $false
            $taskCommon.advanced_settings_visible = $false
        }
        $taskCommon | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $taskFolder 'config.json') -Encoding utf8
        $taskClock = [Diagnostics.Stopwatch]::StartNew()
        $taskProcess = Start-Process -FilePath $taskExe -ArgumentList @('--observe','--headless','--duration', $Seconds.ToString([Globalization.CultureInfo]::InvariantCulture),'--data-dir',('"' + $taskFolder + '"')) -WindowStyle Hidden -RedirectStandardOutput (Join-Path $taskFolder 'stdout.txt') -RedirectStandardError (Join-Path $taskFolder 'stderr.txt') -PassThru
        $taskPeakWorking = 0L
        $taskPeakPrivate = 0L
        $taskCpu = 0.0
        $taskSamples = 0
        while (-not $taskProcess.HasExited) {
            $taskProcess.Refresh()
            if (-not $taskProcess.HasExited) {
                $taskPeakWorking = [Math]::Max($taskPeakWorking, $taskProcess.WorkingSet64)
                $taskPeakPrivate = [Math]::Max($taskPeakPrivate, $taskProcess.PrivateMemorySize64)
                $taskCpu = $taskProcess.TotalProcessorTime.TotalSeconds
                $taskSamples++
            }
            Start-Sleep -Milliseconds 250
        }
        $taskProcess.WaitForExit()
        $taskClock.Stop()
        if ($taskProcess.ExitCode -ne 0) { throw "Observation fixture failed: $taskVariant, exit $($taskProcess.ExitCode)" }
        if (Test-Path -LiteralPath (Join-Path $taskFolder 'state.json')) { throw 'Observation must not create recovery' }
        $taskStatus = Get-Content -LiteralPath (Join-Path $taskFolder 'status.json') -Raw | ConvertFrom-Json
        if ($taskStatus.mode -ne 'observe' -or $taskStatus.recovery_pending) { throw 'Unexpected observation state' }
        $taskResult = [ordered]@{variant=$taskVariant;repeat=$taskRepeat;wall_seconds=$taskClock.Elapsed.TotalSeconds;parent_cpu_seconds=$taskCpu;total_cpu_percent=(100 * $taskCpu / $taskClock.Elapsed.TotalSeconds / $taskCores);peak_working_mib=($taskPeakWorking / 1MB);peak_private_mib=($taskPeakPrivate / 1MB);samples=$taskSamples}
        $taskResults += $taskResult
        "$taskVariant repeat $taskRepeat : CPU $([Math]::Round($taskResult.total_cpu_percent,4))%, working $([Math]::Round($taskResult.peak_working_mib,2)) MiB"
        $taskProcess.Dispose()
    }
}
[ordered]@{mode='private headless observation; no model/server requests or games';logical_processors=$taskCores;sample_seconds=$Seconds;candidate_sha256=(Get-FileHash -LiteralPath $Candidate -Algorithm SHA256).Hash.ToLowerInvariant();baseline_sha256=(Get-FileHash -LiteralPath $Baseline -Algorithm SHA256).Hash.ToLowerInvariant();limitations='Startup-inclusive short parent-process samples; no child CPU, ETW wakeups, healthy provider control, gaming, restoration or dashboard-open acceptance';samples=$taskResults} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $taskOutput 'measurements.json') -Encoding utf8
"Measurements: $taskOutput\measurements.json"
