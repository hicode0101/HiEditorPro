//! 打印支持 — Win32 标准打印流程（系统打印对话框 + GDI 文本打印）
//!
//! 流程（与记事本/EmEditor 同路）：
//! 1. [`run_print_dialog`]：`PrintDlgExW`（comdlg32）弹系统打印对话框，
//!    返回打印机 DC 与打印范围（全部 / 页码区间 / 选定内容 / 当前页）
//! 2. [`page_metrics`]：按返回的 DC 计算分页参数（每页行数、页边距、行高）
//! 3. [`spool`]：后台线程分页绘制（StartDoc → StartPage/ExtTextOut/EndPage
//!    循环 → EndDoc），进度经 channel 回传给 GUI 状态栏
//!
//! 版式：等宽字体、长行按页宽裁剪（不折行）、四边 15mm 页边距、无页眉页脚。
//! 打印份数由驱动处理（PD_USEDEVMODECOPIESANDCOLLATE）。
//!
//! 注：windows-sys 0.59 未收录 gdi32 的 StartDocW/StartPage/EndPage/EndDoc
//! （只收录了 winspool 的同名变体），本文件自行声明这四个外链函数与 DOCINFOW。

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{HGLOBAL, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DeleteDC, DeleteObject, ExtTextOutW, GetDeviceCaps, SelectObject, SetBkMode,
    SetTextColor, SetViewportOrgEx, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    ETO_CLIPPED, FF_MODERN, FIXED_PITCH, FW_NORMAL, HDC, HGDIOBJ, LOGPIXELSX, LOGPIXELSY,
    OUT_TT_PRECIS, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, TRANSPARENT,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Controls::Dialogs::{
    PrintDlgExW, PD_CURRENTPAGE, PD_DISABLEPRINTTOFILE, PD_HIDEPRINTTOFILE,
    PD_PAGENUMS, PD_RESULT_PRINT, PD_RETURNDC, PD_SELECTION, PD_USEDEVMODECOPIESANDCOLLATE,
    PRINTDLGEXW, PRINTPAGERANGE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC};

/// 打印页数上限：防止 GB 级文件"全部打印"物化海量文本拖垮进程
/// （20 000 页 × ~55 行 ≈ 百万行 / 约百 MB 文本量级）。
pub const PRINT_PAGE_CAP: usize = 20_000;
/// 页边距（0.1mm 单位）：15mm
const MARGIN_MM10: i64 = 150;
/// 行距系数（与编辑区 line_height = size × 1.4 接近）
const LINE_SPACING: f32 = 1.38;

// ---------- windows-sys 0.59 缺失的 gdi32 打印管线 ----------

#[repr(C)]
struct DOCINFOW {
    cb_size: i32,
    doc_name: *const u16,
    output: *const u16,
    datatype: *const u16,
    fw_type: u32,
}

extern "system" {
    fn StartDocW(hdc: HDC, lpdi: *const DOCINFOW) -> i32;
    fn StartPage(hdc: HDC) -> i32;
    fn EndPage(hdc: HDC) -> i32;
    fn EndDoc(hdc: HDC) -> i32;
}

// ---------- 对话框结果 ----------

/// 1 起始闭区间页码范围
#[derive(Clone, Copy)]
pub struct PageRange(pub u32, pub u32);

#[derive(Clone)]
pub enum PrintScope {
    All,
    Pages(Vec<PageRange>),
    Selection,
    CurrentPage,
}

pub struct PrintDialog {
    /// 打印机 DC（PD_RETURNDC），用完由 [`discard_dc`] 或 spool 线程释放
    pub hdc: HDC,
    pub scope: PrintScope,
    /// 目标打印机名（DEVNAMES 解析，仅供状态栏显示）
    pub printer: String,
}

/// 弹出系统打印对话框（模态，内部自带消息循环，阻塞至用户关闭）。
/// `total_pages` 供"页码范围"的最大值参考（用屏幕 DC 估算即可，之后按真实 DC 夹紧）。
/// 返回 None 表示用户取消或未选择打印机。
pub fn run_print_dialog(total_pages: u32) -> Option<PrintDialog> {
    unsafe {
        // 对话框内部使用 COM；eframe/winit 通常已按单元线程初始化，
        // 重复调用仅返回 S_FALSE，忽略即可（不配对 CoUninitialize，进程级无害）
        let _ = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED as u32);

        let mut ranges = [PRINTPAGERANGE { nFromPage: 0, nToPage: 0 }; 10];
        let total = total_pages.max(1);
        let mut pd = PRINTDLGEXW {
            lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
            hwndOwner: GetActiveWindow(),
            hDevMode: std::ptr::null_mut(),
            hDevNames: std::ptr::null_mut(),
            hDC: std::ptr::null_mut(),
            // 初始不传任何"范围选中"标志（PD_SELECTION/PD_PAGENUMS/PD_CURRENTPAGE），
            // 对话框即默认选中"全部"；四个单选项仍然全部可选手动切换
            // （不传 PD_NOPAGENUMS/PD_NOSELECTION/PD_NOCURRENTPAGE 即为启用）。
            // 用户实际选了哪一项在返回的 Flags 里判断（见下方 scope 解析）。
            Flags: PD_RETURNDC
                | PD_USEDEVMODECOPIESANDCOLLATE
                | PD_HIDEPRINTTOFILE
                | PD_DISABLEPRINTTOFILE,
            Flags2: 0,
            ExclusionFlags: 0,
            nPageRanges: 0,
            nMaxPageRanges: 10,
            lpPageRanges: ranges.as_mut_ptr(),
            nMinPage: 1,
            nMaxPage: total,
            nCopies: 1,
            hInstance: std::ptr::null_mut(),
            lpPrintTemplateName: std::ptr::null(),
            lpCallback: std::ptr::null_mut(),
            nPropertyPages: 0,
            lphPropertyPages: std::ptr::null_mut(),
            nStartPage: 0xFFFF_FFFF, // START_PAGE_GENERAL（常规页）
            dwResultAction: 0,
        };
        if PrintDlgExW(&mut pd) != 0 || pd.dwResultAction != PD_RESULT_PRINT {
            free_global(pd.hDevMode);
            free_global(pd.hDevNames);
            return None;
        }
        let scope = if pd.Flags & PD_PAGENUMS != 0 {
            let mut rs = Vec::new();
            for r in ranges.iter().take(pd.nPageRanges as usize) {
                let a = r.nFromPage.max(1);
                let b = r.nToPage.clamp(a, total);
                if a <= b {
                    rs.push(PageRange(a, b));
                }
            }
            if rs.is_empty() { PrintScope::All } else { PrintScope::Pages(rs) }
        } else if pd.Flags & PD_SELECTION != 0 {
            PrintScope::Selection
        } else if pd.Flags & PD_CURRENTPAGE != 0 {
            PrintScope::CurrentPage
        } else {
            PrintScope::All
        };
        let printer = read_device_name(pd.hDevNames);
        free_global(pd.hDevMode);
        free_global(pd.hDevNames);
        if pd.hDC.is_null() {
            return None;
        }
        Some(PrintDialog { hdc: pd.hDC, scope, printer })
    }
}

