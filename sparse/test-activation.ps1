# 第 2 层验证：以 Explorer 的方式 CoCreateInstance 激活菜单 COM 类
# （HRESULT=0 即 DLL 加载、类工厂、QI 全通；见 CONTEXT-MENU-CHECKLIST.md E.2）
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class ComTest {
    [DllImport("ole32.dll")] public static extern int CoCreateInstance(ref Guid clsid, IntPtr pUnkOuter, uint dwClsContext, ref Guid riid, out IntPtr ppv);
}
"@
$clsid = [Guid]'2E7EED97-9BAE-4334-8DA1-8DE129DA7024'
$iid   = [Guid]'00000000-0000-0000-C000-000000000046'   # IUnknown
$p = [IntPtr]::Zero
$hr = [ComTest]::CoCreateInstance([ref]$clsid, [IntPtr]::Zero, 1, [ref]$iid, [ref]$p)   # 1 = CLSCTX_INPROC_SERVER
Write-Output ("CLSCTX_INPROC_SERVER  HRESULT: 0x{0:X8}  ptr={1}" -f $hr, $p)
if ($hr -eq 0) { Write-Output 'ACTIVATION OK - DLL loads, DllGetClassObject + QI work' }
else { Write-Output 'ACTIVATION FAILED - check HKCU CLSID registration and dll path' }
