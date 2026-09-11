//! 行偏移索引：并行扫描换行符，建立 `行号 → 字节偏移` 映射。
//!
//! 存储为 u32 差分（绝大多数行 < 4GB）+ 每 4096 行一个 u64 绝对偏移锚点
//! （superblock），随机访问为 O(superblock) 微秒级。5GB/平均100字节行
//! ≈ 5000 万行 → 索引约 200MB。

use crate::parallel::parallel_map;
use crate::storage::{chunk_bytes, FileBase};
use std::io;

/// 索引扫描分块大小（默认 64MB，对齐存储层切块）。
pub const DEFAULT_CHUNK: u64 = 64 << 20;
/// 单次扫描窗口（ReadFile 路径下的 I/O 粒度）。
const SCAN_WINDOW: u64 = 8 << 20;
/// 绝对偏移锚点间隔（行数）。
pub const SUPER_BLOCK: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Cr,
    Mixed,
    /// 无换行（空文件）
    None,
}

impl LineEnding {
    pub fn name(&self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
            LineEnding::Cr => "CR",
            LineEnding::Mixed => "混合",
            LineEnding::None => "无",
        }
    }
}

#[derive(Debug)]
enum Deltas {
    U32(Vec<u32>),
    U64(Vec<u64>),
}

/// 行偏移索引。`start(0) = content_start`（跳过 BOM），
/// `deltas[i] = start(i) - start(i-1)`（deltas[0] 恒为 0），
/// `supers[b] = start(b * SUPER_BLOCK)`。
#[derive(Debug)]
pub struct LineIndex {
    file_len: u64,
    content_start: u64,
    deltas: Deltas,
    supers: Vec<u64>,
    pub line_ending: LineEnding,
}

impl LineIndex {
    pub fn build(base: &dyn FileBase, content_start: u64) -> io::Result<Self> {
        Self::build_with(base, content_start, DEFAULT_CHUNK, 0)
    }

    /// `chunk_size`：扫描分块字节大小；`threads == 0` 自动取理想线程数。
    pub fn build_with(
        base: &dyn FileBase,
        content_start: u64,
        chunk_size: u64,
        threads: usize,
    ) -> io::Result<Self> {
        let file_len = base.len();
        // 相对位置存 u32，块大小封顶 4GB
        let chunk_size = chunk_size.clamp(1 << 16, u32::MAX as u64);
        let content_start = content_start.min(file_len);
        let n_chunks =
            ((file_len - content_start).div_ceil(chunk_size)) as usize;

        // 并行扫描：每块产出块内换行符相对位置（u32，块 ≤ 4GB）
        let results: Vec<Vec<u32>> = parallel_map(n_chunks, threads, |i| -> io::Result<Vec<u32>> {
            let start = content_start + i as u64 * chunk_size;
            let end = (start + chunk_size).min(file_len);
            let mut pos = Vec::new();
            let mut scratch: Vec<u8> = Vec::new();
            let mut w = start;
            while w < end {
                let wend = (w + SCAN_WINDOW).min(end);
                let len = (wend - w) as usize;
                let buf = chunk_bytes(base, w, len, &mut scratch)?;
                let base_rel = (w - start) as u32;
                pos.extend(memchr::memchr_iter(b'\n', buf).map(move |p| base_rel + p as u32));
                w = wend;
            }
            Ok(pos)
        })
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;

        let line_ending = detect_line_ending(base, content_start);
        let (deltas32, supers, overflow) = merge_u32(&results, content_start, file_len, chunk_size);
        let deltas = if overflow {
            // 存在 >4GB 的超长行（罕见），整体升为 u64 差分
            Deltas::U64(merge_u64(&results, content_start, file_len, chunk_size))
        } else {
            Deltas::U32(deltas32)
        };

        Ok(Self {
            file_len,
            content_start,
            deltas,
            supers,
            line_ending,
        })
    }

