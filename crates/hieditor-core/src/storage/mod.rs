//! 存储层：FileBase 抽象 + 三后端 + 后端选择 + 编码采样探测。

pub mod chunked;
pub mod encoding;
pub mod filebase;
pub mod mem;
pub mod mmap;

pub use chunked::ChunkedBase;
pub use encoding::{detect_encoding, Encoding};
pub use filebase::{chunk_bytes, FileBase};
pub use mem::MemBase;
pub use mmap::MmapBase;

use crate::sysinfo::{self, MediaKind};
use std::io;
use std::path::Path;

/// 后端选择参数。
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// ≤ 此阈值的文件整读进内存（MemBase），默认 300 MB（参考 EmEditor 大文件模式线）。
    pub mem_threshold: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            mem_threshold: 300 << 20,
        }
    }
}

/// 按介质类型与文件大小选择后端：
/// - 远程/可移动介质 → ChunkedBase（规避 mmap in-page error 硬异常）
/// - 本地 ≤ mem_threshold → MemBase 整读
/// - 本地 > mem_threshold → MmapBase（MMF + 按需页加载）
pub fn open_backend(path: &Path, cfg: &StorageConfig) -> io::Result<(Box<dyn FileBase>, &'static str)> {
    let len = filebase::file_len_of(path)?;
    let media = sysinfo::classify_media(path);
    if len == 0 {
        return Ok((Box::new(MemBase::from_vec(Vec::new())), "MemBase(空文件)"));
    }
    match media {
        MediaKind::Remote | MediaKind::Removable => {
            Ok((Box::new(ChunkedBase::from_path(path)?), "ChunkedBase(网络/可移动介质)"))
        }
        _ => {
            if len <= cfg.mem_threshold {
                Ok((Box::new(MemBase::from_path(path)?), "MemBase(小文件整读)"))
            } else {
                Ok((Box::new(MmapBase::from_path(path)?), "MmapBase(MMF+按需页)"))
            }
        }
    }
}
