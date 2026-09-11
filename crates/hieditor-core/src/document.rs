//! Document：打开一个文件的完整状态（基座 + 编码 + 行索引），
//! CLI 与 GUI 共用的入口。

use crate::lineindex::LineIndex;
use crate::storage::{chunk_bytes, open_backend, Encoding, StorageConfig};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub storage: StorageConfig,
    /// 0 = 自动（ideal_thread_count）
    pub threads: usize,
}

/// 打开的文档。`index == None` 表示编码暂不可索引（v0.1 仅 UTF-8/ASCII）。
pub struct Document {
    pub path: PathBuf,
    pub base: Arc<dyn crate::storage::FileBase>,
    pub backend: &'static str,
    pub encoding: Encoding,
    pub index: Option<Arc<LineIndex>>,
    pub open_ms: u128,
    pub index_ms: u128,
}

impl Document {
    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<Self> {
        let t0 = Instant::now();
        let (base_box, backend) = open_backend(path, &opts.storage)?;
        let base: Arc<dyn crate::storage::FileBase> = Arc::from(base_box);
        let open_ms = t0.elapsed().as_millis();

        // 采样头部 64KB 做编码检测
        let sample_len = base.len().min(64 << 10) as usize;
        let mut scratch = Vec::new();
        let head: Vec<u8> = if sample_len > 0 {
            chunk_bytes(base.as_ref(), 0, sample_len, &mut scratch)?.to_vec()
        } else {
            Vec::new()
        };
        let encoding = crate::storage::detect_encoding(&head);

        // 编码可索引才建行索引（v0.1：UTF-8/ASCII）
        let (index, index_ms) = if encoding.indexable() {
            let t1 = Instant::now();
            let idx = LineIndex::build(base.as_ref(), encoding.bom_len())?;
            (Some(Arc::new(idx)), t1.elapsed().as_millis())
        } else {
            (None, 0)
        };

        Ok(Self {
            path: path.to_path_buf(),
            base,
            backend,
            encoding,
            index,
            open_ms,
            index_ms,
        })
    }

    /// 内容起点（BOM 之后）。
    pub fn content_start(&self) -> u64 {
        self.encoding.bom_len()
    }

    pub fn line_count(&self) -> Option<u64> {
        self.index.as_ref().map(|i| i.line_count())
    }
}
