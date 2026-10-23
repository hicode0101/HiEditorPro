//! shell_menu —— Windows 资源管理器右键菜单集成（传统动词注册 + 启动参数日志）
//!
//! - `--register` / `--unregister`：用原生注册表 API 写/删 HKCU\Software\Classes
//!   下的静态动词（当前用户作用域，免管理员；Win10 的唯一入口，Win11 仅作
//!   稀疏包注册失败时的回退，避免与包条目在经典菜单中重复）
//! - Win11 新版菜单第一层由 hieditor-menu COM 组件 + 稀疏 MSIX 包提供，
//!   由 `--register` / 设置界面一键注册（包根布局见 sparse/）；
//!   仅此一步需调 PowerShell（加 CREATE_NO_WINDOW 隐藏控制台）
//! - 每次启动把命令行参数追加到 %TEMP%\hieditor_pro.log，便于验证菜单调用链

use std::path::{Path, PathBuf};

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

/// 发行包内置的签名稀疏包 / 公钥证书文件名（位于 exe 旁 sparse\ 目录，构建时生成）
const SIGNED_PACKAGE_FILE: &str = "HiEditorProContextMenu.msix";
const SIGNED_CERT_FILE: &str = "HiEditorPro.cer";
/// 签名包清单声明的可执行文件名（签名后不可改，ExternalLocation 内必须同名）
const MANIFEST_EXE: &str = "HiEditorPro.exe";

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
    /// 成功：Win10 传统动词已注册；Win11 额外注册了稀疏包
    Ok,
    /// 传统动词成功，但 Win11 稀疏包注册失败（附原因）
    Partial(String),
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

/// 把清单 Application@Executable 改成实际 exe 文件名（如发行包内 exe 与清单不一致）
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

/// 查询稀疏包当前是否已注册（PowerShell 子进程，结果影响是否回退传统动词）
fn sparse_package_registered() -> bool {
    let ps = format!("if (Get-AppxPackage -Name {PACKAGE_NAME}) {{ exit 0 }} else {{ exit 1 }}");
    run_powershell(&ps)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 无窗口运行 PowerShell 命令（GUI 进程无控制台，子进程默认闪现新窗口）
fn run_powershell(ps: &str) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("无法启动 powershell: {e}"))
}

/// 注册 Win11 新菜单稀疏包。优先用发行包内置签名包（sparse\*.msix + .cer，
/// 免开发者模式；证书未受信时弹一次 UAC 导入本机受信任存储）。无签名包时
/// 回退未签名清单（-Register，需系统已开启开发者模式）。均幂等：同版本先移除再注册。
fn register_sparse_package() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取自身路径失败: {e}"))?;
    let exe_name = exe
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| MANIFEST_EXE.into());
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "获取程序目录失败".to_string())?
        .to_path_buf();
    let dll = exe_dir.join("hieditor_menu.dll");
    if !dll.is_file() {
        return Err("未找到 hieditor_menu.dll（安装不完整）".into());
    }
    // CLSID 写 HKCU（当前用户，免管理员；菜单激活走清单 com:ComServer）
    let dll_reg = dll.to_string_lossy().to_string();
    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID_BRACED}");
    reg_set(&clsid_key, None, "HiEditor Pro IExplorerCommand")?;
    reg_set(&format!("{clsid_key}\\InprocServer32"), None, &dll_reg)?;
    reg_set(
        &format!("{clsid_key}\\InprocServer32"),
        Some("ThreadingModel"),
        "Apartment",
    )?;
    let msix = exe_dir.join("sparse").join(SIGNED_PACKAGE_FILE);
    if msix.is_file() {
        return register_signed_package(&msix, &exe_dir, &exe_name);
    }
    register_unsigned_package(&exe, &exe_name)
}

