//! hieditor-gui — 最小 GUI 预览。
//!
//! - 自定义绘制行视图：只渲染可视区行（虚拟化），滚动条以"行"为单位映射，
//!   规避超大行数下 f32 像素精度丢失
//! - 搜索栏：后台 SearchJob 并行搜索，完成后命中区间经 StyleSpan 思想着色高亮
//! - 只读浏览定位；编辑输入、多标签、Hex 视图在二期

use eframe::egui;
use hieditor_core::document::{Document, OpenOptions};
use hieditor_core::util::fmt_size;
use hieditor_search::{SearchHit, SearchJob, SearchQuery, SearchStats};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// 每行渲染字节上限（超长行截断显示）
const MAX_LINE_BYTES: usize = 80_000;
/// GUI 搜索命中上限
const GUI_MAX_HITS: usize = 100_000;

const HIGHLIGHT_BG: egui::Color32 = egui::Color32::from_rgb(180, 140, 20);
const SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(220, 90, 20);
const FG: egui::Color32 = egui::Color32::from_rgb(220, 220, 220);
const BG: egui::Color32 = egui::Color32::from_rgb(24, 24, 28);
const GUTTER_FG: egui::Color32 = egui::Color32::from_rgb(120, 120, 130);

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
        Box::new(move |_cc| Ok(Box::new(App::new(path)))),
    )
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
    doc: Option<Arc<Document>>,
    open_rx: Option<Receiver<Result<Arc<Document>, String>>>,
    open_error: Option<String>,
    path_input: String,
    first_line: u64,
    font_size: f32,
    /// 最近一帧可视行数（命中导航跳转用）
    last_visible: u64,
    search: SearchUi,
    status: String,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            doc: None,
            open_rx: None,
            open_error: None,
            path_input: path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            first_line: 0,
            font_size: 15.0,
            last_visible: 40,
            search: SearchUi::default(),
            status: String::new(),
        };
        if let Some(p) = path {
            app.request_open(p);
        }
        app
    }

    fn request_open(&mut self, path: PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.open_rx = Some(rx);
        self.doc = None;
        self.open_error = None;
        self.status = format!("正在打开 {} …", path.display());
        self.search = SearchUi::default();
        self.first_line = 0;
        std::thread::spawn(move || {
            let res = Document::open(&path, &OpenOptions::default())
                .map(Arc::new)
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
    }

    fn start_search(&mut self) {
        let Some(doc) = self.doc.clone() else { return };
        let Some(index) = doc.index.clone() else { return };
        if self.search.input.is_empty() {
            return;
        }
        if let Some(job) = &self.search.job {
            job.cancel();
        }
        let query = SearchQuery::new(self.search.input.as_bytes().to_vec(), self.search.case_sensitive);
        match SearchJob::start(doc.base.clone(), index, query, GUI_MAX_HITS, 0) {
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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 轮询后台打开
        if let Some(rx) = &self.open_rx {
            match rx.try_recv() {
                Ok(Ok(doc)) => {
                    let desc = format!(
                        "{} · {} · {} · 索引 {} ms",
                        doc.backend,
                        doc.encoding.name(),
                        fmt_size(doc.base.len()),
                        doc.index_ms
                    );
                    self.status = format!("已打开 {}（{}）", doc.path.display(), desc);
                    self.doc = Some(doc);
                    self.open_rx = None;
                }
                Ok(Err(e)) => {
                    self.open_error = Some(e);
                    self.open_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.open_rx = None;
                }
            }
        }

        // 轮询后台搜索
        if let Some(job) = &self.search.job {
            if job.is_done() {
                if let Some(err) = job.error() {
                    self.status = format!("搜索失败: {err}");
                } else {
                    let mut hits = job.hits_snapshot();
                    hits.sort_by_key(|h| h.offset);
                    self.search.stats = Some(SearchStats {
                        hits: hits.len(),
                        elapsed: job.elapsed(),
                        bytes_scanned: 0,
                        truncated: false,
                    });
                    self.search.hits = Arc::new(hits);
                    self.search.selected = 0;
                    self.status = format!(
                        "搜索完成: 命中 {} 条，耗时 {:.0} ms",
                        self.search.hits.len(),
                        job.elapsed().as_secs_f64() * 1e3
                    );
                }
                self.search.job = None;
            } else {
                let (p, total) = job.progress();
                self.status = format!("搜索中… {p}/{total} 块，已命中 {}", job.hit_count());
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("文件:");
                let open_clicked = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .desired_width(ui.available_width() - 190.0)
                            .hint_text("输入文件路径…"),
                    )
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("打开").clicked() || open_clicked {
                    let p = PathBuf::from(self.path_input.trim());
                    if !self.path_input.trim().is_empty() {
                        self.request_open(p);
                    }
                }
                if ui.button("重新建索引").clicked() {
                    if let Some(p) = self.doc.as_ref().map(|d| d.path.clone()) {
                        self.request_open(p);
                    }
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("搜索:");
                let submit = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.search.input)
                            .desired_width(ui.available_width() - 330.0)
                            .hint_text("输入关键词回车搜索…"),
                    )
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("搜索").clicked() || submit {
                    self.start_search();
                }
                ui.checkbox(&mut self.search.case_sensitive, "区分大小写");
                let total = self.search.hits.len();
                if !self.search.hits.is_empty() {
                    if ui.button("▲").clicked() {
                        self.search.selected = self.search.selected.saturating_sub(1);
                        self.goto_hit(self.last_visible);
                    }
                    ui.label(format!(
                        "{}/{}",
                        self.search.selected + 1,
                        total
                    ));
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
                if let Some(doc) = &self.doc {
                    let n = doc.index.as_ref().map(|i| i.line_count()).unwrap_or(0);
                    ui.label(format!(
                        "{} 行 · {} · {}",
                        n,
                        doc.encoding.name(),
                        doc.backend
                    ));
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
                    ui.colored_label(egui::Color32::from_rgb(240, 100, 100), format!("打开失败: {err}"));
                    return;
                }
                if self.open_rx.is_some() {
                    ui.label("正在打开并建立行索引（大文件可能需要数秒）…");
                    return;
                }
                let Some(doc) = self.doc.clone() else {
                    ui.heading("HiEditor Pro — 大文件浏览预览");
                    ui.label("在上方输入文件路径打开（或在命令行运行: hieditor-gui <文件路径>）");
                    ui.label("特性: GB 级文件秒开（MMF+并行行索引）· 虚拟化渲染 · 后台并行搜索 + 关键词高亮");
                    return;
                };
                let Some(index) = doc.index.clone() else {
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 160, 80),
                        format!("编码 {} 暂不支持浏览（v0.1 支持 UTF-8/ASCII）", doc.encoding.name()),
                    );
                    return;
                };
                self.render_view(ui, &doc, &index);
            });
    }
}

