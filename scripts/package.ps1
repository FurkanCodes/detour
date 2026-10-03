#requires -Version 5
# Builds the GUI-only release. The driver and presets are embedded in the app.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    $rustCargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (-not (Test-Path -LiteralPath $rustCargo)) { throw 'Install Rust before building Detour.' }
    $cargoPath = $rustCargo
} else { $cargoPath = $cargo.Source }
& $cargoPath build --release -p detour-app
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$dist = Join-Path $root 'dist\Detour-Desktop'
New-Item -ItemType Directory -Force $dist | Out-Null

Copy-Item 'target\release\detour-app.exe' (Join-Path $dist 'Detour.exe')
Copy-Item 'vendor\windivert\LICENSE' (Join-Path $dist 'WinDivert-LICENSE.txt')
Copy-Item 'assets\fonts\OFL.txt' (Join-Path $dist 'PlusJakartaSans-LICENSE.txt')
Copy-Item 'LICENSE' (Join-Path $dist 'LICENSE.txt')
Copy-Item 'README.md' $dist -ErrorAction SilentlyContinue

Write-Host "Packaged to $dist"
Get-ChildItem $dist | Select-Object Name, Length | Format-Table -AutoSize
