//! desktop shortcut —— 原生 COM（IShellLinkW + IPersistFile）创建桌面快捷方式。
//!
//! 不经 PowerShell/WScript.Shell 等外部进程：GUI 进程无控制台，
//! 每个控制台子进程都会闪现新窗口。windows-sys 不含 COM 接口 vtable
//! 定义，此处按 ABI 手工声明用到的两个接口（槽位顺序即 COM 虚表顺序）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::core::{GUID, PCWSTR, PWSTR};
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows_sys::Win32::UI::Shell::{SHGetKnownFolderPath, ShellLink, FOLDERID_Desktop};

const IID_ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const IID_IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

type HResult = i32;

/// IUnknown + IShellLinkW 全部 18 个方法的虚表（槽位必须齐全且按 IDL 顺序）
#[repr(C)]
struct ShellLinkVtbl {
    query_interface: unsafe extern "system" fn(*mut IShellLinkW, *const GUID, *mut *mut core::ffi::c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut IShellLinkW) -> u32,
    release: unsafe extern "system" fn(*mut IShellLinkW) -> u32,
    get_path: unsafe extern "system" fn(*mut IShellLinkW, PWSTR, i32, *mut core::ffi::c_void, u32) -> HResult,
    get_id_list: unsafe extern "system" fn(*mut IShellLinkW, *mut *mut core::ffi::c_void) -> HResult,
    set_id_list: unsafe extern "system" fn(*mut IShellLinkW, *mut core::ffi::c_void) -> HResult,
    get_description: unsafe extern "system" fn(*mut IShellLinkW, PWSTR, i32) -> HResult,
    set_description: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HResult,
    get_working_dir: unsafe extern "system" fn(*mut IShellLinkW, PWSTR, i32) -> HResult,
    set_working_dir: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HResult,
    get_arguments: unsafe extern "system" fn(*mut IShellLinkW, PWSTR, i32) -> HResult,
    set_arguments: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HResult,
    get_hotkey: unsafe extern "system" fn(*mut IShellLinkW, *mut u16) -> HResult,
    set_hotkey: unsafe extern "system" fn(*mut IShellLinkW, u16) -> HResult,
    get_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, *mut i32) -> HResult,
    set_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, i32) -> HResult,
    get_icon_location: unsafe extern "system" fn(*mut IShellLinkW, PWSTR, i32, *mut i32) -> HResult,
    set_icon_location: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR, i32) -> HResult,
    set_relative_path: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR, u32) -> HResult,
    resolve: unsafe extern "system" fn(*mut IShellLinkW, *mut core::ffi::c_void, u32) -> HResult,
    set_path: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HResult,
}

#[repr(C)]
struct IShellLinkW {
    lp_vtbl: *const ShellLinkVtbl,
}

/// IUnknown + IPersist（GetClassID）+ IPersistFile 虚表
#[repr(C)]
struct PersistFileVtbl {
    query_interface: unsafe extern "system" fn(*mut IPersistFile, *const GUID, *mut *mut core::ffi::c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut IPersistFile) -> u32,
    release: unsafe extern "system" fn(*mut IPersistFile) -> u32,
    get_class_id: unsafe extern "system" fn(*mut IPersistFile, *mut GUID) -> HResult,
    is_dirty: unsafe extern "system" fn(*mut IPersistFile) -> HResult,
    load: unsafe extern "system" fn(*mut IPersistFile, PCWSTR, u32) -> HResult,
    save: unsafe extern "system" fn(*mut IPersistFile, PCWSTR, i32) -> HResult,
    save_completed: unsafe extern "system" fn(*mut IPersistFile, PCWSTR) -> HResult,
    get_cur_file: unsafe extern "system" fn(*mut IPersistFile, *mut PWSTR) -> HResult,
}

#[repr(C)]
struct IPersistFile {
    lp_vtbl: *const PersistFileVtbl,
}

fn to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn pwz_to_string(p: PWSTR) -> String {
    let mut len = 0usize;
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// 在桌面创建 "HiEditor Pro.lnk"，指向当前 exe。全程进程内，返回是否成功。
pub fn create_desktop_shortcut() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let dir = exe.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    unsafe { create_shortcut(&exe, &dir) }
}

unsafe fn create_shortcut(exe: &Path, workdir: &Path) -> bool {
    // 桌面目录
    let mut desktop: PWSTR = std::ptr::null_mut();
    if SHGetKnownFolderPath(&FOLDERID_Desktop, 0, std::ptr::null_mut(), &mut desktop) != 0 {
        return false;
    }
    let desktop_dir = pwz_to_string(desktop);
    CoTaskMemFree(desktop.cast());

    // COM 初始化：winit 已在线程上初始化过（STA），S_FALSE / RPC_E_CHANGED_MODE 均可直接用
    let init = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    let need_uninit = init == 0 || init == 1; // S_OK / S_FALSE

    let mut unk: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut ok = CoCreateInstance(
        &ShellLink,
        std::ptr::null_mut(),
        CLSCTX_INPROC_SERVER,
        &IID_ISHELL_LINK_W,
        &mut unk,
    ) == 0;

    if ok {
        let link = unk as *mut IShellLinkW;
        let vt = (*link).lp_vtbl;
        let exe_w = to_wide(exe.as_os_str());
        let dir_w = to_wide(workdir.as_os_str());
        let lnk_w = to_wide(OsStr::new(
            format!("{}\\HiEditor Pro.lnk", desktop_dir.trim_end_matches('\\')).as_str(),
        ));
        ok &= ((*vt).set_path)(link, exe_w.as_ptr()) == 0;
        ok &= ((*vt).set_working_dir)(link, dir_w.as_ptr()) == 0;
        ok &= ((*vt).set_icon_location)(link, exe_w.as_ptr(), 0) == 0;
        // QueryInterface(IPersistFile) 后 Save 写出 .lnk
        let mut pf_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        if ((*vt).query_interface)(link, &IID_IPERSIST_FILE, &mut pf_ptr) == 0 {
            let pf = pf_ptr as *mut IPersistFile;
            let pvt = (*pf).lp_vtbl;
            ok &= ((*pvt).save)(pf, lnk_w.as_ptr(), 1) == 0;
            ((*pvt).release)(pf);
        } else {
            ok = false;
        }
        ((*vt).release)(link);
    }
    if need_uninit {
        CoUninitialize();
    }
    ok
}