    /// 行数（含第 0 行；文件以换行结尾时不计幽灵空行）。
    pub fn line_count(&self) -> u64 {
        match &self.deltas {
            Deltas::U32(v) => v.len() as u64,
            Deltas::U64(v) => v.len() as u64,
        }
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// 内容起点（BOM 之后）。
    pub fn content_start(&self) -> u64 {
        self.content_start
    }

    /// 索引自身内存占用估算。
    pub fn index_memory_bytes(&self) -> u64 {
        let deltas = match &self.deltas {
            Deltas::U32(v) => v.len() as u64 * 4,
            Deltas::U64(v) => v.len() as u64 * 8,
        };
        deltas + self.supers.len() as u64 * 8
    }

    fn delta(&self, i: u64) -> u64 {
        match &self.deltas {
            Deltas::U32(v) => v[i as usize] as u64,
            Deltas::U64(v) => v[i as usize],
        }
    }

    /// 第 `line` 行的起始字节偏移。O(SUPER_BLOCK)。
    pub fn start_of_line(&self, line: u64) -> u64 {
        let line = line.min(self.line_count() - 1);
        let block = (line / SUPER_BLOCK) as usize;
        let mut off = self.supers[block];
        let base_line = block as u64 * SUPER_BLOCK;
        for i in (base_line + 1)..=line {
            off += self.delta(i);
        }
        off
    }

    /// 字节偏移所在行号。O(SUPER_BLOCK)。
    pub fn line_of_offset(&self, offset: u64) -> u64 {
        let offset = offset.max(self.content_start);
        let b = self
            .supers
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let mut line = b as u64 * SUPER_BLOCK;
        let mut cur = self.supers[b];
        let n = self.line_count();
        while line + 1 < n {
            let d = self.delta(line + 1);
            if cur + d > offset {
                break;
            }
            cur += d;
            line += 1;
        }
        line
    }

    /// 第 `line` 行的字节范围 [start, end)（不含行尾换行符位置约定由调用方处理）。
    pub fn line_bounds(&self, line: u64) -> (u64, u64) {
        let start = self.start_of_line(line);
        let end = if line + 1 >= self.line_count() {
            self.file_len
        } else {
            self.start_of_line(line + 1)
        };
        (start, end)
    }
}

/// 顺序归并所有块的换行位置为 u32 差分。返回 (deltas, supers, 是否溢出)。
fn merge_u32(
    results: &[Vec<u32>],
    content_start: u64,
    file_len: u64,
    chunk_size: u64,
) -> (Vec<u32>, Vec<u64>, bool) {
    let mut deltas: Vec<u32> = Vec::new();
    deltas.push(0);
    let mut supers: Vec<u64> = vec![content_start];
    let mut prev = content_start;
    let mut line: u64 = 0;
    let mut overflow = false;
    for (ci, res) in results.iter().enumerate() {
        let chunk_start = content_start + ci as u64 * chunk_size;
        for &rel in res {
            let start = chunk_start + rel as u64 + 1;
            if start >= file_len {
                continue; // 尾部换行不产生幽灵空行
            }
            let delta = start - prev;
            if delta > u32::MAX as u64 {
                overflow = true;
            }
            deltas.push(delta as u32);
            prev = start;
            line += 1;
            if line % SUPER_BLOCK == 0 {
                supers.push(start);
            }
        }
    }
    (deltas, supers, overflow)
}

/// u64 版归并（超长行 >4GB 时启用）。
fn merge_u64(
    results: &[Vec<u32>],
    content_start: u64,
    file_len: u64,
    chunk_size: u64,
) -> Vec<u64> {
    let mut deltas: Vec<u64> = Vec::new();
    deltas.push(0);
    let mut prev = content_start;
    for (ci, res) in results.iter().enumerate() {
        let chunk_start = content_start + ci as u64 * chunk_size;
        for &rel in res {
            let start = chunk_start + rel as u64 + 1;
            if start >= file_len {
                continue;
            }
            deltas.push(start - prev);
            prev = start;
        }
    }
    deltas
}

/// 行尾风格检测：采样头部 4MB 统计 CRLF / 裸 LF / 裸 CR。
fn detect_line_ending(base: &dyn FileBase, content_start: u64) -> LineEnding {
    let n = base.len().saturating_sub(content_start).min(4 << 20) as usize;
    if n == 0 {
        return LineEnding::None;
    }
    let mut scratch: Vec<u8> = Vec::new();
    let buf = match chunk_bytes(base, content_start, n, &mut scratch) {
        Ok(b) => b,
        Err(_) => return LineEnding::Lf,
    };
    let (mut crlf, mut lf, mut cr) = (0u64, 0u64, 0u64);
    for (i, &b) in buf.iter().enumerate() {
        match b {
            b'\n' => {
                if i > 0 && buf[i - 1] == b'\r' {
                    crlf += 1;
                } else {
                    lf += 1;
                }
            }
            b'\r' => {
                if i + 1 >= buf.len() || buf[i + 1] != b'\n' {
                    cr += 1;
                }
            }
            _ => {}
        }
    }
    let kinds = [crlf > 0, lf > 0, cr > 0].into_iter().filter(|x| *x).count();
    match kinds {
        0 => LineEnding::Lf, // 单行无换行文件按 LF 处理
        _ if crlf > 0 && lf == 0 && cr == 0 => LineEnding::Crlf,
        _ if lf > 0 && crlf == 0 && cr == 0 => LineEnding::Lf,
        _ if cr > 0 && crlf == 0 && lf == 0 => LineEnding::Cr,
        _ => LineEnding::Mixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemBase;

    fn build(data: &[u8], chunk: u64) -> LineIndex {
        let base = MemBase::from_vec(data.to_vec());
        LineIndex::build_with(&base, 0, chunk, 2).unwrap()
    }

    fn text_of<'a>(idx: &LineIndex, data: &'a [u8], line: u64) -> &'a str {
        // line_bounds 含行尾换行符（显示层职责），此处裁掉后断言行内容
        let (s, e) = idx.line_bounds(line);
        let mut end = e as usize;
        while end > s as usize && (data[end - 1] == b'\n' || data[end - 1] == b'\r') {
            end -= 1;
        }
        std::str::from_utf8(&data[s as usize..end]).unwrap()
    }

