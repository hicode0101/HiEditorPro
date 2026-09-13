// release 版以窗口子系统运行（不附带控制台窗口）；debug 版保留控制台便于调试
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! hieditor-gui — HiEditor Pro 图形界面（多标签页 + 原型图布局）。
//!
//! - 多 Tab：每个 Tab 独立的 EditableBuffer / 光标 / 滚动 / 搜索状态
//! - 首屏先行：open_head 挂基座后毫秒级显示，行索引后台顺序构建，进度可见
//! - 菜单栏（文件/编辑/搜索/查看）+ 字体工具栏 + 查找/替换面板
//! - 编辑：光标/选区、双击选词、剪贴板（Ctrl+C/X/V/A）、右键菜单、IME 中文输入
//! - 保存：Ctrl+S 原子替换

use eframe::egui;
use hieditor_core::document::OpenOptions;
use hieditor_core::edit::EditableBuffer;
use hieditor_core::lineindex::{LineIndex, LineIndexBuilder};
use hieditor_core::util::fmt_size;
use hieditor_search::{SearchHit, SearchJob, SearchQuery};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

// ---------- 主题颜色 ----------
// 所有界面颜色集中在 theme.rs（唯一改色入口），本文件不再出现具体色值
mod theme;
use theme::{editor_theme, ui as theme_ui};

// ---------- 用户字体目录 ----------
// 扫描 exe 旁 fonts/ 下的 ttf/otf/ttc，解析家族名并入字体下拉
mod fonts;
use fonts::UserFont;

// ---------- 打印 ----------
// Win32 标准打印对话框 + GDI 分页文本打印（后台线程）
mod printing;

/// 每行渲染字节上限（超长行截断显示）
const MAX_LINE_BYTES: usize = 80_000;
/// GUI 搜索命中上限
const GUI_MAX_HITS: usize = 100_000;
/// 首窗上报字节数（1MB：冷盘 ~百毫秒出第一屏）
const HEAD_SCAN: u64 = 1 << 20;
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");

// 浅色主题（对齐原型图）

/// 后台打开流程消息（tab_id 路由到对应标签页）
enum OpenMsg {
    Head { tab_id: u64, eb: EditableBuffer, total: u64 },
    Coverage { tab_id: u64, starts: Arc<Vec<u64>>, covered_lines: u64, covered_bytes: u64 },
    Index { tab_id: u64, idx: Box<LineIndex> },
    Error { tab_id: u64, msg: String },
}

/// 编辑字体候选（显示名, 字体文件；空串 = egui 默认等宽）

// ---------- 国际化 ----------
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Follow,
    Zh,
    En,
}

impl Lang {
    fn from_str(s: &str) -> Self {
        match s {
            "zh" => Lang::Zh,
            "en" => Lang::En,
            _ => Lang::Follow,
        }
    }
    fn save_key(&self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
            Lang::Follow => "follow",
        }
    }
    fn is_zh(self) -> bool {
        match self {
            Lang::Zh => true,
            Lang::En => false,
            Lang::Follow => sys_locale::get_locale()
                .map(|l| l.to_ascii_lowercase().starts_with("zh"))
                .unwrap_or(false),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    fn from_str(s: &str) -> Self {
        match s {
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::Light,
        }
    }
    fn save_key(&self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }
}

/// 界面文案（zh/en 双语，未命中回退英语）。
fn tr(lang: Lang, key: &str) -> &'static str {
    let zh = lang.is_zh();
    match (zh, key) {
        (_, "menu.file") => if zh { "文件" } else { "File" },
        (_, "menu.edit") => if zh { "编辑" } else { "Edit" },
        (_, "menu.search") => if zh { "搜索" } else { "Search" },
        (_, "menu.view") => if zh { "查看" } else { "View" },
        (_, "menu.about") => if zh { "关于" } else { "About" },
        (_, "file.open") => if zh { "打开…" } else { "Open…" },
        (_, "file.save") => if zh { "保存" } else { "Save" },
        (_, "file.settings") => if zh { "设置" } else { "Settings" },
        (_, "file.exit") => if zh { "退出" } else { "Exit" },
        (_, "edit.undo") => if zh { "撤销" } else { "Undo" },
        (_, "edit.redo") => if zh { "恢复" } else { "Redo" },
        (_, "edit.selectall") => if zh { "全选" } else { "Select All" },
        (_, "edit.cut") => if zh { "剪切" } else { "Cut" },
        (_, "edit.copy") => if zh { "复制" } else { "Copy" },
        (_, "edit.paste") => if zh { "粘贴" } else { "Paste" },
        (_, "edit.delete") => if zh { "删除" } else { "Delete" },
        (_, "search.find") => if zh { "查找" } else { "Find" },
        (_, "search.findreplace") => if zh { "查找并替换" } else { "Find & Replace" },
        (_, "search.label") => if zh { "查找" } else { "Find" },
        (_, "search.next") => if zh { "下一个" } else { "Next" },
        (_, "search.prev") => if zh { "上一个" } else { "Previous" },
        (_, "search.all") => if zh { "全部" } else { "All" },
        (_, "search.case") => if zh { "区分大小写" } else { "Match case" },
        (_, "search.word") => if zh { "全词匹配" } else { "Whole word" },
        (_, "search.regex") => if zh { "正则" } else { "Regex" },
        (_, "search.regex_pending") => if zh { "正则搜索将在后续版本提供" } else { "Regex search coming soon" },
        (_, "search.replace") => if zh { "替换" } else { "Replace" },
        (_, "search.replaceall") => if zh { "全部替换" } else { "Replace All" },
        (_, "search.findhint") => if zh { "输入查找内容回车" } else { "Find, Enter to search" },
        (_, "search.replacehint") => if zh { "输入替换内容" } else { "Replacement" },
        (_, "view.wrap") => if zh { "自动换行" } else { "Wrap lines" },
        (_, "view.zoom") => if zh { "缩放" } else { "Zoom" },
        (_, "view.zoom_in") => if zh { "放大" } else { "Zoom In" },
        (_, "view.zoom_out") => if zh { "缩小" } else { "Zoom Out" },
        (_, "view.zoom_reset") => if zh { "恢复默认缩放" } else { "Reset Zoom" },
        (_, "toolbar.font") => if zh { "字体" } else { "Font" },
        (_, "toolbar.size") => if zh { "字号" } else { "Size" },
        (_, "font.default") => if zh { "默认字体" } else { "Default font" },
        (_, "status.fonts_loaded") => if zh { "已从 fonts 目录加载 {} 个字体" } else { "Loaded {} font(s) from the fonts folder" },
        (_, "status.fonts_failed") => if zh { "{} 个字体文件无法识别" } else { "{} font file(s) could not be recognized" },
        (_, "tab.new") => if zh { "新建标签页" } else { "New Tab" },
        (_, "tab.close") => if zh { "关闭" } else { "Close" },
        (_, "tab.untitled") => if zh { "无标题" } else { "Untitled" },
        (_, "settings.title") => if zh { "设置" } else { "Settings" },
        (_, "settings.lang") => if zh { "界面语言" } else { "Language" },
        (_, "settings.theme") => if zh { "主题风格" } else { "Theme" },
        (_, "lang.follow") => if zh { "跟随系统" } else { "Follow system" },
        (_, "lang.zh") => if zh { "简体中文" } else { "Simplified Chinese" },
        (_, "lang.en") => if zh { "英语" } else { "English" },
        (_, "theme.light") => if zh { "浅色模式" } else { "Light" },
        (_, "theme.dark") => if zh { "深色模式" } else { "Dark" },
        (_, "settings.shortcut") => if zh { "创建桌面快捷方式" } else { "Create desktop shortcut" },
        (_, "settings.shortcut_ok") => if zh { "桌面快捷方式已创建" } else { "Desktop shortcut created" },
        (_, "settings.shortcut_fail") => if zh { "创建桌面快捷方式失败" } else { "Failed to create shortcut" },
        (_, "about.title") => if zh { "关于" } else { "About" },
        (_, "about.src") => if zh { "开源地址" } else { "Source" },
        (_, "about.wx") => if zh { "作者微信" } else { "Author WeChat" },
        (_, "btn.open") => if zh { "打开" } else { "Open" },
        (_, "btn.copy") => if zh { "复制" } else { "Copy" },
        (_, "btn.copywx") => if zh { "复制微信号" } else { "Copy WeChat ID" },
        (_, "status.copied") => if zh { "已复制" } else { "Copied" },
        (_, "status.loading") => if zh { "读取…" } else { "Reading…" },
        (_, "status.indexing") => if zh { "建索引中" } else { "Building index" },
        (_, "status.ready") => if zh { "索引就绪" } else { "Index ready" },
        (_, "status.editable") => if zh { "可编辑/搜索" } else { "editing & search unlocked" },
        (_, "status.browseable") => if zh { "行可浏览" } else { "lines browsable" },
        (_, "status.searchdone") => if zh { "搜索完成: 命中" } else { "Search done: hits" },
        (_, "status.searching") => if zh { "搜索中…" } else { "Searching…" },
        (_, "status.copiedn") => if zh { "已复制" } else { "Copied" },
        (_, "status.cutn") => if zh { "已剪切" } else { "Cut" },
        (_, "status.clipempty") => if zh { "剪贴板为空或不可用" } else { "Clipboard empty or unavailable" },
        (_, "status.replaced1") => if zh { "已替换 1 处" } else { "Replaced 1 occurrence" },
        (_, "status.replaceall") => if zh { "全部替换完成（Ctrl+Z 可整批撤销）" } else { "Replace all done (Ctrl+Z to undo)" },
        (_, "status.wordall") => if zh { "全词匹配暂不支持全部替换" } else { "Whole-word Replace All not supported yet" },
        (_, "status.savefail") => if zh { "保存失败" } else { "Save failed" },
        (_, "status.saved") => if zh { "已保存" } else { "Saved" },
        (_, "status.untitlednosave") => if zh { "无标题文档尚未关联文件，请先打开一个文件" } else { "Untitled document has no file yet" },
        (_, "status.openfailed") => if zh { "打开失败" } else { "Open failed" },
        (_, "file.new_tab") => if zh { "新建标签页" } else { "New Tab" },
        (_, "file.new_window") => if zh { "新建窗口" } else { "New Window" },
        (_, "file.open_recent") => if zh { "打开最近所用文件" } else { "Open Recent" },
        (_, "file.save_all") => if zh { "全部保存" } else { "Save All" },
        (_, "file.print") => if zh { "打印…" } else { "Print…" },
        (_, "print.noindex") => if zh { "行索引构建中，完成后可打印" } else { "Printing unlocks when indexing finishes" },
        (_, "print.empty") => if zh { "当前文档为空" } else { "Document is empty" },
        (_, "print.nosel") => if zh { "没有选中内容" } else { "Nothing selected" },
        (_, "print.nopages") => if zh { "页码超出文档范围" } else { "Page range out of document" },
        (_, "print.toomany") => if zh { "打印范围过大（上限 {} 页），请用页码范围缩小" } else { "Print range too large (cap {} pages); narrow the page range" },
        (_, "print.status.printing") => if zh { "正在打印 {} 页…" } else { "Printing {} page(s)…" },
        (_, "print.status.done") => if zh { "已发送 {} 页至 {}" } else { "Sent {} page(s) to {}" },
        (_, "print.status.fail") => if zh { "打印失败" } else { "Print failed" },
        (_, "file.close_tab") => if zh { "关闭标签页" } else { "Close Tab" },
        (_, "file.close_window") => if zh { "关闭窗口" } else { "Close Window" },
        (_, "status.saveall_done") => if zh { "全部保存完成" } else { "All tabs saved" },
        (_, "status.reading") => if zh { "读取…" } else { "Reading…" },
        (_, "editor.truncated") => if zh { " …截断" } else { " …truncated" },
        (_, "editor.indexing") => if zh { "行索引构建中 {}%（顺序读盘）— 内容随加载可浏览，完成后可编辑与搜索" } else { "Building line index {}% (sequential read) — content browsable while loading; edit & search unlock when done" },
        (_, "confirm.title") => if zh { "关闭前保存？" } else { "Save before closing?" },
        (_, "confirm.msg") => if zh { "有未保存的修改。" } else { "has unsaved changes." },
        (_, "confirm.save") => if zh { "保存并关闭" } else { "Save & Close" },
        (_, "confirm.nosave") => if zh { "不保存" } else { "Don't Save" },
        (_, "confirm.cancel") => if zh { "取消" } else { "Cancel" },
        (_, "drop.hint") => if zh { "松开以在新标签页打开文件" } else { "Drop to open in a new tab" },
        (_, "status.line") => if zh { "行" } else { "Ln" },
        (_, "status.col") => if zh { "列" } else { "Col" },
        (_, "status.total") => if zh { "共" } else { "Total" },
        (_, "status.lines") => if zh { "行" } else { "lines" },
        (_, "status.unsaved") => if zh { "● 未保存" } else { "● Unsaved" },
        (_, "empty.heading") => if zh { "HiEditor Pro" } else { "HiEditor Pro" },
        (_, "empty.hint1") => if zh { "拖入文件、点标签栏 ＋ 新建，或在文件框输入路径" } else { "Drop a file, click ＋ in the tab bar, or type a path above" },
        (_, "settings.saved") => if zh { "设置已保存" } else { "Settings saved" },
        (_, "search.menufind") => if zh { "查找（Ctrl+F）" } else { "Find (Ctrl+F)" },
        (_, "tab.close_menu") => if zh { "关闭选项卡" } else { "Close Tab" },
        (_, "tab.close_others") => if zh { "关闭其它选项卡" } else { "Close Other Tabs" },
        (_, "tab.close_others_hint") => if zh { "其它标签页有未保存的修改" } else { "Other tabs have unsaved changes" },
        (_, "file.saveas") => if zh { "另存为…" } else { "Save As…" },
        (_, "status.comingsoon") => if zh { "即将支持" } else { "Coming soon" },
        (_, "tab.openfolder") => if zh { "打开所在文件夹" } else { "Open Containing Folder" },
        (_, "tab.copydir") => if zh { "复制当前目录路径" } else { "Copy Folder Path" },
        (_, "tab.copyfile") => if zh { "复制当前文件路径" } else { "Copy File Path" },        (_, "search.menufr") => if zh { "查找并替换（Ctrl+H）" } else { "Find & Replace (Ctrl+H)" },        (_, "edit.upper") => if zh { "转大写" } else { "UPPERCASE" },        (_, "edit.lower") => if zh { "转小写" } else { "lowercase" },        (_, "edit.title") => if zh { "首字母大写其余小写" } else { "Title Case" },        (_, "status.opened") => if zh { "已打开" } else { "Opened" },        (_, "status.replaced_all") => if zh { "全部替换完成（Ctrl+Z 可整批撤销）" } else { "Replace all done (Ctrl+Z to undo)" },        (_, "status.indexed") => if zh { "已索引" } else { "Indexed" },        (_, "status.editwhen") => if zh { "索引完成后可编辑/搜索" } else { "edit & search unlock when done" },        (_, "about.desc") => if zh { "Win平台大文本快速编辑工具，秒开GB级以上文件" } else { "Windows platform large text editing tool, opens files larger than GB in seconds." },        _ => "",
    }
}

