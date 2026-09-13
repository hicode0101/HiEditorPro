//! 行偏移索引：并行扫描换行符，建立 `行号 → 字节偏移` 映射。
//!
//! 两级块状结构：每块约 4096 行，块内存相邻行间距（u32，超 4GB 巨行自动升 u64），
//! 块首存绝对偏移。查询 O(块大小)，编辑后局部重建（splice）只改受影响块 +
//! O(块数) 的偏移平移，与文件总行数无关。
//!
//! Phase 1（只读）经 [ByteSource] 抽象支持 mmap/分块读；
//! Phase 2（编辑）由 [crate::edit::EditableBuffer] 实现 ByteSource 后复用同一索引。

use crate::parallel::parallel_map;
use crate::storage::FileBase;
use std::io;
use std::sync::Arc;

/// 索引扫描分块大小（默认 64MB）。
pub const DEFAULT_CHUNK: u64 = 64 << 20;
/// 单次扫描窗口（ReadFile 路径下的 I/O 粒度）。
const SCAN_WINDOW: u64 = 8 << 20;
/// 每块目标行数。
pub const BLOCK_LINES: u64 = 4096;
const BLOCK_LINES_MAX: u64 = 8192;

/// 字节源抽象：编辑缓冲与只读文件基座共用同一索引。
pub trait ByteSource: Sync {
    fn len(&self) -> u64;
    fn read_at(&self, start: u64, buf: &mut [u8]) -> io::Result<()>;
    /// 零拷贝视图（可 None，如分块读后端/编辑缓冲）。
    fn slice_at(&self, start: u64, len: usize) -> Option<&[u8]> {
        let _ = (start, len);
        None
    }
}

impl<T: FileBase + ?Sized> ByteSource for T {
    fn len(&self) -> u64 {
        FileBase::len(self)
    }
    fn read_at(&self, start: u64, buf: &mut [u8]) -> io::Result<()> {
        FileBase::read_range(self, start, buf.len(), buf)
    }
    fn slice_at(&self, start: u64, len: usize) -> Option<&[u8]> {
        FileBase::slice(self, start, len)
    }
}

pub(crate) fn source_bytes<'a, T: ByteSource + ?Sized>(
    src: &'a T,
    start: u64,
    len: usize,
    scratch: &'a mut Vec<u8>,
) -> io::Result<&'a [u8]> {
    if let Some(s) = src.slice_at(start, len) {
        return Ok(s);
    }
    scratch.clear();
    scratch.resize(len, 0);
    src.read_at(start, scratch)?;
    Ok(&scratch[..len])
}

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

/// 块内行间距序列。gaps[i] = 本块第 i+1 行起点 − 第 i 行起点。
/// Block 以 Arc<Deltas> 共享：编辑拼接时块整体 move 为 O(1)，只有边界块写时复制。
#[derive(Debug, Clone)]
enum Deltas {
    U32(Vec<u32>),
    U64(Vec<u64>),
}

impl Deltas {
    fn len(&self) -> usize {
        match self {
            Deltas::U32(v) => v.len(),
            Deltas::U64(v) => v.len(),
        }
    }
    fn get(&self, i: usize) -> u64 {
        match self {
            Deltas::U32(v) => v[i] as u64,
            Deltas::U64(v) => v[i],
        }
    }
    fn from_diffs(starts: &[u64]) -> Deltas {
        // starts 为块内连续行起点；宽度按"差分"判断（绝对起点可以很大而间距很小）
        if starts.windows(2).all(|w| w[1] - w[0] <= u32::MAX as u64) {
            Deltas::U32(starts.windows(2).map(|w| (w[1] - w[0]) as u32).collect())
        } else {
            Deltas::U64(starts.windows(2).map(|w| w[1] - w[0]).collect())
        }
    }
    fn from_gaps(vals: &[u64]) -> Deltas {
        if vals.iter().all(|&d| d <= u32::MAX as u64) {
            Deltas::U32(vals.iter().map(|&d| d as u32).collect())
        } else {
            Deltas::U64(vals.to_vec())
        }
    }
}

#[derive(Debug, Clone)]
struct Block {
    /// 本块首行的绝对字节偏移
    start_offset: u64,
    /// 行间距：gaps[i] = 第 i+1 行起点 − 第 i 行起点；块行数 = gaps.len() + 1
    deltas: Arc<Deltas>,
}

impl Block {
    fn line_count(&self) -> u64 {
        self.deltas.len() as u64 + 1
    }
}

