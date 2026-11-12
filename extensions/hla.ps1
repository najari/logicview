<#
Install Saleae Logic 2 High Level Analyzer (HLA) extensions as SLogicView /
PulseView protocol decoders.

  .\hla.ps1 install <extension folder | .zip | GitHub URL> [-Protocol auto|all|<protocol>...]
  .\hla.ps1 list
  .\hla.ps1 remove <extension name | decoder id>
  .\hla.ps1 package add <name>... [-From auto|msys2|pypi] [-Msys2 C:\msys64]
  .\hla.ps1 package list
  .\hla.ps1 package remove <name>

Protocols: uart, i2c, spi, can, lin (on uart), onewire (on onewire_link),
manchester (on the manchester decoder installed with the bridge), i2s,
parallel.

Each HLA becomes one decoder per input protocol, hla_<extension>_<protocol>,
in libsigrokdecode's per-user decoder directory
(%LOCALAPPDATA%\libsigrokdecode\decoders). The shared Saleae API layer goes to
%LOCALAPPDATA%\libsigrokdecode\saleae_compat. -Protocol auto picks the input
protocol from the frame types the HLA code uses; with no match it installs
uart, i2c, spi and can. Restart SLogicView afterwards.

'package add' puts Python packages that HLAs import into saleae_compat\site-packages
(DLLs into saleae_compat\bin). The app's Python is MSYS2's UCRT64 build, so
packages with compiled code come from MSYS2 (mingw-w64-ucrt-x86_64-python-<name>,
installed there with pacman, then copied with their DLL dependencies); pure
Python packages can also come from PyPI wheels (py3-none-any).
#>
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('install', 'list', 'remove', 'package')]
    [string]$Command,
    [Parameter(Position = 1)]
    [string]$Source,
    [Parameter(Position = 2, ValueFromRemainingArguments)]
    [string[]]$Names,
    [ValidateSet('auto', 'all', 'uart', 'i2c', 'spi', 'can', 'lin', 'onewire', 'manchester', 'i2s', 'parallel')]
    [string[]]$Protocol = @('auto'),
    [ValidateSet('auto', 'msys2', 'pypi')]
    [string]$From = 'auto',
    [string]$Msys2 = 'C:\msys64',
    # Python version of the app's decoder runtime (libpython3.14.dll).
    [string]$PythonVersion = '3.14',
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'libsigrokdecode')
)
$ErrorActionPreference = 'Stop'
$Decoders = Join-Path $Root 'decoders'
$CompatDest = Join-Path $Root 'saleae_compat'
$AllProtocols = 'uart', 'i2c', 'spi', 'can', 'lin', 'onewire', 'manchester', 'i2s', 'parallel'
# Installed when the HLA gives no hint of its input.
$DefaultProtocols = 'uart', 'i2c', 'spi', 'can'
$Labels = @{
    uart = 'Async Serial'; i2c = 'I2C'; spi = 'SPI'; can = 'CAN'; lin = 'LIN'
    onewire = '1-Wire'; manchester = 'Manchester'; i2s = 'I2S / PCM'; parallel = 'Simple Parallel'
}
# The sigrok decoder each input stacks on (saleae_bridge.INPUTS).
$Lower = @{
    uart = 'uart'; i2c = 'i2c'; spi = 'spi'; can = 'can'; lin = 'uart'
    onewire = 'onewire_link'; manchester = 'manchester'; i2s = 'i2s'; parallel = 'parallel'
}

