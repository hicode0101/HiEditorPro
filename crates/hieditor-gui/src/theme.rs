//! HiEditor Pro — 主题颜色集中定义（唯一改色入口）
//!
//! 界面上出现的所有颜色值都集中在本文件，后期调色只需改这里，不必动渲染代码。
//!
//! ═══════════════════════════════════════════════════════════════
//!  如何改颜色：
//!  每个颜色都用 `rgb(0xRRGGBB)` 十六进制书写，可直接从取色器/
//!  设计稿粘贴色值，例如把 `rgb(0x18181C)` 改成 `rgb(0x2C2C32)`。
//!  每个常量上方都有中文注释说明用途。
//!  注意：不要用 egui 自带的 `hex_color!` 宏——它无法用于 `const`
//!  声明（官方文档注明 const 上下文不可用），编译会报错。
//! ═══════════════════════════════════════════════════════════════
//!
//! 文件结构：
//! - [`rgb`]：十六进制 → Color32 的 const 辅助函数
//! - [`EditorTheme`]：编辑区配色结构（背景 / 行号栏 / 选区 / 搜索高亮 / 光标 / 滚动条 / 正文文字）
//! - [`light`] / [`dark`]：两套具体色值，每个常量都有用途备注
//! - [`ui`]：不随主题切换的通用界面色（拖放覆盖层、加载角标、未保存标记、错误文字等）
//!
//! 使用方式：渲染代码通过 `editor_theme(dark)` 取整套配色（`main.rs` 中 `let th = editor_theme(...)`），
//! 零散界面色通过 `theme_ui::XXX` 引用。
//!
//! 补充说明：菜单栏 / 工具栏 / 标签栏 / 弹窗等 egui 控件面板的底色与文字色
//! 来自 `egui::Visuals::light() / dark()`（在 `main.rs` 的 `set_visuals` 处应用），
//! 不在本文件内；若要自定义面板底色，需在 set_visuals 后覆盖 `Visuals` 的
//! `panel_fill` / `window_fill` / `extreme_bg_color` 等字段。

use eframe::egui::Color32;

// ---------- 十六进制颜色工具 ----------

/// `0xRRGGBB` → Color32（const 兼容，可用于 `pub const` 声明）。
const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// ---------- 编辑区配色结构 ----------

/// 编辑区一套完整配色，由 [`editor_theme`] 按当前主题（浅色/深色）返回。
#[derive(Clone, Copy)]
pub struct EditorTheme {
    /// 编辑区背景色
    pub bg: Color32,
    /// 行号栏背景色
    pub gutter_bg: Color32,
    /// 行号文字颜色
    pub gutter_fg: Color32,
    /// 行号栏与正文之间的分隔线颜色
    pub gutter_sep: Color32,
    /// 鼠标选中文本的背景色
    pub sel: Color32,
    /// 搜索命中高亮背景色（普通命中）
    pub highlight: Color32,
    /// 搜索命中高亮背景色（当前定位的那一条）
    pub selected: Color32,
    /// 光标（插入符）颜色
    pub cursor: Color32,
    /// 滚动条轨道颜色
    pub track: Color32,
    /// 滚动条滑块颜色
    pub thumb: Color32,
    /// 正文文字颜色（含"…截断"等正文区域文字）
    pub text: Color32,
}

/// 按主题取整套编辑区配色。`dark = true` 返回深色，否则浅色。
pub fn editor_theme(dark: bool) -> EditorTheme {
    if dark {
        dark::palette()
    } else {
        light::palette()
    }
}

// ---------- 浅色主题 ----------

/// 浅色主题（对齐原型图，默认主题）。
pub mod light {
    use super::*;

    /// 编辑区背景：纯白
    pub const BG: Color32 = rgb(0xFFFFFF);
    /// 行号栏背景：浅灰
    pub const GUTTER_BG: Color32 = rgb(0xE5E5E5);
    /// 行号文字：中灰
    pub const GUTTER_FG: Color32 = rgb(0x787878);
    /// 行号栏分隔线：比行号栏背景略深一档的灰
    pub const GUTTER_SEP: Color32 = rgb(0xCECECE);
    /// 选中文本背景：淡蓝
    pub const SEL: Color32 = rgb(0xADD6FF);
    /// 搜索命中高亮：淡黄（VSCode 风格）
    pub const HIGHLIGHT: Color32 = rgb(0xFFEC78);
    /// 当前搜索命中高亮：橙黄，比普通命中更醒目
    pub const SELECTED: Color32 = rgb(0xFFBE50);
    /// 光标：近黑
    pub const CURSOR: Color32 = rgb(0x141414);
    /// 滚动条轨道：与行号栏背景接近的浅灰
    pub const TRACK: Color32 = rgb(0xEBEBEB);
    /// 滚动条滑块：比轨道深一档的灰
    pub const THUMB: Color32 = rgb(0xBEBEBE);
    /// 正文文字：中灰（与浅色背景对比柔和）
    pub const TEXT: Color32 = rgb(0x888888);