/// 签名稀疏包注册：Add-AppxPackage -Path <msix> -ExternalLocation <exe 目录>。
/// 流程尽量不破坏已注册的可用状态：先直接注册；证书未受信（0x800B0109/0x800B010A，
/// AppXSvc 以 SYSTEM 运行只查本机受信任存储）时管理员导入 .cer 后重试（UAC 取消
/// 则原样返回，已注册的旧包不受影响）；同版本已注册（0x80073CFB/0x80073CF9）才
/// 先移除再注册（幂等升级）。
fn register_signed_package(msix: &Path, exe_dir: &Path, exe_name: &str) -> Result<(), String> {
    if exe_name != MANIFEST_EXE {
        return Err(format!(
            "内置签名包要求主程序保持文件名 {MANIFEST_EXE}（如需改名请删除 sparse\\{SIGNED_PACKAGE_FILE} 改用未签名注册）"
        ));
    }
    let msix_s = msix.to_string_lossy().to_string();
    let dir_s = exe_dir.to_string_lossy().to_string();
    let add_cmd = |remove_first: bool| {
        let rm = if remove_first {
            format!("Get-AppxPackage -Name {PACKAGE_NAME} | Remove-AppxPackage -ErrorAction SilentlyContinue; ")
        } else {
            String::new()
        };
        format!(
            "$ErrorActionPreference='Stop'; {rm}Add-AppxPackage -Path '{}' -ExternalLocation '{}'",
            msix_s, dir_s
        )
    };
    let is_untrusted = |s: &str| {
        let u = s.to_ascii_uppercase();
        u.contains("0X800B0109") || u.contains("0X800B010A") || u.contains("CERT_E_UNTRUSTED")
    };
    let is_same_version = |s: &str| {
        let u = s.to_ascii_uppercase();
        u.contains("0X80073CFB") || u.contains("0X80073CF9")
    };
    let mut out = run_powershell(&add_cmd(false))?;
    if !out.status.success() && is_untrusted(&String::from_utf8_lossy(&out.stderr)) {
        let cer = msix.with_file_name(SIGNED_CERT_FILE);
        if !cer.is_file() {
            return Err(format!(
                "{}（未找到 {SIGNED_CERT_FILE}，无法自动导入签名证书）",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let import = format!(
            "try {{ $p = Start-Process -FilePath certutil.exe -ArgumentList '-addstore','TrustedPeople','{}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode }} catch {{ exit 1 }}",
            cer.to_string_lossy()
        );
        let imp = run_powershell(&import)?;
        if !imp.status.success() {
            return Err(format!(
                "签名证书导入未完成（需在 UAC 弹窗中确认），已保留现有注册。可重试注册，或手动以管理员执行: certutil -addstore TrustedPeople \"{}\"",
                cer.to_string_lossy()
            ));
        }
        out = run_powershell(&add_cmd(false))?;
    }
    if !out.status.success() && is_same_version(&String::from_utf8_lossy(&out.stderr)) {
        out = run_powershell(&add_cmd(true))?;
    }
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// 未签名清单注册（开发布局回退）：组装包根（复制 exe/dll、对齐清单 Executable）→
/// Add-AppxPackage -Register -ExternalLocation。清单含 AllowExternalContent，
/// 必须带外部位置；需系统已开启开发者模式（免签名免管理员）。
fn register_unsigned_package(exe: &Path, exe_name: &str) -> Result<(), String> {
    let Some(dir) = locate_sparse_dir() else {
        return Err("未找到 sparse\\AppxManifest.xml（安装不完整，请用发行包重装）".into());
    };
    // 组件与主程序就位（同路径则跳过，运行中的 exe 无法自我覆盖）
    let exe_dst = dir.join(exe_name);
    if exe != exe_dst {
        std::fs::copy(exe, &exe_dst).map_err(|e| format!("复制 exe 失败: {e}"))?;
    }
    let dll_dst = dir.join("hieditor_menu.dll");
    if !dll_dst.is_file() {
        std::fs::copy(exe.parent().unwrap_or(Path::new(".")).join("hieditor_menu.dll"), &dll_dst)
            .map_err(|e| format!("复制 dll 失败: {e}"))?;
    }
    // 清单 Executable 对齐实际 exe 名
    let manifest = dir.join("AppxManifest.xml");
    let xml = std::fs::read_to_string(&manifest).map_err(|e| format!("读取清单失败: {e}"))?;
    let patched = patch_manifest_executable(&xml, exe_name);
    if patched != xml {
        std::fs::write(&manifest, patched).map_err(|e| format!("写清单失败: {e}"))?;
    }
    // 幂等注册稀疏包（0x80073CFB：同版本包必须先移除）；AllowExternalContent
    // 清单必须显式指明外部位置，否则报 0x80073CF9
    let ps = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {PACKAGE_NAME} | Remove-AppxPackage -ErrorAction SilentlyContinue; Add-AppxPackage -Register '{}' -ExternalLocation '{}'",
        manifest.display(),
        dir.display()
    );
    let out = run_powershell(&ps)?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "{detail}（未签名注册需系统开启开发者模式：设置 → 系统 → 开发者选项；发行包自带签名稀疏包，无需开发者模式）"
        ));
    }
    Ok(())
}