function Write-Utf8([string]$Path, [string]$Text) {
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Get-Slug([string]$Text) {
    $s = ($Text.ToLowerInvariant() -replace '[^a-z0-9]+', '_').Trim('_')
    if (-not $s) { $s = 'ext' }
    if ($s -match '^[0-9]') { $s = "x$s" }
    return $s
}

function Get-Installed {
    if (-not (Test-Path -LiteralPath $Decoders)) { return @() }
    Get-ChildItem -LiteralPath $Decoders -Directory -Filter 'hla_*' | ForEach-Object {
        $m = Join-Path $_.FullName 'hla.json'
        if (Test-Path -LiteralPath $m) {
            $j = Get-Content -Raw -LiteralPath $m | ConvertFrom-Json
            [pscustomobject]@{ Dir = $_.FullName; Manifest = $j }
        }
    }
}

# Returns the folder that holds extension.json, downloading or unpacking first.
function Resolve-Extension([string]$Src, [string]$Work) {
    $dir = $null
    if ($Src -match '^https?://github\.com/([^/]+)/([^/#?]+?)(?:\.git)?(?:/tree/([^?#]+?))?/?$') {
        $owner, $repo, $tree = $Matches[1], $Matches[2], $Matches[3]
        $ref, $sub = 'HEAD', ''
        if ($tree) { $parts = $tree -split '/', 2; $ref = $parts[0]; if ($parts.Count -gt 1) { $sub = $parts[1] } }
        $zip = Join-Path $Work 'source.zip'
        Write-Host "Downloading $owner/$repo ($ref)"
        Invoke-WebRequest "https://codeload.github.com/$owner/$repo/zip/$ref" -OutFile $zip -UseBasicParsing
        Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $Work 'src')
        $top = @(Get-ChildItem -LiteralPath (Join-Path $Work 'src') -Directory)[0].FullName
        $dir = if ($sub) { Join-Path $top $sub } else { $top }
    } elseif ($Src -match '\.zip$' -and (Test-Path -LiteralPath $Src -PathType Leaf)) {
        Expand-Archive -LiteralPath $Src -DestinationPath (Join-Path $Work 'src')
        $dir = Join-Path $Work 'src'
    } elseif (Test-Path -LiteralPath $Src -PathType Container) {
        $dir = (Resolve-Path -LiteralPath $Src).Path
    } else {
        throw "Not an extension folder, .zip file or GitHub URL: $Src"
    }
    if (Test-Path -LiteralPath (Join-Path $dir 'extension.json')) { return $dir }
    $found = @(Get-ChildItem -LiteralPath $dir -Recurse -File -Filter extension.json |
        Where-Object { $_.FullName -notmatch '\\(\.git|node_modules)\\' })
    if ($found.Count -eq 1) { return $found[0].DirectoryName }
    if ($found.Count -eq 0) { throw "No extension.json in $Src" }
    throw "Several extensions in $Src; pass one of these folders:`n  " + (($found | ForEach-Object DirectoryName) -join "`n  ")
}

# Input protocol(s) an HLA expects: frame types its code checks, plus the
# analyzers its name, description, README and code comments mention.
function Find-Protocols([string]$Dir, $Json) {
    $code = (Get-ChildItem -LiteralPath $Dir -Recurse -File -Filter *.py |
        Where-Object { $_.FullName -notmatch '\\(\.git|__pycache__)\\' } |
        ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }) -join "`n"
    $found = @{}
    if ($code -match "['""](identifier_field|control_field|data_field|crc_field|ack_field|can_error)['""]") { $found.can = 1 }
    if ($code -match "['""](mosi|miso)['""]") { $found.spi = 1 }
    if ($code -match "['""]address['""]" -or $code -match "==\s*['""](start|stop)['""]") { $found.i2c = 1 }
    if ($code -match "['""](header_break|header_sync|header_pid|data_or_checksum|protected_id)['""]") { $found.lin = 1 }
    if ($code -match "['""](rom_command|family_code|invalid_rom_command)['""]") { $found.onewire = 1 }
    $text = "$($Json.name) $($Json.description) $code"
    $readme = Get-ChildItem -LiteralPath $Dir -File -Filter 'README*' | Select-Object -First 1
    if ($readme) { $text += ' ' + (Get-Content -Raw -LiteralPath $readme.FullName) }
    $text = $text.ToLowerInvariant()
    if ($text -match '\bi2c\b') { $found.i2c = 1 }
    if ($text -match '\bspi\b') { $found.spi = 1 }
    # Not a bare "can": that is mostly the English word.
    if ($text -match '\bcan[ -]?(bus|frames?|fd|analyzer|messages?|2\.0)\b|\bcanbus\b') { $found.can = 1 }
    if ($text -match 'async serial|asynchronous serial|\buart\b|\bserial\b') { $found.uart = 1 }
    if ($text -match '\blin\b|\blin[ -]?bus\b') { $found.lin = 1 }
    if ($text -match '\b1-wire\b|\bone-?wire\b') { $found.onewire = 1 }
    if ($text -match 'manchester|bi-?phase') { $found.manchester = 1 }
    if ($text -match '\bi2s\b|\bpcm\b') { $found.i2s = 1 }
    if ($text -match 'simple parallel|parallel bus|parallel analyzer') { $found.parallel = 1 }
    $list = @($AllProtocols | Where-Object { $found.ContainsKey($_) })
    if ($list) { return $list }
    return $DefaultProtocols
}

