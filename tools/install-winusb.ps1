# Install the WinUSB driver for the analyzer (default VID 0925 / PID 3881) by
# driving Zadig's window: pick the device, check that the target driver is
# WinUSB, press Install and wait until Windows reports the WinUSB service.
# Run it elevated (Zadig runs elevated, and a non-elevated process cannot
# send messages to its window), e.g.:
#   Start-Process pwsh -Verb RunAs -Wait -ArgumentList '-File', '<this file>',
#       '-ZadigExe', '<zadig.exe>', '-Log', '<log file>'
param(
    [Parameter(Mandatory)][string]$ZadigExe,
    [Parameter(Mandatory)][string]$Log,
    [string]$Vid = '0925',
    [string]$PidHex = '3881'
)
$ErrorActionPreference = 'Stop'
function L([string]$m) { $line = "$(Get-Date -Format HH:mm:ss) $m"; Add-Content -Path $Log -Value $line -Encoding utf8; Write-Host $line }

Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices; using System.Collections.Generic;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode, EntryPoint="SendMessageW")] public static extern IntPtr SendMessageSB(IntPtr h, uint m, IntPtr w, StringBuilder l);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetDlgCtrlID(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
  public static string Cls(IntPtr h){ var sb=new StringBuilder(256); GetClassName(h,sb,256); return sb.ToString(); }
  public static string Txt(IntPtr h){ var sb=new StringBuilder(2048); SendMessageSB(h, 0x000D, (IntPtr)2048, sb); return sb.ToString(); }
  public static string CbItem(IntPtr h, int i){ int n=(int)SendMessage(h,0x0149,(IntPtr)i,IntPtr.Zero); if(n<0) return ""; var sb=new StringBuilder(n+2); SendMessageSB(h,0x0148,(IntPtr)i,sb); return sb.ToString(); }
  public static List<IntPtr> Children(IntPtr p){ var r=new List<IntPtr>(); EnumChildWindows(p,(h,l)=>{r.Add(h);return true;},IntPtr.Zero); return r; }
  public static List<IntPtr> TopWindows(uint pid){ var r=new List<IntPtr>(); EnumWindows((h,l)=>{uint p; GetWindowThreadProcessId(h,out p); if(p==pid && IsWindowVisible(h)) r.Add(h); return true;},IntPtr.Zero); return r; }
}
"@

function Get-DevService {
    $d = Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -match "VID_$Vid&PID_$PidHex" } | Select-Object -First 1
    if (-not $d) { return $null }
    $svc = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName DEVPKEY_Device_Service).Data
    $pc  = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName DEVPKEY_Device_ProblemCode).Data
    [pscustomobject]@{ Id = $d.InstanceId; Status = $d.Status; Service = $svc; Problem = $pc }
}

function Dump-Children([IntPtr]$h) {
    foreach ($c in [W]::Children($h)) {
        $t = [W]::Txt($c); if ($t.Length -gt 80) { $t = $t.Substring(0,80) + '...' }
        L ("  [{0}] id={1} '{2}'" -f [W]::Cls($c), [W]::GetDlgCtrlID($c), $t)
    }
}