/// 带占位符的文案：按顺序替换 {}
fn trf(lang: Lang, key: &str, args: &[&str]) -> String {
    let mut s = tr(lang, key).to_string();
    for a in args {
        s = s.replacen("{}", a, 1);
    }
    s
}

// ---------- 设置持久化 ----------
fn conf_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .map(|d| d.join("hieditor.conf"))
}

fn load_settings() -> (Lang, ThemeMode) {
    let mut lang = Lang::Follow;
    let mut theme = ThemeMode::Light;
    if let Some(p) = conf_path() {
        if let Ok(text) = std::fs::read_to_string(p) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("language=") {
                    lang = Lang::from_str(v.trim());
                }
                if let Some(v) = line.strip_prefix("theme=") {
                    theme = ThemeMode::from_str(v.trim());
                }
            }
        }
    }
    (lang, theme)
}

/// 应用面板主题（菜单栏/工具栏/标签栏/状态栏等 egui 控件颜色）。
///
/// 必须同时做两件事，否则重启后面板会退回浅色：
/// 1. `set_theme` 固定主题偏好——egui 默认"跟随系统"，每帧 begin_pass 都会
///    用系统主题切换激活槽位，导致写入的颜色被绕开；
/// 2. 深浅两个槽位都写入同一样式——无论 egui 当前激活哪个槽位都显示正确。
fn apply_panel_theme(ctx: &egui::Context, dark: bool) {
    let (pref, visuals) = if dark {
        (egui::ThemePreference::Dark, egui::Visuals::dark())
    } else {
        (egui::ThemePreference::Light, egui::Visuals::light())
    };
    ctx.set_theme(pref);
    ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
    ctx.set_visuals_of(egui::Theme::Light, visuals);
}

fn save_settings(lang: Lang, theme: ThemeMode) {
    if let Some(p) = conf_path() {
        let _ = std::fs::write(
            p,
            format!("language={}\ntheme={}\n", lang.save_key(), theme.save_key()),
        );
    }
}

fn create_desktop_shortcut() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let dir = exe.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; $lnk = $ws.CreateShortcut((New-Object -ComObject Shell.Application).NameSpace(0).Self.Path + '\\HiEditor Pro.lnk'); $lnk.TargetPath = '{}'; $lnk.WorkingDirectory = '{}'; $lnk.IconLocation = '{}'; $lnk.Save()",
        exe.display(),
        dir.display(),
        exe.display()
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}


/// 内置字体候选（显示名, 字体文件）。字体下拉以此为基底，
/// 启动时再追加 exe 旁 `fonts/` 目录识别出的用户字体（见 fonts.rs）。
const BUILTIN_FONT_CHOICES: &[(&str, &str)] = &[
    ("Consolas", "C:\\Windows\\Fonts\\consola.ttf"),
    ("黑体", "C:\\Windows\\Fonts\\simhei.ttf"),
    ("等线", "C:\\Windows\\Fonts\\Deng.ttf"),
];

/// 字体下拉的一项。`path` 为空表示 egui 默认等宽字体；
/// `index` 为 TTC 文件内的字体面序号（普通 ttf/otf 恒为 0）。
struct FontChoice {
    name: String,
    path: String,
    index: u32,
}

/// 构建字体下拉候选：默认字体 + 内置列表 + `fonts/` 目录用户字体。
/// 返回 (候选列表, 成功加载的用户字体数, 无法识别的文件数)。
fn build_font_choices() -> (Vec<FontChoice>, usize, usize) {
    let mut choices = vec![FontChoice {
        name: "默认字体".into(),
        path: String::new(),
        index: 0,
    }];
    for (name, file) in BUILTIN_FONT_CHOICES {
        choices.push(FontChoice {
            name: (*name).into(),
            path: (*file).into(),
            index: 0,
        });
    }
    let fonts_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("fonts")));
    let mut loaded = 0;
    let mut failed = 0;
    if let Some(dir) = fonts_dir {
        let (user_fonts, failed_n): (Vec<UserFont>, usize) = fonts::scan_fonts_dir(&dir);
        failed = failed_n;
        for uf in user_fonts {
            // 同名去重（内置优先）；用户字体排在内置之后
            if choices.iter().any(|c| c.name == uf.family) {
                continue;
            }
            choices.push(FontChoice {
                name: uf.family,
                path: uf.path.display().to_string(),
                index: uf.index,
            });
            loaded += 1;
        }
    }
    (choices, loaded, failed)
}

struct SearchUi {
    input: String,
    case_sensitive: bool,
    whole_word: bool,
    job: Option<SearchJob>,
    hits: Arc<Vec<SearchHit>>,
    selected: usize,
    /// 当前 hits 对应的查询（输入, 区分大小写, 全词）。
    /// 查询未变时不再重启后台搜索（避免"下一个"每点一次就全文件重扫）。
    last_query: Option<(String, bool, bool)>,
    /// 搜索完成后跳到第一个命中（"下一个"首次触发搜索时置位）
    jump_pending: bool,
    /// 搜索完成后选中第一个 >= 该偏移的命中（替换后定位下一命中用）
    jump_to_offset: Option<u64>,
}

impl Default for SearchUi {
    fn default() -> Self {
        Self {
            input: String::new(),
            case_sensitive: false,
            whole_word: false,
            job: None,
            hits: Arc::new(Vec::new()),
            selected: 0,
            last_query: None,
            jump_pending: false,
            jump_to_offset: None,
        }
    }
}

/// 一个标签页：独立的文档缓冲与视图状态。
struct Tab {
    id: u64,
    eb: EditableBuffer,
    /// 光标（虚拟字节偏移）
    cursor: u64,
    /// 选区锚点
    anchor: Option<u64>,
    /// 最近一帧可视行数
    last_visible: u64,
    first_line: u64,
    wrap_skip: u64,
    h_scroll: f32,
    h_extent: f32,
    /// 滚动条拖拽锁定：0=无，1=纵向，2=横向（拖动中指针滑出轨道仍保持滚动条模式）
    scroll_drag: u8,
    last_cursor: u64,
    wrap_rows: HashMap<u64, u32>,
    last_wrap_w: f32,
    load_starts: Vec<u64>,
    covered_lines: u64,
    covered_bytes: u64,
    load_total: u64,
    search: SearchUi,
    replace_input: String,
}

impl Tab {
    fn new(id: u64, eb: EditableBuffer) -> Self {
        Self {
            id,
            eb,
            cursor: 0,
            anchor: None,
            last_visible: 40,
            first_line: 0,
            wrap_skip: 0,
            h_scroll: 0.0,
            h_extent: 0.0,
            scroll_drag: 0,
            last_cursor: 0,
            wrap_rows: HashMap::new(),
            last_wrap_w: 0.0,
            load_starts: vec![0],
            covered_lines: 1,
            covered_bytes: 0,
            load_total: 0,
            search: SearchUi::default(),
            replace_input: String::new(),
        }
    }

    fn new_empty(id: u64) -> Self {
        Self::new(id, EditableBuffer::from_vec(Vec::new()))
    }

    fn title(&self) -> String {
        let name = self
            .eb
            .path()
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "无标题".into());
        if self.is_loading() {
            format!("{name} ⏳")
        } else {
            name
        }
    }

    fn is_loading(&self) -> bool {
        !self.eb.has_index()
    }

    fn sel_range(&self) -> Option<(u64, u64)> {
        let a = self.anchor?;
        let (s, e) = (a.min(self.cursor), a.max(self.cursor));
        (s < e).then_some((s, e))
    }

    fn invalidate_search(&mut self) {
        if let Some(job) = self.search.job.take() {
            job.cancel();
        }
        self.search.hits = Arc::new(Vec::new());
        self.search.selected = 0;
        self.search.last_query = None;
        self.search.jump_pending = false;
        self.search.jump_to_offset = None;
        self.wrap_rows.clear();
    }

    fn scroll_to_cursor(&mut self) {
        if !self.eb.has_index() {
            return;
        }
        let line = self.eb.line_of_offset(self.cursor);
        if line < self.first_line || line >= self.first_line + self.last_visible {
            self.first_line = line.saturating_sub(self.last_visible / 4);
            self.wrap_skip = 0;
        }
    }

    // ---------- 编辑动作（状态信息由调用方显示）----------

    fn insert_text(&mut self, s: &str) {
        let range = self.sel_range();
        let pos = range.map(|(st, _)| st).unwrap_or(self.cursor);
        if let Some((_, en)) = range {
            let _ = self.eb.delete(pos, en - pos);
        }
        // 按文件编码落盘（GBK 文件把输入转 GBK 字节）
        let bytes = self.eb.encoding().encode_str(s);
        if self.eb.insert(pos, &bytes).is_ok() {
            self.cursor = pos + bytes.len() as u64;
            self.anchor = None;
            self.invalidate_search();
        }
        self.scroll_to_cursor();
    }

    fn delete_at_cursor(&mut self, forward: bool) {
        let range = self.sel_range();
        let cursor = self.cursor;
        if let Some((st, en)) = range {
            let _ = self.eb.delete(st, en - st);
            self.cursor = st;
        } else if forward {
            if cursor < self.eb.len() {
                let end = next_boundary(&self.eb, cursor);
                let _ = self.eb.delete(cursor, end - cursor);
            }
        } else if cursor > 0 {
            let start = prev_boundary(&self.eb, cursor);
            let _ = self.eb.delete(start, cursor - start);
            self.cursor = start;
        } else {
            return;
        }
        self.anchor = None;
        self.invalidate_search();
        self.scroll_to_cursor();
    }

    fn move_cursor(&mut self, new: u64, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = new;
        if !extend {
            self.anchor = None;
        }
        self.scroll_to_cursor();
    }

    fn move_line(&mut self, delta: i64, extend: bool) {
        let cur_line = self.eb.line_of_offset(self.cursor);
        let col = self.cursor - self.eb.start_of_line(cur_line);
        let n = self.eb.line_count();
        let new_line = (cur_line as i64 + delta).clamp(0, n as i64 - 1) as u64;
        let ls = self.eb.start_of_line(new_line);
        let le = self.eb.line_bounds(new_line).1;
        self.move_cursor((ls + col).min(le), extend);
    }

    fn do_undo(&mut self) {
        if self.eb.undo() {
            self.cursor = self.cursor.min(self.eb.len());
            self.anchor = None;
            self.invalidate_search();
        }
        self.scroll_to_cursor();
    }

    fn do_redo(&mut self) {
        if self.eb.redo() {
            self.cursor = self.cursor.min(self.eb.len());
            self.anchor = None;
            self.invalidate_search();
        }
        self.scroll_to_cursor();
    }

    fn copy_selection(&mut self) -> Option<usize> {
        let (st, en) = self.sel_range()?;
        let len = (en - st) as usize;
        let mut buf = vec![0u8; len];
        self.eb.read(st, &mut buf).ok()?;
        let text = self.eb.encoding().decode_str(&buf);
        if !clipboard_set(&text) {
            return None;
        }
        Some(text.chars().count())
    }

    fn cut_selection(&mut self) -> Option<usize> {
        let (st, en) = self.sel_range()?;
        let len = (en - st) as usize;
        let mut buf = vec![0u8; len];
        self.eb.read(st, &mut buf).ok()?;
        let text = self.eb.encoding().decode_str(&buf);
        if !clipboard_set(&text) {
            return None;
        }
        let _ = self.eb.delete(st, en - st);
        self.cursor = st;
        self.anchor = None;
        self.invalidate_search();
        Some(text.chars().count())
    }

    fn delete_selection(&mut self) {
        if let Some((st, en)) = self.sel_range() {
            let _ = self.eb.delete(st, en - st);
            self.cursor = st;
            self.anchor = None;
            self.invalidate_search();
        }
    }

    fn transform_selection(&mut self, mode: CaseMode) {
        if let Some((st, en)) = self.sel_range() {
            let len = (en - st) as usize;
            let mut buf = vec![0u8; len];
            if self.eb.read(st, &mut buf).is_err() {
                return;
            }
            let enc = self.eb.encoding();
            let text = enc.decode_str(&buf);
            let transformed = match mode {
                CaseMode::Upper => text.to_uppercase(),
                CaseMode::Lower => text.to_lowercase(),
                CaseMode::Title => title_case(&text),
            };
            let bytes = enc.encode_str(&transformed);
            let _ = self.eb.replace(st, en - st, &bytes);
            self.cursor = st + bytes.len() as u64;
            self.anchor = None;
            self.invalidate_search();
        }
    }
}