# Warn when the entry class does not subclass HighLevelAnalyzer: early
# (pre-1.0) HLAs used get_capabilities()/dict frames, which Logic 2 dropped.
function Test-EntryClass([string]$Dir, [string]$Entry) {
    $module, $class = $Entry -replace '^(.*)\.([^.]+)$', '$1|$2' -split '\|'
    $file = Join-Path $Dir (($module -replace '\.', '\') + '.py')
    if (-not (Test-Path -LiteralPath $file)) { Write-Warning "Entry point file not found: $file"; return }
    $src = Get-Content -Raw -LiteralPath $file
    if ($src -notmatch "class\s+$([regex]::Escape($class))\s*\([^)]*HighLevelAnalyzer") {
        Write-Warning "$class in $(Split-Path $file -Leaf) does not subclass HighLevelAnalyzer. It looks like the pre-1.0 HLA API, which Logic 2 and this bridge do not run; the decoder will show a load error."
    }
}

function Get-License([string]$Dir, $Json) {
    if ($Json.license) { return [string]$Json.license }
    $file = Get-ChildItem -LiteralPath $Dir -File | Where-Object Name -match '^(LICENSE|COPYING)' | Select-Object -First 1
    if ($file) {
        $line = Get-Content -LiteralPath $file.FullName | Where-Object { $_.Trim() } | Select-Object -First 1
        if ($line) { return $line.Trim() }
    }
    return 'unknown'
}

function Install-Compat {
    New-Item -ItemType Directory -Force -Path $CompatDest | Out-Null
    $old = Join-Path $CompatDest 'saleae'
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'compat\saleae') -Destination $CompatDest -Recurse
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'compat\saleae_bridge.py') -Destination $CompatDest -Force
    # sigrok has no Manchester decoder: install ours for 'manchester' HLAs.
    New-Item -ItemType Directory -Force -Path $Decoders | Out-Null
    $man = Join-Path $Decoders 'manchester'
    if (Test-Path -LiteralPath $man) { Remove-Item -LiteralPath $man -Recurse -Force }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'decoders\manchester') -Destination $Decoders -Recurse
}