try {
    L "=== WinUSB install for VID_$Vid PID_$PidHex ==="
    L ("Before: " + (Get-DevService | ConvertTo-Json -Compress))

    Get-Process zadig* -ErrorAction SilentlyContinue | ForEach-Object { L "Closing old Zadig pid=$($_.Id) ($($_.Path))"; $_ | Stop-Process -Force }
    Start-Sleep -Milliseconds 800

    $proc = Start-Process -FilePath $ZadigExe -WorkingDirectory (Split-Path $ZadigExe) -PassThru
    L "Started Zadig pid=$($proc.Id)"

    $main = [IntPtr]::Zero
    for ($i = 0; $i -lt 60 -and $main -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 500
        foreach ($w in [W]::TopWindows([uint32]$proc.Id)) {
            if ([W]::Children($w) | Where-Object { [W]::Cls($_) -eq 'ComboBox' }) { $main = $w }
        }
    }
    if ($main -eq [IntPtr]::Zero) { throw "Zadig main window not found" }
    L ("Main window: '" + [W]::Txt($main) + "'")
    Start-Sleep -Seconds 2   # let device enumeration finish

    $combo = [W]::Children($main) | Where-Object { [W]::Cls($_) -eq 'ComboBox' } | Select-Object -First 1
    $comboId = [W]::GetDlgCtrlID($combo); $comboParent = [W]::GetParent($combo)
    $count = [int][W]::SendMessage($combo, 0x0146, [IntPtr]::Zero, [IntPtr]::Zero)
    L "Device list count=$count"

    $found = $false
    for ($i = 0; $i -lt $count; $i++) {
        [void][W]::SendMessage($combo, 0x014E, [IntPtr]$i, [IntPtr]::Zero)                       # CB_SETCURSEL
        $wp = [IntPtr](([int]1 -shl 16) -bor ($comboId -band 0xFFFF))                             # CBN_SELCHANGE
        [void][W]::SendMessage($comboParent, 0x0111, $wp, $combo)                                # WM_COMMAND
        Start-Sleep -Milliseconds 400
        $texts = [W]::Children($main) | ForEach-Object { [W]::Txt($_).Trim() }
        $name = [W]::CbItem($combo, $i)
        $hit = ($texts -contains $Vid) -and ($texts -contains $PidHex)
        L ("  item[$i] '$name'" + $(if ($hit) { '  <== MATCH' } else { '' }))
        if ($hit) { $found = $true; break }
    }
    if (-not $found) { L "Device not found in Zadig list. Controls:"; Dump-Children $main; throw "device not in Zadig list" }

    L "Controls after selecting device:"; Dump-Children $main
    $all = [W]::Children($main)
    $target = $all | Where-Object { [W]::Cls($_) -eq 'Edit' -and [W]::Txt($_) -match '^\s*WinUSB' } | Select-Object -First 1
    if (-not $target) { throw "Target driver is not WinUSB - aborting without installing" }
    L ("Target driver: " + [W]::Txt($target))

    $btn = $all | Where-Object { [W]::Cls($_) -eq 'Button' -and [W]::Txt($_) -match '(Install|Replace|Reinstall|Upgrade) Driver' } | Select-Object -First 1
    if (-not $btn) { throw "Install button not found" }
    L ("Clicking '" + [W]::Txt($btn) + "'")
    $bwp = [IntPtr]([W]::GetDlgCtrlID($btn) -band 0xFFFF)                                       # BN_CLICKED = 0
    [void][W]::PostMessage([W]::GetParent($btn), 0x0111, $bwp, $btn)

    $deadline = (Get-Date).AddMinutes(6); $lastDlg = ''
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        $s = Get-DevService
        if ($s -and $s.Service -match 'WinUSB') { L ("SUCCESS: " + ($s | ConvertTo-Json -Compress)); break }
        if ($proc.HasExited) { L "Zadig exited (code $($proc.ExitCode))"; Start-Sleep -Seconds 3; continue }
        $dlgs = [W]::TopWindows([uint32]$proc.Id) | Where-Object { $_ -ne $main } | ForEach-Object {
            $t = [W]::Txt($_); $body = ([W]::Children($_) | ForEach-Object { [W]::Txt($_) } | Where-Object { $_ }) -join ' | '
            "$t :: $body" }
        $d = ($dlgs -join ' || ')
        if ($d -and $d -ne $lastDlg) { L "Dialog: $d"; $lastDlg = $d }
    }
    L ("Final: " + (Get-DevService | ConvertTo-Json -Compress))
}
catch {
    L "ERROR: $($_.Exception.Message)"
}
L "=== done ==="
Start-Sleep -Seconds 4
