# Captures only an agent-profile app's verified client rectangle during lock transitions.
# Requires a Windows agent build running with an explicit synthetic profile and CDP port.
# Unlock the app before a return scenario; StallRendererMs blocks its renderer before departure.
# ComposedClient skips obscured/background frames. PrintWindow is diagnostic only and can omit covers.
# UseThreadInputAttachment is diagnostic only: sharing input queues can suppress activation events.
# UseTitleBarInput clicks only the verified, unobscured synthetic window title bars.
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][int]$AppProcessId,
    [Parameter(Mandatory)][ValidatePattern('^agent-[a-z0-9-]{1,24}$')][string]$ProfileName,
    [Parameter(Mandatory)][int]$CdpPort,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateSet('ComposedClient', 'PrintWindow')][string]$CaptureMode = 'ComposedClient',
    [ValidateSet('capture', 'return', 'restore')][string]$Scenario = 'return',
    [ValidateRange(100, 15000)][int]$DurationMs = 3000,
    [ValidateRange(10, 1000)][int]$IntervalMs = 33,
    [ValidateRange(0, 10000)][int]$StallRendererMs = 0,
    [ValidateRange(0, 10000)][int]$ReturnAfterMs = 400,
    [switch]$UseThreadInputAttachment,
    [switch]$UseTitleBarInput,
    [ValidatePattern('^\d{6}$')][string]$UnlockPin
)
$ErrorActionPreference = 'Stop'
if ($UseThreadInputAttachment -and $UseTitleBarInput) { throw 'Choose one activation method.' }
$resolvedExe = (Resolve-Path -LiteralPath $Exe).Path
$process = Get-CimInstance Win32_Process -Filter "ProcessId=$AppProcessId"
if (-not $process -or $process.ExecutablePath -ne $resolvedExe -or
    $process.CommandLine -notmatch ('--profile=' + [regex]::Escape($ProfileName) + '(?:\s|$|\")')) {
    throw 'The target process must be the explicit executable running the named synthetic profile.'
}
$appProcess = Get-Process -Id $AppProcessId
$window = $appProcess.MainWindowHandle
if ($window -eq [IntPtr]::Zero) { throw 'The synthetic app has no main window.' }
$listeners = @(Get-NetTCPConnection -LocalPort $CdpPort -State Listen -ErrorAction Stop | Select-Object -ExpandProperty OwningProcess -Unique)
if ($listeners.Count -ne 1) { throw 'Expected one CDP listener owned by the synthetic app process tree.' }
$listenerProcess = [int]$listeners[0]
$listenerOwned = $false
for ($depth = 0; $depth -lt 32 -and $listenerProcess -gt 0; $depth++) {
    if ($listenerProcess -eq $AppProcessId) { $listenerOwned = $true; break }
    $ancestor = Get-CimInstance Win32_Process -Filter "ProcessId=$listenerProcess"
    if (-not $ancestor) { break }
    $listenerProcess = [int]$ancestor.ParentProcessId
}
if (-not $listenerOwned) { throw 'The CDP listener does not belong to the verified synthetic app.' }
$pages = Invoke-RestMethod -Uri "http://127.0.0.1:$CdpPort/json/list"
$page = @($pages | Where-Object { $_.type -eq 'page' -and $_.url -match '^https?://(tauri\.localhost|localhost|127\.0\.0\.1)(:|/)' })
if ($page.Count -ne 1) { throw 'Expected exactly one local app page on the explicit CDP port.' }
$socket = [System.Net.WebSockets.ClientWebSocket]::new()
$deadline = [System.Threading.CancellationTokenSource]::new(20000)
$null = $socket.ConnectAsync([Uri]$page[0].webSocketDebuggerUrl, $deadline.Token).GetAwaiter().GetResult()