#[derive(Clone, Copy)]
enum CaseMode {
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

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    open_rx: Option<Receiver<OpenMsg>>,
    load_cancels: HashMap<u64, Arc<AtomicBool>>,
    open_error: Option<String>,
    path_input: String,
    focus_path: bool,
    font_size: f32,
    font_name: String,
    /// 字体下拉候选（默认 + 内置 + fonts/ 目录用户字体）
    font_choices: Vec<FontChoice>,
    wrap: bool,
    show_find: bool,
    show_replace: bool,
    show_settings: bool,
    show_about: bool,
    /// 查找面板打开后请求聚焦查找输入框（一次）
    focus_find: bool,
    /// 后台打印状态接收端（spool 线程 → 状态栏）
    print_rx: Option<Receiver<String>>,
    /// Ctrl+P 暂存标记（渲染期标签页已取出，update 末尾标签放回后处理）
    print_requested: bool,
    lang: Lang,
    theme_mode: ThemeMode,
    /// 已应用到 egui 的面板主题（None = 尚未应用；与 theme_mode 不一致时在
    /// update 开头重新 apply_panel_theme，兜底 eframe 对样式的任何后续覆盖）
    applied_theme: Option<ThemeMode>,
    editor_focused: bool,
    /// 上一帧各标签页矩形（id, rect），供拖拽命中测试
    tab_rects: Vec<(u64, egui::Rect)>,
    /// 本帧指针 x（渲染时记录）
    pointer_x: f32,
    recent_files: Vec<PathBuf>,
    confirm_close: Option<usize>,
    status: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (lang, theme_mode) = load_settings();
        apply_panel_theme(&cc.egui_ctx, theme_mode == ThemeMode::Dark);
        // Ctrl+加号/减号/0 由本程序接管（编辑区字号缩放）；
        // 关闭 egui 原生的整体 UI 键盘缩放，避免两套缩放互相干扰
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let (font_choices, fonts_loaded, fonts_failed) = build_font_choices();
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            open_rx: None,
            load_cancels: HashMap::new(),
            open_error: None,
            path_input: String::new(),
            focus_path: false,
            font_size: 15.0,
            font_name: "默认字体".into(),
            font_choices,
            wrap: false,
            show_find: false,
            show_replace: false,
            show_settings: false,
            show_about: false,
            focus_find: false,
            print_rx: None,
            print_requested: false,
            lang,
            theme_mode,
            applied_theme: None,
            editor_focused: false,
            tab_rects: Vec::new(),
            pointer_x: 0.0,
            recent_files: load_recent_files(),
            confirm_close: None,
            status: tr(lang, "empty.hint1").into(),
        };
        if let Some(p) = std::env::args().nth(1) {
            app.path_input = p.clone();
            app.request_open(PathBuf::from(p));
        }
        // fonts/ 目录扫描结果提示
        if fonts_loaded > 0 || fonts_failed > 0 {
            let zh = lang.is_zh();
            let mut msg = String::new();
            if fonts_loaded > 0 {
                msg.push_str(&trf(lang, "status.fonts_loaded", &[&fonts_loaded.to_string()]));
            }
            if fonts_failed > 0 {
                if !msg.is_empty() {
                    msg.push_str(if zh { "，" } else { ", " });
                }
                msg.push_str(&trf(lang, "status.fonts_failed", &[&fonts_failed.to_string()]));
            }
            app.status = msg;
        }
        app
    }

    fn active_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// 编辑区缩放：按系数调整字号（0.5px 步进，8~40px），并失效全部标签页的排版缓存
    fn zoom_editor(&mut self, factor: f32) {
        let s = self.font_size * factor;
        self.font_size = ((s * 2.0).round() / 2.0).clamp(8.0, 40.0);
        for t in &mut self.tabs {
            t.wrap_rows.clear();
            t.wrap_skip = 0;
            t.h_scroll = 0.0;
        }
    }

    /// 恢复默认字号（15px）
    fn zoom_reset(&mut self) {
        self.font_size = 15.0;
        for t in &mut self.tabs {
            t.wrap_rows.clear();
            t.wrap_skip = 0;
            t.h_scroll = 0.0;
        }
    }

    fn is_loading(&self) -> bool {
        self.tabs.get(self.active).map(|t| t.is_loading()).unwrap_or(false)
    }

    /// 取出当前标签页（渲染期间独占，结束后放回）
    fn new_tab(&mut self) {
        let t = Tab::new_empty(self.next_tab_id);
        self.next_tab_id += 1;
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
    }

    fn save_all_dirty(&mut self) -> usize {
        let mut saved = 0;
        for t in self.tabs.iter_mut() {
            if t.eb.is_dirty() && !t.eb.path().as_os_str().is_empty() {
                if t.eb.save().is_ok() {
                    saved += 1;
                }
            }
        }
        saved
    }

    fn close_active_tab(&mut self) {
        let idx = self.active;
        self.close_tab(idx);
    }

    fn eb_take_active(&mut self) -> Option<Tab> {
        if self.tabs.is_empty() {
            return None;
        }
        let idx = self.active.min(self.tabs.len() - 1);
        Some(self.tabs.remove(idx))
    }

    fn eb_put_back(&mut self, tab: Tab) {
        let idx = self.active.min(self.tabs.len());
        self.tabs.insert(idx, tab);
    }

    fn request_open(&mut self, path: PathBuf) {
        let lang = self.lang;
        // 终止上一个仍在进行的加载
        for c in self.load_cancels.values() {
            c.store(true, Ordering::Relaxed);
        }
        self.load_cancels.clear();

        // 记录到最近文件列表
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !self.recent_files.contains(&canon) {
            self.recent_files.insert(0, canon.clone());
            if self.recent_files.len() > 20 {
                self.recent_files.truncate(20);
            }
        } else if let Some(pos) = self.recent_files.iter().position(|p| *p == canon) {
            let f = self.recent_files.remove(pos);
            self.recent_files.insert(0, f);
        }
        // 持久化到磁盘
        save_recent_files(&self.recent_files);
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancels.insert(id, Arc::clone(&cancel));

        let (tx, rx) = std::sync::mpsc::channel();
        self.open_rx = Some(rx);
        self.open_error = None;
        self.status = format!("{} {}", tr(lang, "status.reading"), path.display());
        std::thread::spawn(move || {
            let eb = match EditableBuffer::open_head(&path, &OpenOptions::default()) {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(OpenMsg::Error { tab_id: id, msg: e.to_string() });
                    return;
                }
            };
            let bom = eb.encoding().bom_len();
            let vlen = eb.len().saturating_sub(bom);
            let base = eb.base_arc();
            let _ = tx.send(OpenMsg::Head { tab_id: id, eb, total: vlen });
            let Some(base) = base else { return };

            // 顺序读盘 + 渐进建索引（机械盘最优 I/O 模式）
            let mut builder = LineIndexBuilder::new(bom);
            let win = 16 << 20;
            let mut buf = vec![0u8; win as usize];
            let mut covered = 0u64;
            let mut win_starts: Vec<u64> = Vec::new();
            let mut pending_gui: Vec<u64> = Vec::new();
            let mut next_report = HEAD_SCAN;
            while covered < vlen {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let wsize = if covered == 0 { HEAD_SCAN } else { win };
                let n = wsize.min(vlen - covered) as usize;
                if let Err(e) = base.read_range(bom + covered, n, &mut buf[..n]) {
                    let _ = tx.send(OpenMsg::Error { tab_id: id, msg: e.to_string() });
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
                    let _ = tx.send(OpenMsg::Coverage {
                        tab_id: id,
                        starts: Arc::new(std::mem::take(&mut pending_gui)),
                        covered_lines: builder.covered_lines(),
                        covered_bytes: covered,
                    });
                    next_report = covered + (64 << 20).max(covered / 10);
                }
            }
            let _ = tx.send(OpenMsg::Index { tab_id: id, idx: Box::new(builder.finish(vlen)) });
        });
    }

    fn start_search(&mut self) {
        let Some(tab) = self.active_mut() else { return };
        if !tab.eb.has_index() {
            self.status = "行索引构建中，完成后可搜索".into();
            return;
        }
        if tab.search.input.is_empty() {
            return;
        }
        // 幂等守卫：同一查询且搜索已完成 → 直接用现有结果，不重扫
        let query_key = (
            tab.search.input.clone(),
            tab.search.case_sensitive,
            tab.search.whole_word,
        );
        if tab.search.job.is_none() && tab.search.last_query.as_ref() == Some(&query_key) {
            return;
        }
        if let Some(job) = &tab.search.job {
            job.cancel();
        }
        // 查找词按文件编码转换（GBK 文件 → GBK 字节）
        let gbk = tab.eb.encoding().is_gbk();
        let needle = tab.eb.encoding().encode_str(&tab.search.input);
        let query = if gbk {
            SearchQuery::new_gbk(needle, tab.search.case_sensitive)
        } else {
            SearchQuery::new(needle, tab.search.case_sensitive)
        };
        let Some(base) = tab.eb.base_arc() else { return };
        match SearchJob::start(base, tab.eb.index_arc(), query, GUI_MAX_HITS, 0) {
            Ok(job) => {
                tab.search.hits = Arc::new(Vec::new());
                tab.search.selected = 0;
                tab.search.last_query = Some(query_key);
                tab.search.job = Some(job);
            }
            Err(e) => self.status = format!("搜索失败: {e}"),
        }
    }

    /// "下一个/上一个"：结果已就绪直接跳；否则先启动搜索并在完成后跳到首个命中。
    fn search_next(&mut self, delta: i64) {
        let Some(tab) = self.active_mut() else { return };
        if tab.search.input.is_empty() {
            return;
        }
        if tab.search.job.is_some() {
            return; // 搜索进行中，等完成后由 jump_pending 跳转
        }
        let query_key = (
            tab.search.input.clone(),
            tab.search.case_sensitive,
            tab.search.whole_word,
        );
        if tab.search.last_query.as_ref() == Some(&query_key) && !tab.search.hits.is_empty() {
            self.goto_hit(delta);
        } else {
            self.start_search();
            if let Some(t) = self.active_mut() {
                t.search.jump_pending = true;
            }
        }
    }

    fn goto_hit(&mut self, delta: i64) {
        let Some(tab) = self.active_mut() else { return };
        let total = tab.search.hits.len() as i64;
        if total == 0 {
            return;
        }
        tab.search.selected =
            ((tab.search.selected as i64 + delta).rem_euclid(total)) as usize;
        // 行号动态计算：命中表可能经过替换后的增量修正，行号字段不可信
        let offset = tab.search.hits[tab.search.selected].offset;
        if tab.eb.has_index() {
            let line = tab.eb.line_of_offset(offset);
            tab.first_line = line.saturating_sub(tab.last_visible / 4);
        }
        tab.wrap_skip = 0;
    }

    fn replace_current(&mut self) {
        let lang = self.lang;
        let Some(repl) = non_empty(&self.replace_text_input()) else { return };
        let Some(tab) = self.active_mut() else { return };
        let sel = tab.search.selected;
        let Some(h) = tab.search.hits.get(sel).cloned() else { return };
        // 防御：命中处内容应与查找词一致（结果过期时放弃本次替换并重扫）
        let enc = tab.eb.encoding();
        let needle = enc.encode_str(&tab.search.input);
        let repl_bytes = enc.encode_str(&repl);
        let gbk = enc.is_gbk();
        let mut cur = vec![0u8; h.len as usize];
        let matched = tab.eb.read(h.offset, &mut cur).is_ok()
            && if tab.search.case_sensitive || gbk {
                // GBK 非大小写敏感场景：命中来自 GBK 感知折叠的搜索，
                // 这里用折叠后比较保持一致
                let mut a = cur.clone();
                let mut b = needle.clone();
                hieditor_core::storage::encoding::fold_ascii(&mut a, gbk);
                hieditor_core::storage::encoding::fold_ascii(&mut b, gbk);
                a == b
            } else {
                cur.eq_ignore_ascii_case(&needle)
            };
        if !matched {
            tab.invalidate_search();
            self.status = "搜索结果已过期，正在重新搜索".into();
            self.start_search();
            return;
        }
        if tab
            .eb
            .replace(h.offset, u64::from(h.len), &repl_bytes)
            .is_err()
        {
            return;
        }
        // 增量修正命中表：移除已替换项，其后命中偏移平移。
        // 不整文件重扫——连续点"替换"时无需等待 GB 级扫描。
        let delta = repl_bytes.len() as i64 - h.len as i64;
        {
            let hits = Arc::make_mut(&mut tab.search.hits);
            hits.remove(sel);
            for hit in hits.iter_mut().skip(sel) {
                hit.offset = (hit.offset as i64 + delta).max(0) as u64;
            }
            // selected 停在原位 = 原命中的下一条，连续替换直接按"替换"即可
            tab.search.selected = if hits.is_empty() { 0 } else { sel.min(hits.len() - 1) };
        }
        tab.cursor = h.offset + repl_bytes.len() as u64;
        tab.anchor = None;
        tab.scroll_to_cursor();
        self.status = tr(lang, "status.replaced1").into();
    }

    fn replace_all_in_active(&mut self) {
        let lang = self.lang;
        let Some(repl) = non_empty(&self.replace_text_input()) else { return };
        let Some(tab) = self.active_mut() else { return };
        if !tab.eb.has_index() {
            self.status = "行索引构建中".into();
            return;
        }
        if tab.search.input.is_empty() {
            self.status = "搜索内容为空".into();
            return;
        }
        if tab.search.whole_word {
            self.status = tr(lang, "status.wordall").into();
            return;
        }
        let enc = tab.eb.encoding();
        let needle = enc.encode_str(&tab.search.input);
        let repl_bytes = enc.encode_str(&repl);
        let cnt = tab.eb.replace_all(
            &needle,
            &repl_bytes,
            !tab.search.case_sensitive,
            enc.is_gbk(),
        );
        tab.invalidate_search();
        self.status = format!("{}: {cnt}", tr(lang, "status.replaced_all"));
    }

    fn replace_text_input(&self) -> String {
        self.tabs
            .get(self.active)
            .map(|t| t.replace_input.clone())
            .unwrap_or_default()
    }

    /// 打印当前标签页：系统打印对话框 → 范围解析 → 物化行文本 → 后台 spool。
    fn start_print(&mut self) {
        let lang = self.lang;
        // ---- 阶段 1：读取当前标签信息（借用随块结束） ----
        struct PrintInfo {
            total_lines: u64,
            cur_line: u64,
            sel_lines: Option<(u64, u64)>,
            doc_name: String,
        }
        let info = {
            let Some(tab) = self.active_mut() else { return };
            if !tab.eb.has_index() {
                self.status = tr(lang, "print.noindex").into();
                return;
            }
            let total_lines = tab.eb.line_count();
            if total_lines == 0 {
                self.status = tr(lang, "print.empty").into();
                return;
            }
            let sel_lines = tab.sel_range().map(|(s, e)| {
                let a = tab.eb.line_of_offset(s);
                let b = tab.eb.line_of_offset(e.saturating_sub(1).max(s));
                (a, b)
            });
            PrintInfo {
                total_lines,
                cur_line: tab.eb.line_of_offset(tab.cursor),
                sel_lines,
                doc_name: tab.title(),
            }
        };
        let font_pt = self.font_size;
        let font_face = match self.font_name.as_str() {
            "Consolas" => "Consolas",
            "黑体" => "SimHei",
            "等线" => "DengXian",
            _ => "Consolas",
        }
        .to_string();

        // ---- 阶段 2：系统打印对话框（模态；取消则静默返回） ----
        let est_lpp = printing::estimate_lines_per_page(font_pt).max(1);
        let est_total =
            (((info.total_lines + est_lpp - 1) / est_lpp) as u32).min(9_999_999);
        let Some(dlg) = printing::run_print_dialog(est_total) else { return };

        // ---- 阶段 3：按真实打印机参数解析范围 → 页码列表 ----
        let lpp = printing::page_metrics(dlg.hdc, font_pt).lines_per_page.max(1);
        let (base_line, line_count) = match &dlg.scope {
            printing::PrintScope::Selection => match info.sel_lines {
                Some((a, b)) if b >= a => (a, b - a + 1),
                _ => {
                    unsafe { printing::discard_dc(dlg.hdc) };
                    self.status = tr(lang, "print.nosel").into();
                    return;
                }
            },
            _ => (0, info.total_lines),
        };
        let total_pages = ((line_count + lpp - 1) / lpp) as u32;
        let mut pages: Vec<u32> = Vec::new();
        match &dlg.scope {
            printing::PrintScope::All | printing::PrintScope::Selection => {
                pages.extend(1..=total_pages);
            }
            printing::PrintScope::CurrentPage => {
                let p = (info.cur_line.saturating_sub(base_line) / lpp + 1) as u32;
                pages.push(p.clamp(1, total_pages.max(1)));
            }
            printing::PrintScope::Pages(ranges) => {
                for r in ranges {
                    let a = r.0.max(1);
                    let b = r.1.max(a);
                    if a <= total_pages {
                        pages.extend(a..=b.min(total_pages));
                    }
                }
                pages.sort_unstable();
                pages.dedup();
            }
        }
        if pages.is_empty() {
            unsafe { printing::discard_dc(dlg.hdc) };
            self.status = tr(lang, "print.nopages").into();
            return;
        }
        if pages.len() > printing::PRINT_PAGE_CAP {
            unsafe { printing::discard_dc(dlg.hdc) };
            self.status =
                trf(lang, "print.toomany", &[&printing::PRINT_PAGE_CAP.to_string()]);
            return;
        }

        // ---- 阶段 4：物化行文本（tab 借用随块结束） ----
        let mut lines: Vec<String> = Vec::new();
        {
            let Some(tab) = self.active_mut() else {
                unsafe { printing::discard_dc(dlg.hdc) };
                return;
            };
            for p in &pages {
                let l0 = base_line + ((*p as u64 - 1) * lpp);
                let l1 = (l0 + lpp - 1).min(base_line + line_count - 1);
                for line in l0..=l1 {
                    let (_s, _e, text) =
                        read_line_text(&tab.eb, true, &[], 0, 0, line);
                    let mut t = text.replace('\t', "    ");
                    if t.chars().count() > 8192 {
                        t = t.chars().take(8192).collect();
                    }
                    lines.push(t);
                }
            }
        }
        self.print_rx = Some(printing::spool(
            dlg.hdc,
            Arc::new(lines),
            lpp,
            info.doc_name,
            font_face,
            font_pt,
            dlg.printer,
        ));
    }

    /// 解析 spool 线程的状态消息为本地化文本
    fn print_status_text(m: &str, lang: Lang) -> String {
        let mut it = m.splitn(3, '\t');
        match it.next() {
            Some("printing") => {
                trf(lang, "print.status.printing", &[it.next().unwrap_or("0")])
            }
            Some("done") => {
                let n = it.next().unwrap_or("0");
                let p = it.next().unwrap_or("");
                trf(lang, "print.status.done", &[n, p])
            }
            Some("fail") => tr(lang, "print.status.fail").into(),
            _ => m.to_string(),
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let dirty = self.tabs[idx].eb.is_dirty();
        if dirty {
            self.confirm_close = Some(idx);
            return;
        }
        self.remove_tab(idx);
    }

    fn remove_tab(&mut self, idx: usize) {
        let id = self.tabs[idx].id;
        if let Some(c) = self.load_cancels.get(&id) {
            c.store(true, Ordering::Relaxed);
        }
        self.load_cancels.remove(&id);
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            let t = Tab::new_empty(self.next_tab_id);
            self.next_tab_id += 1;
            self.tabs.push(t);
            self.active = 0;
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// pos 前一个字符边界（UTF-8 / GBK 安全）。
fn prev_boundary(eb: &EditableBuffer, mut pos: u64) -> u64 {
    if pos == 0 {
        return 0;
    }
    if eb.encoding().is_gbk() {
        // GBK：pos-1 是次字节且 pos-2 是前导 → 双字节汉字，回退 2
        let mut b1 = [0u8; 1];
        let mut b2 = [0u8; 1];
        if pos >= 2
            && eb.read(pos - 1, &mut b1).is_ok()
            && hieditor_core::storage::encoding::is_gbk_trail(b1[0])
            && eb.read(pos - 2, &mut b2).is_ok()
            && hieditor_core::storage::encoding::is_gbk_lead(b2[0])
        {
            return pos - 2;
        }
        return pos - 1;
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

/// pos 后一个字符边界（UTF-8 / GBK 安全）。
fn next_boundary(eb: &EditableBuffer, mut pos: u64) -> u64 {
    let vlen = eb.len();
    if pos >= vlen {
        return vlen;
    }
    if eb.encoding().is_gbk() {
        let mut b = [0u8; 1];
        if eb.read(pos, &mut b).is_ok()
            && hieditor_core::storage::encoding::is_gbk_lead(b[0])
            && pos + 1 < vlen
        {
            return pos + 2;
        }
        return pos + 1;
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

/// 把字节下标回退到最近的 UTF-8 字符边界（用于 LayoutJob 分段区间）。
fn clamp_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 文件字节偏移（行内相对）→ 解码文本字节偏移。
/// UTF-8 系恒等；GBK 按字符逐个累加编码长度（渲染层定位统一走此映射）。
fn file_rel_to_dec(
    enc: hieditor_core::storage::Encoding,
    text: &str,
    off: usize,
) -> usize {
    if enc.is_utf8_family() {
        return off.min(text.len());
    }
    let mut dec = 0usize;
    let mut gb = 0usize;
    for c in text.chars() {
        if gb >= off {
            break;
        }
        gb += enc.char_len(c);
        dec += c.len_utf8();
    }
    dec
}

/// 解码文本字节偏移 → 文件字节偏移（GBK 行做逐字映射，UTF-8 系恒等）。
fn dec_rel_to_file(
    enc: hieditor_core::storage::Encoding,
    text: &str,
    off: usize,
) -> usize {
    if enc.is_utf8_family() {
        return off.min(text.len());
    }
    let mut dec = 0usize;
    let mut gb = 0usize;
    for c in text.chars() {
        if dec + c.len_utf8() > off {
            break;
        }
        dec += c.len_utf8();
        gb += enc.char_len(c);
    }
    gb
}

/// 双击选词：包含 pos 的连续非空白字符段（遇空白结束，UTF-8 / GBK 安全）。
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
    let is_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c);
    if eb.encoding().is_gbk() {
        // GBK：正向扫描收集字符起点（次字节可与 ASCII 重叠，必须从行首走），
        // 空白只可能出现在单字节字符上（GBK 汉字首字节 ≥ 0x81）
        let bytes: &[u8] = v;
        let rel = ((pos - s) as usize).min(bytes.len());
        let mut starts: Vec<usize> = Vec::with_capacity(bytes.len() + 1);
        let mut i = 0usize;
        while i < bytes.len() {
            starts.push(i);
            i += if bytes[i] < 0x80 { 1 } else { 2 };
        }
        let n_chars = starts.len();
        if n_chars == 0 {
            return (s, s);
        }
        let idx = starts
            .partition_point(|&st| st <= rel)
            .saturating_sub(1)
            .min(n_chars - 1);
        let mut lo = idx;
        while lo > 0 && !is_ws(bytes[starts[lo - 1]]) {
            lo -= 1;
        }
        let mut hi = idx;
        while hi < n_chars && !is_ws(bytes[starts[hi]]) {
            hi += 1;
        }
        return (s + starts[lo] as u64, s + starts[hi] as u64);
    }
    let text = String::from_utf8_lossy(v);
    let bytes = text.as_bytes();
    let rel = ((pos - s) as usize).min(bytes.len());
    let mut a = rel;
    while a > 0 && !is_ws(bytes[a - 1]) {
        a -= 1;
    }
    let mut b2 = rel;
    while b2 < bytes.len() && !is_ws(bytes[b2]) {
        b2 += 1;
    }
    while a > 0 && !text.is_char_boundary(a) {
        a -= 1;
    }
    while b2 < bytes.len() && !text.is_char_boundary(b2) {
        b2 += 1;
    }
    (s + a as u64, s + b2 as u64)
}

/// 行界来源：索引就绪走索引；否则走已扫描前缀。
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

/// 读取一行的显示文本，返回 (行起点, 原始行尾, 显示文本)。
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
    (s, e, eb.encoding().decode_str(v))
}

/// 换行模式下某逻辑行的视觉行数（带缓存）。
#[allow(clippy::too_many_arguments)]
fn rows_of_line(
    cache: &mut HashMap<u64, u32>,
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
    let (_, _, text) =
        read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
    let galley = ui.fonts(|f| f.layout(text, font.clone(), theme_ui::TEXT_MEASURE, text_w));
    let r = galley.rows.len().max(1) as u32;
    if cache.len() > 100_000 {
        cache.clear();
    }
    cache.insert(line, r);
    r
}

/// 换行模式滚轮：按视觉行滚动。
#[allow(clippy::too_many_arguments)]
fn step_visual(
    cache: &mut HashMap<u64, u32>,
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
    let cur = rows_of_line(
        cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, *first,
    ) as u64;
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
        let prev = rows_of_line(
            cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes,
            *first,
        ) as u64;
        *skip = prev - 1;
    }
}

/// 非换行模式点击命中（与渲染共用排版引擎）。
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
    let galley = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), theme_ui::TEXT_MEASURE));
    let sp = egui::vec2(pos.x - text_left + h_scroll, line_height * 0.5);
    let cur = galley.cursor_from_pos(sp);
    let enc = eb.encoding();
    if enc.is_utf8_family() {
        let char_idx = cur.index.min(text.chars().count());
        let byte: usize = text.chars().take(char_idx).map(|c| c.len_utf8()).sum();
        s + byte as u64
    } else {
        s + dec_rel_to_file(enc, &text, cur.index.min(text.len())) as u64
    }
}

