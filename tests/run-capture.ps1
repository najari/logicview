# Launch a PulseView-like app with debug logging, press Run via UI Automation,
# wait, screenshot, then summarize the fx2lafw transfer results from the log.
# It moves the mouse and sends keys: leave the desktop alone while it runs.
# The log (<Tag>-err.log) and screenshot (<Tag>.png) go to -OutDir.
param(
    [Parameter(Mandatory)] [string]$Exe,
    [Parameter(Mandatory)] [string]$Tag,
    [int]$WaitSeconds = 60,
    [int[]]$ClickAt,
    [string]$PreRunScript,
    [string]$OutDir = (Join-Path $PSScriptRoot 'out')
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$procName = [IO.Path]::GetFileNameWithoutExtension($Exe)
$err = Join-Path $OutDir "$Tag-err.log"
$out = Join-Path $OutDir "$Tag-out.log"

Get-Process $procName -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
$p = Start-Process $Exe -ArgumentList '-l', '4' -WorkingDirectory (Split-Path $Exe) `
    -RedirectStandardOutput $out -RedirectStandardError $err -PassThru

# Wait for the device to be opened and the main window to exist.
$deadline = (Get-Date).AddSeconds(40)
do {
    Start-Sleep -Milliseconds 500
    $p.Refresh()
    $opened = (Test-Path $err) -and (Select-String -LiteralPath $err -Pattern 'Opened device' -Quiet)
} until (($opened -and $p.MainWindowHandle -ne 0) -or (Get-Date) -gt $deadline)
if (-not $opened) { Write-Output "Device was not opened within 40 s"; }
Start-Sleep -Seconds 2

Add-Type -AssemblyName System.Drawing
if (-not ('RcWin' -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class RcWin { [StructLayout(LayoutKind.Sequential)] public struct R { public int L,T,Rt,B; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
}
[void][RcWin]::SetProcessDPIAware()

if ($PreRunScript) {
    try { & $PreRunScript -Process $p } catch { Stop-Process -Id $p.Id -Force; throw }
}

if ($ClickAt) {
    # App without UI Automation support (PulseView): click the Run button position.
    [void][RcWin]::SetForegroundWindow($p.MainWindowHandle)
    Start-Sleep -Milliseconds 500
    $r = New-Object RcWin+R
    [void][RcWin]::GetWindowRect($p.MainWindowHandle, [ref]$r)
    [void][RcWin]::SetCursorPos($r.L + $ClickAt[0], $r.T + $ClickAt[1])
    Start-Sleep -Milliseconds 200
    [RcWin]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [RcWin]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
} else {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($p.MainWindowHandle)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, 'Run')
    $btn = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if (-not $btn) { Stop-Process -Id $p.Id -Force; throw 'Run button not found' }
    $ip = $null
    [void]$btn.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$ip)
    $ip.Invoke()
}
$runAt = Get-Date
Write-Output "Run pressed at $($runAt.ToString('HH:mm:ss'))"

# Wait until the session stops or the wait time runs out.
do {
    Start-Sleep -Seconds 1
    $stopped = Select-String -LiteralPath $err -Pattern 'session: Stopped' -Quiet
} until ($stopped -or ((Get-Date) - $runAt).TotalSeconds -ge $WaitSeconds)
Write-Output ("Session stopped: {0} after {1:N1} s" -f $stopped, ((Get-Date) - $runAt).TotalSeconds)

# Screenshot.
[void][RcWin]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 700
$r = New-Object RcWin+R
[void][RcWin]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap ($r.Rt - $r.L), ($r.B - $r.T)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$bmp.Save((Join-Path $OutDir "$Tag.png"))

Stop-Process -Id $p.Id -Force
Start-Sleep -Milliseconds 500

# Summary.
$lines = Get-Content -LiteralPath $err
$rx = $lines | Select-String 'receive_transfer\(\): status (\S+)(?: / (\S+))? received (\d+) bytes'
$status = $rx | ForEach-Object { if ($_.Matches[0].Groups[2].Success) { $_.Matches[0].Groups[2].Value } else { $_.Matches[0].Groups[1].Value } }
$bytes = ($rx | ForEach-Object { [int]$_.Matches[0].Groups[3].Value } | Measure-Object -Sum).Sum
Write-Output "Transfer results:"
$status | Group-Object | Sort-Object Count -Descending | ForEach-Object { "  {0,-28} {1}" -f $_.Name, $_.Count }
Write-Output "Bytes received: $bytes (= samples at 8 channels)"
$first = $lines | Select-String 'STALL|TIMED_OUT|CANCELLED|ERROR|rror' | Select-Object -First 1
if ($first) { Write-Output "First problem line: $($first.Line)" }
Write-Output "Start/stop lines:"
$lines | Select-String 'Starting acquisition|Opened device on|session: Stopped|Stopping|clearing halt|samplerate' | Select-Object -First 6 | ForEach-Object { "  $($_.Line)" }
$lines | Select-String 'session: Stopped' | Select-Object -Last 1 | ForEach-Object { "  $($_.Line)" }
if (Test-Path $out) { Get-Content -LiteralPath $out | Select-String 'Acquisition took' | ForEach-Object { "  stdout: $($_.Line)" } }
