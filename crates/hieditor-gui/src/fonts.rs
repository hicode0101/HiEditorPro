//! hieditor-gui — 用户字体目录扫描
//!
//! 扫描 exe 同目录下的 `fonts` 文件夹，识别其中的字体文件并提取**字体家族名**
//! （从字体的 `name` 表解析，而非文件名），供编辑区字体下拉列表使用。
//!
//! 支持格式（egui/ab_glyph 可光栅化的常见格式）：
//! - `.ttf` — TrueType
//! - `.otf` — OpenType（CFF 轮廓）
//! - `.ttc` — TrueType Collection（一个文件多个字体面，逐面解析、去重后全部列出）
//!
//! 不支持 `.woff/.woff2`（网页字体格式，ab_glyph 无法光栅化），扫描时自动忽略。
//!
//! 实现说明：为避免新增依赖，sfnt/name 表解析为手写实现（约百余行）：
//! - TTC 头：`ttcf` + 版本 + 字体数 + 每面的表目录偏移
//! - sfnt 表目录：遍历记录找 `name` 表
//! - name 表：取 nameID=1（家族名），优先 Windows UTF-16BE (3,1)/(3,0)/(3,10)
//!   与 Unicode (0,*)，退回 Mac Roman (1,0)；按优先级取最佳结果

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// fonts 目录中识别出的一个字体（TTC 内同名的多个面只保留一个）
#[derive(Clone)]
pub struct UserFont {
    /// 字体家族名（来自 name 表 nameID=1，用于下拉显示）
    pub family: String,
    /// 字体文件路径
    pub path: PathBuf,
    /// 文件内的字体面序号（TTC 用；普通 ttf/otf 恒为 0）
    pub index: u32,
}

/// 扫描 `dir` 下的字体文件，返回 (识别成功的字体列表, 无法识别的文件数)。
/// 目录不存在时返回空列表。
pub fn scan_fonts_dir(dir: &Path) -> (Vec<UserFont>, usize) {
    let mut out = Vec::new();
    let mut failed = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (out, failed);
    };
    // 按文件名排序，保证多次启动的列表顺序稳定
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()),
                    Some(ref ext) if matches!(ext.as_str(), "ttf" | "otf" | "ttc")
                )
        })
        .collect();
    files.sort();
    for path in files {
        let Ok(data) = std::fs::read(&path) else {
            failed += 1;
            continue;
        };
        let mut seen = HashSet::new();
        let mut any = false;
        for (face_idx, off) in sfnt_face_offsets(&data).into_iter().enumerate() {
            if let Some(family) = face_family(&data, off as usize) {
                if seen.insert(family.clone()) {
                    out.push(UserFont {
                        family,
                        path: path.clone(),
                        index: face_idx as u32,
                    });
                    any = true;
                }
            }
        }
        if !any {
            failed += 1; // 扩展名对但解析不出任何字体面
        }
    }
    (out, failed)
}

/// 取文件内每个字体面（sfnt 表目录）的偏移。
/// TTC 返回头中声明的全部面偏移；普通 ttf/otf 返回 `[0]`（表目录在文件头）。
fn sfnt_face_offsets(data: &[u8]) -> Vec<u32> {
    if data.len() >= 12 && &data[0..4] == b"ttcf" {
        let n = u32be(data, 8).unwrap_or(0).min(64) as usize; // 上限防畸形文件
        (0..n)
            .filter_map(|i| u32be(data, 12 + i * 4))
            .filter(|&off| (off as usize) + 12 <= data.len())
            .collect()
    } else if data.len() >= 12 {
        vec![0]
    } else {
        Vec::new()
    }
}

