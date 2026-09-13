//! EditableBuffer — 编辑增量层。
//!
//! 设计（对应 ARCHITECTURE.md「编辑层与文件层分离」）：
//! - 文件基座永远只读；全部编辑以增量片段（piece table）叠加：
//!   `pieces` 描述虚拟字节流 = 只读基座段 + 追加缓冲（add_buf）段的序列
//! - 插入 = add_buf 追加 + piece 切分；删除 = piece 收缩/移除（基座字节不动）
//! - 行索引随每次编辑**局部重建**（见 lineindex::apply_splice），
//!   成本 ≈ 受影响行区间 + O(块数)，与文件总大小无关
//! - 撤销栈存命令（插入记 add_buf 引用，删除记字节快照），redo 对称
//! - 保存 = 按 piece 流式写出临时文件 → 释放基座映射 → 原子替换 → 重新建基座
//!
//! 限制（v0.2 明确）：
//! - 纯追加 add_buf，不回收空洞（长时间重度编辑后由"另存"紧凑化）
//! - pieces 为扁平 Vec，插入删除 O(piece 数)；万级片段内无感，极端场景二期换树
//! - 保存要求基座 Arc 独占（GUI 后台搜索持有时请另存为目标路径以外的文件或稍后重试）

use crate::document::OpenOptions;
use crate::lineindex::{ByteSource, LineIndex, LineEnding, Splice, DEFAULT_CHUNK};
use crate::storage::{open_backend, Encoding, StorageConfig};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 遍历控制流。
enum Flow {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy)]
enum Src {
    /// 只读基座段（绝对基座偏移，已含 BOM 跳过）
    Base(u64),
    /// 追加缓冲段
    Add(u64),
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    src: Src,
    len: u64,
}

/// 撤销/重做命令。
#[derive(Debug, Clone)]
enum EditCmd {
    /// 插入：内容位于 add_buf[add_off..add_off+len]（append-only，redo 直接引用）
    Insert { pos: u64, add_off: u64, len: u64 },
    /// 删除：字节快照（undo 重插，redo 重删）
    Delete { pos: u64, bytes: Vec<u8> },
    /// 全部替换：整批一个撤销单元（piece 表前后快照）
    ReplaceAllSnap {
        before: (Vec<Piece>, Vec<u64>, u64),
        after: (Vec<Piece>, Vec<u64>, u64),
    },
}

/// 编辑结果信息（调用方/GUI 失效区间用）。
#[derive(Debug, Clone, Copy)]
pub struct EditInfo {
    pub pos: u64,
    pub old_len: u64,
    pub new_len: u64,
    /// 受影响的首行
    pub first_line: u64,
    /// 行数变化（新 − 旧）
    pub lines_delta: i64,
}

pub struct EditableBuffer {
    path: PathBuf,
    base: Option<Arc<dyn crate::storage::FileBase>>,
    backend: &'static str,
    encoding: Encoding,
    add_buf: Vec<u8>,
    pieces: Vec<Piece>,
    /// cum[i] = pieces[..i] 总长；len = pieces.len() + 1
    cum: Vec<u64>,
    vlen: u64,
    /// Arc 包裹：GUI 后台搜索可持有快照引用，编辑经 make_mut 写时复制
    index: Arc<LineIndex>,
    /// 打开时采样的行尾风格（状态栏显示用）
    line_ending_hint: LineEnding,
    /// 打开时建索引耗时
    index_ms: u128,
    undo_stack: Vec<EditCmd>,
    redo_stack: Vec<EditCmd>,
    dirty: bool,
}

impl ByteSource for EditableBuffer {
    fn len(&self) -> u64 {
        self.vlen
    }
    fn read_at(&self, start: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read(start, buf)
    }
}

impl EditableBuffer {
    /// 从文件打开（含全量行索引；CLI/测试入口）。
    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<Self> {
        let mut eb = Self::open_head(path, opts)?;
        let t = std::time::Instant::now();
        let idx = LineIndex::build_with(&eb, 0, DEFAULT_CHUNK, opts.threads)?;
        eb.index_ms = t.elapsed().as_millis();
        eb.index = Arc::new(idx);
        Ok(eb)
    }

