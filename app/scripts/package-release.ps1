# ShiDrive release packaging: build a portable zip (shidrive.exe + uninstall.exe [+ node22]).
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1              # exe only
#   powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -WithNode22 # include node22
param(
    [switch]$WithNode22,
    [string]$Node22Dir = "D:\play\project\ShiDrive\.tools\node22"
)

$ErrorActionPreference = "Stop"
$scriptDir = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Definition }
$root = Split-Path -Parent $scriptDir            # app/
$repo = Split-Path -Parent $root                 # repo root
$exe  = Join-Path $root "src-tauri\target\release\shidrive.exe"
$un   = Join-Path $root "src-tauri\target\release\uninstall.exe"
if (-not (Test-Path $exe)) { throw "shidrive.exe not found: $exe (run cargo build --release --features custom-protocol first)" }
if (-not (Test-Path $un))  { throw "uninstall.exe not found: $un" }

$stage = Join-Path $env:TEMP ("shidrive-pkg-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $exe (Join-Path $stage "shidrive.exe")
Copy-Item $un  (Join-Path $stage "uninstall.exe")
if ($WithNode22) {
    if (-not (Test-Path $Node22Dir)) { throw "node22 not found: $Node22Dir" }
    $tools = Join-Path $stage ".tools\node22"
    New-Item -ItemType Directory -Path $tools -Force | Out-Null
    Copy-Item (Join-Path $Node22Dir "*") $tools -Recurse -Force
}

$suffix = if ($WithNode22) { "-bundled" } else { "" }
$zip = Join-Path $repo ("ShiDrive-portable-win-x64" + $suffix + ".zip")
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Force
Remove-Item $stage -Recurse -Force
$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "OK: $zip ($sizeMb MB)"
