// release 版以窗口子系统运行（不附带控制台窗口）；debug 版保留控制台便于调试
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! hieditor-gui — HiEditor Pro 图形界面。
//!
//! - 自定义绘制行视图：只渲染可视区行（虚拟化），滚动条以"行"为单位映射，
//!   规避超大行数下 f32 像素精度丢失
//! - 首屏先行：open_head 挂基座后立即显示文件头部（预扫描前 1MB 行起点），
//!   行索引进度条可见，后台建完自动切换为完整可编辑状态
//! - 编辑：光标/选区（点击、拖拽、Shift+方向键）、字符输入、退格/删除、
//!   Ctrl+Z/Y 撤销重做、Ctrl+S 原子保存——全部走 EditableBuffer 增量层
//! - 搜索：后台 SearchJob 并行搜索 + 命中高亮；编辑后清空缓存（位置随内容失效）

use eframe::egui;
use hieditor_core::document::OpenOptions;
use hieditor_core::edit::EditableBuffer;
use hieditor_core::lineindex::{LineIndex, LineIndexBuilder};
use hieditor_core::util::fmt_size;
use hieditor_search::{SearchHit, SearchJob, SearchQuery, SearchStats};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// 每行渲染字节上限（超长行截断显示）
const MAX_LINE_BYTES: usize = 80_000;
/// GUI 搜索命中上限
const GUI_MAX_HITS: usize = 100_000;
/// 首屏首窗上报字节数（1MB：冷盘 ~百毫秒内出第一屏，约 1~2 万行）
const HEAD_SCAN: u64 = 1 << 20;

const HIGHLIGHT_BG: egui::Color32 = egui::Color32::from_rgb(180, 140, 20);
const SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(220, 90, 20);
const SELECTION_BG: egui::Color32 = egui::Color32::from_rgb(58, 88, 150);
const CURSOR_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 230, 230);
const FG: egui::Color32 = egui::Color32::from_rgb(220, 220, 220);
const BG: egui::Color32 = egui::Color32::from_rgb(24, 24, 28);
const GUTTER_FG: egui::Color32 = egui::Color32::from_rgb(120, 120, 130);

/// 后台打开流程消息：Head 先到（立即可浏览首屏），
/// Coverage 随顺序扫描增量到达（覆盖范围持续增长），
/// Index 最后到（解锁编辑/搜索）。
enum OpenMsg {
    Head { eb: EditableBuffer, total: u64 },
    Coverage {
        starts: Arc<Vec<u64>>,
        covered_lines: u64,
        covered_bytes: u64,
    },
    Index(Box<LineIndex>),
    Error(String),
}

fn main() -> eframe::Result<()> {
    let path = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_title("HiEditor Pro"),
        ..Default::default()
    };
    eframe::run_native(
        "HiEditor Pro",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(App::new(path)))
        }),
    )
}

