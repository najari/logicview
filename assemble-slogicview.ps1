# Assemble a portable SLogicView folder that also drives fx2lafw (FX2/Saleae-
# compatible) analyzers: release payload + rebuilt libsigrok-4.dll + its extra
# runtime DLL + fx2lafw firmware.
param(
    [Parameter(Mandatory)][string]$ReleaseExe,    # SLogicView-SLogic-<ver>-windows-x86_64.exe
    [Parameter(Mandatory)][string]$LibsigrokDll,  # build\prefix\bin\libsigrok-4.dll
    [Parameter(Mandatory)][string]$Destination,   # portable app folder to create
    [string]$FirmwareDir = 'C:\Program Files\sigrok\PulseView\share\sigrok-firmware',
    [string]$Ucrt64Bin = 'C:\msys64\ucrt64\bin',
    [string]$SevenZip = 'C:\Program Files\7-Zip\7z.exe'
)
$ErrorActionPreference = 'Stop'

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("slogic-extract-" + [guid]::NewGuid())
& $SevenZip x $ReleaseExe "-o$tmp" -y | Out-Null
if ($LASTEXITCODE -ne 0) { throw "7-Zip failed ($LASTEXITCODE)" }
$payload = Get-ChildItem -LiteralPath $tmp -Recurse -Filter slogicview.exe | Select-Object -First 1
if (-not $payload) { throw "slogicview.exe not found in $ReleaseExe" }

if (Test-Path -LiteralPath $Destination) { throw "$Destination already exists; remove it first" }
New-Item -ItemType Directory -Path $Destination | Out-Null
Copy-Item -Path (Join-Path $payload.DirectoryName '*') -Destination $Destination -Recurse
Remove-Item -LiteralPath $tmp -Recurse -Force

# Keep the release DLL for rollback, then drop in the fx2lafw-enabled build.
Rename-Item -LiteralPath (Join-Path $Destination 'libsigrok-4.dll') 'libsigrok-4.dll.release'
Copy-Item -LiteralPath $LibsigrokDll -Destination $Destination

# Newer MSYS2 names zlib libz-1.dll (the release bundles zlib1.dll).
if (-not (Test-Path (Join-Path $Destination 'libz-1.dll'))) {
    Copy-Item -LiteralPath (Join-Path $Ucrt64Bin 'libz-1.dll') -Destination $Destination
}

$fw = Join-Path $Destination 'share\sigrok-firmware'
New-Item -ItemType Directory -Force -Path $fw | Out-Null
Copy-Item -Path (Join-Path $FirmwareDir 'fx2lafw-*.fw') -Destination $fw

Write-Host "Assembled: $Destination"
