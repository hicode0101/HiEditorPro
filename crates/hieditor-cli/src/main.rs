//! hieditor-cli — HiEditorPro 内核验收工具。
//!
//! 子命令：info / open / search / gen。
//! 内存统计采用 working set 与 commit 双口径（映射页只进 working set 不进 commit）。

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use hieditor_core::document::{Document, OpenOptions};
use hieditor_core::sysinfo;
use hieditor_core::util::{fmt_size, parse_size};
use hieditor_search::{SearchJob, SearchQuery, SearchHit};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const ANSI_HIT: &str = "\x1b[43;30m"; // 黄底黑字
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Parser)]
#[command(name = "hieditor-cli", version, about = "HiEditor Pro 内核验收工具")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 文件信息 + 建索引耗时 + 内存占用
    Info {
        path: PathBuf,
        /// 线程数（0=自动 min(核数, 内存GB)）
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
    /// 打开并计时跳行
    Open {
        path: PathBuf,
        /// 要跳转的行号（可多次；缺省取 0 / 1/4 / 1/2 / 3/4 / 最后一行）
        #[arg(long)]
        line: Vec<u64>,
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
    /// 并行搜索（SIMD memmem + 分块多线程）
    Search {
        path: PathBuf,
        needle: String,
        #[arg(long)]
        ignore_case: bool,
        /// 展示前 N 条命中的上下文
        #[arg(long, default_value_t = 20)]
        show: usize,
        /// 命中上限（达到即提前终止）
        #[arg(long, default_value_t = 100_000)]
        max: usize,
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
    /// 生成测试文件（真实行长分布 + 超长单行）
    Gen {
        #[arg(short, long)]
        output: PathBuf,
        /// 目标大小，如 1GB / 512MB
        #[arg(long, default_value = "1GB")]
        size: String,
        #[arg(long, default_value_t = 5)]
        line_min: usize,
        #[arg(long, default_value_t = 30)]
        line_max: usize,
        /// 插入的超长单行条数
        #[arg(long, default_value_t = 3)]
        long_count: usize,
        /// 超长单行大小
        #[arg(long, default_value = "8MB")]
        long_size: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { path, threads } => cmd_info(&path, threads),
        Cmd::Open { path, line, threads } => cmd_open(&path, line, threads),
        Cmd::Search { path, needle, ignore_case, show, max, threads } => {
            cmd_search(&path, &needle, ignore_case, show, max, threads)
        }
        Cmd::Gen { output, size, line_min, line_max, long_count, long_size, seed } => {
            cmd_gen(&output, &size, line_min, line_max, long_count, &long_size, seed)
        }
    }
}

fn open_opts(threads: usize) -> OpenOptions {
    OpenOptions {
        storage: Default::default(),
        threads,
    }
}

fn print_mem(trim: bool) {
    if let Some((ws, commit)) = sysinfo::process_memory() {
        println!(
            "内存: working set {} / commit {}{}",
            fmt_size(ws),
            fmt_size(commit),
            if trim { "（整理后；mmap 文件页属页缓存可回收）" } else { "" }
        );
    }
}

fn cmd_info(path: &PathBuf, threads: usize) -> Result<()> {
    let doc = Document::open(path, &open_opts(threads)).with_context(|| format!("打开失败: {}", path.display()))?;
    println!("文件: {}", doc.path.display());
    println!("大小: {}", fmt_size(doc.base.len()));
    println!("后端: {}", doc.backend);
    println!("编码: {}", doc.encoding.name());
    match &doc.index {
        Some(idx) => {
            println!("行数: {}", idx.line_count());
            println!("行尾: {}", idx.line_ending.name());
            println!("索引内存: {}", fmt_size(idx.index_memory_bytes()));
        }
        None => bail!("编码 {} 暂不可索引（v0.1 支持 UTF-8/ASCII）", doc.encoding.name()),
    }
    println!("打开耗时: {} ms", doc.open_ms);
    println!("建索引耗时: {} ms", doc.index_ms);
    println!("总耗时: {} ms", doc.open_ms + doc.index_ms);
    print_mem(false);
    sysinfo::trim_working_set();
    print_mem(true);
    Ok(())
}

fn cmd_open(path: &PathBuf, lines: Vec<u64>, threads: usize) -> Result<()> {
    let t0 = Instant::now();
    let doc = Document::open(path, &open_opts(threads)).with_context(|| format!("打开失败: {}", path.display()))?;
    let total_ms = t0.elapsed().as_millis();
    let idx = doc
        .index
        .as_ref()
        .with_context(|| format!("编码 {} 暂不可索引", doc.encoding.name()))?;
    let n = idx.line_count();
    println!("文件: {} ({}, {} 行)", doc.path.display(), fmt_size(doc.base.len()), n);
    println!("后端: {} / 编码: {}", doc.backend, doc.encoding.name());
    println!("打开+建索引总耗时: {total_ms} ms（打开 {} ms + 索引 {} ms）", doc.open_ms, doc.index_ms);
    print_mem(false);
    sysinfo::trim_working_set();
    print_mem(true);

    // 首屏读取计时：前 50 行
    let t = Instant::now();
    let mut first_screen_bytes = 0u64;
    for l in 0..50u64.min(n) {
        let (s, e) = idx.line_bounds(l);
        first_screen_bytes += e - s;
        let mut buf = [0u8; 4096];
        let len = (e - s).min(4096) as usize;
        if len > 0 {
            doc.base.read_range(s, len, &mut buf[..len])?;
        }
    }
    println!(
        "首屏(前50行, {}) 读取: {:.1} ms",
        fmt_size(first_screen_bytes),
        t.elapsed().as_secs_f64() * 1e3
    );

    let jumps: Vec<u64> = if lines.is_empty() {
        vec![0, n / 4, n / 2, n * 3 / 4, n.saturating_sub(1)]
    } else {
        lines.clone()
    };
    println!("跳行计时（索引定位 + 读取行首 4KB）:");
    for l in jumps {
        let l = l.min(n - 1);
        let t = Instant::now();
        let s = idx.start_of_line(l);
        let locate = t.elapsed();
        let mut buf = [0u8; 4096];
        let len = 4096.min(doc.base.len().saturating_sub(s)) as usize;
        if len > 0 {
            doc.base.read_range(s, len, &mut buf[..len])?;
        }
        println!(
            "  行 {:>12}: 定位 {:>7.1} µs + 读取 {} 共 {:>7.1} µs",
            l,
            locate.as_secs_f64() * 1e6,
            fmt_size(len as u64),
            t.elapsed().as_secs_f64() * 1e6
        );
    }
    Ok(())
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if (c as u32) < 0x20 || c == '\u{7f}' { '·' } else { c })
        .collect()
}

/// 打印一条命中：行文本上下文 + ANSI 高亮。
/// 原理：先按字节净化非打印字符（只动 ASCII 区，多字节序列原样保留），
/// 再 lossy 解码，字节偏移在输出串中保持对齐。
fn print_hit(doc: &Document, h: &SearchHit, needle: &[u8]) -> Result<()> {
    let idx = doc.index.as_ref().unwrap();
    let (s, e) = idx.line_bounds(h.line);
    let ctx_start = s.max(h.offset.saturating_sub(60));
    let ctx_end = e.min(h.offset + 160);
    if ctx_end <= ctx_start {
        println!("  行 {} 偏移 {}", h.line, h.offset);
        return Ok(());
    }
    let mut raw = vec![0u8; (ctx_end - ctx_start) as usize];
    doc.base.read_range(ctx_start, raw.len(), &mut raw)?;
    // 只替换 ASCII 控制区字节，多字节 UTF-8 序列原样保留 → 字节偏移保持对齐
    for b in raw.iter_mut() {
        if (*b as u32) < 0x20 || *b == 0x7f {
            *b = b'?';
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let rel = (h.offset - ctx_start) as usize;
    let nlen = needle.len();
    let (mut lo, mut hi) = (rel, rel + nlen);
    while lo > 0 && !text.is_char_boundary(lo) { lo -= 1; }
    while hi < text.len() && !text.is_char_boundary(hi) { hi += 1; }
    let needle_s = sanitize(&text[lo..hi.min(text.len())]);
    let out = format!(
        "{}{}{}{}{}{}{}",
        sanitize(&text[..lo.min(text.len())]),
        ANSI_HIT,
        needle_s,
        ANSI_RESET,
        sanitize(&text[hi.min(text.len())..]),
        if e - h.offset > 160 { " …" } else { "" },
        ""
    );
    println!("  行 {:>10} 偏移 {:>14}  {}", h.line, h.offset, out);
    Ok(())
}

fn cmd_search(path: &PathBuf, needle: &str, ignore_case: bool, show: usize, max: usize, threads: usize) -> Result<()> {
    if needle.is_empty() {
        bail!("搜索词为空");
    }
    let doc = Document::open(path, &open_opts(threads)).with_context(|| format!("打开失败: {}", path.display()))?;
    let index = doc
        .index
        .clone()
        .with_context(|| format!("编码 {} 暂不可搜索（v0.1 支持 UTF-8/ASCII）", doc.encoding.name()))?;
    println!(
        "文件: {} ({}, {} 行, 后端 {}, 索引 {} ms)",
        doc.path.display(),
        fmt_size(doc.base.len()),
        index.line_count(),
        doc.backend,
        doc.index_ms
    );

    let query = SearchQuery::new(needle.as_bytes().to_vec(), !ignore_case);
    let job = SearchJob::start(doc.base.clone(), index.clone(), query, max, threads)
        .with_context(|| "启动搜索失败")?;
    let mut last_print = Instant::now();
    while !job.is_done() {
        if last_print.elapsed() >= Duration::from_millis(200) {
            let (p, total) = job.progress();
            eprint!("\r搜索进度: {p}/{total} 块, 命中 {}    ", job.hit_count());
            last_print = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprint!("\r                          \r");
    if let Some(err) = job.error() {
        bail!("{err}");
    }
    let (hits, stats) = job.finish();
    println!(
        "命中 {} 条{} | 耗时 {:.1} ms | 吞吐 {:.2} GB/s | 扫描 {}",
        hits.len(),
        if stats.truncated { "（已达上限提前终止）" } else { "" },
        stats.elapsed.as_secs_f64() * 1e3,
        stats.gb_per_sec(),
        fmt_size(stats.bytes_scanned)
    );
    print_mem(false);
    sysinfo::trim_working_set();
    print_mem(true);
    if show > 0 {
        println!("前 {} 条命中:", show.min(hits.len()));
        for h in hits.iter().take(show) {
            print_hit(&doc, h, needle.as_bytes())?;
        }
        if hits.len() > show {
            println!("  … 共 {} 条", hits.len());
        }
    }
    Ok(())
}

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
    fn range(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "hieditor", "TODO", "FIXME",
    "error", "warning", "2026", "kernel", "index", "search",
];

fn cmd_gen(
    output: &PathBuf,
    size: &str,
    line_min: usize,
    line_max: usize,
    long_count: usize,
    long_size: &str,
    seed: u64,
) -> Result<()> {
    let target = parse_size(size).with_context(|| format!("无法解析大小: {size}"))?;
    let long_len = parse_size(long_size).with_context(|| format!("无法解析大小: {long_size}"))? as usize;
    if line_min == 0 || line_max < line_min {
        bail!("行长范围无效");
    }
    let mut rng = Rng(seed.max(1));
    let file = std::fs::File::create(output)?;
    let mut w = BufWriter::with_capacity(8 << 20, file);
    let mut written: u64 = 0;
    let mut long_done: usize = 0;
    let t0 = Instant::now();
    while written < target {
        // 在进度均匀分布的位点插入超长单行
        if long_done < long_count {
            let at = target / (long_count as u64 + 1) * (long_done as u64 + 1);
            if written >= at {
                let mut buf = Vec::with_capacity(long_len + 1);
                let mut i = 0usize;
                while i < long_len {
                    if i % 4096 == 0 {
                        buf.extend_from_slice(b"alpha ");
                        i += 6;
                    } else {
                        buf.push(b'x');
                        i += 1;
                    }
                }
                buf.push(b'\n');
                w.write_all(&buf)?;
                written += buf.len() as u64;
                long_done += 1;
                continue;
            }
        }
        let words = rng.range(line_max - line_min + 1) + line_min;
        let mut line = String::with_capacity(words * 8);
        for _ in 0..words {
            line.push_str(WORDS[rng.range(WORDS.len())]);
            line.push(' ');
        }
        line.push('\n');
        let bytes = line.as_bytes();
        w.write_all(bytes)?;
        written += bytes.len() as u64;
    }
    w.flush()?;
    let elapsed = t0.elapsed();
    println!(
        "已生成 {}: 目标 {} 实际 {}，耗时 {:.2} s",
        output.display(),
        size,
        fmt_size(std::fs::metadata(output)?.len()),
        elapsed.as_secs_f64()
    );
    Ok(())
}