    #[test]
    fn empty_file() {
        let idx = build(b"", 16);
        assert_eq!(idx.line_count(), 1);
        assert_eq!(idx.line_ending, LineEnding::None);
        assert_eq!(idx.line_bounds(0), (0, 0));
    }

    #[test]
    fn trailing_newline_no_phantom_line() {
        let data = b"a\nb\n";
        let idx = build(data, 16);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(text_of(&idx, data, 0), "a");
        assert_eq!(text_of(&idx, data, 1), "b");
    }

    #[test]
    fn no_trailing_newline() {
        let data = b"a\nb";
        let idx = build(data, 16);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(text_of(&idx, data, 1), "b");
    }

    #[test]
    fn crlf_lines() {
        let data = b"a\r\nbb\r\nccc";
        let idx = build(data, 8);
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line_ending, LineEnding::Crlf);
        // \r\n 属于上一行边界，裁剪后行内容为 "bb"
        assert_eq!(text_of(&idx, data, 1), "bb");
    }

    #[test]
    fn chunk_boundary_split() {
        // 每行 "7bytes\n" = 8 字节，chunk=16 强制边界落在行中间
        let mut data = Vec::new();
        for i in 0..10 {
            data.extend_from_slice(format!("line_{i:02}\n").as_bytes());
        }
        let idx = build(&data, 16);
        assert_eq!(idx.line_count(), 10);
        for l in 0..10u64 {
            assert_eq!(text_of(&idx, &data, l), format!("line_{l:02}"));
        }
    }

    #[test]
    fn bom_content_start() {
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"a\nb\n");
        let base = MemBase::from_vec(data.clone());
        let idx = LineIndex::build_with(&base, 3, 16, 2).unwrap();
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.start_of_line(0), 3);
        assert_eq!(idx.start_of_line(1), 5);
    }

    #[test]
    fn offset_line_roundtrip() {
        // 4096+ 行，覆盖 superblock 跨块
        let mut data = Vec::new();
        for i in 0..9000 {
            data.extend_from_slice(format!("row {i}\n").as_bytes());
        }
        let idx = build(&data, 64 << 10);
        assert_eq!(idx.line_count(), 9000);
        for l in [0u64, 1, 4095, 4096, 4097, 8192, 8999] {
            let s = idx.start_of_line(l);
            assert_eq!(idx.line_of_offset(s), l, "line {l}");
        }
        // 行中间偏移
        assert_eq!(idx.line_of_offset(idx.start_of_line(5000) + 2), 5000);
    }

    #[test]
    fn mixed_endings() {
        let idx = build(b"a\nb\r\nc\rd", 16);
        assert_eq!(idx.line_ending, LineEnding::Mixed);
    }

    #[test]
    fn single_line_no_newline() {
        let data = b"just one line";
        let idx = build(data, 16);
        assert_eq!(idx.line_count(), 1);
        assert_eq!(text_of(&idx, data, 0), "just one line");
    }
}
