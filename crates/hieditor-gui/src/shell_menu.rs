//! shell_menu —— Windows 资源管理器右键菜单集成（传统动词注册 + 启动参数日志）
//!
//! - `--register` / `--unregister`：用原生注册表 API 写/删 HKCU\Software\Classes
//!   下的静态动词（当前用户作用域，免管理员；Win10 菜单第一层可见，Win11 收进「显示更多选项」）
//! - Win11 新版菜单第一层由 hieditor-menu COM 组件 + 稀疏 MSIX 包提供，
//!   部署走根目录 install.bat（见 sparse/ 与 CONTEXT-MENU-CHECKLIST.md）；
//!   仅此一步需调 PowerShell（加 CREATE_NO_WINDOW 隐藏控制台）
//! - 每次启动把命令行参数追加到 %TEMP%\hieditor_pro.log，便于验证菜单调用链

use std::path::PathBuf;

/// 当前 Windows build 号（查询失败返回 0）。Win11 = 22000+
fn windows_build() -> u32 {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut vi: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    vi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    if unsafe { RtlGetVersion(&mut vi) } == 0 {
        vi.dwBuildNumber
    } else {
        0
    }
}

/// Win11 新版菜单（稀疏 MSIX 包）仅在 Windows 11（build ≥ 22000）上可用
fn is_windows_11() -> bool {
    windows_build() >= 22000
}

/// 传统动词显示名（注册时可按系统语言选择，重跑 --register 可切换）
fn menu_title() -> &'static str {
    let zh = sys_locale::get_locale()
        .map(|l| l.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or(false);
    if zh {
        "用 HiEditor Pro 打开"
    } else {
        "Open with HiEditor Pro"
    }
}

/// 稀疏 MSIX 包名 / 菜单 COM 类 CLSID（与 sparse\AppxManifest.xml 保持一致）
const PACKAGE_NAME: &str = "HiEditorProContextMenu";
const CLSID_BRACED: &str = "{2E7EED97-9BAE-4334-8DA1-8DE129DA7024}";

/// GUI 进程无控制台，子进程默认闪现新窗口；此标志让子进程静默运行
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn msg_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION};
    let text_w = to_wide(text);
    let title_w = to_wide("HiEditor Pro");
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_ICONINFORMATION,
        );
    }
}

pub fn log_line(text: &str) {
    let Some(tmp) = std::env::var_os("TEMP") else { return };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::PathBuf::from(tmp).join("hieditor_pro.log"))
    {
        use std::io::Write;
        let _ = writeln!(f, "[{ts}] {text}");
    }
}

/// GUI 启动埋点：记录收到的全部命令行参数（右键菜单调用链的第 5 层验证点）
pub fn log_launch(args: &[String]) {
    if args.is_empty() {
        return;
    }
    log_line(&format!("收到参数: {args:?}"));
}

/// 原生写 HKCU 下 REG_SZ 值（value=None 写默认值）。进程内完成，不经 reg.exe
/// 子进程（GUI 无控制台，子进程会闪现窗口）。
fn reg_set(subkey: &str, value: Option<&str>, data: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_WRITE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    let sub_w = to_wide(subkey);
    let mut hk = std::ptr::null_mut();
    let err = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hk,
            std::ptr::null_mut(),
        )
    };
    if err != 0 {
        return Err(format!("创建注册表键失败 (错误码 {err}): {subkey}"));
    }
    let name_w = value.map(to_wide);
    let data_w = to_wide(data);
    let err = unsafe {
        RegSetValueExW(
            hk,
            name_w.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
            0,
            REG_SZ,
            data_w.as_ptr().cast(),
            (data_w.len() * 2) as u32,
        )
    };
    unsafe { RegCloseKey(hk) };
    if err != 0 {
        Err(format!("写入注册表值失败 (错误码 {err}): {subkey}"))
    } else {
        Ok(())
    }
}