/// 行偏移索引。`start(0) = content_start`（跳过 BOM）。
#[derive(Debug, Clone)]
pub struct LineIndex {
    blocks: Vec<Block>,
    /// cum_lines[b] = 前 b 块行数之和（len = blocks.len() + 1）
    cum_lines: Vec<u64>,
    file_len: u64,
    content_start: u64,
    pub line_ending: LineEnding,
}

impl LineIndex {
    pub fn build<T: ByteSource + ?Sized>(src: &T, content_start: u64) -> io::Result<Self> {
        Self::build_with(src, content_start, DEFAULT_CHUNK, 0)
    }

    /// `chunk_size`：扫描分块字节大小；`threads == 0` 自动取理想线程数。
    /// 渐进式构建（首屏先行/顺序读盘）请改用 [LineIndexBuilder]。
    pub fn build_with<T: ByteSource + ?Sized>(
        src: &T,
        content_start: u64,
        chunk_size: u64,
        threads: usize,
    ) -> io::Result<Self> {
        let file_len = src.len();
        let chunk_size = chunk_size.clamp(1 << 16, u32::MAX as u64);
        let content_start = content_start.min(file_len);
        let n_chunks = ((file_len - content_start).div_ceil(chunk_size)) as usize;

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
                let buf = source_bytes(src, w, len, &mut scratch)?;
                let base_rel = (w - start) as u32;
                pos.extend(memchr::memchr_iter(b'\n', buf).map(move |p| base_rel + p as u32));
                w = wend;
            }
            Ok(pos)
        })
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;

        let line_ending = detect_line_ending(src, content_start);
        let (blocks, overflow) = merge_blocks_u32(&results, content_start, file_len, chunk_size);
        let blocks = if overflow {
            merge_blocks_u64(&results, content_start, file_len, chunk_size)
        } else {
            blocks
        };

        let mut idx = Self {
            blocks,
            cum_lines: Vec::new(),
            file_len,
            content_start,
            line_ending,
        };
        idx.rebuild_cum();
        Ok(idx)
    }

    /// 空索引（编辑缓冲构造时占位，随后由 build_with 重建）。
    pub fn empty() -> Self {
        Self {
            blocks: Vec::new(),
            cum_lines: vec![0],
            file_len: 0,
            content_start: 0,
            line_ending: LineEnding::None,
        }
    }

    fn rebuild_cum(&mut self) {
        self.cum_lines.clear();
        self.cum_lines.push(0);
        let mut acc = 0u64;
        for b in &self.blocks {
            acc += b.line_count();
            self.cum_lines.push(acc);
        }
    }

    fn block_for_line(&self, line: u64) -> (usize, u64) {
        let b = self.cum_lines.partition_point(|&c| c <= line) - 1;
        (b, line - self.cum_lines[b])
    }

    /// 行数（含第 0 行；文件以换行结尾时不计幽灵空行）。
    pub fn line_count(&self) -> u64 {
        self.cum_lines.last().copied().unwrap_or(0)
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// 内容起点（BOM 之后；编辑缓冲恒为 0）。
    pub fn content_start(&self) -> u64 {
        self.content_start
    }

    /// 索引自身内存占用估算。
    pub fn index_memory_bytes(&self) -> u64 {
        let deltas: u64 = self
            .blocks
            .iter()
            .map(|b| match &*b.deltas {
                Deltas::U32(v) => v.capacity() as u64 * 4,
                Deltas::U64(v) => v.capacity() as u64 * 8,
            })
            .sum();
        deltas + self.blocks.capacity() as u64 * std::mem::size_of::<Block>() as u64
            + self.cum_lines.capacity() as u64 * 8
    }

    /// 第 `line` 行的起始字节偏移。O(块大小)。
    pub fn start_of_line(&self, line: u64) -> u64 {
        // 占位索引（blocks 为空，首屏先行阶段）→ 恒返回 0，防御误用
        if self.blocks.is_empty() {
            return 0;
        }
        let n = self.line_count();
        let line = line.min(n.saturating_sub(1));
        let (b, local) = self.block_for_line(line);
        let blk = &self.blocks[b];
        let mut off = blk.start_offset;
        for i in 0..local as usize {
            off += blk.deltas.get(i);
        }
        off
    }

    /// 字节偏移所在行号。O(块数 + 块大小)。
    pub fn line_of_offset(&self, offset: u64) -> u64 {
        let n = self.line_count();
        if n == 0 {
            return 0;
        }
        let offset = offset.max(self.content_start).min(self.file_len);
        let b = self
            .blocks
            .partition_point(|blk| blk.start_offset <= offset)
            .saturating_sub(1);
        let blk = &self.blocks[b];
        let mut line = self.cum_lines[b];
        let mut cur = blk.start_offset;
        for i in 0..blk.deltas.len() {
            let d = blk.deltas.get(i);
            if cur + d > offset {
                break;
            }
            cur += d;
            line += 1;
        }
        line.min(n - 1)
    }

    /// 第 `line` 行的字节范围 [start, end)（含行尾换行符，裁剪是显示层职责）。
    pub fn line_bounds(&self, line: u64) -> (u64, u64) {
        let start = self.start_of_line(line);
        let end = if line + 1 >= self.line_count() {
            self.file_len
        } else {
            self.start_of_line(line + 1)
        };
        (start, end)
    }

    /// 编辑后局部重建：旧行 [first_line, first_line+old_lines) 被新行
    /// （起点为 new_starts，M ≥ 1）替换；region_end = 紧随区域的未变行新起点
    /// （区域到 EOF 时 = new_file_len）；其后所有字节平移 len_delta。
    ///
    /// 实现：前段/尾段整块 move（Block 内为 Arc，移动廉价），仅边界两块
    /// 复制间距数组 → 单次编辑成本 ≈ O(块数) 的指针操作 + 受影响行重扫。
    pub(crate) fn apply_splice(&mut self, sp: &Splice) {
        let n_old = self.line_count();
        let l0 = sp.first_line.min(n_old);
        let m = sp.new_starts.len();

        // 1. 旧块坐标系定位
        let (bi, local) = self.block_for_line(l0.min(n_old.saturating_sub(1)));
        let old_keep = l0 + sp.old_lines;
        let has_tail = old_keep < n_old;
        let (bi2, local2) = if has_tail {
            let (b, l) = self.block_for_line(old_keep);
            (b, l as usize)
        } else {
            (usize::MAX, 0usize)
        };

        let mut new_blocks: Vec<Block> = Vec::with_capacity(self.blocks.len() + 4);

        // 2. 前段 [0, bi)：整体 move
        new_blocks.extend(self.blocks.drain(..bi));

        // 3. 中段（旧块号 bi..=mid_end）：bi 截头、bi2 留尾、其余属于删除区丢弃。
        //    tail 片段属于"编辑点之后的旧内容"，先收集，在 mid 之后入列。
        let last_old_block = self.blocks.len() + bi - 1; // drain 后换算回旧块号
        let mid_end = if has_tail { bi2 } else { last_old_block };
        let mut tail_pieces: Vec<Block> = Vec::new();
        for idx in bi..=mid_end {
            let at = idx - bi;
            let blk = &self.blocks[at];
            if idx == bi && local > 0 {
                // 截头（保留块内前 local 行；local−1 个间距）
                let keep = (local - 1) as usize;
                let head_deltas = match &*blk.deltas {
                    Deltas::U32(v) => Deltas::U32(v[..keep].to_vec()),
                    Deltas::U64(v) => Deltas::U64(v[..keep].to_vec()),
                };
                new_blocks.push(Block {
                    start_offset: blk.start_offset,
                    deltas: Arc::new(head_deltas),
                });
                if has_tail && bi2 == idx {
                    tail_pieces.push(Block {
                        start_offset: sp.region_end,
                        deltas: Arc::new(shift_deltas(&blk.deltas, local2)),
                    });
                }
            } else if has_tail && idx == bi2 {
                tail_pieces.push(Block {
                    start_offset: sp.region_end,
                    deltas: Arc::new(shift_deltas(&blk.deltas, local2)),
                });
            }
            // 其余整块落入删除区 → 丢弃
        }

        // 4. mid：新行 [l0, l0+m)，按 BLOCK_LINES_MAX 切块
        {
            let starts = &sp.new_starts;
            let mut i = 0usize;
            while i < m {
                let take = (BLOCK_LINES_MAX as usize).min(m - i);
                new_blocks.push(Block {
                    start_offset: starts[i],
                    deltas: Arc::new(Deltas::from_diffs(&starts[i..i + take])),
                });
                i += take;
            }
        }
        new_blocks.extend(tail_pieces);

        // 5. 尾段（mid_end+1..）：原样 move + 偏移平移
        for idx in (mid_end + 1)..(self.blocks.len() + bi) {
            let mut b = self.blocks[idx - bi].clone();
            b.start_offset = (b.start_offset as i64 + sp.len_delta).max(0) as u64;
            new_blocks.push(b);
        }

        self.blocks = new_blocks;
        self.file_len = sp.new_file_len;
        self.rebuild_cum();
    }
}

