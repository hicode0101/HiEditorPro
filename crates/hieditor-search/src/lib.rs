//! hieditor-search — 分块多线程并行字面量搜索。
//!
//! - `memchr::memmem`（SIMD 向量化）直接在原始字节上搜（UTF-8 免转码）
//! - 文件按 64MB 切块并行；每块搜索范围向后多看 needle.len()-1 字节（overlap），
//!   命中按"命中起点所在块"归属 → 天然无重复
//! - 每条结果携带 (行号, 字节偏移, 长度)，可直接映射为着色高亮区间
//! - 线程数预算：min(逻辑核数, 物理内存 GB)（见 hieditor-core::sysinfo）

use hieditor_core::lineindex::{LineIndex, DEFAULT_CHUNK};
use hieditor_core::parallel::parallel_map;
use hieditor_core::storage::{chunk_bytes, FileBase};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub line: u64,
    pub offset: u64,
    pub len: u32,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub needle: Vec<u8>,
    pub case_sensitive: bool,
    /// GBK 文件：大小写折叠时跳过双字节序列的次字节（避免汉字误匹配）
    pub gbk: bool,
}

impl SearchQuery {
    pub fn new(needle: impl Into<Vec<u8>>, case_sensitive: bool) -> Self {
        Self {
            needle: needle.into(),
            case_sensitive,
            gbk: false,
        }
    }

    /// GBK 编码文件的查询（needle 应为 GBK 字节）。
    pub fn new_gbk(needle: impl Into<Vec<u8>>, case_sensitive: bool) -> Self {
        Self {
            needle: needle.into(),
            case_sensitive,
            gbk: true,
        }
    }
}