    /// 首屏先行：只挂基座不建索引（index 为空占位，`has_index() == false`），
    /// 立即可浏览文件头部；随后由 [`Self::set_index`] 注入后台建好的索引。
    pub fn open_head(path: &Path, opts: &OpenOptions) -> io::Result<Self> {
        let (base_box, backend) = open_backend(path, &opts.storage)?;
        let base: Arc<dyn crate::storage::FileBase> = Arc::from(base_box);
        let sample_len = base.len().min(64 << 10) as usize;
        let mut head = vec![0u8; sample_len];
        if sample_len > 0 {
            base.read_range(0, sample_len, &mut head)?;
        }
        let encoding = crate::storage::detect_encoding(&head);
        if !encoding.indexable() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("编码 {} 暂不可编辑（v0.2 支持 UTF-8/ASCII）", encoding.name()),
            ));
        }
        let content_start = encoding.bom_len();
        let vlen = base.len() - content_start;
        // 行尾风格采样（状态栏显示）
        let (mut crlf, mut lf, mut cr) = (0u32, 0u32, 0u32);
        for (i, &b) in head.iter().enumerate() {
            match b {
                b'\n' => {
                    if i > 0 && head[i - 1] == b'\r' { crlf += 1 } else { lf += 1 }
                }
                b'\r' => {
                    if i + 1 >= head.len() || head[i + 1] != b'\n' { cr += 1 }
                }
                _ => {}
            }
        }
        let line_ending_hint = match (crlf > 0, lf > 0, cr > 0) {
            (true, false, false) => LineEnding::Crlf,
            (false, true, false) => LineEnding::Lf,
            (false, false, true) => LineEnding::Cr,
            (false, false, false) => LineEnding::None,
            _ => LineEnding::Mixed,
        };
        let eb = Self {
            path: path.to_path_buf(),
            base: Some(base),
            backend,
            encoding,
            add_buf: Vec::new(),
            pieces: if vlen > 0 {
                vec![Piece { src: Src::Base(content_start), len: vlen }]
            } else {
                Vec::new()
            },
            cum: vec![0, vlen],
            vlen,
            index: Arc::new(LineIndex::empty()),
            line_ending_hint,
            index_ms: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dirty: false,
        };
        Ok(eb)
    }

    /// 注入后台构建完成的行索引（首屏先行流程的第二阶段）。
    pub fn set_index(&mut self, index: LineIndex) {
        self.index = Arc::new(index);
    }

    /// 行索引是否就绪（open_head 后、set_index 前为 false）。
    pub fn has_index(&self) -> bool {
        self.index.line_count() > 0
    }

    /// 测试/内存文档入口：以字节构造（基座为内存）。
    pub fn from_vec(data: Vec<u8>) -> Self {
        let vlen = data.len() as u64;
        let mut eb = Self {
            path: PathBuf::new(),
            base: Some(Arc::new(crate::storage::MemBase::from_vec(data))),
            backend: "MemBase(内存)",
            encoding: crate::storage::Encoding::Ascii,
            add_buf: Vec::new(),
            pieces: if vlen > 0 {
                vec![Piece { src: Src::Base(0), len: vlen }]
            } else {
                Vec::new()
            },
            cum: vec![0, vlen],
            vlen,
            index: Arc::new(LineIndex::empty()),
            line_ending_hint: LineEnding::Lf,
            index_ms: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dirty: false,
        };
        eb.index = Arc::new(LineIndex::build_with(&eb, 0, 64 << 10, 2).unwrap());
        eb
    }

    // ---------- 只读查询 ----------

    pub fn len(&self) -> u64 {
        self.vlen
    }

    pub fn is_empty(&self) -> bool {
        self.vlen == 0
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn index(&self) -> &LineIndex {
        &self.index
    }

    /// 建索引耗时（打开时统计，GUI 状态栏用）。
    pub fn index_build_ms(&self) -> u128 {
        self.index_ms
    }

    /// 行尾风格（打开时头部采样）。
    pub fn line_ending_hint(&self) -> LineEnding {
        self.line_ending_hint
    }

    /// 基座只读引用（零拷贝读取用）。
    pub fn base_for_read(&self) -> Option<&dyn crate::storage::FileBase> {
        self.base.as_deref()
    }

    /// 索引快照引用（后台搜索用；编辑经写时复制不影响已持有的快照）。
    pub fn index_arc(&self) -> Arc<LineIndex> {
        Arc::clone(&self.index)
    }

    /// 基座引用（后台搜索用）。
    pub fn base_arc(&self) -> Option<Arc<dyn crate::storage::FileBase>> {
        self.base.clone()
    }

    pub fn line_count(&self) -> u64 {
        self.index.line_count()
    }

    pub fn start_of_line(&self, line: u64) -> u64 {
        self.index.start_of_line(line)
    }

    pub fn line_bounds(&self, line: u64) -> (u64, u64) {
        self.index.line_bounds(line)
    }

    pub fn line_of_offset(&self, pos: u64) -> u64 {
        self.index.line_of_offset(pos)
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// 虚拟字节流读取（跨 piece；buf.len() 即请求长度）。
    pub fn read(&self, start: u64, buf: &mut [u8]) -> io::Result<()> {
        if start + buf.len() as u64 > self.vlen {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("read 越界: start={start} len={} vlen={}", buf.len(), self.vlen),
            ));
        }
        let mut done = 0usize;
        let mut pi = self.cum.partition_point(|&c| c <= start) - 1;
        let mut local = start - self.cum[pi];
        while done < buf.len() {
            let p = self.pieces[pi];
            let avail = (p.len - local) as usize;
            let take = avail.min(buf.len() - done);
            match p.src {
                Src::Base(off) => {
                    let base = self.base.as_ref().expect("Base piece 无基座");
                    if let Some(s) = base.slice(off + local, take) {
                        buf[done..done + take].copy_from_slice(s);
                    } else {
                        base.read_range(off + local, take, &mut buf[done..done + take])?;
                    }
                }
                Src::Add(off) => {
                    let s = &self.add_buf[(off + local) as usize..(off + local + take as u64) as usize];
                    buf[done..done + take].copy_from_slice(s);
                }
            }
            done += take;
            local = 0;
            pi += 1;
        }
        Ok(())
    }

    /// 逐 piece 零拷贝遍历 [start, end)，供扫描（换行定位等）使用。
    fn for_each_chunk(&self, start: u64, end: u64, mut f: impl FnMut(&[u8]) -> Flow) -> io::Result<()> {
        if end <= start {
            return Ok(());
        }
        let mut pi = self.cum.partition_point(|&c| c <= start) - 1;
        let mut local = start - self.cum[pi];
        let mut scratch: Vec<u8> = Vec::new();
        loop {
            let p = self.pieces[pi];
            let p_start = self.cum[pi] + local;
            let take = (p.len - local).min(end - p_start);
            match p.src {
                Src::Base(off) => {
                    let base = self.base.as_ref().expect("Base piece 无基座");
                    if let Some(s) = base.slice(off + local, take as usize) {
                        if let Flow::Stop = f(s) { return Ok(()); }
                    } else {
                        let s = crate::lineindex::source_bytes(
                            base.as_ref(),
                            off + local,
                            take as usize,
                            &mut scratch,
                        )?;
                        if let Flow::Stop = f(s) { return Ok(()); }
                    }
                }
                Src::Add(off) => {
                    let s = &self.add_buf[(off + local) as usize..(off + local + take) as usize];
                    if let Flow::Stop = f(s) { return Ok(()); }
                }
            }
            if p_start + take >= end {
                break;
            }
            pi += 1;
            local = 0;
        }
        Ok(())
    }

    // ---------- 编辑 ----------

    /// 插入字节到虚拟位置 pos（自动夹紧到 EOF）。
    pub fn insert(&mut self, pos: u64, bytes: &[u8]) -> io::Result<EditInfo> {
        let pos = pos.min(self.vlen);
        self.redo_stack.clear();
        let add_off = self.add_buf.len() as u64;
        let len = bytes.len() as u64;
        self.add_buf.extend_from_slice(bytes);
        self.piece_insert(pos, add_off, len);
        let info = self.index_update(pos, 0, len)?;
        self.undo_stack.push(EditCmd::Insert { pos, add_off, len });
        self.dirty = true;
        Ok(info)
    }

    /// 删除 [pos, pos+len)（自动夹紧）。
    pub fn delete(&mut self, pos: u64, len: u64) -> io::Result<EditInfo> {
        let pos = pos.min(self.vlen);
        let len = len.min(self.vlen - pos);
        self.redo_stack.clear();
        let mut bytes = vec![0u8; len as usize];
        if len > 0 {
            self.read(pos, &mut bytes)?;
        }
        self.piece_remove(pos, len);
        let info = self.index_update(pos, len, 0)?;
        self.undo_stack.push(EditCmd::Delete { pos, bytes });
        self.dirty = true;
        Ok(info)
    }

    /// 替换 = 删除 + 插入（两条撤销命令，undo 逆序回放）。
    pub fn replace(&mut self, pos: u64, len: u64, bytes: &[u8]) -> io::Result<EditInfo> {
        let pos = pos.min(self.vlen);
        let len = len.min(self.vlen - pos);
        let d = self.delete(pos, len)?;
        let i = self.insert(pos, bytes)?;
        Ok(EditInfo {
            pos,
            old_len: len,
            new_len: bytes.len() as u64,
            first_line: d.first_line,
            lines_delta: i.lines_delta,
        })
    }

    pub fn undo(&mut self) -> bool {
        let Some(cmd) = self.undo_stack.pop() else { return false };
        match cmd.clone() {
            EditCmd::Insert { pos, add_off: _, len } => {
                self.piece_remove(pos, len);
                let _ = self.index_update(pos, len, 0);
            }
            EditCmd::Delete { pos, ref bytes } => {
                let off = self.add_buf.len() as u64;
                self.add_buf.extend_from_slice(bytes);
                self.piece_insert(pos, off, bytes.len() as u64);
                let _ = self.index_update(pos, 0, bytes.len() as u64);
            }
            EditCmd::ReplaceAllSnap { ref before, .. } => {
                self.restore_state(before);
                self.rebuild_index_full();
            }
        }
        self.redo_stack.push(cmd);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(cmd) = self.redo_stack.pop() else { return false };
        match cmd.clone() {
            EditCmd::Insert { pos, add_off, len } => {
                self.piece_insert(pos, add_off, len);
                let _ = self.index_update(pos, 0, len);
            }
            EditCmd::Delete { pos, ref bytes } => {
                self.piece_remove(pos, bytes.len() as u64);
                let _ = self.index_update(pos, bytes.len() as u64, 0);
            }
            EditCmd::ReplaceAllSnap { ref after, .. } => {
                self.restore_state(after);
                self.rebuild_index_full();
            }
        }
        self.undo_stack.push(cmd);
        true
    }

    fn restore_state(&mut self, s: &(Vec<Piece>, Vec<u64>, u64)) {
        self.pieces = s.0.clone();
        self.cum = s.1.clone();
        self.vlen = s.2;
    }

    fn rebuild_index_full(&mut self) {
        if let Ok(idx) = LineIndex::build_with(self, 0, DEFAULT_CHUNK, 0) {
            self.index = Arc::new(idx);
        }
    }

    /// 全部替换（顺序左到右、不重叠）。整批一个撤销单元。返回替换次数。
    ///
    /// 一次走查构建新 piece 序列（O(pieces + 命中数)）。
    /// 不逐个 splice：piece_insert/remove 每次都重建 cum（O(piece 总数)），
    /// 大量命中时整体退化为 O(n²) 导致界面假死。
    pub fn replace_all(
        &mut self,
        needle: &[u8],
        replacement: &[u8],
        case_insensitive: bool,
        gbk: bool,
    ) -> usize {
        let hits = self.find_all(needle, case_insensitive, gbk);
        if hits.is_empty() {
            return 0;
        }
        let before = (self.pieces.clone(), self.cum.clone(), self.vlen);

        // 替换文本一次性追加进 add_buf（append-only，不影响已有 piece 引用）
        let rlen = replacement.len() as u64;
        let mut add_offs: Vec<u64> = Vec::with_capacity(hits.len());
        for _ in 0..hits.len() {
            add_offs.push(self.add_buf.len() as u64);
            self.add_buf.extend_from_slice(replacement);
        }

        // 沿旧 piece 线性推进，交替拼出 保留段 / 替换段
        let old_pieces = std::mem::take(&mut self.pieces);
        let mut new_pieces: Vec<Piece> = Vec::with_capacity(old_pieces.len() + hits.len() * 2);
        let mut new_cum: Vec<u64> = Vec::with_capacity(old_pieces.len() + hits.len() * 2 + 1);
        new_cum.push(0);
        let mut total = 0u64;
        let (mut pi, mut poff) = (0usize, 0u64); // 旧 piece 游标
        let mut cursor = 0u64; // 已覆盖的虚拟偏移
        let emit_retain = |bound: u64,
                               old: &[Piece],
                               pi: &mut usize,
                               poff: &mut u64,
                               cursor: &mut u64,
                               out: &mut Vec<Piece>,
                               cum: &mut Vec<u64>,
                               total: &mut u64| {
            while *cursor < bound {
                let p = old[*pi];
                let take = (p.len - *poff).min(bound - *cursor);
                cum.push({ *total += take; *total });
                out.push(Piece { src: Self::shift_src(p.src, *poff), len: take });
                *poff += take;
                *cursor += take;
                if *poff == p.len {
                    *pi += 1;
                    *poff = 0;
                }
            }
        };
        for (i, &(pos, hlen)) in hits.iter().enumerate() {
            emit_retain(
                pos, &old_pieces, &mut pi, &mut poff, &mut cursor,
                &mut new_pieces, &mut new_cum, &mut total,
            );
            // 游标跳过被替换的源字节 [pos, pos+hlen)，与新虚拟偏移保持同步
            let mut skip = hlen;
            while skip > 0 {
                let p = old_pieces[pi];
                let take = (p.len - poff).min(skip);
                poff += take;
                skip -= take;
                if poff == p.len {
                    pi += 1;
                    poff = 0;
                }
            }
            cursor = pos + hlen;
            new_cum.push({ total += rlen; total });
            new_pieces.push(Piece { src: Src::Add(add_offs[i]), len: rlen });
        }
        emit_retain(
            self.vlen, &old_pieces, &mut pi, &mut poff, &mut cursor,
            &mut new_pieces, &mut new_cum, &mut total,
        );

        let removed: u64 = hits.iter().map(|&(_, l)| l).sum();
        self.pieces = new_pieces;
        self.cum = new_cum;
        self.vlen = before.2 - removed + hits.len() as u64 * rlen;
        self.dirty = true;
        self.rebuild_index_full();
        let after = (self.pieces.clone(), self.cum.clone(), self.vlen);
        self.undo_stack.push(EditCmd::ReplaceAllSnap { before, after });
        self.redo_stack.clear();
        hits.len()
    }

    /// 全文查找全部命中（虚拟偏移, 长度；顺序左到右不重叠）。
    /// `gbk = true` 时大小写折叠跳过 GBK 双字节序列的次字节。
    pub fn find_all(&self, needle: &[u8], case_insensitive: bool, gbk: bool) -> Vec<(u64, u64)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let eff = if case_insensitive {
            let mut n = needle.to_vec();
            crate::storage::encoding::fold_ascii(&mut n, gbk);
            n
        } else {
            needle.to_vec()
        };
        let finder = memchr::memmem::Finder::new(&eff);
        let nlen = eff.len() as u64;
        let win = 8 << 20;
        let step_back = nlen.saturating_sub(1);
        let mut out: Vec<(u64, u64)> = Vec::new();
        let mut w = 0u64;
        while w < self.vlen {
            let n = win.min(self.vlen - w) as usize;
            let mut buf = vec![0u8; n];
            if self.read(w, &mut buf).is_err() {
                break;
            }
            if case_insensitive {
                crate::storage::encoding::fold_ascii(&mut buf, gbk);
            }
            for rel in finder.find_iter(&buf) {
                let abs = w + rel as u64;
                if let Some(&(lp, ll)) = out.last() {
                    if abs < lp + ll {
                        continue;
                    }
                }
                out.push((abs, nlen));
            }
            if n as u64 == win {
                // 还有后续窗口：回退重叠区，覆盖跨窗口命中
                w += n as u64 - step_back;
            } else {
                w = self.vlen; // 已是最后一个窗口
            }
        }
        out
    }

    // ---------- piece 表维护 ----------

    fn locate(&self, pos: u64) -> (usize, u64) {
        debug_assert!(pos < self.vlen, "locate 越界: pos={pos} vlen={}", self.vlen);
        let pi = self.cum.partition_point(|&c| c <= pos) - 1;
        (pi, pos - self.cum[pi])
    }

    fn rebuild_cum(&mut self) {
        self.cum.clear();
        self.cum.push(0);
        let mut acc = 0u64;
        for p in &self.pieces {
            acc += p.len;
            self.cum.push(acc);
        }
        debug_assert_eq!(acc, self.vlen, "piece 总长与 vlen 不一致");
    }

    fn shift_src(src: Src, by: u64) -> Src {
        match src {
            Src::Base(off) => Src::Base(off + by),
            Src::Add(off) => Src::Add(off + by),
        }
    }

    fn piece_insert(&mut self, pos: u64, add_off: u64, len: u64) {
        if self.pieces.is_empty() {
            self.pieces.push(Piece { src: Src::Add(add_off), len });
            self.vlen += len;
            self.rebuild_cum();
            return;
        }
        if pos >= self.vlen {
            self.pieces.push(Piece { src: Src::Add(add_off), len });
        } else {
            let (pi, local) = self.locate(pos);
            if local == 0 {
                self.pieces.insert(pi, Piece { src: Src::Add(add_off), len });
            } else {
                let p = self.pieces[pi];
                let tail = Piece { src: Self::shift_src(p.src, local), len: p.len - local };
                self.pieces[pi].len = local;
                self.pieces.insert(pi + 1, Piece { src: Src::Add(add_off), len });
                self.pieces.insert(pi + 2, tail);
            }
        }
        self.vlen += len;
        self.rebuild_cum();
    }

    fn piece_remove(&mut self, pos: u64, len: u64) {
        if len == 0 || self.pieces.is_empty() {
            return;
        }
        let end = pos + len;
        let (pi, lp) = self.locate(pos);
        let full_end = end >= self.vlen;
        let (qi, lq) = if full_end {
            (self.pieces.len() - 1, self.pieces[self.pieces.len() - 1].len)
        } else {
            self.locate(end)
        };
        let mut repl: Vec<Piece> = Vec::with_capacity(2);
        if lp > 0 {
            repl.push(Piece { src: self.pieces[pi].src, len: lp });
        }
        if lq > 0 {
            if lq < self.pieces[qi].len {
                repl.push(Piece {
                    src: Self::shift_src(self.pieces[qi].src, lq),
                    len: self.pieces[qi].len - lq,
                });
            }
            // 终点在 qi 内部/末尾 → qi 也被移除
            self.pieces.splice(pi..=qi, repl);
        } else {
            // 终点恰在 piece qi 起点之前 → qi 保留
            self.pieces.splice(pi..qi, repl);
        }
        self.vlen -= len;
        self.rebuild_cum();
    }

    // ---------- 行索引联动 ----------

    /// piece 变更后调用：重建受影响行区间的索引。
    fn index_update(&mut self, pos: u64, old_len: u64, new_len: u64) -> io::Result<EditInfo> {
        // 1. 受影响首行（索引尚未更新，仍为旧坐标；区域首行起点 s 不受编辑影响）
        let l0 = self.index.line_of_offset(pos);
        let s = self.index.start_of_line(l0);

        // 2. 新坐标：从编辑末尾找下一个换行，确定受影响区域边界
        let region_end = self.first_line_break_after(pos + new_len);
        // 尾部内容不变 → 旧坐标区域终点 = 新终点平移 len_delta
        let old_region_end = (region_end as i64 - (new_len as i64 - old_len as i64)).max(0) as u64;
        // K = 旧区域 [s, old_region_end) 覆盖的行数（删除换行会把后续行并入区域）
        let k = if old_region_end <= s {
            1
        } else {
            self.index.line_of_offset(old_region_end - 1) - l0 + 1
        };

        // 3. 新区域内的全部行起点（含 s）
        let new_starts = self.collect_line_starts(s, region_end);
        debug_assert!(!new_starts.is_empty());
        debug_assert_eq!(new_starts[0], s);

        // 4. 拼接（make_mut：无并发持有时零拷贝）
        let m = new_starts.len() as u64;
        Arc::make_mut(&mut self.index).apply_splice(&Splice {
            first_line: l0,
            new_starts,
            region_end,
            old_lines: k,
            len_delta: new_len as i64 - old_len as i64,
            new_file_len: self.vlen,
        });
        Ok(EditInfo {
            pos,
            old_len,
            new_len,
            first_line: l0,
            lines_delta: m as i64 - k as i64,
        })
    }

    /// p 之后（含 p）第一个 '\n' 的下一字节位置；无换行则 vlen。
    fn first_line_break_after(&self, p: u64) -> u64 {
        if p >= self.vlen {
            return self.vlen;
        }
        let mut found: Option<u64> = None;
        let mut offset = p;
        let _ = self.for_each_chunk(p, self.vlen, |chunk| {
            if let Some(rel) = memchr::memchr(b'\n', chunk) {
                found = Some(offset + rel as u64 + 1);
                Flow::Stop
            } else {
                offset += chunk.len() as u64;
                Flow::Continue
            }
        });
        found.unwrap_or(self.vlen)
    }

    /// [s, region_end) 内的全部行起点（含 s；尾部幽灵空行剔除）。
    fn collect_line_starts(&self, s: u64, region_end: u64) -> Vec<u64> {
        let mut starts = vec![s];
        if region_end <= s {
            return starts;
        }
        let mut offset = s;
        let _ = self.for_each_chunk(s, region_end, |chunk| {
            for rel in memchr::memchr_iter(b'\n', chunk) {
                let start = offset + rel as u64 + 1;
                if start < self.vlen.min(region_end) {
                    starts.push(start);
                }
            }
            offset += chunk.len() as u64;
            Flow::Continue
        });
        starts
    }

    // ---------- 保存管线 ----------

    /// 保存到当前路径（原位替换）。
    pub fn save(&mut self) -> io::Result<()> {
        let p = self.path.clone();
        self.save_as(&p)
    }

    /// 另存为：流式写出临时文件 → 释放基座 → 原子替换 → 以新文件重建基座与索引。
    pub fn save_as(&mut self, path: &Path) -> io::Result<()> {
        // 1. 写临时文件（同目录保证 rename 原子性）
        let fname = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into());
        let tmp = path.with_file_name(format!(".{fname}.hetmp{}", std::process::id()));
        {
            let f = File::create(&tmp)?;
            let mut w = BufWriter::with_capacity(8 << 20, f);
            self.write_content(&mut w)?;
            w.flush()?;
            w.get_ref().sync_data()?;
        }

        // 2. Windows：目标文件有活动映射/句柄时无法替换 → 必须先释放基座
        let exclusive = self
            .base
            .as_ref()
            .map(|b| Arc::strong_count(b) == 1)
            .unwrap_or(true);
        if !exclusive {
            let _ = fs::remove_file(&tmp);
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                "文件基座被共享（如后台任务进行中），无法原位替换；请另存为其他路径",
            ));
        }
        self.base = None;

        // 3. 原子替换（std rename 在 Windows 用 MoveFileEx REPLACE_EXISTING）
        fs::rename(&tmp, path)?;

        // 4. 重新打开为新基座，重置编辑状态（内容 == 磁盘文件，piece 表归一）
        let (base_box, backend) = open_backend(path, &StorageConfig::default())?;
        let base: Arc<dyn crate::storage::FileBase> = Arc::from(base_box);
        let content_start = self.encoding.bom_len();
        let vlen = base.len().saturating_sub(content_start);
        self.base = Some(base);
        self.backend = backend;
        self.add_buf.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.dirty = false;
        self.path = path.to_path_buf();
        self.pieces = if vlen > 0 {
            vec![Piece { src: Src::Base(content_start), len: vlen }]
        } else {
            Vec::new()
        };
        self.cum = vec![0, vlen];
        self.vlen = vlen;
        self.index = Arc::new(LineIndex::build_with(self, 0, DEFAULT_CHUNK, 0)?);
        Ok(())
    }

    fn write_content(&self, w: &mut impl Write) -> io::Result<()> {
        let mut scratch = vec![0u8; 8 << 20];
        for p in &self.pieces {
            let mut local = 0u64;
            while local < p.len {
                let take = (p.len - local).min(scratch.len() as u64) as usize;
                match p.src {
                    Src::Base(off) => {
                        let base = self.base.as_ref().expect("Base piece 无基座");
                        if let Some(s) = base.slice(off + local, take) {
                            w.write_all(s)?;
                        } else {
                            base.read_range(off + local, take, &mut scratch[..take])?;
                            w.write_all(&scratch[..take])?;
                        }
                    }
                    Src::Add(off) => {
                        w.write_all(&self.add_buf[(off + local) as usize..(off + local + take as u64) as usize])?;
                    }
                }
                local += take as u64;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn range(&mut self, n: u64) -> u64 {
            if n == 0 { 0 } else { self.next() % n }
        }
    }

    /// 参考模型的行起点表（与索引同一约定：尾部换行不产生幽灵空行）。
    fn ref_line_starts(data: &[u8]) -> Vec<u64> {
        let mut starts = vec![0u64];
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' && i + 1 < data.len() {
                starts.push(i as u64 + 1);
            }
        }
        starts
    }

    fn verify_against(eb: &EditableBuffer, mirror: &[u8], rng: &mut Rng, label: &str) {
        assert_eq!(eb.len(), mirror.len() as u64, "{label}: 长度不一致");
        // 随机读
        for _ in 0..8 {
            let start = rng.range((mirror.len() as u64).max(1));
            let max_len = (mirror.len() as u64 - start).max(1).min(4096);
            let len = rng.range(max_len) as usize;
            let mut buf = vec![0u8; len];
            eb.read(start, &mut buf).unwrap();
            assert_eq!(
                &buf[..],
                &mirror[start as usize..start as usize + len],
                "{label}: 内容不一致 @ {start}"
            );
        }
        // 行查询对拍
        let starts = ref_line_starts(mirror);
        let n = starts.len() as u64;
        assert_eq!(eb.line_count(), n, "{label}: 行数不一致");
        for _ in 0..8 {
            let l = rng.range(n);
            assert_eq!(eb.start_of_line(l), starts[l as usize], "{label}: start_of_line({l})");
            assert_eq!(
                eb.index().line_of_offset(starts[l as usize]),
                l,
                "{label}: line_of_offset({l})"
            );
            let (s, e) = eb.line_bounds(l);
            let e_next = if l + 1 < n { starts[l as usize + 1] } else { mirror.len() as u64 };
            assert_eq!(s, starts[l as usize]);
            assert_eq!(e, e_next, "{label}: line_bounds({l}).1");
        }
        // 随机偏移的 line_of_offset 对拍
        for _ in 0..8 {
            let off = rng.range((mirror.len() as u64).max(1)).min(mirror.len() as u64);
            let mut expect = 0u64;
            for (li, &s) in starts.iter().enumerate() {
                if s <= off {
                    expect = li as u64;
                } else {
                    break;
                }
            }
            assert_eq!(eb.line_of_offset(off), expect, "{label}: line_of_offset(off={off})");
        }
    }

    fn random_text(rng: &mut Rng, len: u64) -> Vec<u8> {
        // 含大量换行的伪文本，覆盖行边界密集场景
        (0..len)
            .map(|_| {
                let r = rng.next() % 12;
                if r == 0 {
                    b'\n'
                } else if r == 1 {
                    b'\r'
                } else {
                    b'a' + (r - 2) as u8
                }
            })
            .collect()
    }

    #[test]
    fn gbk_open_edit_save_roundtrip() {
        // "你好\n世界" 的 GBK 字节：C4E3 BAC3 0A CAC0 BDE7
        let p = std::env::temp_dir().join("hieditor_gbk_roundtrip.log");
        std::fs::write(
            &p,
            [0xC4u8, 0xE3, 0xBA, 0xC3, b'\n', 0xCA, 0xC0, 0xBD, 0xE7],
        )
        .unwrap();
        let mut eb = EditableBuffer::open(&p, &OpenOptions::default()).unwrap();
        assert_eq!(eb.encoding().name(), "GBK/GB2312");
        // 行 1 解码
        let (s, e) = eb.line_bounds(0);
        let mut buf = vec![0u8; (e - s) as usize];
        eb.read(s, &mut buf).unwrap();
        while matches!(buf.last(), Some(0x0A) | Some(0x0D)) {
            buf.pop();
        }
        assert_eq!(eb.encoding().decode_str(&buf), "你好");
        // 行 2 解码
        let (s2, e2) = eb.line_bounds(1);
        let mut buf2 = vec![0u8; (e2 - s2) as usize];
        eb.read(s2, &mut buf2).unwrap();
        while matches!(buf2.last(), Some(0x0A) | Some(0x0D)) {
            buf2.pop();
        }
        assert_eq!(eb.encoding().decode_str(&buf2), "世界");
        // 插入 "中文"（GBK 编码）到文件头并保存，文件字节仍是 GBK
        let ins = eb.encoding().encode_str("中文");
        eb.insert(s, &ins).unwrap();
        eb.save().unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(&raw[..ins.len()], &ins[..]);
        assert_eq!(raw.len(), ins.len() + 9);
        // 保存后重开仍为 GBK 且行数一致（插入无换行，仍 2 行）
        let eb2 = EditableBuffer::open(&p, &OpenOptions::default()).unwrap();
        assert_eq!(eb2.line_count(), 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn replace_all_matches_reference() {
        // 相邻命中 + 替换文本长于查找词 + 整批撤销/重做
        let mut eb = EditableBuffer::from_vec(b"abXabXXab".to_vec());
        assert_eq!(eb.replace_all(b"ab", b"[-]", false, false), 3);
        let mut out = vec![0u8; eb.len() as usize];
        eb.read(0, &mut out).unwrap();
        assert_eq!(out, b"[-]X[-]XX[-]");
        assert_eq!(eb.len(), 12);

        assert!(eb.undo());
        let mut out2 = vec![0u8; eb.len() as usize];
        eb.read(0, &mut out2).unwrap();
        assert_eq!(out2, b"abXabXXab");

        assert!(eb.redo());
        let mut out3 = vec![0u8; eb.len() as usize];
        eb.read(0, &mut out3).unwrap();
        assert_eq!(out3, b"[-]X[-]XX[-]");
    }

    #[test]
    fn replace_all_case_insensitive_and_no_hit() {
        // "Abc" 与 "ABC" 都被大小写不敏感命中，相邻命中各自替换
        let mut eb = EditableBuffer::from_vec(b"aAbcABC".to_vec());
        assert_eq!(eb.replace_all(b"abc", b"Z", true, false), 2);
        let mut out = vec![0u8; eb.len() as usize];
        eb.read(0, &mut out).unwrap();
        assert_eq!(out, b"aZZ");
        // 无命中：内容不变，也不产生撤销命令
        assert_eq!(eb.replace_all(b"nope", b"Z", false, false), 0);
        assert_eq!(eb.undo_depth(), 1);
    }

    #[test]
    fn replace_all_after_edits_spans_pieces() {
        // 先插入/删除使 piece 碎片化，再全部替换（命中横跨 Base/Add 段）
        let mut eb = EditableBuffer::from_vec(b"hello world hello".to_vec());
        eb.insert(5, b"!").unwrap(); // hello! world hello
        eb.delete(0, 1).unwrap(); // ello! world hello
        assert_eq!(eb.replace_all(b"o", b"0", false, false), 3);
        let mut out = vec![0u8; eb.len() as usize];
        eb.read(0, &mut out).unwrap();
        assert_eq!(out, b"ell0! w0rld hell0");
        assert_eq!(eb.len(), 17);
    }

    #[test]
    fn random_edits_match_reference() {
        let mut rng = Rng(20260912);
        let initial = random_text(&mut rng, 60_000);
        let mut mirror = initial.clone();
        let mut eb = EditableBuffer::from_vec(initial);

        for step in 0..300u64 {
            let op = rng.next() % 10;
            if op < 7 {
                // 插入
                let pos = rng.range(mirror.len() as u64 + 1);
                let text_len = 1 + rng.range(300);
                let text = random_text(&mut rng, text_len);
                eb.insert(pos, &text).unwrap();
                mirror.splice(pos as usize..pos as usize, text);
            } else {
                // 删除
                let pos = rng.range(mirror.len() as u64);
                let max_len = (mirror.len() as u64 - pos).max(1).min(500);
                let len = 1 + rng.range(max_len.min(500));
                let len = len.min(mirror.len() as u64 - pos).max(1);
                eb.delete(pos, len).unwrap();
                mirror.drain(pos as usize..(pos + len) as usize);
            }
            if step % 25 == 0 {
                verify_against(&eb, &mirror, &mut rng, &format!("step {step}"));
            }
        }
        verify_against(&eb, &mirror, &mut rng, "final");

        // 全部撤销 → 应与初始内容一致
        while eb.undo() {}
        assert_eq!(eb.len(), 60_000);
        let mut buf = vec![0u8; 60_000];
        eb.read(0, &mut buf).unwrap();
        // 初始 mirror 未保留，重新生成校验（同种子）
        let mut rng2 = Rng(20260912);
        let initial2 = random_text(&mut rng2, 60_000);
        assert_eq!(buf, initial2);

        // 全部重做 → 与最终内容一致
        while eb.redo() {}
        let mut buf2 = vec![0u8; mirror.len()];
        eb.read(0, &mut buf2).unwrap();
        assert_eq!(buf2, mirror);
    }

    #[test]
    fn edit_edges() {
        // 空文件插入
        let mut eb = EditableBuffer::from_vec(Vec::new());
        assert_eq!(eb.line_count(), 1);
        eb.insert(0, b"hello\nworld\n").unwrap();
        assert_eq!(eb.line_count(), 2);
        assert_eq!(eb.line_bounds(1), (6, 12));
        // 尾部换行 + 末尾插入（EOF 处开新行）
        eb.insert(12, b"tail").unwrap();
        assert_eq!(eb.len(), 16);
        assert_eq!(eb.line_count(), 3);
        // 删到空
        eb.delete(0, eb.len()).unwrap();
        assert_eq!(eb.len(), 0);
        assert_eq!(eb.line_count(), 1);
        // 再插入（空 pieces 上）
        eb.insert(0, b"abc").unwrap();
        let mut buf = [0u8; 3];
        eb.read(0, &mut buf).unwrap();
        assert_eq!(&buf, b"abc");
    }

    #[test]
    fn delete_last_newline_merges() {
        let mut eb = EditableBuffer::from_vec(b"aa\nbb\ncc".to_vec());
        assert_eq!(eb.line_count(), 3);
        // 删除第二个 '\n'（偏移 5）→ 合并后两行
        eb.delete(5, 1).unwrap();
        assert_eq!(eb.line_count(), 2);
        assert_eq!(eb.start_of_line(1), 3);
        // 撤销恢复
        eb.undo();
        assert_eq!(eb.line_count(), 3);
        assert_eq!(eb.start_of_line(2), 6);
    }

    #[test]
    fn insert_at_bom_excluded_stream() {
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"one\ntwo\n");
        let dir = std::env::temp_dir();
        let p = dir.join(format!("hieditor_test_bom_{}.txt", std::process::id()));
        std::fs::write(&p, &data).unwrap();
        let mut eb = EditableBuffer::open(&p, &OpenOptions::default()).unwrap();
        // 虚拟流不含 BOM
        assert_eq!(eb.len(), 8);
        assert_eq!(eb.start_of_line(1), 4);
        // 行首插入
        eb.insert(4, b"ZERO\n").unwrap();
        assert_eq!(eb.line_count(), 3);
        assert_eq!(eb.start_of_line(2), 9);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_roundtrip() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("hieditor_test_save_{}.log", std::process::id()));
        std::fs::write(&p, b"line0\nline1\nline2\n").unwrap();
        let mut eb = EditableBuffer::open(&p, &OpenOptions::default()).unwrap();
        eb.insert(6, b"INSERTED ").unwrap(); // line1 前
        eb.delete(0, 6).unwrap(); // 删掉 line0\n
        eb.undo(); // 恢复 line0
        eb.redo(); // 再删
        eb.insert(0, b"HEAD\n").unwrap();

        let mut mirror = b"line0\nline1\nline2\n".to_vec();
        mirror.splice(6..6, b"INSERTED ".to_vec()); // insert(6, ...)
        mirror.drain(0..6); // delete(0, 6)；undo+redo 净效果不变
        let mut head = b"HEAD\n".to_vec();
        head.extend_from_slice(&mirror);
        let mirror = head;

        let target = dir.join(format!("hieditor_test_saved_{}.log", std::process::id()));
        eb.save_as(&target).unwrap();
        assert!(!eb.is_dirty());

        // 磁盘内容 == 虚拟内容
        let on_disk = std::fs::read(&target).unwrap();
        assert_eq!(on_disk, mirror);
        // 重基座后查询仍一致
        assert_eq!(eb.len(), mirror.len() as u64);
        assert_eq!(eb.line_count(), ref_line_starts(&mirror).len() as u64);
        // 临时文件已消失
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .all(|e| !e.file_name().to_string_lossy().contains(".hetmp")));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&target);
    }
}
