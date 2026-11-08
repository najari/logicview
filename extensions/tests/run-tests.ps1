# Install the test HLAs with hla.ps1 into a scratch libsigrokdecode root and
# run the bridge unit tests on them (needs a Python 3; the app embeds 3.14).
param(
    [string]$Python = 'C:\msys64\ucrt64\bin\python.exe',
    [string]$Root = (Join-Path ([IO.Path]::GetTempPath()) ('hla-test-' + [guid]::NewGuid()))
)
$ErrorActionPreference = 'Stop'
$hla = Join-Path $PSScriptRoot '..\hla.ps1'
try {
    foreach ($d in Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'hlas') -Directory) {
        & $hla install $d.FullName -Root $Root 3>&1 | ForEach-Object { "  $_" }
    }
    & $hla list -Root $Root | Out-String | Write-Host
    & $Python -I (Join-Path $PSScriptRoot 'test_bridge.py') $Root
    $rc = $LASTEXITCODE
} finally {
    Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
}
exit $rc
