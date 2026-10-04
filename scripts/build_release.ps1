param([Parameter(Mandatory=$true)][string]$Iscc)
$ErrorActionPreference = 'Stop'
$taskRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $taskRoot
try {
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
    $taskMetadata = cargo metadata --locked --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $taskVersion = ($taskMetadata.packages | Where-Object name -eq 'gamepause-lmstudio').version
    $taskBundle = Join-Path $taskRoot 'dist\GamePause'
    New-Item -ItemType Directory -Path $taskBundle -Force | Out-Null
    Copy-Item -LiteralPath 'target\release\GamePause.exe','target\release\GamePauseCLI.exe' -Destination $taskBundle -Force
    foreach ($taskFile in @('README.md','LICENSE','CHANGELOG.md','config.example.json')) { Copy-Item -LiteralPath $taskFile -Destination $taskBundle -Force }
    New-Item -ItemType Directory -Path (Join-Path $taskBundle 'docs') -Force | Out-Null
    $taskUserDocs = Get-ChildItem -LiteralPath 'docs' -File | Where-Object { $_.Name -notlike '*-review.md' -and $_.Name -notlike '*-plan.md' }
    if (Test-Path -LiteralPath (Join-Path $taskBundle 'docs')) {
        $taskInternalDocs = Get-ChildItem -LiteralPath (Join-Path $taskBundle 'docs') -File | Where-Object { $_.Name -like '*-review.md' -or $_.Name -like '*-plan.md' }
        foreach ($taskInternalDoc in $taskInternalDocs) { Remove-Item -LiteralPath $taskInternalDoc.FullName -Force }
    }
    foreach ($taskDoc in $taskUserDocs) { Copy-Item -LiteralPath $taskDoc.FullName -Destination (Join-Path $taskBundle 'docs') -Force }
    $taskNotices = @()
    foreach ($taskPackage in ($taskMetadata.packages | Sort-Object name,version)) {
        if ($taskPackage.name -eq 'gamepause-lmstudio') { continue }
        $taskNotices += [ordered]@{name=$taskPackage.name; version=$taskPackage.version; license=$taskPackage.license; repository=$taskPackage.repository}
        $taskPackageRoot = Split-Path -Parent $taskPackage.manifest_path
        $taskLicenseFiles = Get-ChildItem -LiteralPath $taskPackageRoot -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)' }
        $taskLicenseTarget = Join-Path $taskBundle ('third-party-licenses\' + $taskPackage.name + '-' + $taskPackage.version)
        New-Item -ItemType Directory -Path $taskLicenseTarget -Force | Out-Null
        foreach ($taskLicense in $taskLicenseFiles) { Copy-Item -LiteralPath $taskLicense.FullName -Destination $taskLicenseTarget -Force }
        if ($taskPackage.license_file) { $taskExplicitLicense = Join-Path $taskPackageRoot $taskPackage.license_file; if (Test-Path -LiteralPath $taskExplicitLicense) { Copy-Item -LiteralPath $taskExplicitLicense -Destination $taskLicenseTarget -Force } }
    }
    $taskNotices | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $taskBundle 'THIRD_PARTY_NOTICES.json') -Encoding utf8
    $taskArchive = Join-Path $taskRoot "dist\GamePause-$taskVersion-windows-x64.zip"
    Compress-Archive -LiteralPath $taskBundle -DestinationPath $taskArchive -Force
    & $Iscc "/DAppVersion=$taskVersion" (Join-Path $taskRoot 'installer\GamePause.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
    $taskInstaller = Join-Path $taskRoot "dist\GamePause-$taskVersion-Setup.exe"
    $taskChecksums = foreach ($taskArtifact in @($taskArchive,$taskInstaller)) { $taskHash = (Get-FileHash -LiteralPath $taskArtifact -Algorithm SHA256).Hash.ToLowerInvariant(); "$taskHash  $(Split-Path -Leaf $taskArtifact)" }
    $taskChecksums | Set-Content -LiteralPath 'dist\SHA256SUMS.txt' -Encoding ascii
    $taskChecksums
} finally { Pop-Location }
