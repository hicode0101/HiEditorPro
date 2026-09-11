# HiEditorPro 架构设计

> 阶段：Phase 1（内核优先，只读浏览 + 并行搜索 + 最小 GUI 预览）
> 技术栈：Rust / Windows 先行（内核平台无关）

## 0. 目标

做一个对标 EmEditor 的大文件编辑器：GB 级以上文本文件"秒开"、全文件快速搜索、
后续支持编辑与二进制（Hex）编辑。本阶段先交付可度量的内核（存储 / 索引 / 搜索 /
着色数据模型）+ CLI 验收工具 + 最小 GUI 预览窗口；编辑层、Hex 视图、语法高亮、
保存管线在 Phase 2。

## 1. EmEditor 技术分析（对标的依据）

### 1.1 官方披露（来自 emeditor.com 官网 / 官方帮助 / 作者 Yutaka Emura 技术博客）

| 能力 | 官方披露 |
|---|---|
| 大文件 | 64 位 + **单进程多线程**是官方明示的根基；直开上限约 242 GB；超过 16 TB 用 Large File Controller 分段打开；宣称"极少的内存打开 16 TB / 1,099 亿行" |
| 搜索/替换/过滤 | **多线程并行 + SIMD**（AVX-512 / AVX-2 / SSE2 / SHA 指令集） |
| 工程分发 | v19.2 起按指令集拆 3 个可执行体（AVX-512 / AVX-2 / SSE2），启动时按 CPU 选择 |
| 线程调度 | v22.5 引入**动态线程负载均衡**（应对 Intel 大小核 P/E core 等速假设失效） |
| 线程数预算 | v22.5 起默认线程数 = min(逻辑核数, 物理内存 GB)。作者实测：16 GB 内存 + 24 线程反而比 3 线程慢（内存带宽/页面竞争） |
| 超长行 | v26.0 针对压缩 JS/JSON、基因组等超长单行，多线程 + SIMD 重写打开/搜索/替换，官方数据 2.2× ~ 666× |
| 二进制编辑 | **"Binary (Hex View)" / "Binary (ASCII View)" 实现为两种"编码"**，二进制走同一条基于行的文本管线，Hex 只是另一种渲染 |
| 工程细节 | 文件外部变更检测从 5 秒轮询改为 Windows API（v22.5）；超阈值（约 300 MB）进入"大文件模式"限制撤销深度等重功能 |

官方跑分页宣称同类领先 2× ~ 187×（自测数据，参考）。

### 1.2 合理推断（与公开行为一致的业界成熟做法，EmEditor 未开源）

1. **只读基座**：文件本体以内存映射（MMF）或分块只读方式挂载，物理内存由 OS 页缓存按需换页——"极少内存开 16 TB"的唯一实现路径；242 GB 直开上限即 64 位地址空间 + 页缓存。
2. **并行建行偏移索引**：文件按字节范围切块，多线程并行扫描换行符（SIMD 友好的 memchr 类操作），归并出"行号 → 字节偏移"表。跳转任意行 = 二分查找 + seek，O(log n)。官方"多线程打开提速"的确切用武之地。
3. **惰性转码**：只把可视窗口内的字节解码显示，不做全量 UTF-8→UTF-16 转换。
4. **编辑层与文件层分离**：文件基座永远只读，编辑以增量片段（piece/span）叠加；保存 = 顺序遍历片段流式写出临时文件 + 原子替换（VS Code 的 Piece Table 同思路）。
5. **自绘虚拟化渲染**：自研 Win32 控件，只排版绘制可视区几十行，与文件大小解耦。
6. **正则引擎**：官方文档为 Onigmo（默认）+ Boost.Regex（可选）；字面量部分走 SIMD 快路径。

### 1.3 普通编辑器卡死的原因（反面清单）

全量读入+全量转码；依赖连续缓冲区的通用编辑控件（插入删除 O(n)）；32 位地址空间；加载/转码/高亮/排版单线程串行；全文排版而非只处理可视区。

## 2. HiEditorPro 分层架构

