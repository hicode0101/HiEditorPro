//! 系统信息：介质类型、物理内存、进程内存（commit vs working set）、理想线程数。
//!
//! Windows 用原生 API；其他平台给保守缺省值，保证内核可移植编译。

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Local,
    Remote,
    Removable,
    Unknown,
}

/// 判断文件所在介质。Windows 上解析盘符调 GetDriveTypeW；UNC 路径直接判远程。
pub fn classify_media(path: &Path) -> MediaKind {
    #[cfg(windows)]
    {
        return classify_media_windows(path);
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        MediaKind::Unknown
    }
}

#[cfg(windows)]
fn classify_media_windows(path: &Path) -> MediaKind {
    // DRIVE_* 常量值来自 Win32（windows-sys 0.59 未在 FileSystem 模块导出，直接本地定义）
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_REMOVABLE: u32 = 2;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let Some(s) = path.to_str() else {
        return MediaKind::Unknown;
    };
    // UNC 路径（\\server\share 或 //server/share）→ 远程
    if s.starts_with("\\\\") || s.starts_with("//") {
        return MediaKind::Remote;
    }
    // 取 "X:" → "X:\\"
    let mut root = String::new();
    for ch in s.chars() {
        root.push(ch);
        if ch == ':' {
            break;
        }
    }
    if !root.ends_with(':') {
        return MediaKind::Unknown;
    }
    root.push('\\');
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let t = unsafe { GetDriveTypeW(wide.as_ptr()) };
    match t {
        DRIVE_FIXED => MediaKind::Local,
        DRIVE_REMOTE => MediaKind::Remote,
        DRIVE_REMOVABLE => MediaKind::Removable,
        _ => MediaKind::Unknown,
    }
}

/// 物理内存字节数；未知返回 0。
pub fn physical_memory() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        unsafe {
            let mut ms: MEMORYSTATUSEX = std::mem::zeroed();
            ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut ms) != 0 {
                return ms.ullTotalPhys;
            }
        }
        0
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 进程内存 (working_set, commit)；未知返回 None。
pub fn process_memory() -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        unsafe {
            let mut pmc: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                pmc.cb,
            ) != 0
            {
                return Some((pmc.WorkingSetSize as u64, pmc.PrivateUsage.max(0) as u64));
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 请求系统整理工作集（演示可回收性：mmap 页属于页缓存，可被系统随时回收）。
pub fn trim_working_set() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        unsafe {
            K32EmptyWorkingSet(GetCurrentProcess());
        }
    }
}

/// 理想线程数：min(逻辑核数, 物理内存 GB)。
/// EmEditor 作者的实测教训：内存带宽是瓶颈，线程数超过内存 GB 数反而变慢。
pub fn ideal_thread_count() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let gb = (physical_memory() / (1 << 30)) as usize;
    if gb == 0 {
        cores
    } else {
        cores.min(gb.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_count_sane() {
        let n = ideal_thread_count();
        assert!(n >= 1 && n <= 64, "ideal_thread_count={n}");
    }

    #[test]
    fn media_kind_works() {
        // 至少不 panic；本地盘应当可判为 Local
        let kind = classify_media(Path::new("C:\\Windows"));
        assert_ne!(kind, MediaKind::Remote);
    }
}
