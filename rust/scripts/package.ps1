<#
M5: build the Windows release zip.

  .\scripts\package.ps1 -RuntimeDir <SLogicView app folder>

The decoder runtime (libsigrokdecode and the DLLs it needs, the Python
standard library, the decoder scripts) and the fx2lafw firmware are taken
from an existing SLogicView/sigrok app folder (e.g. the v2.3.3-fx2lafw
release). Output: dist\logicview-<version>-windows-x64.zip
#>
param(
    [Parameter(Mandatory)][string]$RuntimeDir,
    [string]$Firmware,
    [string]$OutDir = (Join-Path $PSScriptRoot '..\dist'),
    [switch]$NoBuild
)
$ErrorActionPreference = 'Stop'
$rust = Resolve-Path (Join-Path $PSScriptRoot '..')
$repo = Split-Path $rust

# DLLs of the decoder runtime: libsigrokdecode, glib, Python and their
# dependencies, including those of the Python extension modules (found with
# dumpbin /dependents).
$dlls = @(
    'libsigrokdecode-4.dll', 'libglib-2.0-0.dll', 'libpython3.14.dll', 'libintl-8.dll', 'libiconv-2.dll',
    'libpcre2-8-0.dll', 'libgcc_s_seh-1.dll', 'libwinpthread-1.dll', 'libstdc++-6.dll', 'libffi-8.dll',
    'libbz2-1.dll', 'liblzma-5.dll', 'libzstd.dll', 'zlib1.dll'
)
# Standard library parts no decoder needs.
$skipPy = '__pycache__', 'ensurepip', 'venv', 'Tools', 'idlelib', 'tkinter', 'turtledemo', 'test', 'tests'

$version = (Select-String -Path (Join-Path $rust 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "logicview-$version-windows-x64"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = Resolve-Path $OutDir
$stage = Join-Path $OutDir $name
$zip = "$stage.zip"

if (-not $NoBuild) {
    Push-Location $rust
    try {
        & cargo build --release -p logicview -p lvcli
        if ($LASTEXITCODE) { throw "cargo build failed" }
    } finally { Pop-Location }
}

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory $stage | Out-Null
foreach ($exe in 'logicview.exe', 'lvcli.exe') {
    Copy-Item (Join-Path $rust "target\release\$exe") $stage
}

# Firmware
if (-not $Firmware) { $Firmware = Join-Path $RuntimeDir 'share\sigrok-firmware\fx2lafw-saleae-logic.fw' }
if (-not (Test-Path $Firmware)) { throw "firmware not found: $Firmware" }
New-Item -ItemType Directory (Join-Path $stage 'firmware') | Out-Null
Copy-Item $Firmware (Join-Path $stage 'firmware')

# Decoder runtime
$sig = Join-Path $stage 'sigrok'
New-Item -ItemType Directory $sig | Out-Null
foreach ($d in $dlls) {
    $src = Join-Path $RuntimeDir $d
    if (-not (Test-Path $src)) { throw "runtime DLL missing: $src" }
    Copy-Item $src $sig
}
function Copy-Tree([string]$From, [string]$To, [string[]]$Skip) {
    New-Item -ItemType Directory -Force $To | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $From -Force) {
        if ($Skip -contains $item.Name) { continue }
        $dest = Join-Path $To $item.Name
        if ($item.PSIsContainer) { Copy-Tree $item.FullName $dest $Skip } else { Copy-Item -LiteralPath $item.FullName $dest }
    }
}
$py = Get-ChildItem (Join-Path $RuntimeDir 'lib') -Directory -Filter 'python3*' | Select-Object -First 1
if (-not $py) { throw "no lib\python3.* in $RuntimeDir" }
Copy-Tree $py.FullName (Join-Path $sig "lib\$($py.Name)") $skipPy
Copy-Tree (Join-Path $RuntimeDir 'share\libsigrokdecode\decoders') (Join-Path $sig 'share\libsigrokdecode\decoders') @('__pycache__')

# HLA support and the driver helper
$ext = Join-Path $stage 'extensions'
New-Item -ItemType Directory $ext | Out-Null
Copy-Item (Join-Path $repo 'extensions\hla.ps1') $ext
Copy-Tree (Join-Path $repo 'extensions\compat') (Join-Path $ext 'compat') @('__pycache__')
Copy-Tree (Join-Path $repo 'extensions\template') (Join-Path $ext 'template') @('__pycache__')
Copy-Tree (Join-Path $repo 'extensions\decoders') (Join-Path $ext 'decoders') @('__pycache__')
New-Item -ItemType Directory (Join-Path $stage 'tools') | Out-Null
Copy-Item (Join-Path $repo 'tools\install-winusb.ps1') (Join-Path $stage 'tools')

# Docs
$readme = (Get-Content -Raw -Encoding utf8 (Join-Path $rust 'packaging\README.txt')).Replace('{VERSION}', $version)
[IO.File]::WriteAllText((Join-Path $stage 'README.txt'), $readme.Replace("`r`n", "`n").Replace("`n", "`r`n"), [Text.UTF8Encoding]::new($true))
Copy-Item (Join-Path $repo 'sources\libsigrokdecode\COPYING') (Join-Path $stage 'LICENSE.txt')

if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
$files = (Get-ChildItem -Recurse -File $stage | Measure-Object Length -Sum)
'{0}: {1} files, {2:N1} MB unpacked, zip {3:N1} MB' -f $zip, $files.Count, ($files.Sum / 1MB), ((Get-Item $zip).Length / 1MB)
