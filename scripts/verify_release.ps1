param([string]$Distribution = (Join-Path $PSScriptRoot '..\dist'))
$ErrorActionPreference = 'Stop'
$taskDist = (Resolve-Path -LiteralPath $Distribution).Path
$taskInfo = Get-Content -LiteralPath (Join-Path $taskDist 'BUILD-INFO.json') -Raw | ConvertFrom-Json
$taskExpectedNames = @("GamePause-$($taskInfo.version)-windows-x64.zip", "GamePause-$($taskInfo.version)-Setup.exe")
$taskManifestNames = @($taskInfo.artifacts | ForEach-Object name)
if (@(Compare-Object $taskExpectedNames $taskManifestNames).Count -ne 0) { throw 'Unexpected candidate artifact set' }
$taskSums = Get-Content -LiteralPath (Join-Path $taskDist 'SHA256SUMS.txt')
if (@($taskSums).Count -ne 2) { throw 'Expected exactly two checksum entries' }
foreach ($taskArtifact in $taskInfo.artifacts) {
    $taskPath = Join-Path $taskDist $taskArtifact.name
    $taskHash = (Get-FileHash -LiteralPath $taskPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($taskHash -ne $taskArtifact.sha256 -or (Get-Item -LiteralPath $taskPath).Length -ne $taskArtifact.bytes) { throw "Artifact differs from build record: $($taskArtifact.name)" }
    if ($taskSums -cnotcontains "$taskHash  $($taskArtifact.name)") { throw "Checksum file mismatch: $($taskArtifact.name)" }
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$taskArchive = [IO.Compression.ZipFile]::OpenRead((Join-Path $taskDist $taskExpectedNames[0]))
try {
    $taskEntries = @($taskArchive.Entries | ForEach-Object FullName)
    foreach ($taskRequired in @('GamePause.exe','GamePauseCLI.exe','README.md','LICENSE','CHANGELOG.md','config.example.json','THIRD_PARTY_NOTICES.json','docs/USAGE.md','docs/CONFIGURATION.md','docs/VALIDATION.md')) {
        if ($taskEntries -cnotcontains "GamePause/$taskRequired") { throw "Missing packaged file: $taskRequired" }
    }
    if ($taskEntries | Where-Object { $_ -match '(?i)(-review\.md$|-plan\.md$|AGENTS\.md$|playtest_notes\.md$|(^|/)(config|state|status|inventory)\.json$|\.log($|\.))' }) { throw 'Internal/runtime file in candidate archive' }
    $taskConfigEntry = $taskArchive.GetEntry('GamePause/config.example.json')
    $taskReader = [IO.StreamReader]::new($taskConfigEntry.Open())
    try { $taskConfig = $taskReader.ReadToEnd() | ConvertFrom-Json } finally { $taskReader.Dispose() }
    if ($taskConfig.settings_version -ne 4 -or $taskConfig.advanced_settings_visible) { throw 'Unsafe packaged default preferences' }
    $taskNoticeEntry = $taskArchive.GetEntry('GamePause/THIRD_PARTY_NOTICES.json')
    $taskReader = [IO.StreamReader]::new($taskNoticeEntry.Open())
    try { $taskNotices = @($taskReader.ReadToEnd() | ConvertFrom-Json) } finally { $taskReader.Dispose() }
    if ($taskNotices.Count -eq 0) { throw 'Empty dependency notices' }
    foreach ($taskNotice in $taskNotices) {
        if (-not $taskNotice.license) { throw "Missing license declaration: $($taskNotice.name)" }
        $taskLicensePrefix = "GamePause/third-party-licenses/$($taskNotice.name)-$($taskNotice.version)/"
        if (@($taskEntries | Where-Object { $_.StartsWith($taskLicensePrefix) -and -not $_.EndsWith('/') }).Count -eq 0) { throw "Missing dependency license text: $($taskNotice.name)" }
    }
} finally { $taskArchive.Dispose() }
foreach ($taskExe in @('GamePause.exe','GamePauseCLI.exe')) {
    $taskPath = Join-Path $taskDist "GamePause\$taskExe"
    $taskVersion = (Get-Item -LiteralPath $taskPath).VersionInfo.ProductVersion
    if ($taskVersion -ne $taskInfo.version -and $taskVersion -ne "$($taskInfo.version).0") { throw "Executable version mismatch: $taskExe ($taskVersion)" }
}
$taskVersionOutput = & (Join-Path $taskDist 'GamePause\GamePauseCLI.exe') --version
if ($LASTEXITCODE -ne 0 -or "$taskVersionOutput" -notmatch [regex]::Escape($taskInfo.version)) { throw 'CLI version mismatch' }
"GamePause $($taskInfo.version): checksums, metadata, default preferences, license texts and archive contents verified."