/// 释放打印对话框返回的 DC（用户取消打印流程时调用）
pub unsafe fn discard_dc(hdc: HDC) {
    DeleteDC(hdc);
}

unsafe fn free_global(h: HGLOBAL) {
    if !h.is_null() {
        GlobalFree(h);
    }
}

/// 从 DEVNAMES 取目标打印机名（wDeviceOffset 为自结构起始的字符偏移）
unsafe fn read_device_name(h: HGLOBAL) -> String {
    if h.is_null() {
        return String::new();
    }
    let p = GlobalLock(h) as *const u16;
    if p.is_null() {
        return String::new();
    }
    // DEVNAMES 布局：wDriverOffset / wDeviceOffset / wOutputOffset / wDefault（均 u16）
    let dev_off = *p.add(1) as usize;
    let mut name = Vec::new();
    let mut i = dev_off;
    while *p.add(i) != 0 {
        name.push(*p.add(i));
        i += 1;
    }
    GlobalUnlock(h);
    String::from_utf16_lossy(&name)
}

// ---------- 分页参数 ----------

/// 页面分页参数（单位：设备像素）
pub struct PageMetrics {
    /// 每页可容纳的文本行数（≥1）
    pub lines_per_page: u64,
}

/// 按打印机 DC 计算每页行数（15mm 上下边距、行高 = 字高 × LINE_SPACING）
pub fn page_metrics(hdc: HDC, font_pt: f32) -> PageMetrics {
    unsafe {
        let dpi_y = GetDeviceCaps(hdc, LOGPIXELSY as i32).max(96);
        let page_h = GetDeviceCaps(hdc, PHYSICALHEIGHT as i32).max(1);
        let margin = ((MARGIN_MM10 * dpi_y as i64) / 2540) as i32;
        let font_px = (font_pt * dpi_y as f32 / 72.0).max(6.0);
        let line_h = (font_px * LINE_SPACING).ceil() as i32;
        let usable_h = (page_h - 2 * margin).max(line_h);
        PageMetrics { lines_per_page: (usable_h / line_h).max(1) as u64 }
    }
}