impl App {
    fn render_view(&mut self, ui: &mut egui::Ui, doc: &Arc<Document>, index: &Arc<hieditor_core::lineindex::LineIndex>) {
        let n = index.line_count();
        let line_height = self.font_size * 1.4;
        let avail = ui.available_size();
        let visible = ((avail.y / line_height) as u64).max(1);

        // ---- 输入:滚轮 / 键盘（虚拟滚动,以行为单位）----
        let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
        if scroll_y != 0.0 {
            let d = (scroll_y / line_height).trunc() as i64;
            if d != 0 {
                self.first_line = shift(self.first_line, d, n.saturating_sub(1));
            }
        }
        ui.input(|i| {
            let step = visible * 9 / 10;
            for ev in &i.events {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    let d = match key {
                        egui::Key::ArrowDown => Some(1),
                        egui::Key::ArrowUp => Some(-1),
                        egui::Key::PageDown => Some(step as i64),
                        egui::Key::PageUp => Some(-(step as i64)),
                        _ => None,
                    };
                    if let Some(d) = d {
                        self.first_line = shift(self.first_line, d, n.saturating_sub(1));
                    }
                }
            }
        });
        self.first_line = self.first_line.min(n.saturating_sub(1));

        let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        self.last_visible = visible;

        // 拖拽滚动（内容区平移）
        if response.dragged() {
            let dy = response.drag_delta().y;
            let d = (dy / line_height).trunc() as i64;
            self.first_line = shift(self.first_line, -d, n.saturating_sub(1));
        }

        // ---- 滚动条（右侧 16px,以行映射,点击跳转/拖动）----
        const TRACK: f32 = 16.0;
        let track = egui::Rect::from_min_size(
            egui::Pos2::new(rect.right() - TRACK, rect.top()),
            egui::Vec2::new(TRACK, rect.height()),
        );
        painter.rect_filled(track, 2.0, egui::Color32::from_rgb(40, 40, 46));
        if n > 0 && n > visible {
            let thumb_h = ((visible as f32 / n as f32) * track.height()).clamp(24.0, track.height());
            let max_first = (n - visible) as f32;
            let ty = if max_first > 0.0 {
                track.top() + (self.first_line as f32 / max_first) * (track.height() - thumb_h)
            } else {
                track.top()
            };
            let thumb = egui::Rect::from_min_size(
                egui::Pos2::new(track.left() + 2.0, ty),
                egui::Vec2::new(TRACK - 4.0, thumb_h),
            );
            painter.rect_filled(thumb, 3.0, egui::Color32::from_rgb(110, 110, 130));
            let in_track = response
                .interact_pointer_pos()
                .map(|p| track.contains(p))
                .unwrap_or(false);
            if response.dragged() && in_track {
                let ratio = ((response.interact_pointer_pos().unwrap().y - track.top())
                    - thumb_h / 2.0)
                    / (track.height() - thumb_h).max(1.0);
                let target = (ratio.clamp(0.0, 1.0) * max_first) as u64;
                self.first_line = target;
            } else if response.clicked() && in_track {
                let p = response.interact_pointer_pos().unwrap();
                let ratio = ((p.y - track.top()) - thumb_h / 2.0) / (track.height() - thumb_h).max(1.0);
                self.first_line = (ratio.clamp(0.0, 1.0) * max_first) as u64;
            }
        }