/// 换行模式点击命中。
#[allow(clippy::too_many_arguments)]
fn cursor_at_pointer_wrap(
    cache: &mut HashMap<u64, u32>,
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
    let mut rows = rows_of_line(
        cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes, line,
    ) as u64;
    while rem >= rows && line + 1 < n {
        rem -= rows;
        line += 1;
        rows = rows_of_line(
            cache, ui, font, text_w, eb, index_ready, load_starts, covered_lines, covered_bytes,
            line,
        ) as u64;
    }
    if rem >= rows {
        rem = rows - 1;
    }
    let (s, _e, text) =
        read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
    if text.is_empty() {
        return s;
    }
    let galley = ui.fonts(|f| f.layout(text.clone(), font.clone(), theme_ui::TEXT_MEASURE, text_w));
    let sp = egui::vec2(pos.x - text_left, rem as f32 * line_height + line_height * 0.5);
    let cur = galley.cursor_from_pos(sp);
    let enc = eb.encoding();
    if enc.is_utf8_family() {
        let char_idx = cur.index.min(text.chars().count());
        let byte: usize = text.chars().take(char_idx).map(|c| c.len_utf8()).sum();
        s + byte as u64
    } else {
        s + dec_rel_to_file(enc, &text, cur.index.min(text.len())) as u64
    }
}

// 最近文件持久化
fn recent_conf_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("recent_files.txt")))
}

