# Set SLogicView's toolbar to Rate 24 MHz (one below the 48 MHz maximum) and
# Samples 50 M (five steps down from the 1 M default). Throws if it did not stick.
param([Parameter(Mandatory)] $Process)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
if (-not ('ComboKeys' -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class ComboKeys {
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
}
[void][ComboKeys]::SetProcessDPIAware()
$A = [System.Windows.Automation.AutomationElement]
[void][ComboKeys]::SetForegroundWindow($Process.MainWindowHandle)
Start-Sleep -Milliseconds 300

function Combo($i) {
    $root = $A::FromHandle($Process.MainWindowHandle)
    $cond = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)
    @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond))[$i]
}
function Val($i) { (Combo $i).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value.Trim() }
function OpenAndKeys($i, [string[]]$keys) {
    $b = (Combo $i).Current.BoundingRectangle
    [void][ComboKeys]::SetCursorPos([int]($b.X + $b.Width - 12), [int]($b.Y + $b.Height / 2))
    Start-Sleep -Milliseconds 150
    [ComboKeys]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [ComboKeys]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
    foreach ($k in $keys + '{ENTER}') { [System.Windows.Forms.SendKeys]::SendWait($k); Start-Sleep -Milliseconds 200 }
    Start-Sleep -Milliseconds 500
}
OpenAndKeys 1 @('{END}', '{UP}')
OpenAndKeys 0 @('{DOWN}', '{DOWN}', '{DOWN}', '{DOWN}', '{DOWN}')
$rate = Val 1; $samples = Val 0
Write-Output "Settings: Samples=$samples Rate=$rate"
if ($rate -ne '24 MHz' -or $samples -ne '50 M') { throw "Settings did not stick" }
