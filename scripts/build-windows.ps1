# Gera dist\Jarvis_Core-<versão>-windows-x86_64.zip (jarvis.exe + jarvis-gui.exe).
# Uso: pwsh -File scripts/build-windows.ps1
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
cargo build --release -p jarvis-cli -p jarvis-gui
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Stage = Join-Path $Root "target\Jarvis_Core-$Version-windows"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage, (Join-Path $Root 'dist') | Out-Null
Copy-Item target\release\jarvis.exe, target\release\jarvis-gui.exe, README.md, LICENSE, SECURITY.md $Stage
Copy-Item -Recurse docs, examples (Join-Path $Stage '.')
Copy-Item packaging\windows\LEIA-ME-WINDOWS.txt $Stage
$Zip = Join-Path $Root "dist\Jarvis_Core-$Version-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Stage\*" -DestinationPath $Zip
Write-Output "Gerado: $Zip"
