# 拖动设置窗口的最小复现脚本（P/Invoke）
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, System.UIntPtr e);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, System.IntPtr p);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h);
[DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern int GetWindowTextW(System.IntPtr h, System.Text.StringBuilder s, int n);
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
public delegate bool EnumProc(System.IntPtr h, System.IntPtr p);
'@

function Get-Windows($procId) {
  $list = New-Object System.Collections.ArrayList
  $cb = [W.U+EnumProc]{ param($h, $p)
    $pid2 = 0
    [void][W.U]::GetWindowThreadProcessId($h, [ref]$pid2)
    if ($pid2 -eq $procId -and [W.U]::IsWindowVisible($h)) {
      $r = New-Object W.U+RECT
      [void][W.U]::GetWindowRect($h, [ref]$r)
      $sb = New-Object System.Text.StringBuilder 256
      [void][W.U]::GetWindowTextW($h, $sb, 256)
      [void]$list.Add([pscustomobject]@{H=$h; L=$r.L; T=$r.T; R=$r.R; B=$r.B; W=($r.R-$r.L); Hh=($r.B-$r.T); Title=$sb.ToString()})
    }
    return $true
  }
  [void][W.U]::EnumWindows($cb, [IntPtr]::Zero)
  return $list
}

$proc = Get-Process RunAny_RS -ErrorAction SilentlyContinue
if (-not $proc) { "没有进程"; exit 1 }
$wins = Get-Windows $proc.Id
"进程 $($proc.Id)，可见窗口："
$wins | ForEach-Object { "  h=$($_.H) rect=($($_.L),$($_.T)) $($_.W)x$($_.Hh) title='$($_.Title)'" }

# 最大那个当设置窗口
$target = $wins | Sort-Object -Property W -Descending | Select-Object -First 1
if (-not $target) { "没有窗口"; exit 1 }
"拖动目标：h=$($target.H) rect=($($target.L),$($target.T)) $($target.W)x$($target.Hh)"

$startX = $target.L + [int]($target.W / 2)
$startY = $target.T + 12
"按下点：($startX, $startY)"
[void][W.U]::SetCursorPos($startX, $startY)
Start-Sleep -Milliseconds 200
[W.U]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)   # LEFTDOWN
Start-Sleep -Milliseconds 150
for ($i = 1; $i -le 12; $i++) {
  [void][W.U]::SetCursorPos($startX + $i * 6, $startY + $i * 2)
  Start-Sleep -Milliseconds 60
}
Start-Sleep -Milliseconds 200
[W.U]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)   # LEFTUP
Start-Sleep -Milliseconds 400

$proc2 = Get-Process RunAny_RS -ErrorAction SilentlyContinue
if ($proc2) { "进程还活着" } else { "进程已经没了（崩了）" }
$wins2 = Get-Windows $proc2.Id
$wins2 | ForEach-Object { "  h=$($_.H) rect=($($_.L),$($_.T)) $($_.W)x$($_.Hh)" }