function Send-CdpMessage([int]$MessageId, [string]$Method, [hashtable]$Parameters) {
    $payload = @{ id = $MessageId; method = $Method; params = $Parameters } | ConvertTo-Json -Depth 6 -Compress
    $bytes = [Text.Encoding]::UTF8.GetBytes($payload)
    $null = $socket.SendAsync([ArraySegment[byte]]::new($bytes), [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $deadline.Token).GetAwaiter().GetResult()
}

function Send-Cdp([int]$MessageId, [string]$Expression) {
    Send-CdpMessage $MessageId 'Runtime.evaluate' @{ expression = $Expression; awaitPromise = $true; returnByValue = $true }
}

function Read-Cdp([int]$MessageId, [string]$BindingName = '') {
    while ($true) {
        $buffer = [byte[]]::new(65536)
        $message = [IO.MemoryStream]::new()
        do {
            $received = $socket.ReceiveAsync([ArraySegment[byte]]::new($buffer), $deadline.Token).GetAwaiter().GetResult()
            $message.Write($buffer, 0, $received.Count)
        } while (-not $received.EndOfMessage)
        $response = [Text.Encoding]::UTF8.GetString($message.ToArray()) | ConvertFrom-Json
        $message.Dispose()
        if ($BindingName) {
            if ($response.method -eq 'Runtime.bindingCalled' -and $response.params.name -eq $BindingName) { return $response }
        } elseif ($response.id -eq $MessageId) { return $response }
    }
}

Send-Cdp 1 'window.__TAURI_INTERNALS__.invoke("app_paths_roots")'
$proof = Read-Cdp 1
if ($proof.error -or $proof.result.exceptionDetails) { throw 'Could not verify the native synthetic data root.' }
$expected = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) "RisuNestProfiles-agent/$ProfileName/data"))
$actual = [IO.Path]::GetFullPath([string]$proof.result.result.value.data)
if ($actual -ne $expected) { throw 'The native data root does not match the synthetic profile. No capture was made.' }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'The capture output directory must not already exist.' }
$output = (New-Item -ItemType Directory -Path $OutputDirectory).FullName

$drawingReferences = if ($PSVersionTable.PSEdition -eq 'Core') {
    @('System.Drawing.Common', 'System.Drawing.Primitives', 'System.Collections', 'System.Threading', 'System.Threading.Thread', 'System.Runtime', 'System.ComponentModel.Primitives') +
        @([Drawing.Bitmap].Assembly.GetReferencedAssemblies() | Where-Object { $_.Name -like 'System.Private.Windows.*' } | ForEach-Object Name)
} else { @('System.Drawing') }
Add-Type -ReferencedAssemblies $drawingReferences -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Threading;

