# Inspect the built prototype through Windows MSAA; no NVDA claims.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName Accessibility
Add-Type -ReferencedAssemblies ([Accessibility.IAccessible].Assembly.Location) -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Accessibility;
public class FoundationAccessible {
    public string Name;
    public string Description;
    public int Role;
    public int State;
    public int Children;
    public int Rows;
    public bool HasHeader;
    [DllImport("user32.dll")]
    private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("oleacc.dll")]
    private static extern int AccessibleObjectFromWindow(IntPtr window, uint id,
        ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out IAccessible accessible);
    public static FoundationAccessible Read(IntPtr window) {
        var iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71");
        IAccessible accessible;
        Marshal.ThrowExceptionForHR(AccessibleObjectFromWindow(window, 0xFFFFFFFC, ref iid, out accessible));
        try {
            return new FoundationAccessible {
                Name = accessible.get_accName(0),
                Description = accessible.get_accDescription(0),
                Role = Convert.ToInt32(accessible.get_accRole(0)),
                State = Convert.ToInt32(accessible.get_accState(0)),
                Children = accessible.accChildCount,
                Rows = SendMessage(window, 0x1004, IntPtr.Zero, IntPtr.Zero).ToInt32(), // LVM_GETITEMCOUNT
                HasHeader = SendMessage(window, 0x101F, IntPtr.Zero, IntPtr.Zero) != IntPtr.Zero // LVM_GETHEADER
            };
        } finally { Marshal.ReleaseComObject(accessible); }
    }
}
'@

$appPath = if ($env:CARGO_TARGET_DIR) {
    Join-Path $env:CARGO_TARGET_DIR 'debug\iptv-app.exe'
} else {
    Join-Path $PSScriptRoot '..\target\debug\iptv-app.exe'
}
$startInfo = [System.Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $appPath).Path)
$startInfo.UseShellExecute = $false
# Hide only the console; WindowStyle Hidden also hides wx's first GUI window.
$startInfo.CreateNoWindow = $true
$app = [System.Diagnostics.Process]::Start($startInfo)
$cleanupFailed = $false
try {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $app.Refresh()
        if ($app.HasExited -or $app.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($app.MainWindowHandle -eq [IntPtr]::Zero) {
        throw 'Prototype has no native window.'
    }
    $window = [System.Windows.Automation.AutomationElement]::FromHandle($app.MainWindowHandle)
    $list = $window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ClassNameProperty, 'SysListView32'))
    if ($null -eq $list) { throw 'Native virtual list not found.' }
    $accessible = [FoundationAccessible]::Read([IntPtr]$list.Current.NativeWindowHandle)
    if ($accessible.Name -ne 'Channels') { throw "Unexpected MSAA list name: $($accessible.Name)" }
    if ($accessible.Role -ne 0x21) { # ROLE_SYSTEM_LIST
        throw "Unexpected MSAA list role: $($accessible.Role)"
    }
    if (-not ($accessible.State -band 0x100000)) { throw 'Channel list is not keyboard focusable.' }
    if ($accessible.Rows -ne 0) { throw "Empty list has $($accessible.Rows) native channel rows." }
    # A report list includes its header in MSAA child count even with LC_NO_HEADER.
    if ($accessible.Children -ne [int]$accessible.HasHeader) {
        throw "Unexpected MSAA children in empty list: $($accessible.Children), header=$($accessible.HasHeader)."
    }
    if ($accessible.Description -ne 'No playlist loaded. Channel list is empty.') {
        throw "Unexpected MSAA empty-state description: $($accessible.Description)"
    }
    foreach ($name in @('Open playlist', 'Cancel loading', 'Search channels')) {
        $control = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty, $name))
        if ($null -eq $control -or -not $control.Current.IsKeyboardFocusable) {
            throw "Missing or non-focusable control: $name"
        }
    }
    $status = $window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.AndCondition]::new(
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty, 'Playlist status'),
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::Edit)))
    if ($null -eq $status -or -not $status.Current.IsKeyboardFocusable) {
        throw 'Focusable Playlist status field is missing from the accessibility tree.'
    }
    $value = $status.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if (-not $value.Current.IsReadOnly -or $value.Current.Value -ne 'No playlist loaded. Channel list is empty.') {
        throw 'Playlist status is not read-only or has incorrect initial empty guidance.'
    }
    Write-Output "PASS: MSAA Channels, native list role, zero items; named focusable actions/search; focusable read-only empty status. HasKeyboardFocus=$([bool]($accessible.State -band 4)). Announcements and traversal require separate validation."

} finally {
    if (-not $app.HasExited) {
        if (-not $app.CloseMainWindow() -or -not $app.WaitForExit(10000)) {
            $cleanupFailed = $true
            Write-Warning "Prototype did not close normally; inspect process $($app.Id)."
        }
    }
}
if ($cleanupFailed) { throw 'Native smoke cleanup failed.' }
if ($app.ExitCode -ne 0) { throw "Prototype exited with code $($app.ExitCode)." }
