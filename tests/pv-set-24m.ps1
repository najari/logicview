# PulseView has no UI Automation tree: open its toolbar combos by clicking the
# arrow positions (window-relative, 1016x759 window) and pick with the keyboard.
# Samples: five steps down from 1 M; Rate: one below the maximum.
param([Parameter(Mandatory)] $Process, [string]$OutDir = (Join-Path $PSScriptRoot 'out'))
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
if (-not ('PvKeys' -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class PvKeys { [StructLayout(LayoutKind.Sequential)] public struct R { public int L,T,Rt,B; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
}
[void][PvKeys]::SetProcessDPIAware()
[void][PvKeys]::SetForegroundWindow($Process.MainWindowHandle)
Start-Sleep -Milliseconds 400
$r = New-Object PvKeys+R
[void][PvKeys]::GetWindowRect($Process.MainWindowHandle, [ref]$r)
function OpenAndKeys($x, $y, [string[]]$keys) {
    [void][PvKeys]::SetCursorPos($r.L + $x, $r.T + $y)
    Start-Sleep -Milliseconds 150
    [PvKeys]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [PvKeys]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
    foreach ($k in $keys + '{ENTER}') { [System.Windows.Forms.SendKeys]::SendWait($k); Start-Sleep -Milliseconds 200 }
    Start-Sleep -Milliseconds 500
}
OpenAndKeys 806 100 @('{END}', '{UP}')
OpenAndKeys 716 100 @('{DOWN}', '{DOWN}', '{DOWN}', '{DOWN}', '{DOWN}')
# Screenshot of the toolbar so the settings can be checked afterwards.
$bmp = New-Object System.Drawing.Bitmap ($r.Rt - $r.L), 130
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$bmp.Save((Join-Path $OutDir 'pv-settings.png'))
Write-Output "Window $($r.Rt - $r.L)x$($r.B - $r.T); settings screenshot saved"