public static class RisuNestLockFrames {
    public static bool AttachInput = false;
    public static bool TitleBarInput = false;
    public static int AppProcessId;
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int left, top, right, bottom; }
    [StructLayout(LayoutKind.Sequential)] struct Message { public IntPtr hwnd; public uint message; public UIntPtr wparam; public IntPtr lparam; public uint time; public int x, y; public uint privateData; }
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern short GetAsyncKeyState(int key);
    [StructLayout(LayoutKind.Sequential)] struct MouseInput { public int x, y; public uint data, flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct Input { public uint type; public MouseInput mouse; }
    [DllImport("user32.dll")] static extern uint SendInput(uint count, Input[] input, int size);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll")] static extern uint GetCurrentProcessId();
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint attach, uint attachTo, bool value);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetWindowLongPtrW(IntPtr hwnd, int index);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string klass, string title);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint command);
    [StructLayout(LayoutKind.Sequential)] struct Point { public int x, y; }
    delegate bool WindowCallback(IntPtr hwnd, IntPtr value);
    [DllImport("user32.dll")] static extern bool EnumWindows(WindowCallback callback, IntPtr value);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr destination, int x, int y, int width, int height, IntPtr source, int sourceX, int sourceY, uint operation);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, uint attribute, out uint value, int size);
    [DllImport("dwmapi.dll")] static extern int DwmFlush();
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr CreateWindowEx(uint extended, string klass, string title, uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool PeekMessage(out Message message, IntPtr hwnd, uint min, uint max, uint remove);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref Message message);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref Message message);
    public sealed class Frame { public string file; public long atMs; public bool foreground, coverExists, coverVisible, coverTop; public int width, height; }
    static void Pump() { Message message; while (PeekMessage(out message, IntPtr.Zero, 0, 0, 1)) { TranslateMessage(ref message); DispatchMessage(ref message); } }
    static void Foreground(IntPtr hwnd) {
        if (TitleBarInput) { ClickTitleBar(hwnd); return; }
        uint unused;
        var foregroundThread = GetWindowThreadProcessId(GetForegroundWindow(), out unused);
        var currentThread = GetCurrentThreadId();
        var attached = AttachInput && foregroundThread != 0 && foregroundThread != currentThread && AttachThreadInput(currentThread, foregroundThread, true);
        try { BringWindowToTop(hwnd); SetForegroundWindow(hwnd); }
        finally { if (attached) AttachThreadInput(currentThread, foregroundThread, false); }
    }
    static void ClickTitleBar(IntPtr hwnd) {
        uint owner;
        GetWindowThreadProcessId(hwnd, out owner);
        if (owner != AppProcessId && owner != GetCurrentProcessId()) throw new InvalidOperationException("Activation target is not an owned synthetic window.");
        if (GetAsyncKeyState(1) < 0) throw new InvalidOperationException("Mouse input is already active.");
        var wasTopmost = (GetWindowLongPtrW(hwnd, -20).ToInt64() & 8) != 0;
        if (!wasTopmost && !SetWindowPos(hwnd, new IntPtr(-1), 0, 0, 0, 0, 19)) throw new InvalidOperationException("Cannot expose the synthetic title bar without activation.");
        try {
        Rect bounds;
        var client = new Point();
        if (!GetWindowRect(hwnd, out bounds) || !ClientToScreen(hwnd, ref client) || client.y - bounds.top < 8) throw new InvalidOperationException("A verified title bar is unavailable.");
        var point = new Point { x = (bounds.left + bounds.right) / 2, y = (bounds.top + client.y) / 2 };
        if (GetAncestor(WindowFromPoint(point), 2) != hwnd || GetAsyncKeyState(1) < 0) throw new InvalidOperationException("The title bar is obscured or mouse input is already active.");
        Point previous;
        if (!GetCursorPos(out previous) || !SetCursorPos(point.x, point.y)) throw new InvalidOperationException("Cannot position the pointer on the owned title bar.");
        try {
            Rect current;
            uint currentOwner;
            GetWindowThreadProcessId(hwnd, out currentOwner);
            if (currentOwner != owner || !GetWindowRect(hwnd, out current) || current.left != bounds.left || current.top != bounds.top || current.right != bounds.right || current.bottom != bounds.bottom || GetAncestor(WindowFromPoint(point), 2) != hwnd) throw new InvalidOperationException("The activation target moved or became obscured.");
            var input = new[] { new Input { mouse = new MouseInput { flags = 2 } }, new Input { mouse = new MouseInput { flags = 4 } } };
            if (SendInput(2, input, Marshal.SizeOf(typeof(Input))) != 2) throw new InvalidOperationException("The synthetic title-bar click was not accepted.");
            var activation = Stopwatch.StartNew();
            while (GetForegroundWindow() != hwnd && activation.ElapsedMilliseconds < 1000) { Pump(); Thread.Sleep(1); }
            if (GetForegroundWindow() != hwnd) throw new InvalidOperationException("The clicked synthetic window did not become foreground.");
        } finally {
            Point cursor;
            if (GetCursorPos(out cursor) && cursor.x == point.x && cursor.y == point.y) SetCursorPos(previous.x, previous.y);
        }
        } finally { if (!wasTopmost) SetWindowPos(hwnd, new IntPtr(-2), 0, 0, 0, 0, 19); }
    }
    public static void VerifyUnlockWindow(IntPtr hwnd) {
        var deadline = Stopwatch.StartNew();
        while (true) {
            if (!UnoccludedForeground(hwnd)) throw new InvalidOperationException("The synthetic app lost unobscured foreground before PIN verification.");
            var cover = FindWindowEx(hwnd, IntPtr.Zero, "RisuNestLockCover", null);
            if (cover == IntPtr.Zero || !IsWindowVisible(cover)) return;
            if (deadline.ElapsedMilliseconds >= 5000) throw new InvalidOperationException("The native cover did not hand over to the PIN screen.");
            Pump(); Thread.Sleep(10);
        }
    }
    static bool UnoccludedForeground(IntPtr hwnd) {
        if (GetForegroundWindow() != hwnd) return false;
        Rect bounds;
        var origin = new Point();
        if (!GetClientRect(hwnd, out bounds) || !ClientToScreen(hwnd, ref origin)) return false;
        bounds.left += origin.x; bounds.right += origin.x; bounds.top += origin.y; bounds.bottom += origin.y;
        bool reached = false, blocked = false;
        EnumWindows((other, value) => {
            if (other == hwnd) { reached = true; return false; }
            if (!IsWindowVisible(other)) return true;
            uint cloaked;
            if (DwmGetWindowAttribute(other, 14, out cloaked, 4) == 0 && cloaked != 0) return true;
            Rect rect;
            if (GetWindowRect(other, out rect) && rect.right > rect.left && rect.bottom > rect.top &&
                rect.left < bounds.right && rect.right > bounds.left && rect.top < bounds.bottom && rect.bottom > bounds.top) {
                blocked = true; return false;
            }
            return true;
        }, IntPtr.Zero);
        return reached && !blocked;
    }
    public static IntPtr Depart() {
        var focus = CreateWindowEx(0, "STATIC", "RisuNest synthetic focus target", 0x10C00000, 0, 0, 300, 80, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (focus == IntPtr.Zero) throw new InvalidOperationException("Cannot create the synthetic focus target.");
        ShowWindow(focus, 5);
        Foreground(focus);
        Pump();
        if (GetForegroundWindow() != focus) { DestroyWindow(focus); throw new InvalidOperationException("The synthetic focus target did not become active."); }
        if (DwmFlush() < 0) { DestroyWindow(focus); throw new InvalidOperationException("The synthetic departure was not presented."); }
        return focus;
    }
    public static void CloseFocus(IntPtr focus) { if (focus != IntPtr.Zero) DestroyWindow(focus); }
    public static void Minimize(IntPtr hwnd) { ShowWindow(hwnd, 6); }
    public static Frame[] Capture(IntPtr hwnd, int processId, string output, int duration, int interval, int returnAt, bool switchBack, bool composedClient) {
        var previousDpi = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
        var timer = Stopwatch.StartNew();
        var frames = new List<Frame>();
        bool returned = false;
        while (timer.ElapsedMilliseconds < duration) {
            Pump();
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != processId) throw new InvalidOperationException("The captured HWND no longer belongs to the verified process.");
            if (switchBack && !returned && timer.ElapsedMilliseconds >= returnAt) {
                ShowWindow(hwnd, 9);
                Foreground(hwnd);
                returned = true;
            }
            if (composedClient && !UnoccludedForeground(hwnd)) { Thread.Sleep(interval); continue; }
            if (composedClient && DwmFlush() < 0) throw new InvalidOperationException("The display compositor did not complete a presentation.");
            if (composedClient && !UnoccludedForeground(hwnd)) { Thread.Sleep(interval); continue; }
            Rect bounds;
            if (!GetClientRect(hwnd, out bounds) || bounds.right <= 0 || bounds.bottom <= 0 || bounds.right > 4096 || bounds.bottom > 4096) throw new InvalidOperationException("Invalid capture dimensions.");
            var file = String.Format("frame-{0:D5}.png", frames.Count);
            var at = timer.ElapsedMilliseconds;
            using (var bitmap = new Bitmap(bounds.right, bounds.bottom, PixelFormat.Format32bppArgb)) {
                using (var graphics = Graphics.FromImage(bitmap)) {
                    var dc = graphics.GetHdc();
                    try {
                        if (composedClient) {
                            var origin = new Point();
                            if (!ClientToScreen(hwnd, ref origin)) throw new InvalidOperationException("The verified client rectangle has no display coordinates.");
                            var source = GetDC(IntPtr.Zero);
                            if (source == IntPtr.Zero) throw new InvalidOperationException("The composed display DC is unavailable.");
                            try { if (!BitBlt(dc, 0, 0, bounds.right, bounds.bottom, source, origin.x, origin.y, 0x40CC0020)) throw new InvalidOperationException("The verified client rectangle could not be captured."); }
                            finally { ReleaseDC(IntPtr.Zero, source); }
                        } else if (!PrintWindow(hwnd, dc, 3)) throw new InvalidOperationException("The target window rejected PrintWindow.");
                    }
                    finally { graphics.ReleaseHdc(dc); }
                }
                if (composedClient && !UnoccludedForeground(hwnd)) continue;
                bitmap.Save(System.IO.Path.Combine(output, file), ImageFormat.Png);
            }
            var cover = FindWindowEx(hwnd, IntPtr.Zero, "RisuNestLockCover", null);
            frames.Add(new Frame { file = file, atMs = at, foreground = GetForegroundWindow() == hwnd, width = bounds.right, height = bounds.bottom,
                coverExists = cover != IntPtr.Zero, coverVisible = cover != IntPtr.Zero && IsWindowVisible(cover), coverTop = cover != IntPtr.Zero && GetWindow(cover, 3) == IntPtr.Zero });
            var wait = interval - (int)(timer.ElapsedMilliseconds - at);
            if (wait > 0) Thread.Sleep(wait);
        }
        if (switchBack && (!returned || GetForegroundWindow() != hwnd)) throw new InvalidOperationException("The app did not regain foreground focus.");
        if (frames.Count == 0) throw new InvalidOperationException("No unobscured synthetic app frames were captured.");
        return frames.ToArray();
        } finally { if (previousDpi != IntPtr.Zero) SetThreadDpiAwarenessContext(previousDpi); }
    }
}
'@