/// 用屏幕 DPI 按 A4 纸估算每页行数（对话框弹出前为"页码范围"提供总页数参考）。
/// 注意：PHYSICALWIDTH/HEIGHT 仅打印机 DC 支持，屏幕 DC 返回 0，不能走 page_metrics。
pub fn estimate_lines_per_page(font_pt: f32) -> u64 {
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let dpi = if dc.is_null() {
            96
        } else {
            GetDeviceCaps(dc, LOGPIXELSY as i32).max(96)
        };
        if !dc.is_null() {
            ReleaseDC(std::ptr::null_mut(), dc);
        }
        // A4 纵向：297mm 高
        let page_h = (297.0 * dpi as f32 / 25.4) as i32;
        let margin = ((MARGIN_MM10 * dpi as i64) / 2540) as i32;
        let font_px = (font_pt * dpi as f32 / 72.0).max(6.0);
        let line_h = (font_px * LINE_SPACING).ceil() as i32;
        (((page_h - 2 * margin).max(line_h)) / line_h).max(1) as u64
    }
}

// ---------- 后台打印 ----------

/// 后台分页打印 `lines`（每 lpp 行一页），进度经 channel 回传：
/// `printing\tN` / `done\tN\t打印机名` / `fail\t`。
/// 结束时由本线程释放 DC 与字体资源。
pub fn spool(
    hdc: HDC,
    lines: Arc<Vec<String>>,
    lpp: u64,
    doc_name: String,
    font_face: String,
    font_pt: f32,
    printer: String,
) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    // HDC 是裸指针（非 Send），按地址值传给后台线程
    let hdc_addr = hdc as usize;
    let _ = std::thread::Builder::new().name("hieditor-print".into()).spawn(move || {
        unsafe {
            print_worker(hdc_addr as HDC, lines, lpp, doc_name, font_face, font_pt, printer, &tx)
        }
    });
    rx
}