    pub(crate) fn palette() -> EditorTheme {
        EditorTheme {
            bg: BG,
            gutter_bg: GUTTER_BG,
            gutter_fg: GUTTER_FG,
            gutter_sep: GUTTER_SEP,
            sel: SEL,
            highlight: HIGHLIGHT,
            selected: SELECTED,
            cursor: CURSOR,
            track: TRACK,
            thumb: THUMB,
            text: TEXT,
        }
    }
}

// ---------- 深色主题 ----------

/// 深色主题。
pub mod dark {
    use super::*;

    /// 编辑区背景：近黑的深灰蓝
    pub const BG: Color32 = rgb(0x2C2C32);
    /// 行号栏背景：比编辑区背景更深一档
    pub const GUTTER_BG: Color32 = rgb(0x18181C);
    /// 行号文字：中灰
    pub const GUTTER_FG: Color32 = rgb(0x9B9BA5);
    /// 行号栏分隔线：介于行号栏与编辑区背景之间
    pub const GUTTER_SEP: Color32 = rgb(0x46464E);
    /// 选中文本背景：深蓝
    pub const SEL: Color32 = rgb(0x325AA0);
    /// 搜索命中高亮：暗琥珀
    pub const HIGHLIGHT: Color32 = rgb(0x966E14);
    /// 当前搜索命中高亮：亮橙，比普通命中更醒目
    pub const SELECTED: Color32 = rgb(0xBE641E);
    /// 光标：近白
    pub const CURSOR: Color32 = rgb(0xEBEBEB);
    /// 滚动条轨道：略亮于背景的深灰
    pub const TRACK: Color32 = rgb(0x28282E);
    /// 滚动条滑块：中灰
    pub const THUMB: Color32 = rgb(0x6E6E82);
    /// 正文文字：中灰（嫌暗可调亮，如 rgb(0xC8C8D2)）
    pub const TEXT: Color32 = rgb(0x888888);

    pub(crate) fn palette() -> EditorTheme {
        EditorTheme {
            bg: BG,
            gutter_bg: GUTTER_BG,
            gutter_fg: GUTTER_FG,
            gutter_sep: GUTTER_SEP,
            sel: SEL,
            highlight: HIGHLIGHT,
            selected: SELECTED,
            cursor: CURSOR,
            track: TRACK,
            thumb: THUMB,
            text: TEXT,
        }
    }
}

// ---------- 通用界面色（不随主题切换） ----------

/// 与主题无关的零散界面色。调色时同样只改本模块。
pub mod ui {
    use super::*;

    /// 文件拖放覆盖层：整窗半透明蓝色底（RGB 0x508CDC，透明度 36/255）。
    /// （带透明度的构造非 const，无法声明为常量，故用函数）
    pub fn drop_overlay_bg() -> Color32 {
        Color32::from_rgba_unmultiplied(0x50, 0x8C, 0xDC, 0x24)
    }
    /// 文件拖放覆盖层：内缩 6px 的蓝色描边
    pub const DROP_OVERLAY_STROKE: Color32 = rgb(0x5A96E6);
    /// 文件拖放覆盖层：居中提示文字（"释放鼠标打开文件"）
    pub const DROP_OVERLAY_TEXT: Color32 = rgb(0xEBF2FF);

    /// 标签页加载中图标（⏳）的琥珀色
    pub const TAB_LOADING_ICON: Color32 = rgb(0xDCA03C);
    /// 编辑区右上角"建索引中 N%"角标文字的琥珀色
    pub const INDEXING_BADGE: Color32 = rgb(0xDC9628);

    /// 状态栏"●未保存"标记的橙色
    pub const DIRTY_MARK: Color32 = rgb(0xC8781E);
    /// 打开失败等错误提示文字的红色
    pub const ERROR_TEXT: Color32 = rgb(0xC83C3C);

    /// 排版测量专用的文字颜色：仅用于 galley 宽度/换行测量，颜色本身不影响测量结果。
    /// 与渲染用的 `EditorTheme::text` 保持同值，便于对照。
    pub const TEXT_MEASURE: Color32 = rgb(0x888888);
}
