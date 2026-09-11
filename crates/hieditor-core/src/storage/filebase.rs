use std::io;
use std::path::Path;

/// 只读文件基座抽象：GB+ 场景下的"只读层"，上层（索引/搜索/渲染）只通过它取字节。
///
/// 三个后端：
/// - [`super::MemBase`]    小文件整读（≤ 阈值，默认 300 MB）
/// - [`super::MmapBase`]   内存映射 + OS 按需页加载（本地大文件默认路径）
/// - [`super::ChunkedBase`] ReadFile 式分块读（网络盘/可移动介质，规避 in-page error）
pub trait FileBase: Send + Sync {
    /// 文件总字节数。
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 零拷贝字节视图。后端不支持（如 ChunkedBase）或范围越界时返回 None。
    fn slice(&self, start: u64, len: usize) -> Option<&[u8]>;

    /// 把 [start, start+len) 读入 buf（buf.len() >= len）。
    /// 默认实现基于 [`FileBase::slice`]；ChunkedBase 覆盖为真正的分块读。
    fn read_range(&self, start: u64, len: usize, buf: &mut [u8]) -> io::Result<()> {
        assert!(buf.len() >= len, "read_range 目标缓冲区过小");
        match self.slice(start, len) {
            Some(s) => {
                buf[..len].copy_from_slice(s);
                Ok(())
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("read_range 越界: start={start} len={len} file_len={}", self.len()),
            )),
        }
    }

    fn backend_name(&self) -> &'static str;
}

/// 取一段字节：优先零拷贝视图，否则读入调用方提供的 scratch 缓冲区。
/// 返回的切片与 `base`、`scratch` 的借用同寿（mmap 路径实际指向映射视图，与 scratch 无关）。
pub fn chunk_bytes<'a>(
    base: &'a dyn FileBase,
    start: u64,
    len: usize,
    scratch: &'a mut Vec<u8>,
) -> io::Result<&'a [u8]> {
    if let Some(s) = base.slice(start, len) {
        return Ok(s);
    }
    scratch.clear();
    scratch.resize(len, 0);
    base.read_range(start, len, scratch)?;
    Ok(&scratch[..len])
}

pub fn file_len_of(path: &Path) -> io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}