#[derive(Debug)]
pub enum SearchError {
    Io(io::Error),
    /// needle 为空或包含换行（v0.1 不支持跨行搜索）
    InvalidQuery(&'static str),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Io(e) => write!(f, "搜索 I/O 错误: {e}"),
            SearchError::InvalidQuery(m) => write!(f, "无效搜索词: {m}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<io::Error> for SearchError {
    fn from(e: io::Error) -> Self {
        SearchError::Io(e)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub hits: usize,
    pub elapsed: Duration,
    pub bytes_scanned: u64,
    /// 因达到 max_hits 提前终止
    pub truncated: bool,
}

impl SearchStats {
    /// 吞吐量 GB/s。
    pub fn gb_per_sec(&self) -> f64 {
        if self.elapsed.as_secs_f64() <= 0.0 {
            return 0.0;
        }
        self.bytes_scanned as f64 / (1 << 30) as f64 / self.elapsed.as_secs_f64()
    }
}

/// 后台搜索任务：独立线程运行，GUI 轮询共享状态，CLI 阻塞等待。
pub struct SearchJob {
    hits: Arc<Mutex<Vec<SearchHit>>>,
    progress: Arc<AtomicUsize>,
    total_chunks: usize,
    done: Arc<AtomicBool>,
    truncated: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    elapsed_ms: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
    cancel: Arc<AtomicBool>,
}

impl SearchJob {
    /// 启动后台搜索。达到 `max_hits` 后提前终止；`threads == 0` 自动。
    pub fn start(
        base: Arc<dyn FileBase>,
        index: Arc<LineIndex>,
        query: SearchQuery,
        max_hits: usize,
        threads: usize,
    ) -> Result<Self, SearchError> {
        validate_query(&query)?;
        let job = Self {
            hits: Arc::new(Mutex::new(Vec::new())),
            progress: Arc::new(AtomicUsize::new(0)),
            total_chunks: chunk_count(base.len(), index.content_start(), DEFAULT_CHUNK),
            done: Arc::new(AtomicBool::new(false)),
            truncated: Arc::new(AtomicBool::new(false)),
            scanned: Arc::new(AtomicU64::new(0)),
            elapsed_ms: Arc::new(AtomicU64::new(0)),
            error: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let fields = (
            job.hits.clone(),
            job.progress.clone(),
            job.done.clone(),
            job.truncated.clone(),
            job.scanned.clone(),
            job.elapsed_ms.clone(),
            job.error.clone(),
            job.cancel.clone(),
        );
        std::thread::spawn(move || {
            let (hits, progress, done, truncated, scanned, elapsed_ms, error, cancel) = fields;
            let t0 = Instant::now();
            let r = run_core(
                base.as_ref(),
                &index,
                &query,
                max_hits,
                threads,
                Some((&hits, &progress)),
                &cancel,
            );
            match r {
                Ok((_, bytes)) => scanned.store(bytes, Ordering::Relaxed),
                Err(e) => *error.lock().unwrap() = Some(e.to_string()),
            }
            if hits.lock().unwrap().len() >= max_hits {
                truncated.store(true, Ordering::Relaxed);
            }
            elapsed_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
            done.store(true, Ordering::Relaxed);
        });
        Ok(job)
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.progress.load(Ordering::Relaxed), self.total_chunks)
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    /// 当前命中数（后台进行中也会增长）。
    pub fn hit_count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }

    /// 当前命中快照（克隆，未排序）。
    pub fn hits_snapshot(&self) -> Vec<SearchHit> {
        self.hits.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 阻塞等待完成，命中按字节偏移排序，返回 (hits, stats)。
    pub fn finish(self) -> (Vec<SearchHit>, SearchStats) {
        while !self.done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut hits = std::mem::take(&mut *self.hits.lock().unwrap());
        hits.sort_by_key(|h| h.offset);
        let truncated = self.truncated.load(Ordering::Relaxed);
        let stats = SearchStats {
            hits: hits.len(),
            elapsed: self.elapsed(),
            bytes_scanned: self.scanned.load(Ordering::Relaxed),
            truncated,
        };
        (hits, stats)
    }
}

/// 同步搜索入口：内部走 SearchJob。`threads == 0` 自动；`max_hits` 上限保护。
pub fn search(
    base: &dyn FileBase,
    index: &LineIndex,
    query: &SearchQuery,
    max_hits: usize,
    threads: usize,
) -> Result<(Vec<SearchHit>, SearchStats), SearchError> {
    validate_query(query)?;
    let t0 = Instant::now();
    let cancel = AtomicBool::new(false);
    let hits = run_core(base, index, query, max_hits, threads, None, &cancel)?.0;
    let stats = SearchStats {
        hits: hits.len(),
        elapsed: t0.elapsed(),
        bytes_scanned: base.len().saturating_sub(index.content_start()),
        truncated: hits.len() >= max_hits,
    };
    Ok((hits, stats))
}

fn validate_query(query: &SearchQuery) -> Result<(), SearchError> {
    if query.needle.is_empty() {
        return Err(SearchError::InvalidQuery("搜索词为空"));
    }
    if query.needle.contains(&b'\n') || query.needle.contains(&b'\r') {
        return Err(SearchError::InvalidQuery("v0.1 不支持含换行的搜索词"));
    }
    Ok(())
}

fn chunk_count(file_len: u64, content_start: u64, chunk: u64) -> usize {
    file_len.saturating_sub(content_start).div_ceil(chunk) as usize
}

/// 核心执行体：分块并行搜索。
/// `sink = Some((共享命中表, 进度计数))` 时增量写入（SearchJob 后台模式）；
/// 返回 (排序后的命中, 实际扫描字节数)。
fn run_core(
    base: &dyn FileBase,
    index: &LineIndex,
    query: &SearchQuery,
    max_hits: usize,
    threads: usize,
    sink: Option<(&Mutex<Vec<SearchHit>>, &AtomicUsize)>,
    cancel: &AtomicBool,
) -> Result<(Vec<SearchHit>, u64), SearchError> {
    let content_start = index.content_start();
    let file_len = base.len();
    let n = chunk_count(file_len, content_start, DEFAULT_CHUNK);
    let case_sensitive = query.case_sensitive;
    // 大小写不敏感：needle 与块内容同步 ASCII 小写化（1:1 字节变换，偏移不变）；
    // GBK 文件跳过双字节序列的次字节，避免汉字次字节被误折叠造成误匹配
    let lowered = if case_sensitive {
        None
    } else {
        let mut n = query.needle.clone();
        hieditor_core::storage::encoding::fold_ascii(&mut n, query.gbk);
        Some(n)
    };
    let effective: &[u8] = lowered.as_deref().unwrap_or(&query.needle);
    let needle_len = effective.len() as u64;

    let results: Vec<Vec<SearchHit>> = parallel_map(n, threads, |i| -> io::Result<Vec<SearchHit>> {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let start = content_start + i as u64 * DEFAULT_CHUNK;
        let end = (start + DEFAULT_CHUNK).min(file_len);
        // 向后多看 needle_len-1 字节，命中起点仍归属本块 → 跨块边界无遗漏无重复
        let search_end = (end + needle_len.saturating_sub(1)).min(file_len);
        if start >= file_len {
            return Ok(Vec::new());
        }
        let read_len = (search_end - start) as usize;
        let mut scratch = Vec::new();
        let src = chunk_bytes(base, start, read_len, &mut scratch)?;
        if src.len() < effective.len() {
            return Ok(Vec::new());
        }
        let lower_copy: Vec<u8> = if case_sensitive {
            Vec::new()
        } else {
            let mut h = src.to_vec();
            hieditor_core::storage::encoding::fold_ascii(&mut h, query.gbk);
            h
        };
        let hay: &[u8] = if case_sensitive { src } else { &lower_copy };
        let finder = memchr::memmem::Finder::new(effective);
        // 块内换行相对位置（行号计算）
        let nls: Vec<u32> = memchr::memchr_iter(b'\n', &hay[..(end - start) as usize])
            .map(|p| p as u32)
            .collect();
        let start_line = index.line_of_offset(start);
        let mut out = Vec::new();
        for rel in finder.find_iter(hay) {
            let rel64 = rel as u64;
            if rel64 >= end - start {
                break; // 起点落在本块之外 → 属于下一块
            }
            let line = start_line + nls.partition_point(|&p| (p as u64) < rel64) as u64;
            out.push(SearchHit {
                line,
                offset: start + rel64,
                len: effective.len() as u32,
            });
            if out.len() >= max_hits {
                break;
            }
        }
        if let Some((_, progress)) = sink {
            progress.fetch_add(1, Ordering::Relaxed);
        }
        Ok(out)
    })
    .into_iter()
    .collect::<io::Result<Vec<_>>>()?;

    let mut hits: Vec<SearchHit> = results.into_iter().flatten().collect();
    let scanned = base.len().saturating_sub(content_start);
    if let Some((shared, _)) = sink {
        // 后台任务模式：排序/截断后留在共享表，供 GUI/CLI 轮询读取
        let mut guard = shared.lock().unwrap();
        guard.append(&mut hits);
        guard.sort_by_key(|h| h.offset);
        guard.truncate(max_hits);
        if guard.len() >= max_hits {
            cancel.store(true, Ordering::Relaxed);
        }
        Ok((Vec::new(), scanned))
    } else {
        hits.sort_by_key(|h| h.offset);
        hits.truncate(max_hits);
        Ok((hits, scanned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hieditor_core::storage::MemBase;

    fn setup(data: &[u8]) -> (MemBase, LineIndex) {
        let base = MemBase::from_vec(data.to_vec());
        let idx = LineIndex::build_with(&base, 0, 16, 2).unwrap();
        (base, idx)
    }

    #[test]
    fn basic_hits() {
        let data = b"alpha beta\nbeta alpha\nnothing\nalpha";
        let (base, idx) = setup(data);
        let (hits, stats) = search(&base, &idx, &SearchQuery::new("alpha", true), 100, 2).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].line, 0);
        assert_eq!(hits[1].line, 1);
        assert_eq!(hits[2].line, 3);
        assert_eq!(hits[2].offset, 30);
        assert_eq!(stats.bytes_scanned, data.len() as u64);
    }

    #[test]
    fn chunk_boundary_overlap() {
        // 命中跨越 8 字节块边界
        let data = b"aaaaaaaaaaNEEDLEbbbbbb";
        let (base, idx) = setup(data);
        let (hits, _) = search(&base, &idx, &SearchQuery::new("NEEDLE", true), 100, 2).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].offset, 10);
    }

    #[test]
    fn case_insensitive() {
        let data = b"HeLLo world\nhello again\nbye";
        let (base, idx) = setup(data);
        let (hits, _) = search(&base, &idx, &SearchQuery::new("hello", false), 100, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 0);
        assert_eq!(hits[1].line, 1);
    }

    #[test]
    fn max_hits_truncates() {
        let data = b"aXaXaXaXaX";
        let (base, idx) = setup(data);
        let (hits, stats) = search(&base, &idx, &SearchQuery::new("X", true), 3, 2).unwrap();
        assert_eq!(hits.len(), 3);
        assert!(stats.truncated);
    }

    #[test]
    fn invalid_queries() {
        let (base, idx) = setup(b"abc");
        assert!(matches!(
            search(&base, &idx, &SearchQuery::new("", true), 10, 1),
            Err(SearchError::InvalidQuery(_))
        ));
        assert!(matches!(
            search(&base, &idx, &SearchQuery::new("a\nb", true), 10, 1),
            Err(SearchError::InvalidQuery(_))
        ));
    }

    #[test]
    fn needle_not_found() {
        let (base, idx) = setup(b"nothing here");
        let (hits, stats) = search(&base, &idx, &SearchQuery::new("zzz", true), 10, 1).unwrap();
        assert!(hits.is_empty());
        assert!(!stats.truncated);
    }

    #[test]
    fn job_background_sorted() {
        let data = vec![b'a'; 1 << 20];
        let base = MemBase::from_vec(data);
        let idx = LineIndex::build_with(&base, 0, 64 << 10, 2).unwrap();
        let job = SearchJob::start(
            Arc::new(base),
            Arc::new(idx),
            SearchQuery::new("aa", true),
            1000,
            2,
        )
        .unwrap();
        let (hits, _) = job.finish();
        assert!(!hits.is_empty());
        assert!(hits.windows(2).all(|w| w[0].offset < w[1].offset));
    }
}