fn load_recent_files() -> Vec<PathBuf> {
    let Some(p) = recent_conf_path() else { return Vec::new() };
    std::fs::read_to_string(p)
        .map(|text| {
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn save_recent_files(files: &[PathBuf]) {
    if let Some(p) = recent_conf_path() {
        let text: String = files
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(p, text);
    }
}

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

/// 应用编辑字体选择：重建字体定义（保留 CJK 回退）。
/// `index` 为 TTC 文件内的字体面序号（普通 ttf/otf 传 0）。
fn apply_editor_font(ctx: &egui::Context, file: &str, index: u32) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\simhei.ttf") {
        fonts
            .font_data
            .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(data)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
    }
    if !file.is_empty() {
        if let Ok(data) = std::fs::read(file) {
            let mut fd = egui::FontData::from_owned(data);
            fd.index = index;
            fonts.font_data.insert(
                "editor-font".to_owned(),
                Arc::new(fd),
            );
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "editor-font".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    // 窗口图标（任务栏/标题栏）
    let icon_rgba = image::load_from_memory(LOGO_PNG)
        .expect("内置 logo 解码失败")
        .to_rgba8();
    let icon = egui::IconData {
        width: icon_rgba.width(),
        height: icon_rgba.height(),
        rgba: icon_rgba.into_raw(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_title("HiEditor Pro")
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "HiEditor Pro",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(App::new(cc)))
        }),
    )
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let lang = self.lang;
        // 面板主题同步：首帧必发（None），theme_mode 变化后重发。
        // 兜底 eframe/egui 对启动样式的任何延迟覆盖（系统主题事件、持久化恢复等）。
        if self.applied_theme != Some(self.theme_mode) {
            apply_panel_theme(ctx, self.theme_mode == ThemeMode::Dark);
            self.applied_theme = Some(self.theme_mode);
        }
        let th = editor_theme(self.theme_mode == ThemeMode::Dark);

        // ---- 全局快捷键（不依赖编辑区焦点）----
        // F3 / Shift+F3：搜索下一个/上一个；Ctrl+加号/减号/0：缩放；
        // Ctrl+F / Ctrl+H：查找/查找替换（查找面板开着时也可再按）
        {
            let mods = ctx.input(|i| i.modifiers);
            if ctx.input(|i| i.key_pressed(egui::Key::F3)) {
                if mods.shift {
                    self.search_next(-1);
                } else {
                    self.search_next(1);
                }
            }
            if mods.ctrl && ctx.input(|i| i.key_pressed(egui::Key::F)) {
                self.show_find = true;
                self.show_replace = false;
                self.focus_find = true;
            }
            if mods.ctrl && ctx.input(|i| i.key_pressed(egui::Key::H)) {
                self.show_find = true;
                self.show_replace = true;
                self.focus_find = true;
            }
            if mods.ctrl {
                if ctx.input(|i| i.key_pressed(egui::Key::Plus))
                    || ctx.input(|i| i.key_pressed(egui::Key::Equals))
                {
                    self.zoom_editor(1.2);
                } else if ctx.input(|i| i.key_pressed(egui::Key::Minus)) {
                    self.zoom_editor(1.0 / 1.2);
                } else if ctx.input(|i| i.key_pressed(egui::Key::Num0)) {
                    self.zoom_reset();
                }
            }
        }
        self.pointer_x = ctx
            .input(|i| i.pointer.latest_pos().map(|p| p.x).unwrap_or(f32::NAN));
        self.pointer_x = ctx
            .input(|i| i.pointer.latest_pos().map(|p| p.x).unwrap_or(f32::NAN));
        self.pointer_x = ctx
            .input(|i| i.pointer.latest_pos().map(|p| p.x).unwrap_or(f32::NAN));
        // ---- 轮询后台打开（按 tab_id 路由）----
        if let Some(rx) = &self.open_rx {
            loop {
                match rx.try_recv() {
                    Ok(OpenMsg::Head { tab_id, eb, total }) => {
                        if !self.load_cancels.contains_key(&tab_id) {
                            continue; // 标签页已被关闭
                        }
                        let title = eb
                            .path()
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "无标题".into());
                        let backend = eb.backend().to_string();
                        let size = fmt_size(eb.len());
                        let mut t = Tab::new(tab_id, eb);
                        t.load_total = total;
                        self.tabs.push(t);
                        self.active = self.tabs.len() - 1;
                        self.status =
                            format!("{} {title}（{backend} · {size}）· {}", tr(lang, "status.opened"), tr(lang, "status.indexing"));
                    }
                    Ok(OpenMsg::Coverage { tab_id, starts, covered_lines, covered_bytes }) => {
                        if let Some(t) = self
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == tab_id)
                        {
                            t.load_starts.extend(starts.iter().copied());
                            t.covered_lines = covered_lines;
                            t.covered_bytes = covered_bytes;
                        }
                    }
                    Ok(OpenMsg::Index { tab_id, idx }) => {
                        self.load_cancels.remove(&tab_id);
                        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                            t.eb.set_index(*idx);
                            let lines = t.eb.line_count();
                            if self.tabs[self.active].id == tab_id {
                                self.status =
                                    format!("{}: {lines} {}", tr(lang, "status.ready"), tr(lang, "status.editable"));
                            }
                        }
                    }
                    Ok(OpenMsg::Error { tab_id, msg }) => {
                        let _ = tab_id;
                        let _ = tab_id;
                    self.open_error = Some(format!("{}: {msg}", tr(lang, "status.openfailed")));
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

        // ---- 轮询所有标签页的后台搜索 ----
        for ti in 0..self.tabs.len() {
            let done = self.tabs[ti]
                .search
                .job
                .as_ref()
                .map(|j| j.is_done())
                .unwrap_or(false);
            if !done {
                continue;
            }
            let Some(job) = self.tabs[ti].search.job.take() else { continue };
            let tab = &mut self.tabs[ti];
            if let Some(err) = job.error() {
                if ti == self.active {
                    self.status = format!("搜索失败: {err}");
                }
                continue;
            }
            let mut hits = job.hits_snapshot();
            hits.sort_by_key(|h| h.offset);
            // 全词匹配过滤
            if tab.search.whole_word {
                let vlen = tab.eb.len();
                hits.retain(|h| {
                    let mut ok = true;
                    let mut b = [0u8; 1];
                    if h.offset > 0 && tab.eb.read(h.offset - 1, &mut b).is_ok() {
                        ok &= is_word_byte(b[0]);
                    }
                    if (h.offset + h.len as u64) < vlen
                        && tab.eb.read(h.offset + h.len as u64, &mut b).is_ok()
                    {
                        ok &= !is_word_byte(b[0]);
                    }
                    ok
                });
            }
            let n = hits.len();
            tab.search.hits = Arc::new(hits);
            tab.search.selected = 0;
            if ti == self.active {
                self.status = format!("{} {n}", tr(lang, "status.searchdone"));
            }
            // 完成后跳转："下一个"的首个命中 / 替换后的下一命中定位
            let jump_first = std::mem::take(&mut tab.search.jump_pending);
            let jump_off = tab.search.jump_to_offset.take();
            if jump_first || jump_off.is_some() {
                let mut idx = 0usize;
                if let Some(off) = jump_off {
                    idx = tab.search.hits.partition_point(|h| h.offset < off);
                    if idx >= tab.search.hits.len() {
                        idx = tab.search.hits.len().saturating_sub(1);
                    }
                    tab.search.selected = idx;
                }
                if let Some(h) = tab.search.hits.get(idx) {
                    if tab.eb.has_index() {
                        let line = tab.eb.line_of_offset(h.offset);
                        tab.first_line = line.saturating_sub(tab.last_visible / 4);
                        tab.wrap_skip = 0;
                    }
                }
            }
        }
        if let Some(t) = self.tabs.get(self.active) {
            if t.search.job.is_some() {
                let (p, total) = t.search.job.as_ref().unwrap().progress();
                let hits = t.search.job.as_ref().map(|j| j.hit_count()).unwrap_or(0);
                self.status = format!("{} {p}/{total} · {hits}", tr(lang, "status.searching"));
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
        if self.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // ---- 轮询后台打印状态 ----
        let mut print_msgs: Vec<String> = Vec::new();
        let mut print_done = false;
        if let Some(rx) = &self.print_rx {
            loop {
                match rx.try_recv() {
                    Ok(m) => print_msgs.push(m),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        print_done = true;
                        break;
                    }
                }
            }
        }
        if print_done {
            self.print_rx = None;
        }
        if let Some(m) = print_msgs.last() {
            self.status = Self::print_status_text(m, lang);
        }

        // ---- 拖放文件 → 新标签页打开 ----
        let hovered = ctx.input(|i| i.raw.hovered_files.clone());
        if !hovered.is_empty() {
            let screen = ctx.screen_rect();
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_overlay")));
            painter.rect_filled(
                screen,
                0.0,
                theme_ui::drop_overlay_bg(),
            );
            painter.rect_stroke(
                screen.shrink(6.0),
                6.0,
                egui::Stroke::new(3.0_f32, theme_ui::DROP_OVERLAY_STROKE),
                egui::StrokeKind::Inside,
            );
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                tr(lang, "drop.hint"),
                egui::FontId::proportional(30.0),
                theme_ui::DROP_OVERLAY_TEXT,
            );
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped.iter().find_map(|f| f.path.clone()) {
            if file.is_file() {
                self.path_input = file.display().to_string();
                self.request_open(file);
            }
        }

        // ---- 顶部：菜单栏 ----
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                egui::containers::menu::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button(tr(lang, "menu.file"), |ui| {
                        ui.set_min_width(220.0);
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.new_tab"))
                                    .shortcut_text("Ctrl+N"),
                            )
                            .clicked()
                        {
                            self.new_tab();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                false,
                                egui::Button::new(tr(lang, "file.new_window"))
                                    .shortcut_text("Ctrl+Shift+N"),
                            )
                            .clicked()
                        {}
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.open"))
                                    .shortcut_text("Ctrl+O"),
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("文本文件", &["txt", "log", "csv", "json", "md", "rs", "py", "js", "html", "css", "xml", "yaml", "yml", "toml", "cfg", "ini"])
                                .add_filter("所有文件", &["*"])
                                .pick_file()
                            {
                                self.path_input = path.display().to_string();
                                self.request_open(path);
                            }
                            ui.close();
                        }
                        ui.menu_button(tr(lang, "file.open_recent"), |ui| {
                            ui.set_min_width(280.0);
                            if self.recent_files.is_empty() {
                                ui.label(
                                    egui::RichText::new(tr(lang, "status.no_recent")).weak(),
                                );
                            } else {
                                for path in self.recent_files.clone() {
                                    let name = path
                                        .file_name()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_else(|| path.display().to_string());
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new(&name).size(12.0),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        self.path_input = path.display().to_string();
                                        self.request_open(path.clone());
                                        ui.close();
                                    }
                                }
                            }
                        });
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.save"))
                                    .shortcut_text("Ctrl+S"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                self.status = t.do_save(lang);
                            }
                            ui.close();
                        }
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.saveas"))
                                    .shortcut_text("Ctrl+Shift+S"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                let suggested = t
                                    .eb
                                    .path()
                                    .file_name()
                                    .map(|s| format!("{}.sav", s.to_string_lossy()))
                                    .unwrap_or_else(|| "untitled.txt".into());
                                if let Some(target) = rfd::FileDialog::new()
                                    .set_file_name(&suggested)
                                    .save_file()
                                {
                                    match t.eb.save_as(&target) {
                                        Ok(()) => {
                                            self.status = format!(
                                                "{} {}",
                                                tr(lang, "status.saved"),
                                                target.display()
                                            );
                                        }
                                        Err(e) => {
                                            self.status = format!(
                                                "{}: {e}",
                                                tr(lang, "status.savefail")
                                            );
                                        }
                                    }
                                }
                            }
                            ui.close();
                        }
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.save_all"))
                                    .shortcut_text("Ctrl+Alt+S"),
                            )
                            .clicked()
                        {
                            let n = self.save_all_dirty();
                            self.status = format!(
                                "{}: {}",
                                tr(lang, "status.saveall_done"),
                                n
                            );
                            ui.close();
                        }
                        // 打印需行索引就绪且文档非空
                        let printable = self
                            .tabs
                            .get(self.active)
                            .map(|t| t.eb.has_index() && t.eb.line_count() > 0)
                            .unwrap_or(false);
                        if ui
                            .add_enabled(
                                printable,
                                egui::Button::new(tr(lang, "file.print"))
                                    .shortcut_text("Ctrl+P"),
                            )
                            .clicked()
                        {
                            self.start_print();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.close_tab"))
                                    .shortcut_text("Ctrl+W"),
                            )
                            .clicked()
                        {
                            self.close_active_tab();
                            ui.close();
                        }
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.close_window"))
                                    .shortcut_text("Ctrl+Shift+W"),
                            )
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        ui.separator();
                        if ui.button(tr(lang, "file.settings")).clicked() {
                            self.show_settings = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(tr(lang, "file.exit"))
                                    .shortcut_text("Alt+F4"),
                            )
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                        ui.menu_button(tr(lang, "menu.edit"), |ui| {
                        ui.set_min_width(130.0);
                        let (can_undo, can_redo, has_sel, len) = self
                            .active_mut()
                            .map(|t| {
                                (
                                    t.eb.undo_depth() > 0,
                                    t.eb.redo_depth() > 0,
                                    t.sel_range().is_some(),
                                    t.eb.len(),
                                )
                            })
                            .unwrap_or((false, false, false, 0));
                        if ui
                            .add_enabled(
                                can_undo,
                                egui::Button::new(tr(lang, "edit.undo")).shortcut_text("Ctrl+Z"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.do_undo();
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                can_redo,
                                egui::Button::new(tr(lang, "edit.redo")).shortcut_text("Ctrl+Y"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.do_redo();
                            }
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                len > 0,
                                egui::Button::new(tr(lang, "edit.selectall"))
                                    .shortcut_text("Ctrl+A"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.anchor = Some(0);
                                t.cursor = t.eb.len();
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                has_sel,
                                egui::Button::new(tr(lang, "edit.cut")).shortcut_text("Ctrl+X"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.cut_selection();
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                has_sel,
                                egui::Button::new(tr(lang, "edit.copy")).shortcut_text("Ctrl+C"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.copy_selection();
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                clipboard_get().is_some(),
                                egui::Button::new(tr(lang, "edit.paste")).shortcut_text("Ctrl+V"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.paste_from_clipboard();
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                has_sel,
                                egui::Button::new(tr(lang, "edit.delete")).shortcut_text("Del"),
                            )
                            .clicked()
                        {
                            if let Some(t) = self.active_mut() {
                                t.delete_selection();
                            }
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.search"), |ui| {
                        ui.set_min_width(130.0);
                        if ui.button(tr(lang, "search.menufind")).clicked() {
                            self.show_find = true;
                            self.show_replace = false;
                            self.focus_find = true;
                            ui.close();
                        }
                        if ui.button(tr(lang, "search.menufr")).clicked() {
                            self.show_find = true;
                            self.show_replace = true;
                            self.focus_find = true;
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.view"), |ui| {
                        ui.set_min_width(130.0);
                        ui.menu_button(tr(lang, "view.zoom"), |ui| {
                            ui.set_min_width(160.0);
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "view.zoom_in"))
                                        .shortcut_text("Ctrl + 加号"),
                                )
                                .clicked()
                            {
                                self.zoom_editor(1.2);
                                ui.close();
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "view.zoom_out"))
                                        .shortcut_text("Ctrl + 减号"),
                                )
                                .clicked()
                            {
                                self.zoom_editor(1.0 / 1.2);
                                ui.close();
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "view.zoom_reset"))
                                        .shortcut_text("Ctrl + 0"),
                                )
                                .clicked()
                            {
                                self.zoom_reset();
                                ui.close();
                            }
                        });
                    });
                    if ui.button(tr(lang, "menu.about")).clicked() {
                        self.show_about = true;
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let _ = ui.button("⚙"); // 预留：设置入口
                    if ui.button("🔔").clicked() {
                        self.status = "HiEditor Pro v0.3".into();
                    }
                });
            });
        });

        // ---- 字体工具栏 ----
        egui::TopBottomPanel::top("font_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(tr(lang, "toolbar.font"));
                let cur = self.font_name.clone();
                // 克隆候选列表，避免闭包内同时读写 self
                let choices: Vec<(String, String, u32)> = self
                    .font_choices
                    .iter()
                    .map(|c| (c.name.clone(), c.path.clone(), c.index))
                    .collect();
                egui::ComboBox::from_id_salt("font_choice")
                    .selected_text(if cur == "默认字体" { tr(lang, "font.default").to_string() } else { cur.clone() })
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for (name, path, index) in &choices {
                            let label = if name == "默认字体" {
                                tr(lang, "font.default").to_string()
                            } else {
                                name.clone()
                            };
                            if ui.selectable_label(cur == *name, label).clicked() {
                                self.font_name = name.clone();
                                apply_editor_font(ctx, path, *index);
                            }
                        }
                    });
                ui.label(tr(lang, "toolbar.size"));
                egui::ComboBox::from_id_salt("size_choice")
                    .selected_text(format!("{} px", self.font_size as u32))
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for s in [10, 12, 14, 15, 16, 18, 20, 24, 28] {
                            if ui
                                .selectable_label(self.font_size == s as f32, format!("{s} px"))
                                .clicked()
                            {
                                self.font_size = s as f32;
                                if let Some(t) = self.active_mut() {
                                    t.wrap_rows.clear();
                                    t.wrap_skip = 0;
                                }
                            }
                        }
                    });
                // 自动换行开关（原"查看"菜单项移至此处）
                if ui.checkbox(&mut self.wrap, tr(lang, "view.wrap")).changed() {
                    for t in &mut self.tabs {
                        t.wrap_rows.clear();
                        t.wrap_skip = 0;
                        t.h_scroll = 0.0;
                    }
                }
                let _ = ui.button("…"); // 预留：更多工具栏入口
            });
            ui.add_space(2.0);
        });

        // ---- 标签页栏（卡片式自绘，配色见 theme.rs TabColors） ----
        const TAB_W: f32 = 200.0;
        const TAB_H: f32 = 30.0;
        const TAB_CLOSE_W: f32 = 24.0;
        egui::TopBottomPanel::top("tab_bar")
            .frame(
                egui::Frame::default()
                    .fill(theme::tab_colors(self.theme_mode == ThemeMode::Dark).bar_bg)
                    .inner_margin(3.0),
            )
            .show(ctx, |ui| {
                let mut close_request: Option<usize> = None;
                let mut new_rects: Vec<(u64, egui::Rect)> = Vec::new();
                let tc = theme::tab_colors(self.theme_mode == ThemeMode::Dark);

                // ---- 拖拽排序：命中测试用上一帧的 Tab 矩形 ----
                let dragging_id = egui::DragAndDrop::payload::<u64>(ui.ctx()).map(|a| *a);
                if let Some(drag_id) = dragging_id {
                    if let Some(from) = self.tabs.iter().position(|t| t.id == drag_id) {
                        for (rid, r) in &self.tab_rects {
                            if *rid == drag_id {
                                continue;
                            }
                            let hit = self.pointer_x >= r.left() && self.pointer_x <= r.right();
                            if !hit {
                                continue;
                            }
                            let hovered_idx = self
                                .tabs
                                .iter()
                                .position(|t| t.id == *rid)
                                .unwrap();
                            if hovered_idx == from {
                                continue;
                            }
                            let moved = self.tabs.remove(from);
                            let insert_at = hovered_idx;
                            self.tabs.insert(insert_at, moved);
                            self.active = hovered_idx;
                            break;
                        }
                    }
                }

                ui.horizontal(|ui| {
                for i in 0..self.tabs.len() {
                    let (title, dirty, is_loading) = {
                        let t = &self.tabs[i];
                        let mut ti = t.title();
                        if ti == "无标题" {
                            ti = tr(lang, "tab.untitled").to_string();
                        }
                        (ti, t.eb.is_dirty(), t.is_loading())
                    };
                    let is_dragging_this = dragging_id == Some(self.tabs[i].id);
                    let is_active = i == self.active;
                    let tab_id = egui::Id::new(("tab", self.tabs[i].id));

                    let (tab_rect, tab_resp) = ui
                        .allocate_exact_size(
                            egui::vec2(TAB_W, TAB_H),
                            egui::Sense::click_and_drag(),
                        );
                    // 四态配色：拖动 > 选中 > 悬停 > 常态
                    let (bg, border, fg) = if is_dragging_this {
                        (tc.dragging_bg, tc.dragging_border, tc.dragging_text)
                    } else if is_active {
                        (tc.selected_bg, tc.selected_border, tc.selected_text)
                    } else if tab_resp.hovered() {
                        (tc.hover_bg, tc.idle_border, tc.idle_text)
                    } else {
                        (tc.idle_bg, tc.idle_border, tc.idle_text)
                    };
                    let p = ui.painter_at(tab_rect);
                    p.rect_filled(tab_rect, 3.0, bg);
                    if border != egui::Color32::TRANSPARENT {
                        p.rect_stroke(
                            tab_rect,
                            3.0,
                            egui::Stroke::new(1.0_f32, border),
                            egui::StrokeKind::Inside,
                        );
                    }

                    // 标题（超宽按字符截断加省略号）+ 脏标记 + 加载中
                    let mut label = if dirty {
                        format!("{title} •")
                    } else {
                        title.clone()
                    };
                    if is_loading {
                        label.push_str(" ⏳");
                    }
                    let text_w_max = TAB_W - TAB_CLOSE_W - 20.0;
                    let mut shown = label.clone();
                    loop {
                        let g = ui.fonts(|f| {
                            f.layout_no_wrap(
                                shown.clone(),
                                egui::FontId::proportional(13.0),
                                fg,
                            )
                        });
                        if g.rect.width() <= text_w_max || shown.chars().count() <= 1 {
                            if shown.is_empty() {
                                shown.push('…');
                            } else if g.rect.width() > text_w_max {
                                shown.pop();
                                shown.push('…');
                            }
                            break;
                        }
                        shown.pop();
                    }
                    p.text(
                        egui::Pos2::new(tab_rect.left() + 10.0, tab_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &shown,
                        egui::FontId::proportional(13.0),
                        fg,
                    );

                    // 关闭 ×（内嵌右侧，独立命中区）
                    let close_rect = egui::Rect::from_center_size(
                        egui::Pos2::new(
                            tab_rect.right() - TAB_CLOSE_W / 2.0 - 4.0,
                            tab_rect.center().y,
                        ),
                        egui::vec2(18.0, 18.0),
                    );
                    let close_resp =
                        ui.interact(close_rect, tab_id.with("close"), egui::Sense::click());
                    if close_resp.hovered() {
                        p.rect_filled(close_rect, 3.0, tc.close_hover_bg);
                    }
                    p.text(                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        egui::FontId::proportional(12.0),
                        tc.close,
                    );
                    if close_resp.clicked() {
                        close_request = Some(i);
                    }
                    close_resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                    if tab_resp.drag_started() {
                        egui::DragAndDrop::set_payload(ui.ctx(), self.tabs[i].id);
                        self.active = i;
                    }
                    if tab_resp.clicked() {
                        self.active = i;
                        self.editor_focused = true;
                    }
                    new_rects.push((self.tabs[i].id, tab_rect));

                    // ---- Tab 右键菜单 ----
                    tab_resp.context_menu(|ui| {
                        ui.set_min_width(190.0);
                        let dirty = self.tabs[i].eb.is_dirty();
                        let has_path = !self.tabs[i].eb.path().as_os_str().is_empty();
                        let others = self.tabs.len() > 1;
                        let others_dirty = self
                            .tabs
                            .iter()
                            .enumerate()
                            .any(|(j, tt)| j != i && tt.eb.is_dirty());
                        if ui.button(tr(lang, "tab.close_menu")).clicked() {
                            close_request = Some(i);
                        }
                        if ui
                            .add_enabled(
                                others && !others_dirty,
                                egui::Button::new(tr(lang, "tab.close_others")),
                            )
                            .on_disabled_hover_text(tr(lang, "tab.close_others_hint"))
                            .clicked()
                        {
                            for j in (0..self.tabs.len()).rev() {
                                if j != i {
                                    let cid = self.tabs[j].id;
                                    if let Some(c) = self.load_cancels.get(&cid) {
                                        c.store(true, Ordering::Relaxed);
                                    }
                                    self.load_cancels.remove(&cid);
                                    self.tabs.remove(j);
                                    if self.active >= j && self.active > 0 {
                                        self.active -= 1;
                                    }
                                }
                            }
                            if self.active >= self.tabs.len() {
                                self.active = self.tabs.len().saturating_sub(1);
                            }
                        }
                        ui.separator();
                        if ui
                            .add_enabled(dirty, egui::Button::new(tr(lang, "file.save")))
                            .clicked()
                        {
                            self.status = self.tabs[i].do_save(lang);
                        }
                        if ui
                            .add_enabled(false, egui::Button::new(tr(lang, "file.saveas")))
                            .on_disabled_hover_text(tr(lang, "status.comingsoon"))
                            .clicked()
                        {}
                        ui.separator();
                        if ui
                            .add_enabled(has_path, egui::Button::new(tr(lang, "tab.openfolder")))
                            .clicked()
                        {
                            if let Some(d) = self.tabs[i].eb.path().parent() {
                                let _ = std::process::Command::new("explorer").arg(d).spawn();
                            }
                        }
                        if ui
                            .add_enabled(has_path, egui::Button::new(tr(lang, "tab.copydir")))
                            .clicked()
                        {
                            let d = self.tabs[i]
                                .eb
                                .path()
                                .parent()
                                .map(|d| d.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let _ = clipboard_set(&d);
                        }
                        if ui
                            .add_enabled(has_path, egui::Button::new(tr(lang, "tab.copyfile")))
                            .clicked()
                        {
                            let f = self.tabs[i].eb.path().to_string_lossy().to_string();
                            let _ = clipboard_set(&f);
                        }
                    });
                }
                self.tab_rects = new_rects;

                // ＋ 新建标签页按钮
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("＋")
                                .size(15.0)
                                .color(tc.new_tab),
                        )
                        .frame(false)
                        .min_size(egui::vec2(30.0, TAB_H)),
                    )
                    .on_hover_text(tr(lang, "tab.new"))
                    .clicked()
                {
                    self.new_tab();
                }

                if ui.input(|i| i.pointer.primary_released()) {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                }
                if let Some(i) = close_request {
                    self.close_tab(i);
                }
                });
            });

        // ---- 查找/替换面板 ----
        if self.show_find {
            // Esc 关闭面板（焦点在输入框或编辑区均生效）
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.show_find = false;
                self.show_replace = false;
                self.editor_focused = true;
            }
            let fc = theme::find_colors(self.theme_mode == ThemeMode::Dark);
            egui::TopBottomPanel::top("find_bar")
                .frame(
                    egui::Frame::default()
                        .fill(fc.bar_bg)
                        .inner_margin(6.0),
                )
                .show(ctx, |ui| {
                    // 面板控件观感：白底浅灰描边、4px 圆角、宽松间距（配色见 theme.rs FindColors）
                    ui.style_mut().spacing.item_spacing = egui::vec2(10.0, 6.0);
                    {
                        let v = &mut ui.style_mut().visuals;
                        v.extreme_bg_color = fc.input_bg;
                        for state in [
                            &mut v.widgets.inactive,
                            &mut v.widgets.hovered,
                            &mut v.widgets.active,
                        ] {
                            state.corner_radius = egui::CornerRadius::same(4);
                        }
                        v.widgets.inactive.bg_fill = fc.btn_bg;
                        v.widgets.inactive.weak_bg_fill = fc.btn_bg;
                        v.widgets.inactive.bg_stroke =
                            egui::Stroke::new(1.0_f32, fc.btn_border);
                        v.widgets.inactive.fg_stroke =
                            egui::Stroke::new(1.0_f32, fc.btn_text);
                        v.widgets.hovered.bg_fill = fc.btn_hover_bg;
                        v.widgets.hovered.weak_bg_fill = fc.btn_hover_bg;
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let mut enter_find = false;
                            let mut find_focused = false;
                            if let Some(t) = self.active_mut() {
                                let find_edit = ui.add_sized(
                                    [320.0, 26.0],
                                    egui::TextEdit::singleline(&mut t.search.input)
                                        .hint_text(tr(lang, "search.findhint")),
                                );
                                if self.focus_find {
                                    self.focus_find = false;
                                    find_edit.request_focus();
                                }
                                find_focused = find_edit.has_focus();
                                // 查找框内按回车 = 跳到下一个命中
                                enter_find = find_focused
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            }
                            // 查找框持有焦点时必须停用编辑区键盘处理，
                            // 否则输入会同时进查找框和编辑区（编辑器键盘只看 editor_focused 标志）
                            if find_focused {
                                self.editor_focused = false;
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "search.next"))
                                        .min_size(egui::vec2(76.0, 26.0)),
                                )
                                .clicked()
                                || enter_find
                            {
                                self.search_next(1);
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "search.prev"))
                                        .min_size(egui::vec2(76.0, 26.0)),
                                )
                                .clicked()
                            {
                                self.search_next(-1);
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr(lang, "search.all"))
                                        .min_size(egui::vec2(76.0, 26.0)),
                                )
                                .clicked()
                            {
                                self.start_search();
                            }
                            let mut cs = self
                                .active_mut()
                                .map(|t| t.search.case_sensitive)
                                .unwrap_or(false);
                            if ui.checkbox(&mut cs, tr(lang, "search.case")).changed() {
                                if let Some(t) = self.active_mut() {
                                    t.search.case_sensitive = cs;
                                }
                                // 选项变化 → 按新条件重新搜索
                                self.start_search();
                            }
                            let mut ww = self
                                .active_mut()
                                .map(|t| t.search.whole_word)
                                .unwrap_or(false);
                            if ui.checkbox(&mut ww, tr(lang, "search.word")).changed() {
                                if let Some(t) = self.active_mut() {
                                    t.search.whole_word = ww;
                                }
                                self.start_search();
                            }
                            let mut re = false;
                            if ui
                                .add_enabled(false, egui::Checkbox::without_text(&mut re))
                                .on_hover_text(tr(lang, "search.regex_pending"))
                                .changed()
                            {
                                // 占位
                            }
                            ui.label(egui::RichText::new(tr(lang, "search.regex")).weak());
                            // 面板关闭 ×（行尾右对齐）
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let close = ui.add(
                                        egui::Button::new(
                                            egui::RichText::new("×")
                                                .size(14.0)
                                                .color(fc.close),
                                        )
                                        .frame(false)
                                        .min_size(egui::vec2(24.0, 24.0)),
                                    );
                                    if close.clicked() {
                                        self.show_find = false;
                                        self.show_replace = false;
                                    }
                                },
                            );
                        });
                        if self.show_replace {
                            ui.horizontal(|ui| {
                                let mut enter_replace = false;
                                let mut repl_focused = false;
                                if let Some(t) = self.active_mut() {
                                    let repl_edit = ui.add_sized(
                                        [320.0, 26.0],
                                        egui::TextEdit::singleline(&mut t.replace_input)
                                            .hint_text(tr(lang, "search.replacehint")),
                                    );
                                    repl_focused = repl_edit.has_focus();
                                    // 替换框内按回车 = 替换当前命中
                                    enter_replace = repl_focused
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                }
                                // 同查找框：替换框持有焦点时停用编辑区键盘处理
                                if repl_focused {
                                    self.editor_focused = false;
                                }
                                if ui
                                    .add(
                                        egui::Button::new(tr(lang, "search.replace"))
                                            .min_size(egui::vec2(76.0, 26.0)),
                                    )
                                    .clicked()
                                    || enter_replace
                                {
                                    self.replace_current();
                                }
                                if ui
                                    .add(
                                        egui::Button::new(tr(lang, "search.replaceall"))
                                            .min_size(egui::vec2(76.0, 26.0)),
                                    )
                                    .clicked()
                                {
                                    self.replace_all_in_active();
                                }
                            });
                        }
                    });
                });
        }

        // ---- 状态栏 ----
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(t) = &self.tabs.get(self.active) {
                    if t.eb.has_index() {
                        let cur_line = t.eb.line_of_offset(t.cursor);
                        let col = t.cursor - t.eb.start_of_line(cur_line);
                        ui.label(format!("行 {} 列 {}", cur_line + 1, col + 1));
                        ui.separator();
                        ui.label(format!("共 {} 行", t.eb.line_count()));
                        ui.separator();
                        ui.label(format!(
                            "{} · {}",
                            t.eb.encoding().name(),
                            t.eb.backend()
                        ));
                    } else {
                        let pct = if t.load_total > 0 {
                            (t.covered_bytes * 100 / t.load_total).min(100)
                        } else {
                            0
                        };
                        ui.label(format!(
                            "已索引 {} / {}（{}%）· {} 行可浏览 · 索引完成后可编辑/搜索",
                            fmt_size(t.covered_bytes),
                            fmt_size(t.load_total),
                            pct,
                            t.covered_lines
                        ));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(t) = &self.tabs.get(self.active) {
                        let le = match t.eb.line_ending_hint() {
                            hieditor_core::lineindex::LineEnding::Crlf => "Windows (CRLF)",
                            hieditor_core::lineindex::LineEnding::Lf => "Unix (LF)",
                            hieditor_core::lineindex::LineEnding::Cr => "CR",
                            hieditor_core::lineindex::LineEnding::Mixed => if lang.is_zh() { "混合换行" } else { "Mixed line endings" },
                            hieditor_core::lineindex::LineEnding::None => "—",
                        };
                        ui.label(le);
                        ui.separator();
                        ui.label(t.eb.encoding().name());
                        ui.separator();
                        // 缩放百分比：相对默认字号 15px（18px → 120%，12.5px → 83%）
                        let zoom_pct = (self.font_size / 15.0 * 100.0).round() as i32;
                        ui.label(format!("{zoom_pct}%"));
                    }
                    ui.separator();
                    if self
                        .active_mut()
                        .map(|t| t.eb.is_dirty())
                        .unwrap_or(false)
                    {
                        ui.label(egui::RichText::new(tr(lang, "status.unsaved")).color(theme_ui::DIRTY_MARK));
                    }
                    if self.status.is_empty() {
                        ui.label(tr(lang, "status.ready_init"));
                    } else {
                        ui.label(&self.status);
                    }
                });
            });
        });

        // ---- 编辑区 ----
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(th.bg))
            .show(ctx, |ui| {
                if let Some(err) = &self.open_error {
                    ui.colored_label(
                        theme_ui::ERROR_TEXT,
                        format!("{}: {err}", tr(lang, "status.openfailed")),
                    );
                    return;
                }
                if self.open_rx.is_some()
                    && self
                        .tabs
                        .last()
                        .map(|t| t.is_loading())
                        .unwrap_or(false)
                    && self.active == self.tabs.len() - 1
                {
                    // 最新打开的 tab 仍在读取头部
                }
                let Some(mut tab) = self.eb_take_active() else {
                    ui.heading("HiEditor Pro");
                    ui.label(tr(lang, "empty.hint1"));
                    return;
                };
                self.render_editor(ui, &mut tab);
                self.eb_put_back(tab);
            });

        // Ctrl+P（渲染期暂存，标签页已放回，此处处理）
        if self.print_requested {
            self.print_requested = false;
            self.start_print();
        }

        // ---- 设置窗口（模态：500x300，遮罩不可点击，Esc/✕ 关闭）----
        if self.show_settings {
            let lang = self.lang;
            let mut close = false;
            let modal = egui::Modal::new(egui::Id::new("settings_modal"))
                .frame(egui::Frame::popup(&ctx.style()).inner_margin(14.0));
            let resp = modal.show(ctx, |ui| {
                ui.set_width(500.0);
                ui.set_height(300.0);
                ui.horizontal(|ui| {
                    ui.strong(tr(lang, "settings.title"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("×").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(tr(lang, "settings.lang"));
                    ui.radio(self.lang == Lang::Follow, tr(lang, "lang.follow")).clicked().then(|| {
                        self.lang = Lang::Follow;
                        save_settings(self.lang, self.theme_mode);
                    });
                    ui.radio(self.lang == Lang::Zh, tr(lang, "lang.zh")).clicked().then(|| {
                        self.lang = Lang::Zh;
                        save_settings(self.lang, self.theme_mode);
                    });
                    ui.radio(self.lang == Lang::En, tr(lang, "lang.en")).clicked().then(|| {
                        self.lang = Lang::En;
                        save_settings(self.lang, self.theme_mode);
                    });
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(tr(lang, "settings.theme"));
                    ui.radio(self.theme_mode == ThemeMode::Light, tr(lang, "theme.light")).clicked().then(|| {
                        self.theme_mode = ThemeMode::Light;
                        apply_panel_theme(ctx, false);
                        self.applied_theme = Some(ThemeMode::Light);
                        save_settings(self.lang, self.theme_mode);
                    });
                    ui.radio(self.theme_mode == ThemeMode::Dark, tr(lang, "theme.dark")).clicked().then(|| {
                        self.theme_mode = ThemeMode::Dark;
                        apply_panel_theme(ctx, true);
                        self.applied_theme = Some(ThemeMode::Dark);
                        save_settings(self.lang, self.theme_mode);
                    });
                });
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);
                if ui.button(tr(lang, "settings.shortcut")).clicked() {
                    self.status = if create_desktop_shortcut() {
                        tr(lang, "settings.shortcut_ok").into()
                    } else {
                        tr(lang, "settings.shortcut_fail").into()
                    };
                }
            });
            if resp.should_close() || resp.backdrop_response.clicked() || close {
                self.show_settings = false;
            }
        }

        // ---- 关于窗口（模态：500x300，遮罩不可点击，Esc/✕ 关闭）----
        if self.show_about {
            let lang = self.lang;
            let mut close = false;
            let modal = egui::Modal::new(egui::Id::new("about_modal"))
                .frame(egui::Frame::popup(&ctx.style()).inner_margin(14.0));
            let resp = modal.show(ctx, |ui| {
                ui.set_width(500.0);
                ui.set_height(300.0);
                ui.horizontal(|ui| {
                    ui.strong(tr(lang, "about.title"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("×").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(10.0);
                ui.label(format!(
                    "HiEditor Pro v{} — {}",
                    env!("CARGO_PKG_VERSION"),
                    tr(lang, "about.desc")
                ));
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(tr(lang, "about.src"));
                        ui.label("https://github.com/hicode0101/HiTools");
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr(lang, "btn.open")).clicked() {
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "start", "", "https://github.com/hicode0101/HiTools"])
                                .spawn();
                        }
                        if ui.button(tr(lang, "btn.copy")).clicked() {
                            let _ = clipboard_set("https://github.com/hicode0101/HiTools");
                            self.status = format!("{}: {}", tr(lang, "status.copiedn"), tr(lang, "about.src"));
                        }
                    });
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(tr(lang, "about.wx"));
                        ui.label("hicode0101（微信同号）");
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr(lang, "btn.copywx")).clicked() {
                            let _ = clipboard_set("hicode0101");
                            self.status = format!("{}: {}", tr(lang, "status.copiedn"), tr(lang, "about.wx"));
                        }
                    });
                });
            });
            if resp.should_close() || resp.backdrop_response.clicked() || close {
                self.show_about = false;
            }
        }

        // ---- 关闭确认 ----
        if let Some(idx) = self.confirm_close {
            if idx < self.tabs.len() {
                let mut title = self.tabs[idx].title();
        if title == "无标题" {
            title = tr(lang, "tab.untitled").into();
        }
                egui::Window::new(tr(lang, "confirm.title"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("\"{title}\" {}", tr(lang, "confirm.msg")));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr(lang, "confirm.save")).clicked() {
                                match self.tabs[idx].eb.save() {
                                    Ok(()) => self.status = tr(lang, "status.saved").into(),
                                    Err(e) => self.status = format!("{}: {e}", tr(lang, "status.savefail")),
                                }
                                let idx = self.confirm_close.take().unwrap();
                                if idx < self.tabs.len() {
                                    self.remove_tab(idx);
                                }
                            }
                            if ui.button(tr(lang, "confirm.nosave")).clicked() {
                                let idx = self.confirm_close.take().unwrap();
                                self.remove_tab(idx);
                            }
                            if ui.button(tr(lang, "confirm.cancel")).clicked() {
                                self.confirm_close = None;
                            }
                        });
                    });
            } else {
                self.confirm_close = None;
            }
        }
    }
}


