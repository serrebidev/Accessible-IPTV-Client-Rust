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
    public string Value;
    [DllImport("user32.dll")]
    private static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, System.Text.StringBuilder name, int length);
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
                Value = accessible.get_accValue(0),
                Role = Convert.ToInt32(accessible.get_accRole(0)),
                State = Convert.ToInt32(accessible.get_accState(0)),
                Children = accessible.accChildCount,
                Rows = SendMessage(window, 0x1004, IntPtr.Zero, IntPtr.Zero).ToInt32(), // LVM_GETITEMCOUNT
                HasHeader = SendMessage(window, 0x101F, IntPtr.Zero, IntPtr.Zero) != IntPtr.Zero // LVM_GETHEADER
            };
        } finally { Marshal.ReleaseComObject(accessible); }
    }

    public static IntPtr FindDescendant(IntPtr root, string className, string name) {
        var child = GetWindow(root, 5); // GW_CHILD
        while (child != IntPtr.Zero) {
            var classBuffer = new System.Text.StringBuilder(128);
            GetClassName(child, classBuffer, classBuffer.Capacity);
            if (className == "*" || classBuffer.ToString() == className) {
                try {
                    if (Read(child).Name == name) return child;
                } catch { }
            }
            var nested = FindDescendant(child, className, name);
            if (nested != IntPtr.Zero) return nested;
            child = GetWindow(child, 2); // GW_HWNDNEXT
        }
        return IntPtr.Zero;
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
    $windowHandle = $app.MainWindowHandle
    $listHandle = [FoundationAccessible]::FindDescendant($windowHandle, 'SysListView32', 'Channels')
    if ($listHandle -eq [IntPtr]::Zero) { throw 'Native virtual list not found.' }
    $accessible = [FoundationAccessible]::Read($listHandle)
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
    foreach ($controlSpec in @(
        @{ Class = 'Button'; Name = 'Open playlist' },
        @{ Class = 'Button'; Name = 'Cancel loading' },
        @{ Class = '*'; Name = 'Category' },
        @{ Class = 'Button'; Name = 'Toggle favorite' },
        @{ Class = 'Edit'; Name = 'Search channels' }
    )) {
        $controlHandle = [FoundationAccessible]::FindDescendant(
            $windowHandle, $controlSpec.Class, $controlSpec.Name)
        if ($controlHandle -eq [IntPtr]::Zero) {
            throw "Missing native control: $($controlSpec.Name)"
        }
        $control = [FoundationAccessible]::Read($controlHandle)
        if (-not ($control.State -band 0x100000)) {
            throw "Missing or non-focusable control: $($controlSpec.Name)"
        }
        if ($controlSpec.Name -eq 'Category' -and $control.Role -ne 0x21) {
            throw "Unexpected Category role: $($control.Role)"
        }
    }
    $statusHandle = [FoundationAccessible]::FindDescendant(
        $windowHandle, 'Edit', 'Playlist status')
    if ($statusHandle -eq [IntPtr]::Zero) {
        throw 'Focusable Playlist status field is missing from the accessibility tree.'
    }
    $status = [FoundationAccessible]::Read($statusHandle)
    if (-not ($status.State -band 0x100000) -or
        -not ($status.State -band 0x40) -or
        $status.Value -ne 'No playlist loaded. Channel list is empty.') {
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