/// 原生删除 HKCU 下注册表键（含子键与值），进程内完成。
fn reg_del(subkey: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    let sub_w = to_wide(subkey);
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub_w.as_ptr()) };
    if err == 0 {
        Ok(())
    } else {
        Err(format!("删除注册表键失败 (错误码 {err}): {subkey}"))
    }
}

/// 传统静态动词：右键文件（`*`）。HiEditor 只打开文件，不注册文件夹/空白处。
/// 注册在 HKCU\Software\Classes（当前用户，免管理员）。
fn register_verbs() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取自身路径失败: {e}"))?;
    let exe = exe.to_string_lossy().to_string();
    let base = r"Software\Classes\*\shell\HiEditorPro";
    let icon = format!("{exe},0");
    let cmd = format!("\"{exe}\" \"%1\"");
    reg_set(base, None, menu_title())?;
    reg_set(base, Some("Icon"), &icon)?;
    reg_set(&format!("{base}\\command"), None, &cmd)?;
    Ok(())
}

fn unregister_verbs() -> Result<(), String> {
    reg_del(r"Software\Classes\*\shell\HiEditorPro")
}

// ---------- 应用内一键注册（设置界面按钮） ----------

/// 注册结果：设置按钮据此显示状态栏文案
pub enum RegisterResult {
    /// 全部成功（传统动词 + Win11 稀疏包）
    Ok,
    /// 传统动词成功，Win11 稀疏包未注册（Win10 系统跳过 / Win11 注册失败附原因）
    ClassicOnly(String),
    /// 传统动词注册失败
    Failed(String),
}

/// 定位稀疏包目录（含 AppxManifest.xml 的包根）：
/// 开发布局 <根>\target\release\<exe> → <根>\sparse；
/// 发行布局 <根>\<exe> → <根>\sparse；exe 已在包根内时即自身目录。
fn locate_sparse_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut cands = vec![dir.join("sparse"), dir.join("../../sparse")];
    cands.push(dir.clone()); // exe 已在包根内（注册后的稀疏包目录）
    cands.into_iter().find(|c| c.join("AppxManifest.xml").is_file())
}

/// 把清单 Application@Executable 改成实际 exe 文件名（发行包里 exe 可能叫 hieditor-pro.exe）
fn patch_manifest_executable(xml: &str, exe_name: &str) -> String {
    const KEY: &str = "Executable=\"";
    let Some(start) = xml.find(KEY) else { return xml.to_string() };
    let vs = start + KEY.len();
    let Some(end) = xml[vs..].find('"') else { return xml.to_string() };
    if &xml[vs..vs + end] == exe_name {
        return xml.to_string();
    }
    let mut out = String::with_capacity(xml.len() + exe_name.len());
    out.push_str(&xml[..vs]);
    out.push_str(exe_name);
    out.push_str(&xml[vs + end..]);
    out
}

