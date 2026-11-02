# Inspect the built prototype through Windows MSAA; no NVDA claims.
param([string]$ExecutablePath)
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
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, System.Text.StringBuilder text);
    [DllImport("user32.dll")]
    private static extern int GetDlgCtrlID(IntPtr window);
    public static string ChoiceText(IntPtr window, int index) {
        var text = new System.Text.StringBuilder(256);
        SendMessage(window, 0x0148, new IntPtr(index), text); // CB_GETLBTEXT
        return text.ToString();
    }
    public static void SelectChoice(IntPtr window, int index) {
        SendMessage(window, 0x014E, new IntPtr(index), IntPtr.Zero); // CB_SETCURSEL
        SendMessage(GetParent(window), 0x0111,
            new IntPtr((GetDlgCtrlID(window) & 0xFFFF) | (9 << 16)), window); // CBN_SELENDOK
    }
    public static void InvokeButton(IntPtr window) {
        // Dispatch the command, as keyboard activation does. BM_CLICK simulates
        // a mouse click and Windows itself focuses the button before the handler.
        SendMessage(GetParent(window), 0x0111,
            new IntPtr(GetDlgCtrlID(window) & 0xFFFF), window); // BN_CLICKED
    }
    [DllImport("user32.dll")]
    private static extern IntPtr GetParent(IntPtr window);
    [DllImport("user32.dll")]
    private static extern IntPtr GetNextDlgTabItem(IntPtr dialog, IntPtr control, bool previous);
    public static bool NextTabIs(IntPtr from, IntPtr to) {
        return GetNextDlgTabItem(GetParent(from), from, false) == to &&
            GetNextDlgTabItem(GetParent(to), to, true) == from;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct GuiThreadInfo {
        public int Size;
        public int Flags;
        public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public int Left, Top, Right, Bottom;
    }
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")]
    private static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);
    public static IntPtr FocusedWindow(IntPtr window) {
        uint process;
        var thread = GetWindowThreadProcessId(window, out process);
        var info = new GuiThreadInfo { Size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
        if (!GetGUIThreadInfo(thread, ref info)) throw new Exception("Cannot inspect UI thread focus.");
        return info.Focus;
    }
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
            if (classBuffer.ToString() == className) {
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

$appPath = if ($ExecutablePath) {
    $ExecutablePath
} elseif ($env:CARGO_TARGET_DIR) {
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
        @{ Class = 'ComboBox'; Name = 'Categories' },
        @{ Class = 'Edit'; Name = 'Search channels' },
        @{ Class = 'Button'; Name = 'Toggle favorite' }
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
    }
    $cancelHandle = [FoundationAccessible]::FindDescendant($windowHandle, 'Button', 'Cancel loading')
    $categoryHandle = [FoundationAccessible]::FindDescendant($windowHandle, 'ComboBox', 'Categories')
    $searchHandle = [FoundationAccessible]::FindDescendant($windowHandle, 'Edit', 'Search channels')
    $favoriteHandle = [FoundationAccessible]::FindDescendant($windowHandle, 'Button', 'Toggle favorite')
    if (-not [FoundationAccessible]::NextTabIs($cancelHandle, $categoryHandle) -or
        -not [FoundationAccessible]::NextTabIs($categoryHandle, $searchHandle) -or
        -not [FoundationAccessible]::NextTabIs($searchHandle, $listHandle) -or
        -not [FoundationAccessible]::NextTabIs($listHandle, $favoriteHandle)) {
        throw 'Native tab order must be Cancel loading, Categories, Search channels, Channels, Toggle favorite.'
    }
    $category = [FoundationAccessible]::Read($categoryHandle)
    if ($category.Value -ne 'All Channels') { throw "Unexpected initial category: $($category.Value)" }
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
    if (-not [FoundationAccessible]::NextTabIs($favoriteHandle, $statusHandle)) {
        throw 'Toggle favorite must precede Playlist status in native tab order.'
    }
    if ([FoundationAccessible]::ChoiceText($categoryHandle, 1) -ne 'Favorites') {
        throw 'Stable Favorites category is missing or incorrectly named.'
    }
    $initialFocus = [FoundationAccessible]::FocusedWindow($windowHandle)
    [FoundationAccessible]::SelectChoice($categoryHandle, 1)
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $status = [FoundationAccessible]::Read($statusHandle)
        if ($status.Value -like '*Favorites is empty*') { break }
        Start-Sleep -Milliseconds 50
    }
    if ($status.Value -notlike '*Favorites is empty*' -or
        [FoundationAccessible]::Read($listHandle).Rows -ne 0 -or
        [FoundationAccessible]::Read($categoryHandle).Value -ne 'Favorites') {
        throw "Empty Favorites must expose zero native rows and explicit status. Status=$($status.Value); Rows=$([FoundationAccessible]::Read($listHandle).Rows); Category=$([FoundationAccessible]::Read($categoryHandle).Value)"
    }
    $categoryFocus = [FoundationAccessible]::FocusedWindow($windowHandle)
    [FoundationAccessible]::InvokeButton($favoriteHandle)
    $status = [FoundationAccessible]::Read($statusHandle)
    if ($status.Value -ne 'Select a channel first. Favorites unchanged.') {
        throw "Empty toggle action has incorrect feedback: $($status.Value)"
    }
    if ([FoundationAccessible]::FocusedWindow($windowHandle) -ne $initialFocus) {
        throw "Native focus changed: initial=$initialFocus, after category=$categoryFocus, after button=$([FoundationAccessible]::FocusedWindow($windowHandle)); list=$listHandle, category=$categoryHandle, button=$favoriteHandle."
    }
    Write-Output 'PASS: MSAA names/roles; zero native rows; Favorites category selection and empty status; focusable Toggle favorite, forward/reverse native Channels -> Toggle favorite -> Playlist status tab order; empty toggle command feedback; native keyboard focus unchanged. Native command inspection only; owner-heard NVDA/physical keyboard validation remains open.'

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
