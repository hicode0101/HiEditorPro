# HiEditor Pro

对标 EmEditor 的大文件文本编辑器：GB 级文件秒开、全文件毫秒级搜索、（二期）编辑与二进制 Hex 编辑。

- 设计与分析：[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- 性能验收：[docs/BENCHMARKS.md](docs/BENCHMARKS.md)

## 核心技术（Phase 1 已实现）

- **存储层**：小文件整读（≤300MB）/ 本地大文件内存映射（MMF + OS 按需页加载）/ 网络盘分块读，三后端自动选择
- **索引层**：多线程并行扫描换行符，建立 `行号 → 字节偏移` 索引（u32 差分 + superblock 锚点，约 4B/行）
- **搜索层**：SIMD（memchr::memmem）+ 64MB 分块多线程并行，结果携带 `(行号, 偏移, 长度)`
- **着色层**：StyleSpan 多生产者片段表，搜索高亮为第一个生产者（语法高亮二期接入）
- **线程预算**：min(逻辑核数, 物理内存 GB)，EmEditor 同款经验

## 构建

```bash
cargo build --release
cargo test
```

## 使用

```bash
# 生成测试文件
hieditor-cli gen -o test.log --size 1GB

# 文件信息（大小/编码/行数/建索引耗时/内存）
hieditor-cli info test.log

# 跳行计时
hieditor-cli open test.log

# 并行搜索（ANSI 高亮演示）
hieditor-cli search test.log alpha --show 20

# GUI 预览（虚拟化浏览 + 关键词高亮）
hieditor-gui test.log
```

## Crate 结构

| crate | 职责 |
|---|---|
| hieditor-core | 存储（FileBase 三后端）/ 行索引 / StyleSpan 着色模型 |
| hieditor-search | 分块并行搜索（SIMD memmem） |
| hieditor-cli | 验收工具：info / open / search / gen |
| hieditor-gui | egui 最小预览：虚拟化浏览 + 关键词高亮 |
| hieditor-bench | criterion 基准（mmap vs ReadFile、索引、搜索） |

## Roadmap

- **Phase 2**：编辑增量层（piece table + 撤销）、保存管线（unmap→流式写出→原子替换）、Hex/二进制视图、语法高亮、正则搜索