// ---------- Tab 附加方法（剪贴板/保存） ----------
impl Tab {
    fn paste_from_clipboard(&mut self) -> Option<String> {
        match clipboard_get() {
            Some(text) => {
                self.insert_text(&text);
                None
            }
            None => Some("剪贴板为空或不可用".into()),
        }
    }

    fn do_save(&mut self, lang: Lang) -> String {
        if self.eb.path().as_os_str().is_empty() {
            return tr(lang, "status.untitlednosave").into();
        }
        match self.eb.save() {
            Ok(()) => format!(
                "{} {}（{}）",
                tr(lang, "status.saved"),
                self.eb.path().display(),
                fmt_size(self.eb.len())
            ),
            Err(e) => format!("{}: {e}", tr(lang, "status.savefail")),
        }
    }
}

impl App {
    // ---------- 编辑区渲染（作用于当前标签页） ----------
    #[allow(clippy::too_many_lines)]
    fn render_editor(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let lang = self.lang;
        let th = editor_theme(self.theme_mode == ThemeMode::Dark);
        let index_ready = tab.eb.has_index();
        let n = if index_ready {
            tab.eb.line_count()
        } else {
            tab.covered_lines
        };
        let line_height = self.font_size * 1.4;
        let avail = ui.available_size();
        let visible = ((avail.y / line_height) as u64).max(1);
        tab.last_visible = visible;
        tab.first_line = tab.first_line.min(n.saturating_sub(1));
        let wrap = self.wrap;
        if wrap {
            tab.h_scroll = 0.0;
        }

        // ---- 布局与度量 ----
        let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let font = egui::FontId::monospace(self.font_size);
        let digit_w = ui
            .fonts(|f| f.layout_no_wrap("0".to_string(), font.clone(), theme_ui::TEXT_MEASURE))
            .rect
            .width()
            .max(1.0);
        let digits = format!("{n}").len() as f32;
        let gutter_w = digits * digit_w + 14.0;
        let text_left = rect.left() + gutter_w + 10.0;
        const TRACK: f32 = 14.0;
        let text_w = (rect.right() - text_left - TRACK - 6.0).max(50.0);
        let text_top_pad = (line_height - self.font_size) * 0.42;

        // 文本区宽度变化 → 换行缓存失效
        if (tab.last_wrap_w - text_w).abs() > 0.5 {
            tab.last_wrap_w = text_w;
            tab.wrap_rows.clear();
        }
        tab.h_scroll = tab.h_scroll.clamp(0.0, (tab.h_extent - text_w).max(0.0));

        // ---- 滚轮 ----
        let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
        let scroll_x = ui.input(|i| i.raw_scroll_delta.x);
        let shift_held = ui.input(|i| i.modifiers.shift);
        let h_delta = -scroll_x - if shift_held { scroll_y } else { 0.0 };
        if !wrap && h_delta != 0.0 {
            tab.h_scroll =
                (tab.h_scroll + h_delta).clamp(0.0, (tab.h_extent - text_w).max(0.0));
        } else if scroll_y != 0.0 {
            let dv = (-scroll_y / line_height).trunc() as i64;
            if dv != 0 {
                if wrap {
                    for _ in 0..dv.abs() {
                        step_visual(
                            &mut tab.wrap_rows, ui, &font, text_w, &tab.eb, index_ready,
                            tab.load_starts.as_slice(), tab.covered_lines, tab.covered_bytes,
                            n, &mut tab.first_line, &mut tab.wrap_skip, dv > 0,
                        );
                    }
                } else {
                    tab.first_line = shift_line(tab.first_line, dv, n.saturating_sub(1));
                }
            }
        }

        // 换行模式：skip 越界归一
        if wrap {
            loop {
                let cur = rows_of_line(
                    &mut tab.wrap_rows, ui, &font, text_w, &tab.eb, index_ready,
                    tab.load_starts.as_slice(), tab.covered_lines, tab.covered_bytes,
                    tab.first_line,
                ) as u64;
                if tab.wrap_skip >= cur && tab.first_line + 1 < n {
                    tab.wrap_skip -= cur;
                    tab.first_line += 1;
                } else {
                    if tab.first_line + 1 >= n {
                        tab.wrap_skip = tab.wrap_skip.min(cur.saturating_sub(1));
                    }
                    break;
                }
            }
        }

        // ---- 滚动条几何 ----
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
        let pos_frac = ((tab.first_line as f32) / (max_units as f32)).min(1.0);
        let thumb_y = vtrack.top() + pos_frac * (vtrack.height() - thumb_h).max(1.0);

        let h_max = (tab.h_extent - text_w).max(0.0);
        let htrack = egui::Rect::from_min_size(
            egui::Pos2::new(text_left, rect.bottom() - TRACK),
            egui::Vec2::new(rect.right() - text_left - TRACK, TRACK - 2.0),
        );
        let h_thumb_w = if h_max > 0.0 {
            (text_w / tab.h_extent * htrack.width()).clamp(24.0, htrack.width())
        } else {
            htrack.width()
        };
        let h_thumb_x = htrack.left()
            + if h_max > 0.0 {
                (tab.h_scroll / h_max) * (htrack.width() - h_thumb_w)
            } else {
                0.0
            };

        // ---- 鼠标 ----
        // 悬停时用 hover_pos（interact_pointer_pos 仅在按下/拖动时有值）
        let pointer_pos =
            response.hover_pos().or_else(|| response.interact_pointer_pos());
        let in_htrack = pointer_pos
            .map(|p| htrack.top() <= p.y && p.x >= text_left && !wrap)
            .unwrap_or(false);
        let in_vtrack = pointer_pos.map(|p| p.x >= vtrack.left()).unwrap_or(false);
        // 拖拽起点锁定：拖动滑块时指针滑出轨道仍保持滚动条模式（不会掉进正文拖选）
        if response.drag_started() {
            tab.scroll_drag = if in_htrack {
                2
            } else if in_vtrack {
                1
            } else {
                0
            };
        } else if response.drag_stopped() || response.clicked_elsewhere() {
            tab.scroll_drag = 0;
        }
        let hit_h = in_htrack || (response.dragged() && tab.scroll_drag == 2);
        let hit_v = !hit_h && (in_vtrack || (response.dragged() && tab.scroll_drag == 1));

        // 鼠标图标：滚动条区域（悬停/拖动中）= 箭头，正文区 = I 形
        if in_htrack
            || in_vtrack
            || (response.dragged() && tab.scroll_drag > 0)
        {
            ui.ctx().output_mut(|o| o.cursor_icon = egui::CursorIcon::Default);
        } else if response.hovered() {
            ui.ctx().output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
        }
        if index_ready {
            if hit_h {
                if response.clicked() || response.dragged() {
                    let p = pointer_pos.unwrap();
                    let denom = (htrack.width() - h_thumb_w).max(1.0);
                    let ratio = ((p.x - htrack.left()) - h_thumb_w / 2.0).max(0.0) / denom;
                    tab.h_scroll = (ratio.clamp(0.0, 1.0) * h_max).min(h_max);
                }
            } else if hit_v {
                if response.clicked() || response.dragged() {
                    let p = pointer_pos.unwrap();
                    let ratio = ((p.y - vtrack.top()) - thumb_h / 2.0).max(0.0)
                        / (vtrack.height() - thumb_h).max(1.0);
                    tab.first_line = ((ratio.clamp(0.0, 1.0) * max_units as f32) as u64)
                        .min(n.saturating_sub(1));
                    tab.wrap_skip = 0;
                }
            } else {
                // ---- 拖动选取：边缘自动滚动（在闭包定义前完成视图滚动） ----
                if response.dragged() {
                    if let Some(pos) = pointer_pos {
                        const EDGE: f32 = 28.0;
                        let near_top = pos.y < rect.top() + EDGE;
                        let near_bottom = pos.y > rect.bottom() - TRACK - EDGE;
                        if near_top || near_bottom {
                            if wrap {
                                step_visual(
                                    &mut tab.wrap_rows, ui, &font, text_w, &tab.eb, index_ready,
                                    tab.load_starts.as_slice(), tab.covered_lines, tab.covered_bytes,
                                    n, &mut tab.first_line, &mut tab.wrap_skip, near_bottom,
                                );
                            } else if near_top {
                                tab.first_line = tab.first_line.saturating_sub(1);
                            } else {
                                tab.first_line = (tab.first_line + 1).min(n.saturating_sub(1));
                            }
                        }
                    }
                }
                let mut target_at = |pos: egui::Pos2| -> u64 {
                    if wrap {
                        cursor_at_pointer_wrap(
                            &mut tab.wrap_rows, ui, &font, text_w, &tab.eb, index_ready,
                            tab.load_starts.as_slice(), tab.covered_lines, tab.covered_bytes,
                            n, tab.first_line, tab.wrap_skip, pos, rect, text_left, line_height,
                        )
                    } else {
                        cursor_at_pointer_flat(
                            tab.first_line, n, index_ready, &tab.eb, tab.load_starts.as_slice(),
                            tab.covered_lines, tab.covered_bytes, pos, rect, text_left,
                            &font, ui, line_height, tab.h_scroll,
                        )
                    }
                };
                if response.clicked() {
                    self.editor_focused = true;
                    let extend = ui.input(|i| i.modifiers.shift);
                    let target = target_at(pointer_pos.unwrap_or(rect.min));
                    if extend {
                        if tab.anchor.is_none() {
                            tab.anchor = Some(tab.cursor);
                        }
                        tab.cursor = target;
                    } else {
                        tab.cursor = target;
                        tab.anchor = None;
                    }
                }
                if response.drag_started() {
                    self.editor_focused = true;
                    let target = target_at(pointer_pos.unwrap_or(rect.min));
                    tab.anchor = Some(target);
                    tab.cursor = target;
                }
                if response.dragged() {
                    if let Some(pos) = pointer_pos {
                        tab.cursor = target_at(pos);
                    }
                }
                // 单击被误判为拖动（零位移）时，释放后不留残选区
                if response.drag_stopped() && tab.anchor == Some(tab.cursor) {
                    tab.anchor = None;
                }
                // 双击选词
                if response.double_clicked() {
                    self.editor_focused = true;
                    let pos_byte = target_at(pointer_pos.unwrap_or(rect.min));
                    let (ws, we) = word_range_at(&tab.eb, pos_byte);
                    if ws < we {
                        tab.anchor = Some(ws);
                        tab.cursor = we;
                    }
                }
                // 三击选整行（不含行尾换行符）
                if response.triple_clicked() && index_ready {
                    self.editor_focused = true;
                    let pos_byte = target_at(pointer_pos.unwrap_or(rect.min));
                    let line = tab.eb.line_of_offset(pos_byte);
                    let s = tab.eb.start_of_line(line);
                    let e = line_content_end(&tab.eb, line);
                    tab.anchor = Some(s);
                    tab.cursor = e;
                }
            }
        }
        if response.clicked_elsewhere() {
            self.editor_focused = false;
        }

        // ---- 右键上下文菜单 ----
        if index_ready {
            response.context_menu(|ui| {
                ui.set_min_width(160.0);
                let can_undo = tab.eb.undo_depth() > 0;
                let can_redo = tab.eb.redo_depth() > 0;
                let has_sel = tab.sel_range().is_some();
                let has_clip = clipboard_get().is_some();
                if ui
                    .add_enabled(can_undo, egui::Button::new(tr(lang, "edit.undo")).shortcut_text("Ctrl+Z"))
                    .clicked()
                {
                    tab.do_undo();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new(tr(lang, "edit.redo")).shortcut_text("Ctrl+Y"))
                    .clicked()
                {
                    tab.do_redo();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        tab.eb.len() > 0,
                        egui::Button::new(tr(lang, "edit.selectall")).shortcut_text("Ctrl+A"),
                    )
                    .clicked()
                {
                    tab.anchor = Some(0);
                    tab.cursor = tab.eb.len();
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new(tr(lang, "edit.cut")).shortcut_text("Ctrl+X"))
                    .clicked()
                {
                    if let Some(n) = tab.cut_selection() {
                        self.status = format!("{} {n}", tr(lang, "status.cutn"));
                    }
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new(tr(lang, "edit.copy")).shortcut_text("Ctrl+C"))
                    .clicked()
                {
                    if let Some(n) = tab.copy_selection() {
                        self.status = format!("{} {n}", tr(lang, "status.copiedn"));
                    }
                }
                if ui
                    .add_enabled(has_clip, egui::Button::new(tr(lang, "edit.paste")).shortcut_text("Ctrl+V"))
                    .clicked()
                {
                    if let Some(msg) = tab.paste_from_clipboard() {
                        self.status = msg;
                    }
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new(tr(lang, "edit.delete")).shortcut_text("Del"))
                    .clicked()
                {
                    tab.delete_selection();
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new(tr(lang, "edit.upper"))).clicked() {
                    tab.transform_selection(CaseMode::Upper);
                }
                if ui.add_enabled(has_sel, egui::Button::new(tr(lang, "edit.lower"))).clicked() {
                    tab.transform_selection(CaseMode::Lower);
                }
                if ui
                    .add_enabled(has_sel, egui::Button::new(tr(lang, "edit.title")))
                    .clicked()
                {
                    tab.transform_selection(CaseMode::Title);
                }
            });
        }

        // ---- 键盘 ----
        if self.editor_focused && index_ready {
            let events: Vec<egui::Event> = ui.input(|i| i.events.clone());
            for ev in events {
                match ev {
                    egui::Event::Text(t) => {
                        if !t.is_empty() {
                            tab.insert_text(&t);
                        }
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                        if !text.is_empty() {
                            tab.insert_text(&text);
                        }
                    }
                    egui::Event::Copy => {
                        if let Some(n) = tab.copy_selection() {
                            self.status = format!("{} {n}", tr(lang, "status.copiedn"));
                        }
                    }
                    egui::Event::Cut => {
                        if let Some(n) = tab.cut_selection() {
                            self.status = format!("{} {n}", tr(lang, "status.cutn"));
                        }
                    }
                    egui::Event::Paste(text) => {
                        if !text.is_empty() {
                            tab.insert_text(&text);
                        }
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        let extend = modifiers.shift;
                        match key {
                            egui::Key::ArrowLeft => tab.move_cursor(prev_boundary(&tab.eb, tab.cursor), extend),
                            egui::Key::ArrowRight => tab.move_cursor(next_boundary(&tab.eb, tab.cursor), extend),
                            egui::Key::ArrowUp => tab.move_line(-1, extend),
                            egui::Key::ArrowDown => tab.move_line(1, extend),
                            egui::Key::PageUp => tab.move_line(-((visible * 9 / 10) as i64), extend),
                            egui::Key::PageDown => tab.move_line((visible * 9 / 10) as i64, extend),
                            egui::Key::Home => {
                                let target = if modifiers.ctrl {
                                    0
                                } else {
                                    tab.eb.start_of_line(tab.eb.line_of_offset(tab.cursor))
                                };
                                tab.move_cursor(target, extend);
                            }
                            egui::Key::End => {
                                let target = if modifiers.ctrl {
                                    tab.eb.len()
                                } else {
                                    line_content_end(&tab.eb, tab.eb.line_of_offset(tab.cursor))
                                };
                                tab.move_cursor(target, extend);
                            }
                            egui::Key::Backspace => tab.delete_at_cursor(false),
                            egui::Key::Delete => tab.delete_at_cursor(true),
                            egui::Key::Enter => tab.insert_text("\n"),
                            egui::Key::Tab => tab.insert_text("\t"),
                            egui::Key::Z if modifiers.ctrl && !modifiers.shift => tab.do_undo(),
                            egui::Key::Y if modifiers.ctrl => tab.do_redo(),
                            egui::Key::Z if modifiers.ctrl && modifiers.shift => tab.do_redo(),
                            egui::Key::A if modifiers.ctrl => {
                                tab.anchor = Some(0);
                                tab.cursor = tab.eb.len();
                            }
                            egui::Key::C if modifiers.ctrl => {
                                if let Some(n) = tab.copy_selection() {
                                    self.status = format!("{} {n}", tr(lang, "status.copiedn"));
                                }
                            }
                            egui::Key::X if modifiers.ctrl => {
                                if let Some(n) = tab.cut_selection() {
                                    self.status = format!("{} {n}", tr(lang, "status.cutn"));
                                }
                            }
                            egui::Key::V if modifiers.ctrl => {
                                if let Some(msg) = tab.paste_from_clipboard() {
                                    self.status = msg;
                                }
                            }
                            egui::Key::S if modifiers.ctrl => {
                                self.status = tab.do_save(lang);
                            }
                            egui::Key::P if modifiers.ctrl => {
                                // 渲染期标签页已取出，暂存到 update 末尾处理
                                self.print_requested = true;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- 光标滚动跟随：仅在光标移动过时触发（滚轮浏览不被拉回）----
        let cursor_moved = tab.cursor != tab.last_cursor;
        if index_ready && cursor_moved {
            let cursor_line = tab.eb.line_of_offset(tab.cursor);
            if cursor_line < tab.first_line || cursor_line >= tab.first_line + visible {
                tab.first_line = cursor_line.saturating_sub(visible / 4);
                tab.wrap_skip = 0;
            }
        }
        tab.last_cursor = tab.cursor;

        // ---- 绘制可视内容 ----
        let sel = tab.sel_range();
        let hits = tab.search.hits.clone();
        let selected_offset = hits.get(tab.search.selected).map(|h| h.offset);
        let selected_range =
            hits.get(tab.search.selected).map(|h| (h.offset, h.offset + h.len as u64));
        let cursor = tab.cursor;
        let cursor_line_visible = self.editor_focused && index_ready;
        let cursor_line = if index_ready { tab.eb.line_of_offset(cursor) } else { 0 };
        let eb = &tab.eb;
        let load_starts = tab.load_starts.as_slice();
        let covered_lines = tab.covered_lines;
        let covered_bytes = tab.covered_bytes;
        let h_scroll = tab.h_scroll;

        let pos_in_line = |text: &str, byte: usize| -> (u32, f32) {
            if !wrap {
                let seg = safe_slice(text, 0..byte);
                let w = ui.fonts(|f| f.layout_no_wrap(seg, font.clone(), theme_ui::TEXT_MEASURE)).rect.width();
                return (0, w);
            }
            let prefix = safe_slice(text, 0..byte);
            let g = ui.fonts(|f| f.layout(prefix, font.clone(), theme_ui::TEXT_MEASURE, text_w));
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

        // 行号栏浅灰底 + 分隔线
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::Vec2::new(gutter_w, rect.height())),
            0.0,
            th.gutter_bg,
        );
        painter.line_segment(
            [
                egui::Pos2::new(gutter_w - 1.0, rect.top()),
                egui::Pos2::new(gutter_w - 1.0, rect.bottom()),
            ],
            egui::Stroke::new(1.0_f32, th.gutter_sep),
        );

        // 正文区裁剪：横向滚动后正文/选区/命中/光标一律裁剪在行号栏右侧，
        // 不允许盖到行号栏上（行号栏画在下层，靠此裁剪保持可见）
        let text_painter = painter.with_clip_rect(egui::Rect::from_min_max(
            egui::Pos2::new(text_left, rect.top()),
            egui::Pos2::new(rect.right(), rect.bottom()),
        ));

        let mut y = if wrap {
            rect.top() - tab.wrap_skip as f32 * line_height
        } else {
            rect.top()
        };
        let mut line = tab.first_line;
        let mut max_w_seen = text_w;
        loop {
            let done = if wrap {
                y >= rect.bottom() - TRACK || line >= n
            } else {
                line >= n || line >= tab.first_line + visible
            };
            if done {
                break;
            }

            let (s, e_raw, text) =
                read_line_text(eb, index_ready, load_starts, covered_lines, covered_bytes, line);
            let text_bytes = text.len();

            // 行号（1 起始显示）
            let num =
                ui.fonts(|f| f.layout_no_wrap(format!("{}", line + 1), font.clone(), th.gutter_fg));
            painter.galley(
                egui::Pos2::new(
                    gutter_w - 8.0 - num.rect.width(),
                    y + text_top_pad,
                ),
                num,
                th.gutter_fg,
            );

            // 选区底色（GBK 行：偏移先换算到解码文本坐标）
            let enc = eb.encoding();
            // 行内容末尾（文件字节，裁掉行尾 \r\n）。
            // 基于本行原始行界 e_raw 做字节读——不能走索引路径：
            // 大文件首屏渲染时索引尚未就绪（占位索引会越界崩溃）
            let content_end_file = {
                let mut end = e_raw;
                let mut nb = [0u8; 1];
                if end > s && eb.read(end - 1, &mut nb).is_ok() && nb[0] == b'\n' {
                    end -= 1;
                    if end > s && eb.read(end - 1, &mut nb).is_ok() && nb[0] == b'\r' {
                        end -= 1;
                    }
                }
                end
            };
            if let Some((ss, se)) = sel {
                let s_end = content_end_file;
                let a = ss.max(s).min(s_end);
                let b = se.max(s).min(s_end);
                if a < b {
                    for (r, xf, xt) in span_rows(
                        &text,
                        file_rel_to_dec(enc, &text, (a - s) as usize),
                        file_rel_to_dec(enc, &text, (b - s) as usize),
                    ) {
                        text_painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::Pos2::new(text_left + xf - h_scroll, y + r as f32 * line_height),
                                egui::Vec2::new((xt - xf).max(2.0), line_height),
                            ),
                            0.0,
                            th.sel,
                        );
                    }
                }
            }

            // 搜索命中底色
            let lo = hits.partition_point(|h| h.offset < s);
            for h in &hits[lo.min(hits.len())..] {
                if h.offset >= content_end_file {
                    break;
                }
                let b = (h.offset + h.len as u64).min(content_end_file);
                if b <= h.offset {
                    continue;
                }
                for (r, xf, xt) in span_rows(
                    &text,
                    file_rel_to_dec(enc, &text, (h.offset - s) as usize),
                    file_rel_to_dec(enc, &text, (b - s) as usize),
                ) {
                    text_painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::Pos2::new(
                                text_left + xf - h_scroll,
                                y + r as f32 * line_height,
                            ),
                            egui::Vec2::new((xt - xf).max(2.0), line_height),
                        ),
                        2.0,
                        if selected_offset == Some(h.offset) {
                            th.selected
                        } else {
                            th.highlight
                        },
                    );
                }
            }

            // 整行文本；当前命中的文字段用主题的 selected_text 颜色（多 section 排版）
            let sel_span = selected_range.and_then(|(hs, he)| {
                (hs >= s && he > hs && he <= content_end_file)
                    .then(|| {
                        (
                            file_rel_to_dec(enc, &text, (hs - s) as usize),
                            file_rel_to_dec(enc, &text, (he - s) as usize),
                        )
                    })
            });
            let galley = if let Some((a, b)) = sel_span {
                let a = clamp_char_boundary(&text, a);
                let b = clamp_char_boundary(&text, b).max(a);
                let mut job = egui::text::LayoutJob::default();
                job.text = text.clone();
                let mut push = |r: std::ops::Range<usize>, color| {
                    if r.start < r.end {
                        job.sections.push(egui::text::LayoutSection {
                            leading_space: 0.0,
                            byte_range: r,
                            format: egui::TextFormat::simple(font.clone(), color),
                        });
                    }
                };
                push(0..a, th.text);
                push(a..b, th.selected_text);
                push(b..text.len(), th.text);
                if wrap {
                    job.wrap.max_width = text_w;
                }
                ui.fonts(|f| f.layout_job(job))
            } else if wrap {
                ui.fonts(|f| f.layout(text.clone(), font.clone(), th.text, text_w))
            } else {
                ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), th.text))
            };
            let rows_used = galley.rows.len().max(1) as u32;
            let galley_w = galley.rect.width();
            text_painter.galley(
                egui::Pos2::new(text_left - h_scroll, y + text_top_pad),
                galley,
                th.text,
            );
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
                        th.gutter_fg,
                    )
                });
                text_painter.galley(
                    egui::Pos2::new(
                        text_left + tx - h_scroll + 4.0,
                        y + tr as f32 * line_height + text_top_pad,
                    ),
                    g,
                    th.gutter_fg,
                );
            }

            // 光标条 + IME 锚点
            if cursor_line_visible && cursor_line == line {
                let rel = file_rel_to_dec(
                    enc,
                    &text,
                    ((cursor - s) as usize).min(text.len()),
                );
                let (r, x) = pos_in_line(&text, rel);
                let cursor_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(text_left + x - h_scroll, y + r as f32 * line_height),
                    egui::Vec2::new(2.0, line_height),
                );
                text_painter.rect_filled(cursor_rect, 0.0, th.cursor);
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
        // 可视行中的最大行宽 → 横向滚动条依据（"UI界面大调整"时丢失的回归修复）
        tab.h_extent = max_w_seen;

        // ---- 垂直滚动条 ----
        painter.rect_filled(vtrack, 2.0, th.track);
        if size_frac < 1.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::Pos2::new(vtrack.left() + 1.0, thumb_y),
                    egui::Vec2::new(vtrack.width() - 2.0, thumb_h),
                ),
                3.0,
                th.thumb,
            );
        }

        // ---- 水平滚动条 ----
        if !wrap && h_max > 0.0 {
            painter.rect_filled(htrack, 2.0, th.track);
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::Pos2::new(h_thumb_x + 1.0, htrack.top()),
                    egui::Vec2::new(h_thumb_w - 2.0, htrack.height()),
                ),
                3.0,
                th.thumb,
            );
        }

        // ---- 索引进度角标（深色背景条 + 琥珀文字） ----
        if !index_ready {
            let pct = if tab.load_total > 0 {
                (tab.covered_bytes * 100 / tab.load_total).min(100)
            } else {
                0
            };
            let msg = trf(lang, "editor.indexing", &[&pct.to_string()]);
            let font = egui::FontId::proportional(13.0);
            let galley = ui.fonts(|f| f.layout_no_wrap(msg, font, theme_ui::INDEXING_BADGE));
            let (pad_x, pad_y) = (10.0, 4.0);
            let badge_size = egui::Vec2::new(
                galley.rect.width() + pad_x * 2.0,
                galley.rect.height() + pad_y * 2.0,
            );
            let badge_rect = egui::Rect::from_min_size(
                egui::Pos2::new(
                    rect.right() - TRACK - 12.0 - badge_size.x,
                    rect.top() + 8.0,
                ),
                badge_size,
            );
            painter.rect_filled(badge_rect, 4.0, theme_ui::indexing_badge_bg());
            painter.galley(
                egui::Pos2::new(badge_rect.left() + pad_x, badge_rect.top() + pad_y),
                galley,
                theme_ui::INDEXING_BADGE,
            );
        }
    }
}
