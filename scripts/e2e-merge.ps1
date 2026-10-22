param([string]$Folder, [string[]]$Select, [int]$FlickDx = 0, [int]$FlickDy = -130,
      [byte[]]$Mods = @(0x11, 0x12), [byte]$Key = 0x20)
# Drives the real gesture: Explorer selection -> hold Ctrl+Alt+Space -> flick -> release.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class U {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public struct RECT { public int L, T, R, B; }
}
"@
$KEYUP = 2
explorer.exe $Folder
$sh = New-Object -ComObject Shell.Application
$url = ([Uri]$Folder).AbsoluteUri
$win = $null
for ($i = 0; $i -lt 40 -and -not $win; $i++) {
    Start-Sleep -Milliseconds 250
    $win = @($sh.Windows()) | Where-Object { $_.LocationURL -eq $url } | Select-Object -First 1
}
if (-not $win) { throw "Explorer window for $Folder not found" }
Start-Sleep -Milliseconds 800
$first = $true
foreach ($name in $Select) {
    $item = $win.Document.Folder.ParseName($name)
    # SVSI_SELECT=1, SVSI_DESELECTOTHERS=4, SVSI_FOCUSED=16
    $win.Document.SelectItem($item, $(if ($first) { 1 + 4 + 16 } else { 1 }))
    $first = $false
}
$hwnd = [IntPtr]$win.HWND
[U]::ShowWindow($hwnd, 9) | Out-Null
# A tap of Alt lets this process move another window to the front.
[U]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [U]::keybd_event(0x12, 0, $KEYUP, [UIntPtr]::Zero)
[U]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 500
"foreground is explorer: $([U]::GetForegroundWindow() -eq $hwnd)"
"selected: " + ((@($win.Document.SelectedItems()) | ForEach-Object Name) -join ', ')

$r = New-Object U+RECT; [U]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$cx = [int](($r.L + $r.R) / 2); $cy = [int](($r.T + $r.B) / 2) + 60
[U]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 150

foreach ($m in $Mods) { [U]::keybd_event($m, 0, 0, [UIntPtr]::Zero) }
[U]::keybd_event($Key, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 350
for ($s = 1; $s -le 10; $s++) {
    [U]::SetCursorPos($cx + [int]($FlickDx * $s / 10), $cy + [int]($FlickDy * $s / 10)) | Out-Null
    Start-Sleep -Milliseconds 25
}
Start-Sleep -Milliseconds 250
[U]::keybd_event($Key, 0, $KEYUP, [UIntPtr]::Zero)
foreach ($m in $Mods) { [U]::keybd_event($m, 0, $KEYUP, [UIntPtr]::Zero) }
"gesture done at $cx,$cy -> +$FlickDx,$FlickDy"