unsafe fn print_worker(
    hdc: HDC,
    lines: Arc<Vec<String>>,
    lpp: u64,
    doc_name: String,
    font_face: String,
    font_pt: f32,
    printer: String,
    tx: &Sender<String>,
) {
    // 页面几何
    let dpi_x = GetDeviceCaps(hdc, LOGPIXELSX as i32).max(96);
    let dpi_y = GetDeviceCaps(hdc, LOGPIXELSY as i32).max(96);
    let off_x = GetDeviceCaps(hdc, PHYSICALOFFSETX as i32);
    let off_y = GetDeviceCaps(hdc, PHYSICALOFFSETY as i32);
    let page_w = GetDeviceCaps(hdc, PHYSICALWIDTH as i32).max(1);
    let margin_x = ((MARGIN_MM10 * dpi_x as i64) / 2540) as i32;
    let margin_y = ((MARGIN_MM10 * dpi_y as i64) / 2540) as i32;
    let usable_w = (page_w - 2 * margin_x).max(200);
    let font_px = (font_pt * dpi_y as f32 / 72.0).max(6.0);
    let line_h = (font_px * LINE_SPACING).ceil() as i32;

    // 等宽打印字体（负高度 = 字符 em 高度；GDI 字体映射器按 face 名就近匹配）
    let face: Vec<u16> = font_face.encode_utf16().chain(std::iter::once(0)).collect();
    let hfont = CreateFontW(
        -(font_px.round() as i32),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_TT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (FIXED_PITCH | FF_MODERN) as u32,
        face.as_ptr(),
    );
    if hfont.is_null() {
        let _ = tx.send("fail\t".into());
        DeleteDC(hdc);
        return;
    }
    let old_font: HGDIOBJ = SelectObject(hdc, hfont);
    SetTextColor(hdc, 0x0000_0000); // COLORREF 0x00BBGGRR = 黑
    SetBkMode(hdc, TRANSPARENT as i32);

    // 文档名
    let doc: Vec<u16> = doc_name.encode_utf16().chain(std::iter::once(0)).collect();
    let di = DOCINFOW {
        cb_size: std::mem::size_of::<DOCINFOW>() as i32,
        doc_name: doc.as_ptr(),
        output: std::ptr::null(),
        datatype: std::ptr::null(),
        fw_type: 0,
    };
    if StartDocW(hdc, &di) <= 0 {
        let _ = tx.send("fail\t".into());
        SelectObject(hdc, old_font);
        DeleteObject(hfont);
        DeleteDC(hdc);
        return;
    }

    let total_pages = lines.len().div_ceil(lpp as usize).max(1);
    let _ = tx.send(format!("printing\t{total_pages}"));

    let mut ok = true;
    for page in 0..total_pages {
        if StartPage(hdc) <= 0 {
            ok = false;
            break;
        }
        // 原点移到纸张左上角（抵消不可打印边距），坐标相对纸张
        SetViewportOrgEx(hdc, -off_x, -off_y, std::ptr::null_mut());
        // 部分驱动在页边界重置 DC 状态，每页重设
        SelectObject(hdc, hfont);
        SetTextColor(hdc, 0x0000_0000);
        SetBkMode(hdc, TRANSPARENT as i32);

        let start = page * lpp as usize;
        let end = (start + lpp as usize).min(lines.len());
        for (row, text) in lines[start..end].iter().enumerate() {
            let w: Vec<u16> = text.encode_utf16().collect();
            if w.is_empty() {
                continue;
            }
            let y = margin_y + row as i32 * line_h;
            let clip = RECT {
                left: margin_x,
                top: y,
                right: margin_x + usable_w,
                bottom: y + line_h,
            };
            ExtTextOutW(
                hdc,
                margin_x,
                y,
                ETO_CLIPPED,
                &clip,
                w.as_ptr(),
                w.len() as u32,
                std::ptr::null(),
            );
        }
        if EndPage(hdc) <= 0 {
            ok = false;
            break;
        }
    }
    let _ = EndDoc(hdc);
    SelectObject(hdc, old_font);
    DeleteObject(hfont);
    DeleteDC(hdc);
    if ok {
        let _ = tx.send(format!("done\t{total_pages}\t{printer}"));
    } else {
        let _ = tx.send("fail\t".into());
    }
}