```
┌────────────────────────────────────────────────────────────┐
│ 表现层  hieditor-gui(egui 自绘虚拟化行视图)  hieditor-cli    │
│         文本视图 / (二期 Hex 视图)共用缓冲与着色模型          │
├────────────────────────────────────────────────────────────┤
│ 检索层  hieditor-search：SIMD memmem + 分块多线程并行搜索    │
│         结果 = (行号, 字节偏移, 长度) —— 直接映射为高亮区间   │
├────────────────────────────────────────────────────────────┤
│ 着色层  hieditor-core::style：StyleSpan 片段表              │
│         多生产者(搜索高亮/语法高亮/当前行)按优先级叠加        │
├────────────────────────────────────────────────────────────┤
│ 索引层  hieditor-core::lineindex：并行扫描建行偏移表          │
│         delta(u32) + superblock(u64) 省内存；O(log n) 跳行   │
├────────────────────────────────────────────────────────────┤
│ 存储层  hieditor-core::storage：FileBase trait + 三后端      │
│         MemBase(小文件整读) / MmapBase(MMF+按需页)           │
│         ChunkedBase(网络盘/可移动介质降级)                   │
└────────────────────────────────────────────────────────────┘
```

## 3. 存储层：MMF + 按需页加载（含四个工程补丁）

核心决策：**GB+ 文件用 MMF 只读基座 + OS 按需页加载**。
`MapViewOfFile` 本身 O(1)（只建虚拟映射），真正的成本是建索引时顺序摸一遍文件
（纯顺序 I/O，上限即磁盘带宽）；编辑器访问模式（行索引定位 → 窗口内顺序读）与
4KB 页天然契合，物理内存由页缓存自动管理。

裸用 MMF 的四个坑与对策：

1. **首开不是零成本**：mmap 解决"不复制"，不解决"要扫描"。对策：v0.1 全扫完再显示；
   `Document::open` 预留首屏先行 + 后台增量建索引的接口。
2. **In-page error**：网络盘/U 盘上映射页读盘失败会抛 `EXCEPTION_IN_PAGE_ERROR`
   （SEH 硬异常，Rust 无法 catch）。对策：打开时探测介质类型（`GetDriveTypeW`），
   远程/可移动介质自动改用 `ChunkedBase`（ReadFile 分块读）。
3. **Windows 文件锁**：文件被映射期间不可删除/替换，与"保存=临时文件+原子替换"冲突。
   对策：二期保存管线在写出前 unmap 基座再重建（EmEditor 同款思路）。
4. **纯顺序扫描上 mmap 不一定赢 ReadFile**：缺页中断有开销。
   对策：索引扫描后端做成 trait 可换，基准对比 mmap 顺序读 vs ReadFile 大缓冲择优。

补充路径：**小文件整读**。≤ 阈值（默认 300 MB，参考 EmEditor 大文件模式线）直接
`read` 整个进内存（`MemBase`），索引/搜索/渲染全走最快最简路径。

`FileBase` trait（对象安全，`Send + Sync`）：

```rust
pub trait FileBase: Send + Sync {
    fn len(&self) -> u64;
    fn slice(&self, start: u64, len: usize) -> Option<&[u8]>;   // 零拷贝视图，可 None
    fn read_range(&self, start: u64, len: usize, buf: &mut [u8]) -> io::Result<()>; // 默认实现基于 slice
    fn backend_name(&self) -> &'static str;
}
```

编码：采样头部 64KB 做检测（BOM → 零字节分布 → UTF-8 校验）。
v0.1 完整支持 UTF-8/ASCII（索引/搜索都在原始字节上做）；UTF-16/ANSI 检测后标记
"降级路径"，索引与搜索拒绝并给出明确错误。

## 4. 索引层：并行行偏移索引

- 分块（默认 64 MB）→ 工作线程池（AtomicUsize 抢任务）并行扫描 `\n`
  （`memchr::memchr_iter`，SIMD）→ 每块产出块内相对位置 `Vec<u32>` →
  主线程顺序归并。
- **省内存存储**：`deltas[i] = start(i) - start(i-1)`（绝大多数 < 4 GB，用 u32），
  另存 `supers[b] = start(b * 4096)` 绝对偏移锚点。
  - `start_of_line(line)`：定位 superblock + 累加 ≤ 4096 个 delta（微秒级）
  - `line_of_offset(off)`：supers 二分 + 块内线性
- 行尾约定：文件以 `\n` 结尾不产生幽灵空行；`deltas[0]=0` 对应第 0 行起点。
- 行尾风格检测（LF / CRLF / CR / 混合）：扫描头部 4 MB 统计。
- 5 GB / 平均 100 字节行 ≈ 5,000 万行 → 索引约 200 MB（u32 delta），满足
  working set < 500 MB 验收线。粗索引（每 N 行采样）接口预留，验证百 GB 路径。

## 5. 着色层：StyleSpan（搜索高亮是第一个生产者）

编辑区必须支持字体着色。模型与 EmEditor/主流编辑器一致：**文本运行 + 样式属性**，
样式由多个"生产者"向同一片段表贡献，按优先级叠加，只为可视区物化：

