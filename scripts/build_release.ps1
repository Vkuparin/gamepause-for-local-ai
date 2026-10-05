param([Parameter(Mandatory=$true)][string]$Iscc)
$ErrorActionPreference = 'Stop'
$taskRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $taskRoot
try {
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
    $taskMetadata = cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $taskVersion = ($taskMetadata.packages | Where-Object name -eq 'gamepause-lmstudio').version
    $taskBundle = Join-Path $taskRoot 'dist\GamePause'
    # Rebuild staging from scratch so removed docs/licenses cannot leak into a
    # later candidate. Refuse redirected directories before recursive removal.
    $taskDist = Join-Path $taskRoot 'dist'
    if (Test-Path -LiteralPath $taskDist) {
        if ((Get-Item -LiteralPath $taskDist -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing redirected dist directory' }
    }
    if (Test-Path -LiteralPath $taskBundle) {
        $taskResolvedBundle = (Resolve-Path -LiteralPath $taskBundle).Path
        if ($taskResolvedBundle -ne (Join-Path $taskRoot 'dist\GamePause')) { throw 'Unexpected staging path' }
        if ((Get-Item -LiteralPath $taskBundle -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing redirected staging directory' }
        Remove-Item -LiteralPath $taskResolvedBundle -Recurse -Force
    }
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
    if (Test-Path -LiteralPath 'docs\images') { Copy-Item -LiteralPath 'docs\images' -Destination (Join-Path $taskBundle 'docs') -Recurse -Force }
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
        $taskLicenseFamily = switch -Regex ($taskPackage.name) {
            '^accesskit($|_)' { 'accesskit'; break }
            '^(ecolor|eframe|egui($|[-_])|emath|epaint($|_))' { 'egui'; break }
            '^enum-map($|-)' { 'enum-map'; break }
            default { $taskPackage.name }
        }
        $taskSupplement = Join-Path $taskRoot ('assets\third-party-licenses\' + $taskLicenseFamily)
        if (Test-Path -LiteralPath $taskSupplement) { Copy-Item -Path (Join-Path $taskSupplement '*') -Destination $taskLicenseTarget -Force }
        if ($taskPackage.name -eq 'epaint_default_fonts') {
            Copy-Item -Path (Join-Path $taskPackageRoot 'fonts\*.txt') -Destination $taskLicenseTarget -Force
        }
    }
    $taskNotices | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $taskBundle 'THIRD_PARTY_NOTICES.json') -Encoding utf8
    $taskArchive = Join-Path $taskRoot "dist\GamePause-$taskVersion-windows-x64.zip"
    Compress-Archive -LiteralPath $taskBundle -DestinationPath $taskArchive -Force
    & $Iscc "/DAppVersion=$taskVersion" (Join-Path $taskRoot 'installer\GamePause.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
    $taskInstaller = Join-Path $taskRoot "dist\GamePause-$taskVersion-Setup.exe"
    $taskChecksums = foreach ($taskArtifact in @($taskArchive,$taskInstaller)) { $taskHash = (Get-FileHash -LiteralPath $taskArtifact -Algorithm SHA256).Hash.ToLowerInvariant(); "$taskHash  $(Split-Path -Leaf $taskArtifact)" }
    $taskChecksums | Set-Content -LiteralPath 'dist\SHA256SUMS.txt' -Encoding ascii
    $taskSourceFiles = @('Cargo.toml','Cargo.lock','build.rs','rust-toolchain.toml') | ForEach-Object { Get-Item -LiteralPath $_ }
    $taskSourceFiles += Get-ChildItem -LiteralPath 'src','assets','.cargo' -File -Recurse
    $taskSourceHashes = foreach ($taskSourceFile in ($taskSourceFiles | Sort-Object FullName)) {
        $taskRelativePath = $taskSourceFile.FullName.Substring($taskRoot.Length + 1).Replace('\','/')
        $taskHash = (Get-FileHash -LiteralPath $taskSourceFile.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$taskHash  $taskRelativePath"
    }
    $taskHasher = [Security.Cryptography.SHA256]::Create()
    try { $taskFingerprint = -join ($taskHasher.ComputeHash([Text.Encoding]::UTF8.GetBytes(($taskSourceHashes -join "`n"))) | ForEach-Object { $_.ToString('x2') }) }
    finally { $taskHasher.Dispose() }
    $taskArtifacts = foreach ($taskArtifact in @($taskArchive,$taskInstaller)) {
        [ordered]@{name=(Split-Path -Leaf $taskArtifact); bytes=(Get-Item -LiteralPath $taskArtifact).Length; sha256=(Get-FileHash -LiteralPath $taskArtifact -Algorithm SHA256).Hash.ToLowerInvariant()}
    }
    [ordered]@{
        version=$taskVersion
        built_at_utc=[DateTime]::UtcNow.ToString('o')
        rustc=(& rustc --version)
        source_fingerprint_sha256=$taskFingerprint
        source_files=$taskSourceHashes
        artifacts=@($taskArtifacts)
        signing='unsigned'
        human_acceptance='pending'
        publication='requires human review, feedback and a separate publishing instruction'
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath 'dist\BUILD-INFO.json' -Encoding utf8
    $taskChecksums
} finally { Pop-Location }