/// 注册 Windows 中文字体（egui 默认字体无 CJK 字形）。
/// 只尝试单面 .ttf（ab_glyph 不支持 .ttc 集合）；找不到则静默跳过。
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\Deng.ttf",
        "C:\\Windows\\Fonts\\simfang.ttf",
    ];
    for p in CANDIDATES {
        if let Ok(data) = std::fs::read(p) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(data)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

struct SearchUi {
    input: String,
    case_sensitive: bool,
    job: Option<SearchJob>,
    /// 已完成搜索的排序命中快照（Arc，渲染零拷贝）
    hits: Arc<Vec<SearchHit>>,
    stats: Option<SearchStats>,
    selected: usize,
}

impl Default for SearchUi {
    fn default() -> Self {
        Self {
            input: String::new(),
            case_sensitive: false,
            job: None,
            hits: Arc::new(Vec::new()),
            stats: None,
            selected: 0,
        }
    }
}

struct App {
    eb: Option<EditableBuffer>,
    open_rx: Option<Receiver<OpenMsg>>,
    open_error: Option<String>,
    path_input: String,
    first_line: u64,
    font_size: f32,
    /// 最近一帧可视行数（命中导航/翻页用）
    last_visible: u64,
    /// 光标（虚拟字节偏移）
    cursor: u64,
    /// 选区锚点（None = 无选区）
    anchor: Option<u64>,
    /// 编辑区是否持有键盘焦点（搜索/路径输入框聚焦时为 false）
    editor_focused: bool,
    /// 索引未就绪时的已扫描行起点（虚拟坐标，随顺序扫描增长）
    load_starts: Vec<u64>,
    /// 已覆盖行数（含第 0 行）
    covered_lines: u64,
    /// 已覆盖字节数
    covered_bytes: u64,
    /// 加载目标总字节数（虚拟内容长）
    load_total: u64,
    /// 取消当前加载（重新打开时置位）
    load_cancel: Arc<AtomicBool>,
    /// 自动换行显示
    wrap: bool,
    /// 换行模式下首行顶部被滚过的视觉行数
    wrap_skip: u64,
    /// 换行模式下各逻辑行的视觉行数缓存（键 = 行号；编辑/宽度变化时清空）
    wrap_rows: std::collections::HashMap<u64, u32>,
    /// 上次换行排版的文本区宽度（变化即缓存失效）
    last_wrap_w: f32,
    /// 水平滚动偏移（内容像素；仅非换行模式）
    h_scroll: f32,
    /// 可见行中的最大文本宽度（水平滚动范围依据，逐帧更新）
    h_extent: f32,
    /// 上一帧光标位置（仅光标移动时做滚动跟随，避免滚轮被光标拉回）
    last_cursor: u64,
    search: SearchUi,
    status: String,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            eb: None,
            open_rx: None,
            open_error: None,
            path_input: path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            first_line: 0,
            font_size: 15.0,
            last_visible: 40,
            cursor: 0,
            anchor: None,
            editor_focused: false,
            load_starts: vec![0],
            covered_lines: 1,
            covered_bytes: 0,
            load_total: 0,
            load_cancel: Arc::new(AtomicBool::new(false)),
            wrap: false,
            wrap_skip: 0,
            wrap_rows: std::collections::HashMap::new(),
            last_wrap_w: 0.0,
            h_scroll: 0.0,
            h_extent: 0.0,
            last_cursor: 0,
            search: SearchUi::default(),
            status: String::new(),
        };
        if let Some(p) = path {
            app.request_open(p);
        }
        app
    }

    fn request_open(&mut self, path: PathBuf) {
        // 终止上一个仍在进行的加载
        self.load_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancel = Arc::clone(&cancel);

        let (tx, rx) = std::sync::mpsc::channel();
        self.open_rx = Some(rx);
        self.eb = None;
        self.open_error = None;
        self.status = "读取文件…".into();
        self.search = SearchUi::default();
        self.first_line = 0;
        self.cursor = 0;
        self.anchor = None;
        self.load_starts = vec![0];
        self.covered_lines = 1;
        self.covered_bytes = 0;
        self.wrap_skip = 0;
        self.wrap_rows.clear();
        self.h_scroll = 0.0;
        self.h_extent = 0.0;
        self.last_cursor = 0;
        std::thread::spawn(move || {
            // 阶段一：挂基座（mmap 瞬时完成），先送回首屏
            let eb = match EditableBuffer::open_head(&path, &OpenOptions::default()) {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(OpenMsg::Error(e.to_string()));
                    return;
                }
            };
            let bom = eb.encoding().bom_len();
            let vlen = eb.len().saturating_sub(bom);
            let base = eb.base_arc(); // eb move 进消息前克隆，供扫描使用
            let _ = tx.send(OpenMsg::Head { eb, total: vlen });
            let Some(base) = base else { return };

            // 阶段二：顺序读盘 + 渐进建索引（机械盘最优 I/O 模式；
            // 覆盖范围随扫描增长，已覆盖部分立即可浏览）
            let mut builder = LineIndexBuilder::new(bom);
            let win = 16 << 20;
            let mut buf = vec![0u8; win as usize];
            let mut covered = 0u64; // 虚拟坐标
            let mut win_starts: Vec<u64> = Vec::new(); // 本窗口行起点（builder 只消费一次）
            let mut pending_gui: Vec<u64> = Vec::new(); // 待上报 GUI 的行起点（跨窗累积）
            // 首窗仅 1MB：内容百毫秒级可见（EmEditor 同款体验），此后约每 64MB 增量上报
            let mut next_report = HEAD_SCAN;
            while covered < vlen {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let wsize = if covered == 0 { HEAD_SCAN } else { win };
                let n = wsize.min(vlen - covered) as usize;
                if let Err(e) = base.read_range(bom + covered, n, &mut buf[..n]) {
                    let _ = tx.send(OpenMsg::Error(e.to_string()));
                    return;
                }
                for rel in memchr::memchr_iter(b'\n', &buf[..n]) {
                    let start = covered + rel as u64 + 1;
                    if start < vlen {
                        win_starts.push(start);
                    }
                }
                covered += n as u64;
                builder.push_line_starts(&win_starts);
                pending_gui.append(&mut win_starts);
                if covered >= next_report || covered == vlen {
                    // builder 与 GUI 是两个独立消费者：起点各消费恰好一次；
                    // 未上报的累积在 pending_gui 随下次一并发出（不可丢弃，否则错位）
                    let _ = tx.send(OpenMsg::Coverage {
                        starts: Arc::new(std::mem::take(&mut pending_gui)),
                        covered_lines: builder.covered_lines(),
                        covered_bytes: covered,
                    });
                    next_report = covered + (64 << 20).max(covered / 10);
                }
            }
            let _ = tx.send(OpenMsg::Index(Box::new(builder.finish(vlen))));
        });
    }

    fn start_search(&mut self) {
        let Some(eb) = self.eb.as_ref() else { return };
        if !eb.has_index() {
            self.status = "行索引构建中，完成后可搜索".into();
            return;
        }
        if self.search.input.is_empty() {
            return;
        }
        if let Some(job) = &self.search.job {
            job.cancel();
        }
        let query =
            SearchQuery::new(self.search.input.as_bytes().to_vec(), self.search.case_sensitive);
        let Some(base) = eb.base_arc() else { return };
        match SearchJob::start(base, eb.index_arc(), query, GUI_MAX_HITS, 0) {
            Ok(job) => {
                self.search.hits = Arc::new(Vec::new());
                self.search.stats = None;
                self.search.selected = 0;
                self.search.job = Some(job);
            }
            Err(e) => self.status = format!("搜索失败: {e}"),
        }
    }

    fn goto_hit(&mut self, visible: u64) {
        if let Some(h) = self.search.hits.get(self.search.selected) {
            self.first_line = h.line.saturating_sub(visible / 4);
        }
    }

    // ---------- 编辑辅助（eb 由调用方传入：update() 中已从 self.eb take 出来）----------

    fn sel_range(&self) -> Option<(u64, u64)> {
        let a = self.anchor?;
        let (s, e) = (a.min(self.cursor), a.max(self.cursor));
        (s < e).then_some((s, e))
    }

    /// 编辑使搜索结果失效（命中位置随内容漂移）。
    fn invalidate_search(&mut self) {
        if let Some(job) = self.search.job.take() {
            job.cancel();
        }
        self.search.hits = Arc::new(Vec::new());
        self.search.stats = None;
        self.search.selected = 0;
        // 内容已变化：换行行数缓存全部失效（所有编辑/撤销路径都会经过这里）
        self.wrap_rows.clear();
    }

    fn scroll_to_cursor(&mut self, eb: &EditableBuffer) {
        if !eb.has_index() {
            return;
        }
        let line = eb.line_of_offset(self.cursor);
        if line < self.first_line || line >= self.first_line + self.last_visible {
            self.first_line = line.saturating_sub(self.last_visible / 4);
            self.wrap_skip = 0;
        }
    }

    fn insert_text(&mut self, eb: &mut EditableBuffer, s: &str) {
        let range = self.sel_range();
        let pos = range.map(|(st, _)| st).unwrap_or(self.cursor);
        if let Some((_, en)) = range {
            let _ = eb.delete(pos, en - pos);
        }
        if eb.insert(pos, s.as_bytes()).is_ok() {
            self.cursor = pos + s.len() as u64;
            self.anchor = None;
            self.invalidate_search();
        }
        self.scroll_to_cursor(eb);
    }

    fn delete_at_cursor(&mut self, eb: &mut EditableBuffer, forward: bool) {
        let range = self.sel_range();
        let cursor = self.cursor;
        if let Some((st, en)) = range {
            let _ = eb.delete(st, en - st);
            self.cursor = st;
        } else if forward {
            if cursor < eb.len() {
                let end = next_boundary(eb, cursor);
                let _ = eb.delete(cursor, end - cursor);
            }
        } else if cursor > 0 {
            let start = prev_boundary(eb, cursor);
            let _ = eb.delete(start, cursor - start);
            self.cursor = start;
        } else {
            return;
        }
        self.anchor = None;
        self.invalidate_search();
        self.scroll_to_cursor(eb);
    }

    fn move_cursor(&mut self, eb: &EditableBuffer, new: u64, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = new;
        if !extend {
            self.anchor = None;
        }
        self.scroll_to_cursor(eb);
    }

    /// 垂直移动（按字节列保持，ASCII 场景精确）。
    fn move_line(&mut self, eb: &EditableBuffer, delta: i64, extend: bool) {
        let cur_line = eb.line_of_offset(self.cursor);
        let col = self.cursor - eb.start_of_line(cur_line);
        let n = eb.line_count();
        let new_line = (cur_line as i64 + delta).clamp(0, n as i64 - 1) as u64;
        let ls = eb.start_of_line(new_line);
        let le = eb.line_bounds(new_line).1;
        self.move_cursor(eb, (ls + col).min(le), extend);
    }

    fn do_undo(&mut self, eb: &mut EditableBuffer) {
        if eb.undo() {
            self.cursor = self.cursor.min(eb.len());
            self.anchor = None;
            self.invalidate_search();
            self.status = "已撤销".into();
        } else {
            self.status = "撤销栈已空".into();
        }
        self.scroll_to_cursor(eb);
    }

    fn do_redo(&mut self, eb: &mut EditableBuffer) {
        if eb.redo() {
            self.cursor = self.cursor.min(eb.len());
            self.anchor = None;
            self.invalidate_search();
            self.status = "已重做".into();
        } else {
            self.status = "重做栈已空".into();
        }
        self.scroll_to_cursor(eb);
    }

    fn copy_selection_to_clipboard(&mut self, eb: &EditableBuffer) {
        if let Some((st, en)) = self.sel_range() {
            if let Some(text) = copy_selection_text(eb, st, en) {
                if clipboard_set(&text) {
                    self.status = format!("已复制 {} 字符", text.chars().count());
                } else {
                    self.status = "复制失败：剪贴板不可用".into();
                }
            }
        }
    }

    fn cut_selection(&mut self, eb: &mut EditableBuffer) {
        if let Some((st, en)) = self.sel_range() {
            if let Some(text) = copy_selection_text(eb, st, en) {
                clipboard_set(&text);
                let _ = eb.delete(st, en - st);
                self.cursor = st;
                self.anchor = None;
                self.invalidate_search();
                self.status = format!("已剪切 {} 字符", text.chars().count());
            }
        }
    }

    fn paste_from_clipboard(&mut self, eb: &mut EditableBuffer) {
        match clipboard_get() {
            Some(text) => self.insert_text(eb, &text),
            None => self.status = "剪贴板为空或不可用".into(),
        }
    }

    fn delete_selection(&mut self, eb: &mut EditableBuffer) {
        if let Some((st, en)) = self.sel_range() {
            let _ = eb.delete(st, en - st);
            self.cursor = st;
            self.anchor = None;
            self.invalidate_search();
        }
    }

    fn transform_selection(&mut self, eb: &mut EditableBuffer, mode: CaseMode) {
        if let Some((st, en)) = self.sel_range() {
            if let Some(text) = copy_selection_text(eb, st, en) {
                let transformed = match mode {
                    CaseMode::Upper => text.to_uppercase(),
                    CaseMode::Lower => text.to_lowercase(),
                    CaseMode::Title => title_case(&text),
                };
                let _ = eb.replace(st, en - st, transformed.as_bytes());
                self.cursor = st + transformed.len() as u64;
                self.anchor = None;
                self.invalidate_search();
            }
        }
    }

    fn do_save(eb: &mut EditableBuffer) -> String {
        match eb.save() {
            Ok(()) => format!("已保存 {}（{}）", eb.path().display(), fmt_size(eb.len())),
            Err(e) => format!("保存失败: {e}"),
        }
    }
}