        // ---- 绘制可视行 ----
        let font = egui::FontId::monospace(self.font_size);
        let gutter_w = (n.max(1) as f64).log10().ceil() as f32 * (self.font_size * 0.62) + 24.0;
        let text_left = rect.left() + 6.0 + gutter_w;
        let mut scratch: Vec<u8> = Vec::new();
        let hits = self.search.hits.clone();
        let selected_offset = hits.get(self.search.selected).map(|h| h.offset);

        /// 行内分段：普通 / 命中 / 当前选中命中
        enum Seg {
            Plain,
            Hit(bool),
        }

        for i in 0..visible {
            let line = self.first_line + i;
            if line >= n {
                break;
            }
            let y = rect.top() + i as f32 * line_height;
            let (s, e) = index.line_bounds(line);
            let e2 = e.min(s + MAX_LINE_BYTES as u64);
            if e2 <= s {
                continue;
            }
            let len = (e2 - s) as usize;
            let Ok(buf) = hieditor_core::storage::chunk_bytes(doc.base.as_ref(), s, len, &mut scratch) else {
                continue;
            };
            // 显示文本:裁掉行尾换行符
            let mut view = buf;
            while let Some(&last) = view.last() {
                if last == b'\n' || last == b'\r' {
                    view = &view[..view.len() - 1];
                } else {
                    break;
                }
            }
            if view.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(view).into_owned();

            // 行号
            let num_galley = ui.fonts(|f| {
                f.layout_no_wrap(format!("{line}"), font.clone(), GUTTER_FG)
            });
            painter.galley(
                egui::Pos2::new(
                    rect.left() + gutter_w - num_galley.rect.width(),
                    y + (line_height - self.font_size) * 0.42,
                ),
                num_galley,
                GUTTER_FG,
            );

            // 命中区间（字节相对行首）→ 分段
            let lo = hits.partition_point(|h| h.offset < s);
            let mut segs: Vec<(std::ops::Range<usize>, Seg)> = Vec::new();
            let mut cursor = 0usize;
            for h in &hits[lo.min(hits.len())..] {
                if h.offset >= e2 {
                    break;
                }
                let hs = (h.offset - s) as usize;
                let he = ((h.offset - s + h.len as u64) as usize).min(text.len());
                if hs >= text.len() || he <= hs {
                    continue;
                }
                if cursor < hs {
                    segs.push((cursor..hs, Seg::Plain));
                }
                let is_selected = selected_offset == Some(h.offset);
                segs.push((hs..he, Seg::Hit(is_selected)));
                cursor = he;
            }
            if cursor < text.len() {
                segs.push((cursor..text.len(), Seg::Plain));
            }

            let mut x = text_left;
            for (range, seg) in segs {
                if range.start >= text.len() {
                    break;
                }
                let seg_text = safe_slice(&text, range.clone());
                let color = match seg {
                    Seg::Hit(true) | Seg::Hit(false) => egui::Color32::WHITE,
                    Seg::Plain => FG,
                };
                let galley = ui.fonts(|f| f.layout_no_wrap(seg_text.clone(), font.clone(), color));
                let w = galley.rect.width();
                if let Seg::Hit(is_selected) = seg {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::Pos2::new(x, y),
                            egui::Vec2::new(w, line_height),
                        ),
                        2.0,
                        if is_selected { SELECTED_BG } else { HIGHLIGHT_BG },
                    );
                }
                painter.galley(
                    egui::Pos2::new(x, y + (line_height - self.font_size) * 0.42),
                    galley,
                    color,
                );
                x += w;
            }
            if e > e2 {
                // 截断提示
                let g = ui.fonts(|f| {
                    f.layout_no_wrap(" …截断".to_string(), egui::FontId::monospace(self.font_size * 0.9), GUTTER_FG)
                });
                painter.galley(
                    egui::Pos2::new(x + 4.0, y + (line_height - self.font_size) * 0.42),
                    g,
                    GUTTER_FG,
                );
            }
        }
    }
}

/// 行号平移并夹紧。
fn shift(cur: u64, delta: i64, max: u64) -> u64 {
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