```rust
pub struct Style { pub fg: Option<Rgba>, pub bg: Option<Rgba>, pub bold: bool }
pub enum Producer { Search, Syntax, CurrentLine }   // 数值越大优先级越高
pub struct StyleSpan { pub start: u64, pub end: u64, pub style: Style, pub producer: Producer }
pub struct StyleLayer { spans: Vec<StyleSpan> }      // 按 start 排序
```

- `replace_producer(p, spans)`：整体替换某生产者的全部片段（搜索重跑时先清后插）
- `spans_in_range(start, end)`：二分取可视区间的片段 → GUI 只解析可视窗口
- 语法高亮引擎（syntect 或自研增量）二期接入，接口本阶段预留
- GUI 渲染消费同一模型；CLI 用 ANSI 颜色演示同一模型（二者是同一数据的两个视图）

## 6. 检索层：分块并行 + SIMD

- 字面量搜索用 `memchr::memmem`（内部 SIMD 向量化），**直接在原始字节上搜**
  （UTF-8 字节级搜索等价且免转码）。
- 文件按 64 MB 切块，线程池并行；每块搜索范围向后多看 `needle.len()-1` 字节
  （overlap），命中归属按"命中起点所在块"划分 → 天然无重复、无需去重。
- 每条结果携带 `(行号, 字节偏移, 长度)`：行号 = 块起始行 + 块内 `\n` 计数
  （partition_point），可直接映射为 StyleSpan 高亮区间。
- 大小写不敏感：块内容与 needle 同步 ASCII 小写化（拷贝到线程本地 scratch）。
- 后台任务模型：`SearchJob` 在独立线程运行，暴露
  `hits / progress / done / truncated` 原子共享状态 —— CLI 阻塞等待，GUI 轮询渲染。
- 线程数预算遵循 EmEditor 经验：`min(逻辑核数, 物理内存 GB)`。
- 限制（v0.1 明确声明）：needle 含换行不支持；正则接口预留（二期接 `regex`）。

## 7. 表现层

### 7.1 CLI（hieditor-cli，验收工具）

`info`（大小/编码/后端/行数/耗时/内存）、`open`（跳行计时）、
`search`（并行搜索计时 + ANSI 高亮 + 吞吐量）、`gen`（生成指定大小/行长分布的
测试文件，含超长单行）。内存统计 **commit vs working set 双口径**
（`K32GetProcessMemoryInfo`；映射页只进 working set 不进 commit——这是 EmEditor
官方 FAQ 专门解释过的常见误判，基准报告必须用双口径）。

### 7.2 GUI（hieditor-gui，egui 最小预览）

- 自定义绘制行视图：**只渲染可视区行**（虚拟化），自绘滚动条 + 滚轮/键盘导航，
  滚动条以"行"为单位映射（避免超大行数下 f32 像素精度丢失）
- 搜索栏：后台 `SearchJob` → 完成后命中区间经 StyleSpan 着色高亮
  （底色块 + 分段 galley 绘制）
- 超长行截断显示（每行上限约 2 万字符），只读浏览定位；编辑输入二期

## 8. 线程与内存预算（抄 EmEditor 的作业）

- 线程数 = `min(逻辑核数, 物理内存 GB)`，运行时可通过参数覆盖
- 任务粒度 64 MB 块，AtomicUsize 工作窃取，动态适配大小核速度差（简版负载均衡）
- 内存口径：索引约 4 B/行（u32 delta）+ 8 B/4096 行；大文件粗索引接口预留

## 9. 验收标准（NVMe SSD 参考机）

| 指标 | 目标 |
|---|---|
| 5 GB 建索引 + 首屏可读 | < 3 s |
| 全文件字面量搜索（5 GB） | < 2 s |
| 进程峰值内存（working set 口径） | < 500 MB |
| 跳任意行 | < 10 ms |
| GUI 滚动浏览 | 不掉帧卡顿 |

实测数据见 `docs/BENCHMARKS.md`。

## 10. Phase 2 路线（接口已预留）

1. 编辑增量层：piece table / span 增量 + 撤销栈 + 行索引局部重建
2. 保存管线：unmap 基座 → 流式合并写出临时文件 → 原子替换 → 重建映射；崩溃日志
3. Hex / 二进制视图：同一缓冲层的另一种渲染（EmEditor "编码"思路的推广）
4. 语法高亮引擎接入（StyleLayer 第二个生产者）
5. 正则搜索接入（`regex` crate 起步，后续按需 SIMD 优化）
6. GUI 完整化：编辑输入、多标签、菜单/配置、UTF-16/ANSI 解码路径