/// pos 前一个字符边界（UTF-8 安全）。
fn prev_boundary(eb: &EditableBuffer, mut pos: u64) -> u64 {
    if pos == 0 {
        return 0;
    }
    pos -= 1;
    let mut buf = [0u8; 1];
    let mut steps = 0;
    while steps < 3 && pos > 0 {
        if eb.read(pos, &mut buf).is_err() || buf[0] & 0xC0 != 0x80 {
            break;
        }
        pos -= 1;
        steps += 1;
    }
    pos
}

/// pos 后一个字符边界（UTF-8 安全）。
fn next_boundary(eb: &EditableBuffer, mut pos: u64) -> u64 {
    let vlen = eb.len();
    if pos >= vlen {
        return vlen;
    }
    pos += 1;
    let mut buf = [0u8; 1];
    let mut steps = 0;
    while steps < 3 && pos < vlen {
        if eb.read(pos, &mut buf).is_err() || buf[0] & 0xC0 != 0x80 {
            break;
        }
        pos += 1;
        steps += 1;
    }
    pos.min(vlen)
}

/// 行内容末尾（裁掉行尾 \r\n）。
fn line_content_end(eb: &EditableBuffer, line: u64) -> u64 {
    let (s, e) = eb.line_bounds(line);
    let mut end = e;
    let mut buf = [0u8; 1];
    if end > s && eb.read(end - 1, &mut buf).is_ok() && buf[0] == b'\n' {
        end -= 1;
        if end > s && eb.read(end - 1, &mut buf).is_ok() && buf[0] == b'\r' {
            end -= 1;
        }
    }
    end
}

fn shift_line(cur: u64, delta: i64, max: u64) -> u64 {
    if delta >= 0 {
        (cur + delta as u64).min(max)
    } else {
        cur.saturating_sub((-delta) as u64)
    }
}

/// 按 char 边界安全切片。
fn safe_slice(s: &str, r: std::ops::Range<usize>) -> String {
    let mut lo = r.start.min(s.len());
    let mut hi = r.end.min(s.len());
    while lo > 0 && !s.is_char_boundary(lo) {
        lo -= 1;
    }
    while hi < s.len() && !s.is_char_boundary(hi) {
        hi += 1;
    }
    s[lo..hi].to_string()
}

/// 双击选词：返回包含 pos 的连续非空白字符段（UTF-8 安全，遇空白结束）。
fn word_range_at(eb: &EditableBuffer, pos: u64) -> (u64, u64) {
    let line = eb.line_of_offset(pos);
    let (s, e) = eb.line_bounds(line);
    let len = ((e - s) as usize).min(MAX_LINE_BYTES);
    let mut buf = vec![0u8; len];
    if eb.read(s, &mut buf).is_err() {
        return (pos, pos);
    }
    let mut v: &[u8] = &buf;
    while let Some(&l) = v.last() {
        if l == b'\n' || l == b'\r' {
            v = &v[..v.len() - 1];
        } else {
            break;
        }
    }
    let text = String::from_utf8_lossy(v);
    let bytes = text.as_bytes();
    let rel = ((pos - s) as usize).min(bytes.len());
    let is_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c);
    let mut a = rel;
    while a > 0 && !is_ws(bytes[a - 1]) {
        a -= 1;
    }
    let mut b2 = rel;
    while b2 < bytes.len() && !is_ws(bytes[b2]) {
        b2 += 1;
    }
    // 对齐 char 边界（多字节字符均非空白，只会退到 lead byte）
    while a > 0 && !text.is_char_boundary(a) {
        a -= 1;
    }
    while b2 < bytes.len() && !text.is_char_boundary(b2) {
        b2 += 1;
    }
    (s + a as u64, s + b2 as u64)
}

#[derive(Clone, Copy)]
pub enum CaseMode {
    Upper,
    Lower,
    Title,
}

