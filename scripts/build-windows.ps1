# Builds the Windows installers (MSI + NSIS) and the portable folder.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
#
# Outputs
#   src-tauri\target\release\bundle\msi\Droidlog_0.1.3_x64_en-US.msi
#   src-tauri\target\release\bundle\nsis\Droidlog_0.1.3_x64-setup.exe
#   dist\Droidlog-0.1.3-portable\        (droidlog.exe + platform-tools\)
#
# Prerequisites: Rust (MSVC toolchain), Node 18+ with pnpm, VS Build Tools with
# "Desktop development with C++", WebView2 (built into Windows 11).

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    foreach ($candidate in @("$env:CARGO_HOME\bin", 'D:\DevTools\Rust\cargo\bin', "$env:USERPROFILE\.cargo\bin")) {
        if ($candidate -and (Test-Path (Join-Path $candidate 'cargo.exe'))) {
            $env:Path = "$candidate;$env:Path"
            break
        }
    }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo was not found; install Rust first' }
if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) { throw 'pnpm was not found; npm i -g pnpm' }

if (-not (Test-Path 'node_modules')) { pnpm install }
pnpm tauri build

$release = Join-Path $root 'src-tauri\target\release'
$portable = Join-Path $root 'dist\Droidlog-0.1.3-portable'
if (Test-Path $portable) { Remove-Item $portable -Recurse -Force }
New-Item -ItemType Directory -Path $portable -Force | Out-Null
Copy-Item (Join-Path $release 'droidlog.exe') $portable
# The bundled platform-tools is what lets the portable copy run on a machine with
# no Android SDK: locate.rs prefers a resource directory over PATH.
Copy-Item (Join-Path $root 'src-tauri\platform-tools') $portable -Recurse

Write-Host ''
Write-Host '=== artifacts ==='
$artifacts = @(
    Get-ChildItem (Join-Path $release 'bundle\msi\*.msi') -ErrorAction SilentlyContinue
    Get-ChildItem (Join-Path $release 'bundle\nsis\*.exe') -ErrorAction SilentlyContinue
    Get-ChildItem (Join-Path $release 'droidlog.exe') -ErrorAction SilentlyContinue
)
foreach ($file in $artifacts) {
    $hash = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.Substring(0, 16)
    '{0,-46} {1,8:N2} MB  {2}' -f $file.Name, ($file.Length / 1MB), $hash
}
$portableSize = (Get-ChildItem $portable -Recurse -File | Measure-Object Length -Sum).Sum
'{0,-46} {1,8:N2} MB  (portable folder)' -f 'dist\Droidlog-0.1.3-portable\', ($portableSize / 1MB)
