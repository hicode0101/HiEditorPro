use super::filebase::FileBase;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 分块读后端：ReadFile 式顺序/随机读，无内存映射。
///
/// 用途：网络盘 / 可移动介质。映射页在这些介质上读盘失败会抛
/// EXCEPTION_IN_PAGE_ERROR（SEH 硬异常，Rust 无法捕获），分块读则返回普通 io::Error。
/// 同时作为"ReadFile 大缓冲 vs mmap 顺序扫描"基准对比的 ReadFile 路径。
///
/// 每次读打开独立句柄：多线程并行扫描各自持有句柄，避免共享句柄上
/// seek/read 竞态；8MB 读取窗口下句柄开销可忽略。
pub struct ChunkedBase {
    path: PathBuf,
    len: u64,
}

impl ChunkedBase {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let len = std::fs::metadata(path)?.len();
        Ok(Self {
            path: path.to_path_buf(),
            len,
        })
    }
}

impl FileBase for ChunkedBase {
    fn len(&self) -> u64 {
        self.len
    }

    fn slice(&self, _start: u64, _len: usize) -> Option<&[u8]> {
        None
    }

    fn read_range(&self, start: u64, len: usize, buf: &mut [u8]) -> io::Result<()> {
        assert!(buf.len() >= len, "read_range 目标缓冲区过小");
        if start.saturating_add(len as u64) > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("read_range 越界: start={start} len={len} file_len={}", self.len),
            ));
        }
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf[..len])
    }

    fn backend_name(&self) -> &'static str {
        "ChunkedBase(ReadFile分块)"
    }
}