/// 首字母大写其余小写（按空白分词）
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for c in s.chars() {
        if c.is_whitespace() {
            out.push(c);
            at_word_start = true;
        } else if at_word_start {
            out.extend(c.to_uppercase());
            at_word_start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn clipboard_get() -> Option<String> {
    arboard::Clipboard::new()
        .ok()?
        .get_text()
        .ok()
        .filter(|s| !s.is_empty())
}

fn clipboard_set(text: &str) -> bool {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut c| c.set_text(text.to_owned()).ok())
        .is_some()
}

fn copy_selection_text(eb: &EditableBuffer, st: u64, en: u64) -> Option<String> {
    let len = (en - st) as usize;
    let mut buf = vec![0u8; len];
    if eb.read(st, &mut buf).is_err() {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 行界来源：索引就绪走索引；否则走已扫描前缀（load_starts）。
fn line_bounds_at(
    index_ready: bool,
    eb: &EditableBuffer,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    line: u64,
) -> (u64, u64) {
    if index_ready {
        eb.line_bounds(line)
    } else {
        let i = (line as usize).min(load_starts.len() - 1);
        let s = load_starts[i];
        let e = if i + 1 < covered_lines as usize {
            load_starts[i + 1]
        } else {
            covered_bytes
        };
        (s, e)
    }
}

/// 读取一行的显示文本（裁掉行尾 \r\n，截断到 MAX_LINE_BYTES），
/// 返回 (行起点, 原始行尾, 显示文本)；原始行尾用于截断标记判断。
fn read_line_text(
    eb: &EditableBuffer,
    index_ready: bool,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    line: u64,
) -> (u64, u64, String) {
    let (s, e) = line_bounds_at(index_ready, eb, load_starts, covered_lines, covered_bytes, line);
    let e2 = e.min(s + MAX_LINE_BYTES as u64);
    if e2 <= s {
        return (s, e, String::new());
    }
    let len = (e2 - s) as usize;
    let mut buf = vec![0u8; len];
    if eb.read(s, &mut buf).is_err() {
        return (s, e, String::new());
    }
    let mut v: &[u8] = &buf;
    while let Some(&last) = v.last() {
        if last == b'\n' || last == b'\r' {
            v = &v[..v.len() - 1];
        } else {
            break;
        }
    }
    (s, e, String::from_utf8_lossy(v).into_owned())
}

/// 换行模式下某逻辑行的视觉行数（带缓存；缓存未命中时现场排版）。
#[allow(clippy::too_many_arguments)]
fn rows_of_line(
    cache: &mut std::collections::HashMap<u64, u32>,
    ui: &egui::Ui,
    font: &egui::FontId,
    text_w: f32,
    eb: &EditableBuffer,
    index_ready: bool,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    line: u64,
) -> u32 {
    if let Some(&r) = cache.get(&line) {
        return r;
    }
    let (_, _e, text) = read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
    let galley = ui.fonts(|f| f.layout(text, font.clone(), FG, text_w));    let r = galley.rows.len().max(1) as u32;
    if cache.len() > 100_000 {
        cache.clear();
    }
    cache.insert(line, r);
    r
}

/// 换行模式滚轮：按视觉行逐行滚动（forward = 向内容尾部）。
#[allow(clippy::too_many_arguments)]
fn step_visual(
    cache: &mut std::collections::HashMap<u64, u32>,
    ui: &egui::Ui,
    font: &egui::FontId,
    text_w: f32,
    eb: &EditableBuffer,
    index_ready: bool,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    n: u64,
    first: &mut u64,
    skip: &mut u64,
    forward: bool,
) {
    let cur = rows_of_line(cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, *first) as u64;
    if forward {
        if *skip + 1 < cur {
            *skip += 1;
        } else if *first + 1 < n {
            *first += 1;
            *skip = 0;
        }
    } else if *skip > 0 {
        *skip -= 1;
    } else if *first > 0 {
        *first -= 1;
        let prev = rows_of_line(cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, *first) as u64;
        *skip = prev - 1;
    }
}

/// 非换行模式的点击命中：视觉行 → 行 → 字符列 → 字节偏移。
#[allow(clippy::too_many_arguments)]
fn cursor_at_pointer_flat(
    first_line: u64,
    n: u64,
    index_ready: bool,
    eb: &EditableBuffer,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    pos: egui::Pos2,
    rect: egui::Rect,
    text_left: f32,
    font: &egui::FontId,
    ui: &egui::Ui,
    line_height: f32,
    h_scroll: f32,
) -> u64 {
    // 与渲染共用同一排版引擎做命中测试：消除"字符数×单字符宽度"估算
    // 在中英文混排/字间距下的累积误差
    let row = (((pos.y - rect.top()) / line_height).floor().max(0.0)) as u64;
    let line = (first_line + row).min(n.saturating_sub(1));
    let (s, e) = line_bounds_at(index_ready, eb, load_starts, covered_lines, covered_bytes, line);
    let len = ((e - s) as usize).min(MAX_LINE_BYTES);
    let mut buf = vec![0u8; len];
    if eb.read(s, &mut buf).is_err() {
        return s;
    }
    let mut v: &[u8] = &buf;
    while let Some(&l) = v.last() {
        if l == b'\n' || l == b'\r' {
            v = &v[..v.len() - 1];
        } else {
            break;
        }
    }
    let text = String::from_utf8_lossy(v).into_owned();
    if text.is_empty() {
        return s;
    }
    let galley = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), FG));
    let sp = egui::vec2(pos.x - text_left + h_scroll, line_height * 0.5);
    let cur = galley.cursor_from_pos(sp);
    let char_idx = cur.index.min(text.chars().count());
    let byte: usize = text.chars().take(char_idx).map(|c| c.len_utf8()).sum();
    s + byte as u64
}