/// 注册 Win11 新菜单稀疏包：组装包根（exe/dll/清单）→ CLSID HKCU → Add-AppxPackage -Register。
/// 幂等：同版本先移除再注册；需系统已开启开发者模式（免签名免管理员）。
fn register_sparse_package() -> Result<(), String> {
    let Some(dir) = locate_sparse_dir() else {
        return Err("未找到 sparse\\AppxManifest.xml（安装不完整，请用发行包重装）".into());
    };
    let exe = std::env::current_exe().map_err(|e| format!("获取自身路径失败: {e}"))?;
    let exe_name = exe
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "hieditor-pro.exe".into());
    let dll = exe
        .parent()
        .map(|d| d.join("hieditor_menu.dll"))
        .ok_or_else(|| "获取程序目录失败".to_string())?;
    if !dll.is_file() {
        return Err("未找到 hieditor_menu.dll（安装不完整）".into());
    }
    // 组件与主程序就位（同路径则跳过，运行中的 exe 无法自我覆盖）
    let exe_dst = dir.join(&exe_name);
    if exe != exe_dst {
        std::fs::copy(&exe, &exe_dst).map_err(|e| format!("复制 exe 失败: {e}"))?;
    }
    let dll_dst = dir.join("hieditor_menu.dll");
    if dll != dll_dst {
        std::fs::copy(&dll, &dll_dst).map_err(|e| format!("复制 dll 失败: {e}"))?;
    }
    // 清单 Executable 对齐实际 exe 名
    let manifest = dir.join("AppxManifest.xml");
    let xml = std::fs::read_to_string(&manifest).map_err(|e| format!("读取清单失败: {e}"))?;
    let patched = patch_manifest_executable(&xml, &exe_name);
    if patched != xml {
        std::fs::write(&manifest, patched).map_err(|e| format!("写清单失败: {e}"))?;
    }
    // CLSID 写 HKCU（当前用户，免管理员；菜单激活走清单 com:ComServer）
    let dll_reg = dll_dst.to_string_lossy().to_string();
    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID_BRACED}");
    reg_set(&clsid_key, None, "HiEditor Pro IExplorerCommand")?;
    reg_set(&format!("{clsid_key}\\InprocServer32"), None, &dll_reg)?;
    reg_set(
        &format!("{clsid_key}\\InprocServer32"),
        Some("ThreadingModel"),
        "Apartment",
    )?;
    // 幂等注册稀疏包（0x80073CFB：同版本包必须先移除）
    let ps = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {PACKAGE_NAME} | Remove-AppxPackage -ErrorAction SilentlyContinue; Add-AppxPackage -Register '{}'",
        manifest.display()
    );
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("无法启动 powershell: {e}"))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "{detail}（Win11 新菜单需要系统开启开发者模式：设置 → 系统 → 开发者选项）"
        ));
    }
    Ok(())
}

/// 一键注册：传统动词（Win10 菜单 / Win11「显示更多选项」）+ Win11 新菜单稀疏包。
pub fn register_context_menu() -> RegisterResult {
    if let Err(e) = register_verbs() {
        log_line(&format!("注册失败: {e}"));
        return RegisterResult::Failed(e);
    }
    log_line("注册成功");
    // Win10 上稀疏包清单 MinVersion 要求 Win11（22000+），强行注册只会报 0x80073CFD
    if !is_windows_11() {
        let build = windows_build();
        let msg = if build > 0 {
            format!("当前系统为 Windows 10（build {build}），Win11 新版菜单仅 Windows 11 提供，已跳过")
        } else {
            "当前系统不是 Windows 11，Win11 新版菜单已跳过".to_string()
        };
        log_line(&format!("跳过稀疏包注册: {msg}"));
        return RegisterResult::ClassicOnly(msg);
    }
    match register_sparse_package() {
        Ok(()) => RegisterResult::Ok,
        Err(e) => {
            log_line(&format!("稀疏包注册失败: {e}"));
            RegisterResult::ClassicOnly(e)
        }
    }
}

/// 处理 --register / --unregister 并弹窗反馈；返回 true 表示已处理，进程直接退出。
pub fn handle_cli(arg: &str) -> bool {
    let title = menu_title();
    match arg {
        "--register" => {
            let text = match register_verbs() {
                Ok(()) => {
                    log_line("注册成功");
                    let mut text = format!(
                        "注册成功！\n\n右键文件 → 「{title}」\n- Windows 10：菜单第一层直接可见\n- Windows 11：在「显示更多选项」中"
                    );
                    if is_windows_11() {
                        text.push_str(
                            "\n\n要把条目放进 Win11 新版菜单第一层，请在项目目录运行 install.bat（稀疏包注册，免管理员，需已开启开发者模式）。",
                        );
                    }
                    text
                }
                Err(e) => {
                    log_line(&format!("注册失败: {e}"));
                    format!("注册失败：\n{e}")
                }
            };
            msg_box(&text);
            true
        }
        "--unregister" => {
            let text = match unregister_verbs() {
                Ok(()) => {
                    log_line("已取消注册");
                    "已取消注册，右键菜单项已删除。".to_string()
                }
                Err(e) => format!("取消注册失败：\n{e}"),
            };
            msg_box(&text);
            true
        }
        _ => false,
    }
}
