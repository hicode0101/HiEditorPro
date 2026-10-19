// hieditor-menu —— Windows 11 新版右键菜单原生条目（IExplorerCommand COM 进程内组件）
//
// Explorer 通过稀疏 MSIX 包 (sparse/AppxManifest.xml) 里 com:ComServer 声明的 CLSID
// 加载本 DLL，枚举选中项并把路径作为命令行参数启动同目录的主程序。
// 注意：Explorer 只认包清单里声明的 COM 类，HKCU\Software\Classes\CLSID 的注册表
// 注册仅供 CoCreateInstance 激活测试，二者缺一不可（详见 CONTEXT-MENU-CHECKLIST.md）。
// 日志写入 %TEMP%\hieditor_menu.log，便于排查加载失败。

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER, HMODULE,
};
use windows::Win32::System::Com::{
    CoTaskMemAlloc, CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItem, IShellItemArray,
    ShellExecuteW, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_core::{
    implement, BOOL, Error, GUID, HRESULT, Interface, IUnknown, PCWSTR, PWSTR, Ref, Result,
};

// 「用 HiEditor Pro 打开」的 CLSID，与 sparse/AppxManifest.xml 中
// com:Class@Id 和 desktop5:Verb@Clsid 保持一致（小写、不带花括号）
const CLSID_HIEDITOR_COMMAND: GUID = GUID::from_u128(0x2E7EED97_9BAE_4334_8DA1_8DE129DA7024);

const MENU_TITLE: &str = "用 HiEditor Pro 打开";
const MENU_TOOLTIP: &str = "用 HiEditor Pro 打开选中的文件";

/// 主程序候选名：统一为 hieditor-pro.exe（hieditor-gui.exe 为旧版兼容回退），
/// 取第一个与 DLL 同目录存在者（不写死绝对路径，随安装位置迁移）
const EXE_CANDIDATES: [&str; 2] = ["hieditor-pro.exe", "hieditor-gui.exe"];

#[no_mangle]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    unsafe {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        if *rclsid != CLSID_HIEDITOR_COMMAND {
            return Error::from(CLASS_E_CLASSNOTAVAILABLE).into();
        }
        let factory: IClassFactory = ClassFactory.into();
        factory.query(riid, ppv)
    }
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // 恒返回 S_FALSE，避免 Explorer 卸载本 DLL 引发崩溃
    HRESULT(1)
}

fn log_line(text: &str) {
    let Some(tmp) = std::env::var_os("TEMP") else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::PathBuf::from(tmp).join("hieditor_menu.log"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{text}");
    }
}

// 与主程序同目录的绝对路径（UTF-16）。用 DllGetClassObject 函数地址反查所在模块
// 获取本 DLL 的 HINSTANCE——不能依赖 DllMain 存句柄：用户定义的 DllMain 在
// 部分 COM 加载场景下不会被调用，导致 G_HINSTANCE 为空、GetIcon 拿不到路径。
fn host_exe_path() -> Option<Vec<u16>> {
    let mut hmod = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR::from_raw(DllGetClassObject as *const _),
            &mut hmod,
        )
        .ok()?;
        let mut buf = [0u16; 1024];
        let n = GetModuleFileNameW(Some(hmod), &mut buf);
        if n == 0 {
            return None;
        }
        let path = &buf[..n as usize];
        let pos = path.iter().rposition(|&c| c == '\\' as u16)?;
        let dir = &path[..=pos];
        for name in EXE_CANDIDATES {
            let mut out = dir.to_vec();
            out.extend(name.encode_utf16());
            out.push(0); // ShellExecuteW 与 host_exe_path_string 都依赖 0 终止
            if file_exists(&out) {
                return Some(out);
            }
        }
        // 都不存在时退回首选名，让 ShellExecute 给出可见的失败
        let mut out = dir.to_vec();
        out.extend(EXE_CANDIDATES[0].encode_utf16());
        out.push(0);
        Some(out)
    }
}

fn file_exists(wide: &[u16]) -> bool {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    std::fs::metadata(String::from_utf16_lossy(&wide[..len]))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn host_exe_path_string() -> Option<String> {
    let wide = host_exe_path()?;
    let len = wide.iter().position(|&c| c == 0)?;
    Some(String::from_utf16_lossy(&wide[..len]))
}

// 在 COM 分配器上分配 UTF-16 字符串（调用方负责 CoTaskMemFree）
fn alloc_pw(s: &str) -> Result<PWSTR> {
    let mut w: Vec<u16> = s.encode_utf16().collect();
    w.push(0);
    unsafe {
        let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
        if p.is_null() {
            return Err(Error::from(E_FAIL));
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        Ok(PWSTR(p))
    }
}

#[implement(IExplorerCommand)]
struct HiEditorCommand;

impl IExplorerCommand_Impl for HiEditorCommand_Impl {
    fn GetTitle(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        alloc_pw(MENU_TITLE)
    }

    fn GetIcon(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        match host_exe_path_string() {
            Some(exe) => alloc_pw(&format!("{exe},0")),
            None => Err(Error::from(E_FAIL)),
        }
    }

    fn GetToolTip(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        alloc_pw(MENU_TOOLTIP)
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID_HIEDITOR_COMMAND)
    }

    fn GetState(&self, _psiitemarray: Ref<IShellItemArray>, _foktobeslow: BOOL) -> Result<u32> {
        Ok(0) // ECS_ENABLED
    }

    fn Invoke(&self, psiitemarray: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
        // 组件跑在 Explorer/dllhost 里，任何路径都不能 panic
        if psiitemarray.is_null() {
            return Ok(());
        }
        let arr = psiitemarray.unwrap();
        unsafe {
            let count = arr.GetCount().unwrap_or(0);
            let mut args = String::new();
            for i in 0..count {
                let Ok(item): std::result::Result<IShellItem, _> = arr.GetItemAt(i) else {
                    continue;
                };
                let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else {
                    continue;
                };
                if !name.is_null() {
                    let s = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const std::ffi::c_void));
                    if !s.is_empty() {
                        if !args.is_empty() {
                            args.push(' ');
                        }
                        args.push_str(&format!("\"{s}\""));
                    }
                }
            }
            let Some(exe) = host_exe_path() else {
                return Err(Error::from(E_FAIL));
            };
            let args_w: Vec<u16> = args.encode_utf16().chain(std::iter::once(0)).collect();
            log_line(&format!(
                "Invoke: exe={} args={args}",
                String::from_utf16_lossy(&exe)
            ));
            ShellExecuteW(
                None,
                PCWSTR::null(),
                PCWSTR(exe.as_ptr()),
                PCWSTR(args_w.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
        Ok(())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(0) // ECF_DEFAULTLOCAL：叶子命令，无子菜单
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(Error::from(E_NOTIMPL))
    }
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut std::ffi::c_void,
    ) -> Result<()> {
        unsafe {
            if !punkouter.is_null() {
                return Err(Error::from(CLASS_E_NOAGGREGATION));
            }
            if ppvobject.is_null() {
                return Err(Error::from(E_POINTER));
            }
            *ppvobject = std::ptr::null_mut();
            let cmd: IUnknown = HiEditorCommand.into();
            cmd.query(riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}