/// 换行模式的点击命中：视觉行 → (逻辑行, 行内视觉行)，再经整行 galley 命中测试取字符。
#[allow(clippy::too_many_arguments)]
fn cursor_at_pointer_wrap(
    cache: &mut std::collections::HashMap<u64, u32>,
    ui: &egui::Ui,
    font: &egui::FontId,
    text_w: f32,
    eb: &EditableBuffer,
    index_ready: bool,
    load_starts: &[u64],
    covered_lines: u64,
    covered_bytes: u64,
    n: u64,
    first: u64,
    skip: u64,
    pos: egui::Pos2,
    rect: egui::Rect,
    text_left: f32,
    line_height: f32,
) -> u64 {
    let vrow = (((pos.y - rect.top()) / line_height).floor().max(0.0)) as u64;
    let mut line = first;
    let mut rem = skip + vrow;
    let mut rows = rows_of_line(cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, line) as u64;
    while rem >= rows && line + 1 < n {
        rem -= rows;
        line += 1;
        rows = rows_of_line(cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, line) as u64;
    }
    if rem >= rows {
        rem = rows - 1;
    }
    let (s, _e, text) = read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
    if text.is_empty() {
        return s;
    }
    let galley = ui.fonts(|f| f.layout(text.clone(), font.clone(), FG, text_w));
    // 行内视觉行中心做命中测试
    let sp = egui::vec2(pos.x - text_left, rem as f32 * line_height + line_height * 0.5);
    let cur = galley.cursor_from_pos(sp);
    let char_idx = cur.index.min(text.chars().count());
    let byte: usize = text.chars().take(char_idx).map(|c| c.len_utf8()).sum();
    s + byte as u64
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 轮询后台打开（Head 先到 → 立即可浏览；
        // Coverage 随顺序扫描增量到达（覆盖范围持续增长）；Index 后到 → 解锁编辑/搜索）
        if let Some(rx) = &self.open_rx {
            loop {
                match rx.try_recv() {
                    Ok(OpenMsg::Head { eb, total }) => {
                        let desc = format!(
                            "{} · {} · {}",
                            eb.backend(),
                            eb.encoding().name(),
                            fmt_size(eb.len())
                        );
                        self.status =
                            format!("已打开 {}（{}）· 建索引中", eb.path().display(), desc);
                        self.eb = Some(eb);
                        self.load_total = total;
                        self.cursor = 0;
                        self.anchor = None;
                        self.first_line = 0;
                    }
                    Ok(OpenMsg::Coverage { starts, covered_lines, covered_bytes }) => {
                        self.load_starts.extend(starts.iter().copied());
                        self.covered_lines = covered_lines;
                        self.covered_bytes = covered_bytes;
                    }
                    Ok(OpenMsg::Index(idx)) => {
                        let lines = if let Some(eb) = self.eb.as_mut() {
                            eb.set_index(*idx);
                            eb.line_count()
                        } else {
                            0
                        };
                        self.load_starts = Vec::new();
                        self.status = format!("索引就绪：{} 行，可编辑/搜索", lines);
                    }
                    Ok(OpenMsg::Error(e)) => {
                        self.open_error = Some(e);
                        self.open_rx = None;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.open_rx = None;
                        break;
                    }
                }
            }
        }
        // 加载中：跟随进度刷新
        if self.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // 轮询后台搜索（任务完成后收割结果，按偏移排序供高亮与跳转）
        if let Some(job) = &self.search.job {
            if job.is_done() {
                if let Some(err) = job.error() {
                    self.status = format!("搜索失败: {err}");
                } else {
                    let mut hits = job.hits_snapshot();
                    hits.sort_by_key(|h| h.offset);
                    let n_hits = hits.len();
                    let elapsed = job.elapsed();
                    self.search.hits = Arc::new(hits);
                    self.search.selected = 0;
                    self.status = format!(
                        "搜索完成: 命中 {} 条，耗时 {:.0} ms（点 ▲▼ 在命中间跳转）",
                        n_hits,
                        elapsed.as_secs_f64() * 1e3
                    );
                }
                self.search.job = None;
            } else {
                let (p, total) = job.progress();
                self.status = format!("搜索中… {p}/{total} 块，已命中 {}", job.hit_count());
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }

        // 拖放文件到窗口 → 直接打开（悬停时显示提示遮罩）
        let hovered = ctx.input(|i| i.raw.hovered_files.clone());
        if !hovered.is_empty() {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(
                screen,
                0.0,
                egui::Color32::from_rgba_unmultiplied(80, 140, 220, 36),
            );
            painter.rect_stroke(
                screen.shrink(6.0),
                6.0,
                egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(90, 150, 230)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "松开以打开文件",
                egui::FontId::proportional(30.0),
                egui::Color32::from_rgb(235, 242, 255),
            );
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped.iter().find_map(|f| f.path.clone()) {
            if file.is_file() {
                self.path_input = file.display().to_string();
                self.request_open(file);
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("文件:");
                let path_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .desired_width(ui.available_width() - 250.0)
                        .hint_text("输入文件路径…"),
                );
                if path_resp.has_focus() {
                    self.editor_focused = false;
                }
                let open_clicked =
                    path_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("打开").clicked() || open_clicked {
                    if !self.path_input.trim().is_empty() {
                        let p = PathBuf::from(self.path_input.trim());
                        self.request_open(p);
                    }
                }
                if ui.button("重新打开").clicked() {
                    if let Some(p) = self.eb.as_ref().map(|e| e.path().to_path_buf()) {
                        self.request_open(p);
                    }
                }
                if ui.button("保存 Ctrl+S").clicked() {
                    if let Some(eb) = self.eb.as_mut() {
                        self.status = Self::do_save(eb);
                    }
                }
                if self.eb.as_ref().map(|e| e.is_dirty()).unwrap_or(false) {
                    ui.colored_label(egui::Color32::from_rgb(240, 170, 80), "● 未保存");
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("搜索:");
                let search_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search.input)
                        .desired_width(ui.available_width() - 330.0)
                        .hint_text("输入关键词回车搜索…"),
                );
                if search_resp.has_focus() {
                    self.editor_focused = false;
                }
                let submit =
                    search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("搜索").clicked() || submit {
                    self.start_search();
                }
                ui.checkbox(&mut self.search.case_sensitive, "区分大小写");
                if ui.checkbox(&mut self.wrap, "自动换行").changed() {
                    self.wrap_skip = 0;
                    self.wrap_rows.clear();
                    self.h_scroll = 0.0;
                }
                let total = self.search.hits.len();
                if !self.search.hits.is_empty() {
                    if ui.button("▲").clicked() {
                        self.search.selected = self.search.selected.saturating_sub(1);
                        self.goto_hit(self.last_visible);
                    }
                    ui.label(format!("{}/{}", self.search.selected + 1, total));
                    if ui.button("▼").clicked() {
                        self.search.selected = (self.search.selected + 1).min(total - 1);
                        self.goto_hit(self.last_visible);
                    }
                }
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(eb) = &self.eb {
                    if eb.has_index() {
                        let cur_line = eb.line_of_offset(self.cursor);
                        let col = self.cursor - eb.start_of_line(cur_line);
                        ui.label(format!(
                            "行 {} 列 {} · {} 行 · {} · {}{}",
                            cur_line + 1,
                            col + 1,
                            eb.line_count(),
                            eb.encoding().name(),
                            eb.backend(),
                            if eb.is_dirty() { " · ●未保存" } else { "" }
                        ));
                    } else {
                        let pct = if self.load_total > 0 {
                            (self.covered_bytes * 100 / self.load_total).min(100)
                        } else {
                            0
                        };
                        ui.label(format!(
                            "已索引 {} / {}（{}%）· {} 行可浏览 · 完成后可编辑 · {}",
                            fmt_size(self.covered_bytes),
                            fmt_size(self.load_total),
                            pct,
                            self.covered_lines,
                            eb.encoding().name()
                        ));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                });
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG))
            .show(ctx, |ui| {
                if let Some(err) = &self.open_error {
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 100, 100),
                        format!("打开失败: {err}"),
                    );
                    return;
                }
                if self.open_rx.is_some() && self.eb.is_none() {
                    ui.label("正在读取文件…");
                    return;
                }
                let Some(mut eb) = self.eb.take() else {
                    ui.heading("HiEditor Pro — 大文件编辑器");
                    ui.label("在上方输入文件路径打开（或在命令行运行: hieditor-gui <文件路径>）");
                    ui.label("特性: GB 级秒开 · 首屏先行 · 虚拟化渲染 · 并行搜索高亮 · 光标/选区编辑 · Ctrl+Z/Y 撤销重做 · Ctrl+S 保存");
                    return;
                };
                self.render_editor(ui, &mut eb);
                self.eb = Some(eb);
            });
    }
}

impl App {
    fn is_loading(&self) -> bool {
        self.eb.as_ref().map(|e| !e.has_index()).unwrap_or(false)
    }

