# 第 3+4+5 层端到端验证（尽力而为，UIA 自动化受环境影响，人工右键最可靠）：
# 在资源管理器里对测试文件右键 → 在弹出的新版菜单里找「用 HiEditor Pro 打开」→
# 触发 → 确认 hieditor-pro 进程出现 → 输出 %TEMP%\hieditor_menu.log / hieditor_pro.log 尾部。
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class Native {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr i);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int L, T, R, B; }
}
"@

# 沙箱/自动化环境下 FindWindowExW 可能枚举不到 Explorer 窗口，
# 统一走 EnumWindows + GetClassName/GetWindowText。
# 按窗口标题（= 文件夹名）精确匹配，避免误抓用户已打开的其它文件夹窗口。
function Get-ExplorerHwndByTitle([string]$title) {
    $found = [IntPtr]::Zero
    $cb = [Native+EnumWindowsProc]{
        param($h, $l)
        if ([Native]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 512
            [Native]::GetClassName($h, $sb, 512) | Out-Null
            if ($sb.ToString() -eq 'CabinetWClass') {
                $sb2 = New-Object System.Text.StringBuilder 512
                [Native]::GetWindowText($h, $sb2, 512) | Out-Null
                if ($sb2.ToString() -eq $title) {
                    $script:foundHwnd = $h
                    return $false
                }
            }
        }
        return $true
    }
    $script:foundHwnd = [IntPtr]::Zero
    [Native]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return $script:foundHwnd
}

$testDir = Join-Path $env:TEMP 'hieditor_menu_test'
$testFile = Join-Path $testDir 'menu-test.txt'
$dirName = Split-Path -Leaf $testDir
New-Item -ItemType Directory -Force -Path $testDir | Out-Null
Set-Content -Path $testFile -Value 'HiEditor Pro context menu test file' -Encoding UTF8

[System.Windows.Forms.SendKeys]::SendWait('{ESC}')
$hwnd = Get-ExplorerHwndByTitle $dirName
if ($hwnd -eq [IntPtr]::Zero) {
    Start-Process explorer.exe $testDir
    Start-Sleep -Seconds 6
    $hwnd = Get-ExplorerHwndByTitle $dirName
}
if ($hwnd -eq [IntPtr]::Zero) { Write-Output 'RESULT: NO_EXPLORER_WINDOW'; exit 1 }
if ([Native]::IsIconic($hwnd)) { [void][Native]::ShowWindow($hwnd, 9) }   # SW_RESTORE
Start-Sleep -Milliseconds 500
[void][Native]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 800

# 用 UIA 找到测试文件的列表项，右键其可点击点（本菜单只注册了文件 ItemType *）
$root = [System.Windows.Automation.AutomationElement]::RootElement
$win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NativeWindowHandleProperty, $hwnd.ToInt32())))
if (-not $win) { Write-Output 'RESULT: EXPLORER_NOT_FOUND_IN_UIA'; exit 1 }
# 通配匹配：系统开启"隐藏已知扩展名"时 UIA Name 不带 .txt
$item = $null
$all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
foreach ($el in $all) {
    if ($el.Current.Name -like 'menu-test*') { $item = $el; break }
}
if (-not $item) { Write-Output 'RESULT: TEST_FILE_ITEM_NOT_FOUND'; exit 1 }
try { $pt = $item.GetClickablePoint() } catch {
    $r = $item.BoundingRectangle
    $pt = New-Object System.Windows.Point ($r.X + $r.Width / 2), ($r.Y + $r.Height / 2)
}
$x = [int]$pt.X; $y = [int]$pt.Y
Write-Output ("right-click on '{0}' at ({1}, {2})" -f $item.Current.Name, $x, $y)
[void][Native]::SetCursorPos($x, $y)
Start-Sleep -Milliseconds 400
[Native]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)   # RIGHTDOWN
Start-Sleep -Milliseconds 90
[Native]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)   # RIGHTUP
Start-Sleep -Seconds 2

# 在点击点附近的顶层弹出窗口里找 HiEditor 菜单项
$found = $null
$wins2 = $root.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
foreach ($w in $wins2) {
    $wr = $w.BoundingRectangle
    if ([Math]::Abs($wr.X - $x) -gt 800 -or [Math]::Abs($wr.Y - $y) -gt 900) { continue }
    if ($wr.Width -lt 40 -or $wr.Height -lt 80) { continue }
    $els = $w.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($el in $els) {
        if ($el.Current.Name -like '*HiEditor*' -and -not $el.Current.IsOffscreen) {
            $found = $el
            Write-Output ("FOUND in '{0}': '{1}' ({2})" -f $w.Current.ClassName, $el.Current.Name, $el.Current.ControlType.ProgrammaticName)
            break
        }
    }
    if ($found) { break }
}

if (-not $found) {
    Write-Output 'RESULT: MENU_ITEM_NOT_FOUND (Win11: 项可能在「显示更多选项」里，或需重启 Explorer)'
    [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
    exit 1
}

$before = @(Get-Process -Name hieditor-gui,hieditor-pro -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$inv = $null
if ($found.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$inv)) {
    $inv.Invoke()
    Start-Sleep -Seconds 5
    $after = Get-Process -Name hieditor-gui,hieditor-pro -ErrorAction SilentlyContinue
    $new = @($after | Where-Object { $before -notcontains $_.Id })
    if ($new.Count -gt 0) {
        Write-Output 'RESULT: PASS - HiEditor launched from context menu'
        foreach ($p in $new) {
            [void]$p.CloseMainWindow()
            Start-Sleep -Milliseconds 800
            if (!$p.HasExited) { Stop-Process -Id $p.Id -Force }
        }
    } elseif ($after) {
        Write-Output 'RESULT: ITEM_INVOKED - existing HiEditor instance reused window (process existed before test)'
    } else {
        Write-Output 'RESULT: ITEM_FOUND_BUT_NO_PROCESS (check %TEMP%\hieditor_menu.log)'
    }
} else {
    Write-Output 'RESULT: ITEM_FOUND_NO_INVOKE_PATTERN'
}
[System.Windows.Forms.SendKeys]::SendWait('{ESC}')
Write-Output '--- %TEMP%\hieditor_menu.log tail ---'
Get-Content "$env:TEMP\hieditor_menu.log" -Tail 5 -ErrorAction SilentlyContinue
Write-Output '--- %TEMP%\hieditor_pro.log tail ---'
Get-Content "$env:TEMP\hieditor_pro.log" -Tail 5 -ErrorAction SilentlyContinue