$focus = [IntPtr]::Zero
[RisuNestLockFrames]::AttachInput = $UseThreadInputAttachment.IsPresent
[RisuNestLockFrames]::TitleBarInput = $UseTitleBarInput.IsPresent
[RisuNestLockFrames]::AppProcessId = $AppProcessId
try {
    if ($StallRendererMs -gt 0) {
        Send-CdpMessage 2 'Runtime.enable' @{}
        $null = Read-Cdp 2
        Send-CdpMessage 3 'Runtime.addBinding' @{ name = '__risuNestFrameStallStarted' }
        $null = Read-Cdp 3
        Send-Cdp 4 "(() => { window.__risuNestFrameStallStarted('started'); const until = performance.now() + $StallRendererMs; while (performance.now() < until) {} return true })()"
        $null = Read-Cdp 0 '__risuNestFrameStallStarted'
    }
    if ($Scenario -eq 'return') { $focus = [RisuNestLockFrames]::Depart() }
    if ($Scenario -eq 'restore') { [RisuNestLockFrames]::Minimize($window) }
    $frames = [RisuNestLockFrames]::Capture($window, $AppProcessId, $output, $DurationMs, $IntervalMs, $ReturnAfterMs, $Scenario -ne 'capture', $CaptureMode -eq 'ComposedClient')
    $unlocked = $false
    if ($UnlockPin) {
        [RisuNestLockFrames]::VerifyUnlockWindow($window)
        $digits = $UnlockPin | ConvertTo-Json -Compress
        Send-Cdp 10 "(async () => { for (const digit of $digits) { const key = [...document.querySelectorAll('.app-lock-screen .keypad button')].find(button => button.textContent.trim() === digit); if (!key || key.disabled) throw new Error('PIN input is unavailable'); key.click(); await new Promise(resolve => requestAnimationFrame(resolve)); } const deadline = performance.now() + 5000; while (document.querySelector('.app-lock-screen,.app-lock-cover')) { if (performance.now() >= deadline) throw new Error('PIN did not unlock'); await new Promise(resolve => setTimeout(resolve, 25)); } return true; })()"
        $result = Read-Cdp 10
        [RisuNestLockFrames]::VerifyUnlockWindow($window)
        if ($result.error -or $result.result.exceptionDetails -or $result.result.result.value -ne $true) { throw 'The foreground PIN screen did not unlock.' }
        $unlocked = $true
    }
    $limitation = if ($CaptureMode -eq 'ComposedClient') { 'Copies only the verified foreground HWND client rectangle from the composed display, with pre/post occlusion checks; no desktop-wide image is created and samples do not capture every refresh.' } else { 'PrintWindow may omit custom child controls; it does not prove monitor presentation or capture every display refresh.' }
    @{ profile = $ProfileName; processId = $AppProcessId; executableSha256 = (Get-FileHash -LiteralPath $resolvedExe).Hash; nativeRootVerified = $true; scenario = $Scenario; stallRendererMs = $StallRendererMs; stallBeforeDepartureConfirmed = $StallRendererMs -gt 0; activation = $(if ($UseTitleBarInput) { 'guarded-title-bar-input' } elseif ($UseThreadInputAttachment) { 'attached-diagnostic' } else { 'SetForegroundWindow' }); unlockVerified = $unlocked; capture = $CaptureMode; limitation = $limitation; frames = $frames } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output 'frames.json') -Encoding utf8
    Write-Output "Captured $($frames.Count) verified synthetic-window frames in $output"
} finally {
    [RisuNestLockFrames]::CloseFocus($focus)
    $socket.Abort()
    $socket.Dispose()
    $deadline.Dispose()
}