/// 解析某个字体面（表目录位于 `face_off`）的家族名。
fn face_family(data: &[u8], face_off: usize) -> Option<String> {
    let num_tables = u16be(data, face_off + 4)? as usize;
    let mut name_off = None;
    for i in 0..num_tables {
        let rec = face_off + 12 + i * 16;
        if rec + 16 > data.len() {
            return None;
        }
        if &data[rec..rec + 4] == b"name" {
            name_off = Some(u32be(data, rec + 8)? as usize);
            break;
        }
    }
    let no = name_off?;
    let count = u16be(data, no + 2)? as usize;
    let string_off = no + u16be(data, no + 4)? as usize;

    // 收集全部 nameID=1 记录，按 (平台优先级) 取最佳：
    // 2 = Windows/Unicode 的 UTF-16BE，能正确表示中文等非 ASCII 名称
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let rec = no + 6 + i * 12;
        if rec + 12 > data.len() {
            return None;
        }
        let platform = u16be(data, rec)?;
        let encoding = u16be(data, rec + 2)?;
        let _language = u16be(data, rec + 4)?; // 语言 ID，不据此筛选（按平台优先级取最佳）
        let name_id = u16be(data, rec + 6)?;
        let len = u16be(data, rec + 8)? as usize;
        let off = u16be(data, rec + 10)? as usize;
        if name_id != 1 {
            continue; // 1 = Family name
        }
        let s_off = string_off + off;
        if s_off + len > data.len() {
            continue;
        }
        let raw = &data[s_off..s_off + len];
        let (prio, name) = match (platform, encoding) {
            (3, 0) | (3, 1) | (3, 10) | (0, _) => (2u8, decode_utf16be(raw)),
            (1, 0) => (1u8, String::from_utf8_lossy(raw).into_owned()),
            _ => continue,
        };
        if !name.is_empty() && best.as_ref().is_none_or(|(p, _)| prio > *p) {
            best = Some((prio, name));
        }
    }
    best.map(|(_, name)| name)
}

fn u16be(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn u32be(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// UTF-16BE → String（代理对等非法序列按损失转换处理）
fn decode_utf16be(raw: &[u8]) -> String {
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_face_offsets_rejects_garbage() {
        assert!(sfnt_face_offsets(b"").is_empty());
        assert!(sfnt_face_offsets(b"no").is_empty());
        assert_eq!(sfnt_face_offsets(b"0123456789AB"), vec![0]);
    }

    #[test]
    fn test_helpers_bounds() {
        assert_eq!(u16be(b"abcd", 3), None);
        assert_eq!(u32be(b"abcd", 1), None);
        assert_eq!(u16be(&[0x12, 0x34], 0), Some(0x1234));
        assert_eq!(decode_utf16be(&[0x4E, 0x2D]), "中");
    }

    /// 端到端：临时 fonts 目录 → 扫描（拷入 ttf/ttc/无关文件）
    #[test]
    fn test_scan_dir_end_to_end() {
        let src1 = Path::new("C:\\Windows\\Fonts\\simhei.ttf");
        let src2 = Path::new("C:\\Windows\\Fonts\\simsun.ttc");
        if !src1.exists() || !src2.exists() {
            return;
        }
        let dir = std::env::temp_dir().join("hieditor_fonts_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(src1, dir.join("simhei.ttf")).unwrap();
        std::fs::copy(src2, dir.join("simsun.ttc")).unwrap();
        std::fs::write(dir.join("readme.txt"), b"not a font").unwrap();
        let (fonts, failed) = scan_fonts_dir(&dir);
        assert_eq!(failed, 0, "txt 应被忽略而非计为失败");
        assert!(
            fonts.iter().any(|f| f.path.extension().and_then(|e| e.to_str()) == Some("ttf")),
            "应识别出 ttf"
        );
        assert!(
            fonts.iter().any(|f| f.path.extension().and_then(|e| e.to_str()) == Some("ttc")),
            "应识别出 ttc"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真实 TTF：系统能找到 simhei.ttf 时应解析出非空家族名（黑体/SimHei）
    #[test]
    fn test_parse_real_ttf_family() {
        let p = Path::new("C:\\Windows\\Fonts\\simhei.ttf");
        if !p.exists() {
            return;
        }
        let data = std::fs::read(p).unwrap();
        let fam = face_family(&data, 0);
        assert!(fam.is_some_and(|f| !f.is_empty()), "应解析出家族名");
    }

    /// 真实 TTC：simsun.ttc 至少一个面，且每个面都能解析出家族名
    #[test]
    fn test_parse_real_ttc_faces() {
        let p = Path::new("C:\\Windows\\Fonts\\simsun.ttc");
        if !p.exists() {
            return;
        }
        let data = std::fs::read(p).unwrap();
        let offs = sfnt_face_offsets(&data);
        assert!(!offs.is_empty(), "TTC 应含至少一个字体面");
        for off in offs {
            assert!(face_family(&data, off as usize).is_some());
        }
    }
}
