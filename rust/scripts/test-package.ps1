<#
M5 acceptance: unpack the release zip into an empty folder and use it the
way a new PC would, with nothing else found on PATH and no development
environment variables.

  .\scripts\test-package.ps1 -Zip dist\logicview-0.1.0-windows-x64.zip -Python <python.exe> [-Hardware]

-Python runs the test helpers (make-capture.py, mcp/tests/test_mcp.py); the
package itself does not use it. -Hardware adds a capture from the analyzer.
#>
param(
    [Parameter(Mandatory)][string]$Zip,
    [Parameter(Mandatory)][string]$Python,
    [switch]$Hardware
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Resolve-Path (Join-Path $PSScriptRoot '..'))
$work = Join-Path ([IO.Path]::GetTempPath()) ("lv-package-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $work | Out-Null
Expand-Archive -Path $Zip -DestinationPath $work
$app = (Get-ChildItem $work -Directory | Select-Object -First 1).FullName
$failed = @()
function Check([string]$Name, [bool]$Ok, [string]$Detail = '') {
    if ($Ok) { "ok   $Name" } else { "FAIL $Name  $Detail"; $script:failed += $Name }
}

# A clean environment: system folders only, no development settings.
$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot;$env:SystemRoot\System32\WindowsPowerShell\v1.0"
foreach ($v in 'LOGICVIEW_SIGROK_DIR', 'LOGICVIEW_FIRMWARE_DIR', 'PYTHONHOME', 'PYTHONPATH', 'SIGROKDECODE_DIR', 'SIGROKDECODE_PATH') {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}
$lv = Join-Path $app 'lvcli.exe'

$out = & $lv decoders 2>&1
$count = @($out | Where-Object { $_ -match '^\S+\s{2,}\S' }).Count
Check 'decoders load from the package' ($count -ge 130 -and -not ($out -match 'WARN')) "$count decoders; $($out | Select-Object -Last 2)"

$sr = Join-Path $work 'test.sr'
& $Python -I (Join-Path $repo 'extensions\tests\make-capture.py') $sr (Join-Path $work 'test.pvs') | Out-Null
$dec = & $lv decode $sr -P 'uart:rx=D0:format=ascii' -A 'uart=rx-data' 2>$null
Check 'uart decode' (($dec -join '|') -match 'rx-data: H\|.*rx-data: i') ($dec | Select-Object -First 3)
$dec = & $lv decode $sr -P 'can:can_rx=D7' -A 'can=id' 2>$null
Check 'can decode' (($dec -join '|') -match '0x123') ($dec | Select-Object -First 2)

$lvcap = Join-Path $work 'test.lvcap'
& $lv import $sr -o $lvcap | Out-Null
& $lv export $lvcap -f csv -o (Join-Path $work 'test.csv') | Out-Null
$csv = Get-Content (Join-Path $work 'test.csv') -TotalCount 2
Check 'import and csv export' ($csv[0] -eq 'Time [s],Channel 0,Channel 1,Channel 2,Channel 3,Channel 4,Channel 5,Channel 6,Channel 7') ($csv -join ' / ')

if ($Hardware) {
    $devs = & $lv devices 2>&1
    Check 'device found' (($devs -join ' ') -match '0925:3881') ($devs -join ' / ')
    $cap = Join-Path $work 'hw.lvcap'
    $r = & $lv capture -r 1M -s 1M -o $cap 2>&1
    Check 'capture 1 M samples at 1 MHz' (($r -join ' ') -match '1,000,000') ($r | Select-Object -Last 3)
}

# MCP server from the package, through the repository's end-to-end test.
$testArgs = @('-I', (Join-Path $repo 'mcp\tests\test_mcp.py'), '--sigrok-cli', 'unused', '--server', (Join-Path $app 'logicview.exe'))
if ($Hardware) { $testArgs += '--hardware' }
$mcp = & $Python @testArgs 2>&1
Check 'mcp/tests/test_mcp.py against the package' (($mcp | Select-Object -Last 1) -eq 'all tests passed') (($mcp | Where-Object { $_ -match '^FAIL' }) -join ' / ')

"package folder: $app"
if ($failed) { "FAILED: $($failed -join ', ')"; exit 1 }
'all package checks passed'