fn shift_deltas(d: &Deltas, skip: usize) -> Deltas {
    match d {
        Deltas::U32(v) => Deltas::U32(v[skip..].to_vec()),
        Deltas::U64(v) => Deltas::U64(v[skip..].to_vec()),
    }
}

/// 编辑拼接参数（crate 内部）。
pub(crate) struct Splice {
    pub first_line: u64,
    pub new_starts: Vec<u64>,
    pub region_end: u64,
    pub old_lines: u64,
    pub len_delta: i64,
    pub new_file_len: u64,
}

/// 顺序归并所有块的换行位置为块状索引（u32 路径）。
fn merge_blocks_u32(
    results: &[Vec<u32>],
    content_start: u64,
    file_len: u64,
    chunk_size: u64,
) -> (Vec<Block>, bool) {
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    let mut cur_start = content_start;
    let mut prev = content_start;
    // 隐式第 0 行（起点 = content_start）已就位，扫描只产出第 1 行起的起点
    let mut in_block: u64 = 1;
    let mut overflow = false;
    for (ci, res) in results.iter().enumerate() {
        let chunk_start = content_start + ci as u64 * chunk_size;
        for &rel in res {
            let start = chunk_start + rel as u64 + 1;
            if start >= file_len {
                continue; // 尾部换行不产生幽灵空行
            }
            if in_block == 0 {
                // 新块首行：块首绝对起点 = 本行起点
                cur_start = start;
            } else {
                let delta = start - prev;
                if delta > u32::MAX as u64 {
                    overflow = true;
                }
                cur.push(delta as u32);
            }
            prev = start;
            in_block += 1;
            if in_block >= BLOCK_LINES {
                blocks.push(Block {
                    start_offset: cur_start,
                    deltas: Arc::new(Deltas::U32(std::mem::take(&mut cur))),
                });
                cur = Vec::new();
                in_block = 0;
            }
        }
    }
    if in_block > 0 || blocks.is_empty() {
        blocks.push(Block {
            start_offset: cur_start,
            deltas: Arc::new(Deltas::U32(cur)),
        });
    }
    (blocks, overflow)
}

