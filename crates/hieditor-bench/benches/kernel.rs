//! 内核基准：索引构建（mmap vs ReadFile 对比）、跳行、搜索。
//!
//! 测试文件默认 256MB，生成在 `target/testfiles/`，可通过环境变量
//! `HIEDITOR_BENCH_FILE` 指向已有文件（如 1GB/5GB 验收文件）复用。

use criterion::{criterion_group, criterion_main, Criterion};
use hieditor_core::lineindex::LineIndex;
use hieditor_core::storage::{ChunkedBase, MmapBase, FileBase};
use hieditor_search::{search, SearchQuery};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

const BENCH_SIZE: u64 = 256 << 20;

fn bench_file() -> &'static PathBuf {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("testfiles");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bench_256mb.log");
        if !path.exists() || std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) != BENCH_SIZE {
            gen_file(&path, BENCH_SIZE, 42);
        }
        path
    })
}

/// 与 hieditor-cli gen 相同风格的内容生成。
fn gen_file(path: &PathBuf, target: u64, seed: u64) {
    const WORDS: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
        "sierra", "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
        "hieditor", "TODO", "FIXME", "error", "warning", "2026", "kernel", "index", "search",
    ];
    let mut x = seed.max(1);
    let mut rng = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let f = std::fs::File::create(path).unwrap();
    let mut w = BufWriter::with_capacity(8 << 20, f);
    let mut written = 0u64;
    while written < target {
        let words = (rng() % 26) as usize + 5;
        let mut line = String::with_capacity(words * 8);
        for _ in 0..words {
            line.push_str(WORDS[(rng() % WORDS.len() as u64) as usize]);
            line.push(' ');
        }
        line.push('\n');
        written += line.len() as u64;
        w.write_all(line.as_bytes()).unwrap();
    }
    w.flush().unwrap();
}

fn bench_kernel(c: &mut Criterion) {
    let path = bench_file().clone();

    // mmap vs ReadFile 索引扫描对比（存储层补丁 4：择优）
    let mmap = MmapBase::from_path(&path).unwrap();
    let chunked = ChunkedBase::from_path(&path).unwrap();

    let idx_mmap = LineIndex::build(&mmap, 0).unwrap();
    let n = idx_mmap.line_count();

    let mut group = c.benchmark_group("lineindex");
    group.throughput(criterion::Throughput::Bytes(mmap.len()));
    group.bench_function("build_256mb_mmap", |b| {
        b.iter(|| {
            let idx = LineIndex::build(&mmap, 0).unwrap();
            std::hint::black_box(idx.line_count())
        })
    });
    group.bench_function("build_256mb_readfile", |b| {
        b.iter(|| {
            let idx = LineIndex::build(&chunked, 0).unwrap();
            std::hint::black_box(idx.line_count())
        })
    });
    group.finish();

    c.bench_function("jump_to_line", |b| {
        b.iter(|| {
            let mut acc = 0u64;
            for l in [0u64, n / 4, n / 2, n * 3 / 4, n - 1] {
                acc += idx_mmap.start_of_line(l);
            }
            std::hint::black_box(acc)
        })
    });

    let mut group = c.benchmark_group("search");
    group.throughput(criterion::Throughput::Bytes(mmap.len()));
    group.bench_function("literal_cs_256mb", |b| {
        b.iter(|| {
            let (hits, _) =
                search(&mmap, &idx_mmap, &SearchQuery::new("november", true), 100_000, 0).unwrap();
            std::hint::black_box(hits.len())
        })
    });
    group.bench_function("literal_ci_256mb", |b| {
        b.iter(|| {
            let (hits, _) =
                search(&mmap, &idx_mmap, &SearchQuery::new("NOVEMBER", false), 100_000, 0).unwrap();
            std::hint::black_box(hits.len())
        })
    });
    group.finish();
}

criterion_group!(benches, bench_kernel);
criterion_main!(benches);
