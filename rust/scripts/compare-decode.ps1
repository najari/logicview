<#
M3 regression test: decode the same capture with sigrok-cli and with
`lvcli decode` and compare the annotation lines (sample range, decoder
instance, class, text) including their order.

  python -I ..\extensions\tests\make-capture.py test.sr test.pvs
  .\scripts\compare-decode.ps1 -Sr test.sr -Lvcli target\release\lvcli.exe -SigrokDir <app folder>

With -HlaRoot, the repo's test HLAs are installed there (extensions\hla.ps1)
and stacked on uart, i2c, spi and can as well.
#>
param(
    [Parameter(Mandatory)][string]$Sr,
    [Parameter(Mandatory)][string]$Lvcli,
    [Parameter(Mandatory)][string]$SigrokDir,
    [string]$OutDir = "$env:TEMP\lv-compare",
    [string]$HlaRoot
)
$cases = @(
    @{ P = 'uart:rx=D0'; A = 'uart' },
    @{ P = 'uart:rx=D0:format=ascii'; A = 'uart' },
    @{ P = 'i2c:scl=D1:sda=D2'; A = 'i2c' },
    @{ P = 'spi:clk=D3:mosi=D4:miso=D5:cs=D6'; A = 'spi' },
    @{ P = 'can:can_rx=D7'; A = 'can' },
    @{ P = 'spi:clk=D3:mosi=D4:miso=D5:cs=D6,spiflash'; A = 'spi,spiflash' },
    @{ P = 'i2c:scl=D1:sda=D2,eeprom24xx'; A = 'i2c,eeprom24xx' }
)
if ($HlaRoot) {
    $repo = Split-Path (Split-Path $PSScriptRoot)
    foreach ($h in 'line-reader', 'i2c-transactions', 'spi-burst', 'can-messages') {
        & (Join-Path $repo 'extensions\hla.ps1') install (Join-Path $repo "extensions\tests\hlas\$h") -Root $HlaRoot | Out-Null
    }
    $env:SIGROKDECODE_DIR = Join-Path $HlaRoot 'decoders'
    $cases += @(
        @{ P = 'uart:rx=D0,hla_line_reader_uart'; A = 'hla_line_reader_uart' },
        @{ P = 'i2c:scl=D1:sda=D2,hla_i2c_transactions_i2c'; A = 'hla_i2c_transactions_i2c' },
        @{ P = 'spi:clk=D3:mosi=D4:miso=D5:cs=D6,hla_spi_burst_spi'; A = 'hla_spi_burst_spi' },
        @{ P = 'can:can_rx=D7,hla_can_messages_can'; A = 'hla_can_messages_can' }
    )
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$env:LOGICVIEW_SIGROK_DIR = $SigrokDir
$cli = Join-Path $SigrokDir 'sigrok-cli.exe'
$fail = 0
foreach ($c in $cases) {
    $name = ($c.P -replace '[:=,]', '_')
    $a = & $cli -i $Sr -P $c.P -A $c.A --protocol-decoder-samplenum --protocol-decoder-ann-class 2>$null
    $b = & $Lvcli decode $Sr -P $c.P -A $c.A 2>$null
    $a | Set-Content "$OutDir\$name.sigrok.txt"; $b | Set-Content "$OutDir\$name.lv.txt"
    $sa = @($a | Sort-Object); $sb = @($b | Sort-Object)
    $same = ($sa.Count -eq $sb.Count) -and -not (Compare-Object $sa $sb -SyncWindow 0)
    $ordered = ($a.Count -eq $b.Count) -and -not (Compare-Object @($a) @($b) -SyncWindow 0)
    if (-not $same) { $fail++ }
    '{0,-46} sigrok {1,4}  lv {2,4}  {3}{4}' -f $c.P, $a.Count, $b.Count, $(if ($same) { 'SAME' } else { 'DIFF' }), $(if ($same -and -not $ordered) { ' (order differs)' } else { '' })
    if (-not $same) {
        Compare-Object $sa $sb | Select-Object -First 6 | ForEach-Object { '    {0} {1}' -f $_.SideIndicator, $_.InputObject }
    }
}
"cases failed: $fail"
exit $fail

