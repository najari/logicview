<#
Install Saleae Logic 2 High Level Analyzer (HLA) extensions as SLogicView /
PulseView protocol decoders.

  .\hla.ps1 install <extension folder | .zip | GitHub URL> [-Protocol auto|uart|i2c|spi|can|all]
  .\hla.ps1 list
  .\hla.ps1 remove <extension name | decoder id>

Each HLA becomes one decoder per input protocol, hla_<extension>_<protocol>,
in libsigrokdecode's per-user decoder directory
(%LOCALAPPDATA%\libsigrokdecode\decoders). The shared Saleae API layer goes to
%LOCALAPPDATA%\libsigrokdecode\saleae_compat. -Protocol auto picks the input
protocol from the frame types the HLA code uses. Restart SLogicView afterwards.
#>
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('install', 'list', 'remove')]
    [string]$Command,
    [Parameter(Position = 1)]
    [string]$Source,
    [ValidateSet('auto', 'uart', 'i2c', 'spi', 'can', 'all')]
    [string[]]$Protocol = @('auto'),
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'libsigrokdecode')
)
$ErrorActionPreference = 'Stop'
$Decoders = Join-Path $Root 'decoders'
$CompatDest = Join-Path $Root 'saleae_compat'
$AllProtocols = 'uart', 'i2c', 'spi', 'can'
$Labels = @{ uart = 'Async Serial'; i2c = 'I2C'; spi = 'SPI'; can = 'CAN' }

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
    $text = "$($Json.name) $($Json.description) $code"
    $readme = Get-ChildItem -LiteralPath $Dir -File -Filter 'README*' | Select-Object -First 1
    if ($readme) { $text += ' ' + (Get-Content -Raw -LiteralPath $readme.FullName) }
    $text = $text.ToLowerInvariant()
    if ($text -match '\bi2c\b') { $found.i2c = 1 }
    if ($text -match '\bspi\b') { $found.spi = 1 }
    # Not a bare "can": that is mostly the English word.
    if ($text -match '\bcan[ -]?(bus|frames?|fd|analyzer|messages?|2\.0)\b|\bcanbus\b') { $found.can = 1 }
    if ($text -match 'async serial|asynchronous serial|\buart\b|\bserial\b') { $found.uart = 1 }
    $list = @($AllProtocols | Where-Object { $found.ContainsKey($_) })
    if ($list) { return $list }
    return $AllProtocols
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
                Write-Host ("Installed {0} -> {1} (stack on the '{2}' decoder)" -f $title, $id, $p)
            }
        }
        Write-Host "Restart SLogicView to load it. The decoders are listed under the tag 'Saleae HLA'."
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

switch ($Command) {
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