/// u64 路径（存在 >4GB 巨行时）。
fn merge_blocks_u64(
    results: &[Vec<u32>],
    content_start: u64,
    file_len: u64,
    chunk_size: u64,
) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Vec<u64> = Vec::new();
    let mut cur_start = content_start;
    let mut prev = content_start;
    // 隐式第 0 行（起点 = content_start）已就位
    let mut in_block: u64 = 1;
    for (ci, res) in results.iter().enumerate() {
        let chunk_start = content_start + ci as u64 * chunk_size;
        for &rel in res {
            let start = chunk_start + rel as u64 + 1;
            if start >= file_len {
                continue;
            }
            if in_block == 0 {
                cur_start = start;
            } else {
                cur.push(start - prev);
            }
            prev = start;
            in_block += 1;
            if in_block >= BLOCK_LINES {
                blocks.push(Block {
                    start_offset: cur_start,
                    deltas: Arc::new(Deltas::U64(std::mem::take(&mut cur))),
                });
                cur = Vec::new();
                in_block = 0;
            }
        }
    }
    if in_block > 0 || blocks.is_empty() {
        blocks.push(Block {
            start_offset: cur_start,
            deltas: Arc::new(Deltas::U64(cur)),
        });
    }
    blocks
}

/// 行尾风格检测：采样头部 4MB 统计 CRLF / 裸 LF / 裸 CR。
fn detect_line_ending<T: ByteSource + ?Sized>(src: &T, content_start: u64) -> LineEnding {
    let n = src.len().saturating_sub(content_start).min(4 << 20) as usize;
    if n == 0 {
        return LineEnding::None;
    }
    let mut scratch: Vec<u8> = Vec::new();
    let buf = match source_bytes(src, content_start, n, &mut scratch) {
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

/// 渐进式索引构建器：按文件顺序推送行起点，随时可查已覆盖行数；
/// finish 产出与一次性构建等价的 [LineIndex]。
///
/// 用于"首屏先行"：GUI 后台线程顺序读盘（机械盘最优 I/O 模式），
/// 每扫完一段即推送，已覆盖前缀立即可浏览，全部扫完后解锁编辑/搜索。
pub struct LineIndexBuilder {
    blocks: Vec<Block>,
    /// 当前未关闭块的行间距（u64 暂存，关闭块时收窄为 u32）
    cur: Vec<u64>,
    cur_start: u64,
    prev: u64,
    /// 当前块已含行数（含首行）；0 = 待开新块
    in_block: u64,
    lines: u64,
    content_start: u64,
}

impl LineIndexBuilder {
    /// `content_start` 为第 0 行起点（BOM 之后）；隐式第 0 行已就位。
    pub fn new(content_start: u64) -> Self {
        Self {
            blocks: Vec::new(),
            cur: Vec::new(),
            cur_start: content_start,
            prev: content_start,
            in_block: 1,
            lines: 1,
            content_start,
        }
    }

    /// 推送一个行起点（必须按文件顺序递增；文件末尾的幽灵行起点由调用方跳过）。
    pub fn push_line_start(&mut self, start: u64) {
        if self.in_block == 0 {
            // 新块首行：绝对起点记录在本块，间距从下一行开始
            self.cur_start = start;
            self.in_block = 1;
        } else {
            self.cur.push(start - self.prev);
            self.in_block += 1;
        }
        self.prev = start;
        self.lines += 1;
        if self.in_block >= BLOCK_LINES {
            self.blocks.push(Block {
                start_offset: self.cur_start,
                deltas: Arc::new(Deltas::from_gaps(&self.cur)),
            });
            self.cur = Vec::new();
            self.in_block = 0;
        }
    }

    pub fn push_line_starts(&mut self, starts: &[u64]) {
        for &s in starts {
            self.push_line_start(s);
        }
    }

    /// 已覆盖行数（含第 0 行；扫描期间的值单调递增）。
    pub fn covered_lines(&self) -> u64 {
        self.lines
    }

    /// 完成构建。`file_len` 为虚拟内容总长。
    pub fn finish(self, file_len: u64) -> LineIndex {
        let mut blocks = self.blocks;
        if self.in_block > 0 || blocks.is_empty() {
            blocks.push(Block {
                start_offset: self.cur_start,
                deltas: Arc::new(Deltas::from_gaps(&self.cur)),
            });
        }
        let mut idx = LineIndex {
            blocks,
            cum_lines: Vec::new(),
            file_len,
            content_start: self.content_start,
            line_ending: LineEnding::None,
        };
        idx.rebuild_cum();
        idx
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use crate::storage::MemBase;

    #[test]
    fn builder_matches_one_shot_build() {
        // 与一次性 build 对拍：跨块、多行、含尾部换行
        let mut data = Vec::new();
        for i in 0..(BLOCK_LINES as usize * 2 + 7) {
            data.extend_from_slice(format!("row {i}\n").as_bytes());
        }
        let file_len = data.len() as u64;
        let base = MemBase::from_vec(data.clone());
        let one_shot = LineIndex::build_with(&base, 0, 64 << 10, 2).unwrap();

        let mut b = LineIndexBuilder::new(0);
        for (i, byte) in data.iter().enumerate() {
            if *byte == b'\n' && (i as u64) + 1 < file_len {
                b.push_line_start(i as u64 + 1);
            }
        }
        let progressive = b.finish(file_len);

        assert_eq!(progressive.line_count(), one_shot.line_count());
        for l in [0u64, 1, 4095, 4096, 8192, one_shot.line_count() - 1] {
            assert_eq!(progressive.start_of_line(l), one_shot.start_of_line(l));
            assert_eq!(progressive.line_of_offset(one_shot.start_of_line(l)), l);
        }
    }

    #[test]
    fn builder_empty_and_trailing_newline() {
        let b = LineIndexBuilder::new(0);
        let idx = b.finish(0);
        assert_eq!(idx.line_count(), 1);

        let data = b"a\nb\n";
        let mut b = LineIndexBuilder::new(0);
        for (i, byte) in data.iter().enumerate() {
            if *byte == b'\n' && i + 1 < data.len() {
                b.push_line_start(i as u64 + 1);
            }
        }
        let idx = b.finish(data.len() as u64);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.line_bounds(1), (2, 4));
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
        // 每行 "line_NN\n" = 8 字节，chunk=16 强制边界落在行中间
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
        // 4096+ 行，覆盖跨块
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

    #[test]
    fn block_boundaries() {
        // 恰好多块 + 每块边界行查询（BLOCK_LINES = 4096）
        let mut data = Vec::new();
        for i in 0..(BLOCK_LINES as usize * 2 + 5) {
            data.extend_from_slice(format!("l{i}\n").as_bytes());
        }
        let idx = build(&data, 1 << 20);
        let n = BLOCK_LINES * 2 + 5;
        assert_eq!(idx.line_count(), n);
        for l in [0u64, BLOCK_LINES - 1, BLOCK_LINES, BLOCK_LINES * 2, n - 1] {
            assert_eq!(idx.line_of_offset(idx.start_of_line(l)), l);
        }
    }
}
