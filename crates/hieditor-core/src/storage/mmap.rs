use super::filebase::FileBase;
use memmap2::Mmap;
use std::fs::File;
use std::io;
use std::path::Path;

/// 内存映射后端：只读基座 + OS 按需页加载。
///
/// 映射本身 O(1)；访问到哪，OS 才把对应页换进物理内存，页缓存自动回收。
/// 这是 GB~百 GB 级文件"极少内存打开"的核心机制。
/// 注意：映射期间 Windows 不允许删除/替换原文件（保存管线二期需先 unmap）。
pub struct MmapBase {
    mmap: Mmap,
    len: u64,
}

impl MmapBase {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "空文件请使用 MemBase（mmap 不支持 0 长度映射）",
            ));
        }
        // SAFETY: 只读映射，文件句柄生命周期由 Mmap 管理，无写入方修改保护需求（v0.1 只读场景）。
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Self { mmap, len })
    }
}

impl FileBase for MmapBase {
    fn len(&self) -> u64 {
        self.len
    }

    fn slice(&self, start: u64, len: usize) -> Option<&[u8]> {
        let end = start.checked_add(len as u64)?;
        if end > self.len {
            return None;
        }
        Some(&self.mmap[start as usize..end as usize])
    }

    fn backend_name(&self) -> &'static str {
        "MmapBase(MMF+按需页)"
    }
}
