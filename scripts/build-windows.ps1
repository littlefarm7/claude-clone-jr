# Gera dist\Claudio_Code-<versão>-windows-x86_64.zip (claudio.exe + claudio-gui.exe).
# Uso: pwsh -File scripts/build-windows.ps1
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
cargo build --release -p claudio-cli -p claudio-gui
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Stage = Join-Path $Root "target\Claudio_Code-$Version-windows"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage, (Join-Path $Root 'dist') | Out-Null
Copy-Item target\release\claudio.exe, target\release\claudio-gui.exe, README.md, LICENSE, SECURITY.md $Stage
Copy-Item -Recurse docs, examples (Join-Path $Stage '.')
Copy-Item packaging\windows\LEIA-ME-WINDOWS.txt $Stage
$Zip = Join-Path $Root "dist\Claudio_Code-$Version-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Stage\*" -DestinationPath $Zip
Write-Output "Gerado: $Zip"