function Install-Extension([string]$Src) {
    $work = Join-Path ([IO.Path]::GetTempPath()) ('hla-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $dir = Resolve-Extension $Src $work
        $json = Get-Content -Raw -LiteralPath (Join-Path $dir 'extension.json') | ConvertFrom-Json
        $entries = @($json.extensions.PSObject.Properties)
        foreach ($e in $entries | Where-Object { $_.Value.type -ne 'HighLevelAnalyzer' }) {
            Write-Warning "Skipped $($e.Name): '$($e.Value.type)' extensions are not supported (only HighLevelAnalyzer)."
        }
        $hlas = @($entries | Where-Object { $_.Value.type -eq 'HighLevelAnalyzer' })
        if (-not $hlas) { throw "$($json.name) has no HighLevelAnalyzer extension." }

        $protocols = if ($Protocol -contains 'all') { $AllProtocols }
                     elseif ($Protocol -contains 'auto') { Find-Protocols $dir $json }
                     else { $Protocol }
        $extName = if ($json.name) { [string]$json.name } else { Split-Path $dir -Leaf }
        $license = Get-License $dir $json
        $origin = if ($Src -match '^https?://') { $Src } else { (Resolve-Path -LiteralPath $Src).Path }
        Install-Compat
        New-Item -ItemType Directory -Force -Path $Decoders | Out-Null

        foreach ($h in $hlas) {
            Test-EntryClass $dir ([string]$h.Value.entryPoint)
            $slug = Get-Slug $extName
            $title = $extName
            if ($hlas.Count -gt 1) { $slug += '_' + (Get-Slug $h.Name); $title += " / $($h.Name)" }
            # Replace an earlier install of the same HLA (all protocols).
            Get-Installed | Where-Object { $_.Manifest.slug -eq $slug } | ForEach-Object { Remove-Item -LiteralPath $_.Dir -Recurse -Force }
            foreach ($p in $protocols) {
                $id = "hla_${slug}_$p"
                $dest = Join-Path $Decoders $id
                New-Item -ItemType Directory -Path $dest | Out-Null
                Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'template\__init__.py'), (Join-Path $PSScriptRoot 'template\pd.py') -Destination $dest
                $ext = Join-Path $dest 'ext'
                New-Item -ItemType Directory -Path $ext | Out-Null
                Get-ChildItem -LiteralPath $dir -Force | Where-Object { $_.Name -notin '.git', '.github', '__pycache__' } |
                    Copy-Item -Destination $ext -Recurse
                Get-ChildItem -LiteralPath $ext -Recurse -Directory -Force -Filter __pycache__ | Remove-Item -Recurse -Force
                $manifest = [ordered]@{
                    id             = $id
                    slug           = $slug
                    input          = $p
                    entry          = [string]$h.Value.entryPoint
                    hla_name       = [string]$h.Name
                    name           = "$title [$p]"
                    longname       = "$title (Saleae HLA on $($Labels[$p]))"
                    desc           = if ($json.description) { [string]$json.description } else { "Saleae Logic 2 HLA $title" }
                    license        = $license
                    extension_name = $extName
                    version        = [string]$json.version
                    author         = [string]$json.author
                    source         = $origin
                }
                Write-Utf8 (Join-Path $dest 'hla.json') ($manifest | ConvertTo-Json)
                Write-Host ("Installed {0} -> {1} ({2} input, stack on the '{3}' decoder)" -f $title, $id, $Labels[$p], $Lower[$p])
            }
        }
        Write-Host "Restart SLogicView to load it. The decoders are listed under the tag 'Saleae HLA'."
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# --- Python packages for HLAs (saleae_compat\site-packages, saleae_compat\bin) ---

$Packages = Join-Path $CompatDest 'site-packages'
$PackageDlls = Join-Path $CompatDest 'bin'
$PackageIndex = Join-Path $CompatDest 'packages.json'
$MingwPrefix = 'mingw-w64-ucrt-x86_64-'

function Get-PackageIndex {
    $index = [ordered]@{}
    if (Test-Path -LiteralPath $PackageIndex) {
        (Get-Content -Raw -LiteralPath $PackageIndex | ConvertFrom-Json).PSObject.Properties |
            ForEach-Object { $index[$_.Name] = $_.Value }
    }
    return $index
}

function Save-PackageIndex($Index) {
    New-Item -ItemType Directory -Force -Path $CompatDest | Out-Null
    Write-Utf8 $PackageIndex ([pscustomobject]$Index | ConvertTo-Json -Depth 5)
}

function Get-PackageKey([string]$Name) { return ($Name.ToLowerInvariant() -replace '[-_.]+', '-') }

function Invoke-Pacman([string[]]$Arguments) {
    $pacman = Join-Path $Msys2 'usr\bin\pacman.exe'
    if (-not (Test-Path -LiteralPath $pacman)) { throw "MSYS2 not found at $Msys2 (pass -Msys2 <folder>)" }
    # Windows PowerShell 5.1 turns native stderr into errors under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $out = & $pacman @Arguments 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "pacman $($Arguments -join ' ') failed:`n$($out -join "`n")" }
    return $out
}

function Test-Msys2Package([string]$Package) {
    $pacman = Join-Path $Msys2 'usr\bin\pacman.exe'
    if (-not (Test-Path -LiteralPath $pacman)) { return $false }
    $ErrorActionPreference = 'Continue'
    & $pacman -Si $Package 2>&1 | Out-Null
    return $LASTEXITCODE -eq 0
}

# MSYS2 packages that $Package needs, without Python itself (the app has it).
function Get-Msys2Closure([string]$Package) {
    $seen = [ordered]@{}
    $todo = [Collections.Generic.Queue[string]]::new()
    $todo.Enqueue($Package)
    while ($todo.Count) {
        $p = $todo.Dequeue()
        if ($seen.Contains($p) -or $p -eq "${MingwPrefix}python") { continue }
        $seen[$p] = 1
        $line = (Invoke-Pacman @('-Qi', $p)) | Where-Object { $_ -match '^Depends On\s*:' }
        $deps = ($line -replace '^Depends On\s*:\s*', '').Trim()
        if ($deps -and $deps -ne 'None') {
            foreach ($d in $deps -split '\s+') { $todo.Enqueue(($d -split '[<>=]')[0]) }
        }
    }
    return @($seen.Keys)
}

# Copy the Python files and DLLs of an MSYS2 package (installed with pacman
# first) and of its dependencies. Returns the copied paths, relative to saleae_compat.
function Add-Msys2Package([string]$Name) {
    $pkg = "${MingwPrefix}python-$(Get-PackageKey $Name)"
    $py = ((Invoke-Pacman @('-Q', "${MingwPrefix}python")) -split '\s+')[1]
    if ($py -notlike "$PythonVersion.*") {
        throw "MSYS2 has Python $py, the app $PythonVersion; their compiled modules do not mix."
    }
    Write-Host "Installing $pkg in MSYS2 ($Msys2)"
    Invoke-Pacman @('-S', '--needed', '--noconfirm', $pkg) | ForEach-Object { "  $_" } | Write-Host
    $site = "/ucrt64/lib/python$PythonVersion/site-packages/"
    $files = [Collections.Generic.List[string]]::new()
    $names = @()
    foreach ($p in Get-Msys2Closure $pkg) {
        $names += $p
        foreach ($line in Invoke-Pacman @('-Ql', $p)) {
            $path = ($line -split ' ', 2)[1]
            if (-not $path -or $path.EndsWith('/')) { continue }
            if ($path.StartsWith($site)) {
                $rel = 'site-packages\' + ($path.Substring($site.Length) -replace '/', '\')
            } elseif ($path -match '^/ucrt64/bin/[^/]+\.dll$') {
                $rel = 'bin\' + ($path -split '/')[-1]
            } else { continue }
            if ($rel -match '\\__pycache__\\') { continue }
            $dest = Join-Path $CompatDest $rel
            New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
            Copy-Item -LiteralPath (Join-Path $Msys2 ($path.TrimStart('/') -replace '/', '\')) -Destination $dest -Force
            $files.Add($rel)
        }
    }
    $version = ((Invoke-Pacman @('-Q', $pkg)) -split '\s+')[1]
    return [pscustomobject]@{ source = 'msys2'; version = $version; packages = $names; files = @($files) }
}

# A pure Python wheel from PyPI, and the requirements it lists without
# environment markers.
function Add-PypiPackage([string]$Name, $Index, [string]$Work) {
    try {
        $meta = Invoke-RestMethod "https://pypi.org/pypi/$Name/json" -UseBasicParsing
    } catch {
        throw "$Name is neither on PyPI nor in MSYS2 (${MingwPrefix}python-$(Get-PackageKey $Name)): $($_.Exception.Message)"
    }
    $wheel = @($meta.urls | Where-Object { $_.packagetype -eq 'bdist_wheel' -and $_.filename -match '-none-any\.whl$' })[0]
    if (-not $wheel) {
        throw "$Name $($meta.info.version) has no pure Python wheel on PyPI, and MSYS2 has no ${MingwPrefix}python-$(Get-PackageKey $Name). Compiled packages must come from MSYS2."
    }
    Write-Host "Downloading $($wheel.filename)"
    $zip = Join-Path $Work ($wheel.filename -replace '\.whl$', '.zip')
    Invoke-WebRequest $wheel.url -OutFile $zip -UseBasicParsing
    $unpacked = Join-Path $Work ($wheel.filename -replace '\.whl$', '')
    Expand-Archive -LiteralPath $zip -DestinationPath $unpacked
    $files = [Collections.Generic.List[string]]::new()
    $requires = @()
    foreach ($f in Get-ChildItem -LiteralPath $unpacked -Recurse -File) {
        $rel = $f.FullName.Substring($unpacked.Length + 1)
        # <name>.data\purelib and \platlib go to site-packages; scripts and headers are not needed.
        if ($rel -match '^[^\\]+\.data\\(purelib|platlib)\\(.+)$') { $rel = $Matches[2] }
        elseif ($rel -match '^[^\\]+\.data\\') { continue }
        if ($rel -match '^[^\\]+\.dist-info\\METADATA$') {
            $requires = Get-Content -LiteralPath $f.FullName | Where-Object { $_ -match '^Requires-Dist:' -and $_ -notmatch ';' } |
                ForEach-Object { (($_ -replace '^Requires-Dist:\s*', '') -split '[\s\[(<>=!~]')[0] }
        }
        $dest = Join-Path $Packages $rel
        New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
        Copy-Item -LiteralPath $f.FullName -Destination $dest -Force
        $files.Add('site-packages\' + $rel)
    }
    return [pscustomobject]@{ source = 'pypi'; version = [string]$meta.info.version; packages = @($wheel.filename); files = @($files); requires = @($requires) }
}

function Add-Package([string]$Name, $Index, [string]$Work) {
    $key = Get-PackageKey $Name
    if ($Index.Contains($key)) { Write-Host "$key is already added ($($Index[$key].version))"; return }
    $useMsys2 = $From -eq 'msys2' -or ($From -eq 'auto' -and (Test-Msys2Package "${MingwPrefix}python-$key"))
    $entry = if ($useMsys2) { Add-Msys2Package $key } else { Add-PypiPackage $Name $Index $Work }
    $Index[$key] = $entry
    Write-Host ("Added {0} {1} from {2} ({3} files)" -f $key, $entry.version, $entry.source, $entry.files.Count)
    foreach ($r in @($entry.requires)) { if ($r) { Add-Package $r $Index $Work } }
}

function Remove-Package([string]$Name, $Index) {
    $key = Get-PackageKey $Name
    if (-not $Index.Contains($key)) { throw "Not added: $Name (see 'package list')" }
    # Top-level folders (numpy\, numpy-2.5.4.dist-info\) go as a whole, with
    # the __pycache__ files Python wrote there; shared files and folders stay.
    $top = { param($f) if ($f -match '^(site-packages\\[^\\]+)\\') { $Matches[1] } }
    $keep = @{}
    foreach ($k in @($Index.Keys)) {
        if ($k -ne $key) { foreach ($f in $Index[$k].files) { $keep[$f] = 1; $t = & $top $f; if ($t) { $keep[$t] = 1 } } }
    }
    foreach ($f in $Index[$key].files) {
        $t = & $top $f
        $target = if ($t -and -not $keep.ContainsKey($t)) { $t } else { $f }
        $path = Join-Path $CompatDest $target
        if (-not $keep.ContainsKey($target) -and (Test-Path -LiteralPath $path)) { Remove-Item -LiteralPath $path -Recurse -Force }
    }
    foreach ($dir in $Packages, $PackageDlls) {
        if (Test-Path -LiteralPath $dir) {
            Get-ChildItem -LiteralPath $dir -Recurse -Directory | Sort-Object { $_.FullName.Length } -Descending |
                Where-Object { -not (Get-ChildItem -LiteralPath $_.FullName -Force) } | Remove-Item -Force
        }
    }
    $Index.Remove($key)
    Write-Host "Removed $key"
}

function Invoke-PackageCommand {
    $index = Get-PackageIndex
    switch ($Source) {
        'add' {
            if (-not $Names) { throw 'package add needs one or more package names' }
            $work = Join-Path ([IO.Path]::GetTempPath()) ('hla-pkg-' + [guid]::NewGuid())
            New-Item -ItemType Directory -Path $work | Out-Null
            try {
                foreach ($n in $Names) { Add-Package $n $index $work; Save-PackageIndex $index }
            } finally {
                Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
            }
            Write-Host "Restart the app to load them."
        }
        'list' {
            $index.GetEnumerator() | ForEach-Object {
                [pscustomobject]@{ Name = $_.Key; Version = $_.Value.version; From = $_.Value.source; Files = @($_.Value.files).Count }
            } | Format-Table -AutoSize
        }
        'remove' {
            if (-not $Names) { throw 'package remove needs a package name (see package list)' }
            foreach ($n in $Names) { Remove-Package $n $index }
            Save-PackageIndex $index
        }
        default { throw "package needs add, list or remove (got '$Source')" }
    }
}

switch ($Command) {
    'package' { Invoke-PackageCommand }
    'install' {
        if (-not $Source) { throw 'install needs a source: extension folder, .zip or GitHub URL' }
        Install-Extension $Source
    }
    'list' {
        Get-Installed | Group-Object { $_.Manifest.slug } | ForEach-Object {
            $m = $_.Group[0].Manifest
            [pscustomobject]@{
                Name     = $m.extension_name
                Version  = $m.version
                Inputs   = ($_.Group | ForEach-Object { $_.Manifest.input }) -join ', '
                Decoders = ($_.Group | ForEach-Object { $_.Manifest.id }) -join ', '
                Source   = $m.source
            }
        } | Format-Table -AutoSize -Wrap
    }
    'remove' {
        if (-not $Source) { throw 'remove needs an extension name or decoder id (see list)' }
        $hits = @(Get-Installed | Where-Object {
            $_.Manifest.id -eq $Source -or $_.Manifest.slug -eq $Source -or
            $_.Manifest.extension_name -eq $Source -or $_.Manifest.slug -eq (Get-Slug $Source) })
        if (-not $hits) { throw "Not installed: $Source" }
        foreach ($h in $hits) { Remove-Item -LiteralPath $h.Dir -Recurse -Force; Write-Host "Removed $($h.Manifest.id)" }
    }
}