    fn render_editor(&mut self, ui: &mut egui::Ui, eb: &mut EditableBuffer) {
        let index_ready = eb.has_index();
        let n = if index_ready {
            eb.line_count()
        } else {
            self.covered_lines
        };
        let line_height = self.font_size * 1.4;
        let avail = ui.available_size();
        let visible = ((avail.y / line_height) as u64).max(1);
        self.last_visible = visible;
        self.first_line = self.first_line.min(n.saturating_sub(1));
        let wrap = self.wrap;
        if wrap {
            self.h_scroll = 0.0; // 换行模式无横向滚动
        }

        // ---- 布局与度量 ----
        let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let font = egui::FontId::monospace(self.font_size);
        // 行号栏宽度 = 最大行号位数 × 数字宽 + 窄边距，随行数自动变宽
        let digit_w = ui
            .fonts(|f| f.layout_no_wrap("0".to_string(), font.clone(), FG))
            .rect
            .width()
            .max(1.0);
        let digits = format!("{n}").len() as f32;
        let gutter_w = digits * digit_w + 14.0;
        let text_left = rect.left() + gutter_w;
        const TRACK: f32 = 14.0;
        let text_w = (rect.right() - text_left - TRACK - 6.0).max(50.0);
        let text_top_pad = (line_height - self.font_size) * 0.42;

        // 文本区宽度变化（窗口缩放）→ 换行行数缓存失效；水平滚动范围收敛
        if (self.last_wrap_w - text_w).abs() > 0.5 {
            self.last_wrap_w = text_w;
            self.wrap_rows.clear();
        }
        self.h_scroll = self
            .h_scroll
            .clamp(0.0, (self.h_extent - text_w).max(0.0));

        // ---- 滚轮（向下滚 = 后面的内容；Shift+滚轮或横向滚 = 水平平移）----
        let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
        let scroll_x = ui.input(|i| i.raw_scroll_delta.x);
        let shift_held = ui.input(|i| i.modifiers.shift);
        let h_delta = -scroll_x - if shift_held { scroll_y } else { 0.0 };
        if !wrap && h_delta != 0.0 {
            self.h_scroll = (self.h_scroll + h_delta).clamp(0.0, (self.h_extent - text_w).max(0.0));
        } else if scroll_y != 0.0 {
            let dv = (-scroll_y / line_height).trunc() as i64;
            if dv != 0 {
                if wrap {
                    for _ in 0..dv.abs() {
                        step_visual(
                            &mut self.wrap_rows, ui, &font, text_w, eb, index_ready,
                            self.load_starts.as_slice(), self.covered_lines, self.covered_bytes,
                            n, &mut self.first_line, &mut self.wrap_skip, dv > 0,
                        );
                    }
                } else {
                    self.first_line = shift_line(self.first_line, dv, n.saturating_sub(1));
                }
            }
        }

        // 换行模式：skip 越界归一（缓存被清/内容变化后）
        if wrap {
            loop {
                let cur = rows_of_line(
                    &mut self.wrap_rows, ui, &font, text_w, eb, index_ready,
                    self.load_starts.as_slice(), self.covered_lines, self.covered_bytes,
                    self.first_line,
                ) as u64;
                if self.wrap_skip >= cur && self.first_line + 1 < n {
                    self.wrap_skip -= cur;
                    self.first_line += 1;
                } else {
                    if self.first_line + 1 >= n {
                        self.wrap_skip = self.wrap_skip.min(cur.saturating_sub(1));
                    }
                    break;
                }
            }
        }

        // ---- 滚动条几何（垂直 + 水平；换行模式水平条隐藏）----
        let vtrack = egui::Rect::from_min_size(
            egui::Pos2::new(rect.right() - TRACK, rect.top()),
            egui::Vec2::new(TRACK - 2.0, rect.height() - TRACK),
        );
        let max_units = if wrap {
            n.max(1)
        } else {
            n.saturating_sub(visible).max(1)
        };
        let size_frac = ((visible as f32) / (n.max(1) as f32)).min(1.0);
        let thumb_h = (size_frac * vtrack.height()).clamp(24.0, vtrack.height());
        let pos_frac = ((self.first_line as f32) / (max_units as f32)).min(1.0);
        let thumb_y = vtrack.top() + pos_frac * (vtrack.height() - thumb_h).max(1.0);

        let h_max = (self.h_extent - text_w).max(0.0);
        let htrack = egui::Rect::from_min_size(
            egui::Pos2::new(text_left, rect.bottom() - TRACK),
            egui::Vec2::new(rect.right() - text_left - TRACK, TRACK - 2.0),
        );
        let h_thumb_w = if h_max > 0.0 {
            (text_w / self.h_extent * htrack.width()).clamp(24.0, htrack.width())
        } else {
            htrack.width()
        };
        let h_thumb_x = htrack.left()
            + if h_max > 0.0 {
                (self.h_scroll / h_max) * (htrack.width() - h_thumb_w)
            } else {
                0.0
            };

        // ---- 鼠标：滚动条点击/拖拽；内容区点击定位光标、拖拽选区 ----
        let pointer_pos = response.interact_pointer_pos();
        let in_htrack = pointer_pos
            .map(|p| htrack.top() <= p.y && p.x >= text_left && !wrap)
            .unwrap_or(false);
        let in_vtrack = pointer_pos.map(|p| p.x >= vtrack.left()).unwrap_or(false);
        if index_ready {
            if in_htrack {
                if response.clicked() || response.dragged() {
                    let p = pointer_pos.unwrap();
                    let denom = (htrack.width() - h_thumb_w).max(1.0);
                    let ratio = ((p.x - htrack.left()) - h_thumb_w / 2.0).max(0.0) / denom;
                    self.h_scroll = (ratio.clamp(0.0, 1.0) * h_max).min(h_max);
                }
            } else if in_vtrack {
                if response.clicked() || response.dragged() {
                    let p = pointer_pos.unwrap();
                    let ratio = ((p.y - vtrack.top()) - thumb_h / 2.0).max(0.0)
                        / (vtrack.height() - thumb_h).max(1.0);
                    self.first_line = ((ratio.clamp(0.0, 1.0) * max_units as f32) as u64)
                        .min(n.saturating_sub(1));
                    self.wrap_skip = 0;
                }
            } else {
                let mut target_at = |pos: egui::Pos2| -> u64 {
                    if wrap {
                        cursor_at_pointer_wrap(
                            &mut self.wrap_rows, ui, &font, text_w, eb, index_ready,
                            self.load_starts.as_slice(), self.covered_lines, self.covered_bytes,
                            n, self.first_line, self.wrap_skip, pos, rect, text_left, line_height,
                        )
                    } else {
                        cursor_at_pointer_flat(
                            self.first_line, n, index_ready, eb, self.load_starts.as_slice(),
                            self.covered_lines, self.covered_bytes, pos, rect, text_left,
                            &font, ui, line_height, self.h_scroll,
                        )
                    }
                };
                if response.clicked() {
                    self.editor_focused = true;
                    let extend = ui.input(|i| i.modifiers.shift);
                    let target = target_at(pointer_pos.unwrap_or(rect.min));
                    if extend {
                        if self.anchor.is_none() {
                            self.anchor = Some(self.cursor);
                        }
                        self.cursor = target;
                    } else {
                        self.cursor = target;
                        self.anchor = None;
                    }
                }
                if response.drag_started() {
                    self.editor_focused = true;
                    let target = target_at(pointer_pos.unwrap_or(rect.min));
                    self.anchor = Some(target);
                    self.cursor = target;
                }
                if response.dragged() {
                    if let Some(pos) = pointer_pos {
                        self.cursor = target_at(pos);
                    }
                }
                // 双击：选中包含点击位置的连续非空白段（英文单词/汉字词，遇空格结束）
                if response.double_clicked() {
                    self.editor_focused = true;
                    let pos_byte = target_at(pointer_pos.unwrap_or(rect.min));
                    let (ws, we) = word_range_at(eb, pos_byte);
                    if ws < we {
                        self.anchor = Some(ws);
                        self.cursor = we;
                    }
                }
            }
        }
        // 编辑键盘焦点保持粘性：右键菜单/滚动条操作不夺走焦点，
        // 仅当路径/搜索输入框获得焦点时才让出（各自的 has_focus 处理器置 false）
        if response.secondary_clicked() && !in_vtrack && !in_htrack {
            self.editor_focused = true; // 右键呼出菜单视为仍在编辑区工作
        }

        // ---- 右键上下文菜单 ----
        if index_ready && !in_vtrack && !in_htrack {
            response.context_menu(|ui| {
                let can_undo = eb.undo_depth() > 0;
                let can_redo = eb.redo_depth() > 0;
                let has_sel = self.sel_range().is_some();
                let has_clip = clipboard_get().is_some();
                if ui
                    .add_enabled(can_undo, egui::Button::new("撤销").shortcut_text("Ctrl+Z"))
                    .clicked()
                {
                    self.do_undo(eb);
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("恢复").shortcut_text("Ctrl+Y"))
                    .clicked()
                {
                    self.do_redo(eb);
                }
                ui.separator();
                if ui
                    .add_enabled(eb.len() > 0, egui::Button::new("全选").shortcut_text("Ctrl+A"))
                    .clicked()
                {
                    self.anchor = Some(0);
                    self.cursor = eb.len();
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new("剪切").shortcut_text("Ctrl+X"))
                    .clicked()
                {
                    self.cut_selection(eb);
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new("复制").shortcut_text("Ctrl+C"))
                    .clicked()
                {
                    self.copy_selection_to_clipboard(eb);
                }
                if ui
                    .add_enabled(has_clip, egui::Button::new("粘贴").shortcut_text("Ctrl+V"))
                    .clicked()
                {
                    self.paste_from_clipboard(eb);
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new("删除").shortcut_text("Del"))
                    .clicked()
                {
                    self.delete_selection(eb);
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new("转大写")).clicked() {
                    self.transform_selection(eb, CaseMode::Upper);
                }
                if ui.add_enabled(has_sel, egui::Button::new("转小写")).clicked() {
                    self.transform_selection(eb, CaseMode::Lower);
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new("首字母大写其余小写"))
                    .clicked()
                {
                    self.transform_selection(eb, CaseMode::Title);
                }
            });
        }

        // ---- 键盘（编辑区持有焦点且索引就绪时）----
        if self.editor_focused && index_ready {
            let events: Vec<egui::Event> = ui.input(|i| i.events.clone());
            for ev in events {
                match ev {
                    egui::Event::Text(t) => {
                        if !t.is_empty() {
                            self.insert_text(eb, &t);
                        }
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                        // 输入法上屏（中文/日文/韩文等）
                        if !text.is_empty() {
                            self.insert_text(eb, &text);
                        }
                    }
                    // Ctrl+C/X/V 被 egui-winit 转换为以下事件（原始 Key 已被吞掉）
                    egui::Event::Copy => self.copy_selection_to_clipboard(eb),
                    egui::Event::Cut => self.cut_selection(eb),
                    egui::Event::Paste(text) => {
                        if !text.is_empty() {
                            self.insert_text(eb, &text);
                        }
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        let extend = modifiers.shift;
                        match key {
                            egui::Key::ArrowLeft => self.move_cursor(eb, prev_boundary(eb, self.cursor), extend),
                            egui::Key::ArrowRight => self.move_cursor(eb, next_boundary(eb, self.cursor), extend),
                            egui::Key::ArrowUp => self.move_line(eb, -1, extend),
                            egui::Key::ArrowDown => self.move_line(eb, 1, extend),
                            egui::Key::PageUp => self.move_line(eb, -((visible * 9 / 10) as i64), extend),
                            egui::Key::PageDown => self.move_line(eb, (visible * 9 / 10) as i64, extend),
                            egui::Key::Home => {
                                let target = if modifiers.ctrl {
                                    0
                                } else {
                                    eb.start_of_line(eb.line_of_offset(self.cursor))
                                };
                                self.move_cursor(eb, target, extend);
                            }
                            egui::Key::End => {
                                let target = if modifiers.ctrl {
                                    eb.len()
                                } else {
                                    line_content_end(eb, eb.line_of_offset(self.cursor))
                                };
                                self.move_cursor(eb, target, extend);
                            }
                            egui::Key::Backspace => self.delete_at_cursor(eb, false),
                            egui::Key::Delete => self.delete_at_cursor(eb, true),
                            egui::Key::Enter => self.insert_text(eb, "\n"),
                            egui::Key::Tab => self.insert_text(eb, "\t"),
                            egui::Key::Z if modifiers.ctrl && !modifiers.shift => self.do_undo(eb),
                            egui::Key::Y if modifiers.ctrl => self.do_redo(eb),
                            egui::Key::Z if modifiers.ctrl && modifiers.shift => self.do_redo(eb),
                            egui::Key::S if modifiers.ctrl => {
                                self.status = Self::do_save(eb);
                            }
                            egui::Key::A if modifiers.ctrl => {
                                self.anchor = Some(0);
                                self.cursor = eb.len();
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- 定位工具（光标/选区/命中 → 视觉坐标；换行模式多行，非换行单行）----
        let prefix_w = |text: &str, byte: usize| -> f32 {
            let seg = safe_slice(text, 0..byte);
            ui.fonts(|f| f.layout_no_wrap(seg, font.clone(), FG)).rect.width()
        };
        let pos_in_line = |text: &str, byte: usize| -> (u32, f32) {
            if !wrap {
                return (0, prefix_w(text, byte));
            }
            let prefix = safe_slice(text, 0..byte);
            let g = ui.fonts(|f| f.layout(prefix, font.clone(), FG, text_w));
            let r = g.rows.len().saturating_sub(1);
            let x = g.rows.get(r).map(|row| row.rect().right()).unwrap_or(0.0);
            (r as u32, x)
        };
        let span_rows = |text: &str, a: usize, b: usize| -> Vec<(u32, f32, f32)> {
            let (ra, xa) = pos_in_line(text, a);
            let (rb, xb) = pos_in_line(text, b);
            let mut out = Vec::new();
            for r in ra..=rb {
                let from = if r == ra { xa } else { 0.0 };
                let to = if r == rb { xb } else { text_w };
                out.push((r, from, to));
            }
            out
        };

        // ---- 光标滚动跟随：仅在光标移动过时触发（滚轮浏览不再被拉回）----
        let cursor_moved = self.cursor != self.last_cursor;
        if index_ready && cursor_moved {
            let cursor_line = eb.line_of_offset(self.cursor);
            // 垂直
            if wrap {
                if cursor_line < self.first_line {
                    self.first_line = cursor_line;
                    self.wrap_skip = 0;
                } else if cursor_line > self.first_line {
                    let dist = cursor_line - self.first_line;
                    if dist > visible * 4 {
                        self.first_line = cursor_line.saturating_sub(visible / 4).min(cursor_line);
                        self.wrap_skip = 0;
                    } else {
                        let cache = &mut self.wrap_rows;
                        let mut acc: i64 = -(self.wrap_skip as i64);
                        let mut ln = self.first_line;
                        while ln < cursor_line {
                            acc += rows_of_line(
                                cache, ui, &font, text_w, eb, index_ready,
                                self.load_starts.as_slice(), self.covered_lines, self.covered_bytes,
                                ln,
                            ) as i64;
                            ln += 1;
                        }
                        if acc < 0 || (acc as u64) + 1 >= visible {
                            self.first_line = cursor_line.saturating_sub(visible / 4).min(cursor_line);
                            self.wrap_skip = 0;
                        }
                    }
                }
            } else if cursor_line < self.first_line || cursor_line >= self.first_line + visible {
                self.first_line = cursor_line.saturating_sub(visible / 4);
            }
            // 水平（非换行模式）：光标超出视口时平移
            if !wrap {
                let (cs, ce) = line_bounds_at(
                    index_ready, eb, self.load_starts.as_slice(),
                    self.covered_lines, self.covered_bytes, cursor_line,
                );
                let len = ((ce - cs) as usize).min(MAX_LINE_BYTES);
                let mut buf = vec![0u8; len];
                let _ = eb.read(cs, &mut buf);
                let mut v: &[u8] = &buf;
                while let Some(&l) = v.last() {
                    if l == b'\n' || l == b'\r' {
                        v = &v[..v.len() - 1];
                    } else {
                        break;
                    }
                }
                let line_text = String::from_utf8_lossy(v).into_owned();
                let rel = ((self.cursor - cs) as usize).min(line_text.len());
                let (_, cx) = pos_in_line(&line_text, rel);
                if cx < self.h_scroll {
                    self.h_scroll = (cx - text_w * 0.1).max(0.0);
                } else if cx - self.h_scroll > text_w - 40.0 {
                    self.h_scroll = (cx - text_w * 0.9).clamp(0.0, (self.h_extent - text_w).max(0.0));
                }
            }
        }
        self.last_cursor = self.cursor;

        // ---- 绘制可视内容 ----
        let sel = self.sel_range();
        let hits = self.search.hits.clone();
        let selected_offset = hits.get(self.search.selected).map(|h| h.offset);
        let cursor = self.cursor;
        let cursor_line_visible = self.editor_focused && index_ready;
        let cursor_line = if index_ready { eb.line_of_offset(cursor) } else { 0 };
        let eb = &*eb;
        let load_starts = self.load_starts.as_slice();
        let covered_lines = self.covered_lines;
        let covered_bytes = self.covered_bytes;
        let h_scroll = self.h_scroll;

        let mut y = if wrap {
            rect.top() - self.wrap_skip as f32 * line_height
        } else {
            rect.top()
        };
        let mut line = self.first_line;
        let mut max_w_seen = text_w;
        loop {
            let done = if wrap {
                y >= rect.bottom() - TRACK || line >= n
            } else {
                line >= n || line >= self.first_line + visible
            };
            if done {
                break;
            }

            let (s, e_raw, text) =
                read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
            let text_bytes = text.len();

            // 行号（右对齐，窄边距；固定不随水平滚动；1 起始显示）
            let num =
                ui.fonts(|f| f.layout_no_wrap(format!("{}", line + 1), font.clone(), GUTTER_FG));
            painter.galley(
                egui::Pos2::new(text_left - 8.0 - num.rect.width(), y + text_top_pad),
                num,
                GUTTER_FG,
            );

            // 整行文本 galley（换行模式按 text_w 折行）
            let galley = if wrap {
                ui.fonts(|f| f.layout(text.clone(), font.clone(), FG, text_w))
            } else {
                ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), FG))
            };
            let rows_used = galley.rows.len().max(1) as u32;
            let galley_w = galley.rect.width();

            // 内容水平偏移：屏幕x = text_left + 内容x - h_scroll
            let ox = |x: f32| -> f32 { text_left + x - h_scroll };

            // 选区底色
            if let Some((ss, se)) = sel {
                let s_end = s + text_bytes as u64;
                let a = ss.max(s).min(s_end);
                let b = se.max(s).min(s_end);
                if a < b {
                    for (r, xf, xt) in span_rows(&text, (a - s) as usize, (b - s) as usize) {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::Pos2::new(ox(xf), y + r as f32 * line_height),
                                egui::Vec2::new((xt - xf).max(2.0), line_height),
                            ),
                            0.0,
                            SELECTION_BG,
                        );
                    }
                }
            }

            // 搜索命中底色
            let lo = hits.partition_point(|h| h.offset < s);
            for h in &hits[lo.min(hits.len())..] {
                if h.offset >= s + text_bytes as u64 {
                    break;
                }
                let b = (h.offset + h.len as u64).min(s + text_bytes as u64);
                if b <= h.offset {
                    continue;
                }
                for (r, xf, xt) in span_rows(&text, (h.offset - s) as usize, (b - s) as usize) {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::Pos2::new(ox(xf), y + r as f32 * line_height),
                            egui::Vec2::new((xt - xf).max(2.0), line_height),
                        ),
                        2.0,
                        if selected_offset == Some(h.offset) {
                            SELECTED_BG
                        } else {
                            HIGHLIGHT_BG
                        },
                    );
                }
            }

            // 文本
            painter.galley(egui::Pos2::new(ox(0.0), y + text_top_pad), galley, FG);
            if !wrap {
                max_w_seen = max_w_seen.max(galley_w);
            }

            // 截断标记
            if (e_raw - s) as usize > text_bytes + 2 {
                let (tr, tx) = pos_in_line(&text, text_bytes);
                let g = ui.fonts(|f| {
                    f.layout_no_wrap(
                        " …截断".to_string(),
                        egui::FontId::monospace(self.font_size * 0.9),
                        GUTTER_FG,
                    )
                });
                painter.galley(
                    egui::Pos2::new(
                        ox(tx) + 4.0,
                        y + tr as f32 * line_height + text_top_pad,
                    ),
                    g,
                    GUTTER_FG,
                );
            }

            // 光标条
            if cursor_line_visible && cursor_line == line {
                let rel = ((cursor - s) as usize).min(text_bytes);
                let (r, x) = pos_in_line(&text, rel);
                let cursor_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(ox(x), y + r as f32 * line_height),
                    egui::Vec2::new(2.0, line_height),
                );
                painter.rect_filled(cursor_rect, 0.0, CURSOR_COLOR);
                // 上报 IME 锚点：egui-winit 据此 set_ime_allowed(true) 并定位候选窗
                ui.ctx().output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        rect: cursor_rect.expand(2.0),
                        cursor_rect,
                    });
                });
            }

            y += rows_used as f32 * line_height;
            line += 1;
        }
        self.h_extent = max_w_seen;

        // ---- 垂直滚动条 ----
        painter.rect_filled(vtrack, 2.0, egui::Color32::from_rgb(40, 40, 46));
        if size_frac < 1.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::Pos2::new(vtrack.left() + 1.0, thumb_y),
                    egui::Vec2::new(vtrack.width() - 2.0, thumb_h),
                ),
                3.0,
                egui::Color32::from_rgb(110, 110, 130),
            );
        }

        // ---- 水平滚动条（非换行且内容超宽时出现）----
        if !wrap && h_max > 0.0 {
            painter.rect_filled(htrack, 2.0, egui::Color32::from_rgb(40, 40, 46));
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::Pos2::new(h_thumb_x + 1.0, htrack.top()),
                    egui::Vec2::new(h_thumb_w - 2.0, htrack.height()),
                ),
                3.0,
                egui::Color32::from_rgb(110, 110, 130),
            );
        }

        // ---- 索引进度角标 ----
        if !index_ready {
            let pct = if self.load_total > 0 {
                (self.covered_bytes * 100 / self.load_total).min(100)
            } else {
                0
            };
            painter.text(
                egui::Pos2::new(rect.right() - TRACK - 12.0, rect.top() + 8.0),
                egui::Align2::RIGHT_TOP,
                format!(
                    "⏳ 行索引构建中 {}%（顺序读盘）— 内容随加载可浏览，完成后可编辑与搜索",
                    pct
                ),
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(240, 200, 120),
            );
        }
    }
}
