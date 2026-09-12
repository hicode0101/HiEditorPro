# HiEditor Pro

对标 EmEditor 的大文件文本编辑器：GB 级文件秒开、全文件毫秒级搜索、**编辑增量层已就绪**（piece table + 撤销/重做 + 保存管线）、二进制 Hex 编辑待做。

- 设计与分析：[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- 性能验收：[docs/BENCHMARKS.md](docs/BENCHMARKS.md)

## 核心技术

- **存储层**：小文件整读（≤300MB）/ 本地大文件内存映射（MMF + OS 按需页加载）/ 网络盘分块读，三后端自动选择
- **索引层**：多线程并行扫描换行符，两级块状行偏移索引（块内间距 u32 + Arc 写时复制），编辑后局部重建
- **搜索层**：SIMD（memchr::memmem）+ 64MB 分块多线程并行，结果携带 `(行号, 偏移, 长度)`
- **编辑层**：文件基座只读，编辑以 piece table 叠加；5GB 文件单次编辑 0.3ms；命令式撤销/重做
- **保存管线**：流式写出临时文件 → 释放映射 → 原子替换 → 重基座
- **着色层**：StyleSpan 多生产者片段表，搜索高亮为第一个生产者（语法高亮待接入）
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

# 编辑（piece table 增量层：插入/删除/撤销/重做/保存）
hieditor-cli edit test.log --insert-line 100 --text "new line\n" --delete 500 32 --save
hieditor-cli edit test.log --insert-line 100 --text "new line\n" --save-as out.log

# GUI 编辑器（虚拟化浏览 + 搜索高亮 + 光标/选区编辑 + Ctrl+Z/Y/S）
hieditor-gui test.log
```

## 自定义字体

在 `hieditor-gui.exe` 旁边新建 `fonts` 文件夹，放入字体文件后启动程序，
字体家族名会自动出现在编辑区上方工具栏的"字体"下拉列表中：

- 支持 `.ttf` / `.otf` / `.ttc`（ttc 集合内的每个字体面都会列出，同名去重）
- 字体名从字体文件内部的 `name` 表解析（非文件名），中文显示名可正常识别
- 无法识别的文件启动时会在状态栏提示数量；`.woff/.woff2` 为网页格式，不支持

## Crate 结构

| crate | 职责 |
|---|---|
| hieditor-core | 存储（FileBase 三后端）/ 两级块状行索引 / EditableBuffer 编辑层 / StyleSpan 着色模型 |
| hieditor-search | 分块并行搜索（SIMD memmem） |
| hieditor-cli | 验收工具：info / open / search / edit / gen |
| hieditor-gui | egui 编辑器界面：虚拟化浏览 + 搜索高亮 + 光标/选区编辑 |
| hieditor-bench | criterion 基准（mmap vs ReadFile、索引、搜索） |

## Roadmap

- **已完成**：只读内核（秒开/快搜）+ 编辑增量层（piece table、撤销/重做、保存管线）+ GUI 编辑输入（光标/选区/键盘）
- **下一步**：Hex/二进制视图 → 语法高亮 → 正则搜索 → 首屏先行/增量索引 → GUI 多标签/配置