/// 取消注册 Win11 新菜单稀疏包：删 HKCU CLSID → Remove-AppxPackage（幂等，包未注册时不报错）。
fn unregister_sparse_package() -> Result<(), String> {
    let _ = reg_del(&format!(r"Software\Classes\CLSID\{CLSID_BRACED}"));
    let ps = format!(
        "$ErrorActionPreference='Stop'; Get-AppxPackage -Name {PACKAGE_NAME} | Remove-AppxPackage"
    );
    let out = run_powershell(&ps)?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(detail);
    }
    Ok(())
}

/// 一键注册。Win11：只注册稀疏包——它同时覆盖新版菜单与经典菜单（显示更多选项），
/// 传统动词会造成经典菜单重复，仅在稀疏包注册失败时回退注册；Win10：只注册传统动词
/// （无新版菜单，稀疏包清单 MinVersion 要求 22000+）。
pub fn register_context_menu() -> RegisterResult {
    if !is_windows_11() {
        if let Err(e) = register_verbs() {
            log_line(&format!("注册失败: {e}"));
            return RegisterResult::Failed(e);
        }
        log_line("注册成功");
        return RegisterResult::Ok;
    }
    match register_sparse_package() {
        Ok(()) => {
            // 清理可能残留的传统动词，避免经典菜单出现两个相同条目
            let _ = unregister_verbs();
            RegisterResult::Ok
        }
        Err(e) => {
            log_line(&format!("稀疏包注册失败: {e}"));
            // 回退传统动词仅在包未注册时进行：包仍注册着（如证书未确认但旧包可用）
            // 时补动词会让经典菜单出现重复条目
            if sparse_package_registered() {
                RegisterResult::Partial(e)
            } else {
                let fallback = match register_verbs() {
                    Ok(()) => {
                        Some("已回退注册传统动词，条目在经典菜单（显示更多选项）中可见".to_string())
                    }
                    Err(ve) => {
                        log_line(&format!("传统动词回退注册失败: {ve}"));
                        None
                    }
                };
                RegisterResult::Partial(match fallback {
                    Some(msg) => format!("{e}\n{msg}"),
                    None => e,
                })
            }
        }
    }
}

/// 处理 --register / --unregister 并弹窗反馈；返回 true 表示已处理，进程直接退出。
pub fn handle_cli(arg: &str) -> bool {
    let title = menu_title();
    match arg {
        "--register" => {
            let text = match register_context_menu() {
                RegisterResult::Ok if is_windows_11() => format!(
                    "注册成功！\n\n右键文件 → 「{title}」\n- 新版菜单第一层直接可见\n- 经典菜单（显示更多选项）中同样可见"
                ),
                RegisterResult::Ok => {
                    format!("注册成功！\n\n右键文件 → 「{title}」，菜单第一层直接可见。")
                }
                RegisterResult::Partial(e) => {
                    format!("传统动词注册成功，但 Win11 新菜单稀疏包注册失败：\n{e}")
                }
                RegisterResult::Failed(e) => format!("注册失败：\n{e}"),
            };
            msg_box(&text);
            true
        }
        "--unregister" => {
            let mut text = match unregister_verbs() {
                Ok(()) => {
                    log_line("已取消注册");
                    "已取消注册，右键菜单项已删除。".to_string()
                }
                Err(e) => format!("取消注册失败：\n{e}"),
            };
            if is_windows_11() {
                match unregister_sparse_package() {
                    Ok(()) => text.push_str("\nWin11 新版菜单稀疏包已移除。"),
                    Err(e) => text.push_str(&format!("\nWin11 新版菜单稀疏包移除失败：{e}")),
                }
            }
            msg_box(&text);
            true
        }
        _ => false,
    }
}
