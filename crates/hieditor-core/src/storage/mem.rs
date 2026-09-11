use super::filebase::FileBase;
use std::io;
use std::path::Path;

/// 小文件整读后端：全部字节驻留进程内存，所有操作最快最简单。
pub struct MemBase {
    data: Vec<u8>,
}

impl MemBase {
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn from_path(path: &Path) -> io::Result<Self> {
        Ok(Self {
            data: std::fs::read(path)?,
        })
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }
}

impl FileBase for MemBase {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn slice(&self, start: u64, len: usize) -> Option<&[u8]> {
        let start = start as usize;
        let end = start.checked_add(len)?;
        if end > self.data.len() {
            return None;
        }
        Some(&self.data[start..end])
    }

    fn backend_name(&self) -> &'static str {
        "MemBase(整读)"
    }
}
