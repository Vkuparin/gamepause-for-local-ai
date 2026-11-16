<#
Submits a published GamePause release to winget (microsoft/winget-pkgs).

wingetcreate copies the newest Vkuparin.GamePause manifest in winget-pkgs,
points it at the new installer, recomputes the hash and writes it in the
current manifest schema, so manifests never fall behind a deprecated version.

Without -Submit the manifests are written to scratch\winget\<version> and
checked with `winget validate`. With -Submit they are opened as a pull
request from your fork, using the GitHub CLI's existing login.

Only updates are supported: the package must already be in winget-pkgs.
#>
param(
    [string]$Version,
    [switch]$Submit
)
$ErrorActionPreference = 'Stop'
$taskRoot = Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')
$taskPackage = 'Vkuparin.GamePause'
$taskRepo = 'Vkuparin/gamepause-for-local-ai'

foreach ($taskTool in @('gh', 'wingetcreate', 'winget')) {
    if (-not (Get-Command $taskTool -ErrorAction SilentlyContinue)) {
        if ($taskTool -eq 'wingetcreate') { throw 'wingetcreate is not installed. Run: winget install --id Microsoft.WingetCreate -e' }
        throw "$taskTool is not on PATH"
    }
}

if (-not $Version) {
    $taskCargo = Get-Content -LiteralPath (Join-Path $taskRoot 'Cargo.toml') -Raw
    if ($taskCargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'No package version in Cargo.toml' }
    $Version = $Matches[1]
}
$taskTag = "v$Version"

$taskRelease = gh release view $taskTag --repo $taskRepo --json isDraft,isPrerelease,publishedAt,assets | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "No GitHub release $taskTag" }
if ($taskRelease.isDraft -or $taskRelease.isPrerelease) { throw "$taskTag is a draft or prerelease" }
$taskInstaller = "GamePause-$Version-Setup.exe"
if (-not ($taskRelease.assets | Where-Object name -eq $taskInstaller)) { throw "$taskTag has no $taskInstaller asset" }
$taskUrl = "https://github.com/$taskRepo/releases/download/$taskTag/$taskInstaller"
$taskDate = ([datetime]$taskRelease.publishedAt).ToUniversalTime().ToString('yyyy-MM-dd')

# The package must already exist; a first submission needs hand-written manifests.
$taskDir = "manifests/v/$($taskPackage.Replace('.', '/'))"
$taskListing = gh api "repos/microsoft/winget-pkgs/contents/$taskDir" 2>$null
$taskVersions = if ($LASTEXITCODE -eq 0) { @($taskListing | ConvertFrom-Json | Where-Object type -eq 'dir' | ForEach-Object name) } else { @() }
if (-not $taskVersions) {
    throw "$taskPackage is not in winget-pkgs yet. Wait for the first submission to merge before using this script."
}
if ($taskVersions -contains $Version) { Write-Host "$taskPackage $Version is already in winget-pkgs."; return }

$taskOpen = gh pr list --repo microsoft/winget-pkgs --state open --search "$taskPackage $Version in:title" --json number,url | ConvertFrom-Json
if ($taskOpen) { Write-Host "An open pull request already submits $Version`: $($taskOpen[0].url)"; return }

# wingetcreate reads winget-pkgs through the GitHub API; without a token it hits the anonymous rate limit.
$taskToken = gh auth token
if ($LASTEXITCODE -ne 0 -or -not $taskToken) { throw 'gh is not logged in. Run: gh auth login' }
$taskCommon = @('update', $taskPackage, '--version', $Version, '--urls', $taskUrl,
    '--release-date', $taskDate, '--release-notes-url', "https://github.com/$taskRepo/releases/tag/$taskTag",
    '--token', $taskToken)

if ($Submit) {
    wingetcreate @taskCommon --submit --prtitle "Update: $taskPackage to $Version"
    if ($LASTEXITCODE -ne 0) { throw "wingetcreate failed with exit code $LASTEXITCODE" }
    Write-Host 'Submitted. Watch the pull request for bot comments; winget closes stale requests after about two weeks.'
} else {
    $taskOut = Join-Path $taskRoot "scratch\winget\$Version"
    if (Test-Path -LiteralPath $taskOut) { Remove-Item -LiteralPath $taskOut -Recurse -Force }
    wingetcreate @taskCommon --out $taskOut
    if ($LASTEXITCODE -ne 0) { throw "wingetcreate failed with exit code $LASTEXITCODE" }
    $taskManifest = Get-ChildItem -LiteralPath $taskOut -Recurse -Filter "$taskPackage.yaml" | Select-Object -First 1
    winget validate --manifest $taskManifest.DirectoryName
    if ($LASTEXITCODE -ne 0) { throw 'winget validate failed' }
    Write-Host "Manifests written to $($taskManifest.DirectoryName). Run again with -Submit to open the pull request."
}
