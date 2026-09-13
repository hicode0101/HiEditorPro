//! 文本编码（采样检测 + 编解码助手）。
//!
//! 完整支持 Utf8 / Ascii / Gbk(含 GB2312)：
//! - 行索引与搜索直接在原始字节上做（GBK 的 0x0A/0x0D 不会出现在双字节
//!   序列内部，按字节扫换行安全）
//! - 渲染/输入经 [`Encoding::decode_str`] / [`Encoding::encode_str`] 转换
//!
//! Utf16 / Latin1 检测后仍标记为降级路径，编辑拒绝并给出明确错误。

use encoding_rs::GBK;

/// 文本编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 带 BOM
    Utf8Bom,
    /// UTF-8 无 BOM
    Utf8,
    /// 纯 ASCII
    Ascii,
    /// GBK / GB2312（中文 Windows ANSI；GB2312 是 GBK 子集，同一解码器）
    Gbk,
    Utf16Le,
    Utf16Be,
    /// 非 UTF-8/GBK 的 8-bit 编码（Latin1 等，暂不支持编辑）
    Latin1,
}

impl Encoding {
    pub fn name(&self) -> &'static str {
        match self {
            Encoding::Utf8Bom => "UTF-8 (BOM)",
            Encoding::Utf8 => "UTF-8",
            Encoding::Ascii => "ASCII",
            Encoding::Gbk => "GBK/GB2312",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "ANSI/Latin1",
        }
    }

    /// 是否可建索引/编辑。
    pub fn indexable(&self) -> bool {
        matches!(
            self,
            Encoding::Utf8 | Encoding::Utf8Bom | Encoding::Ascii | Encoding::Gbk
        )
    }

    /// BOM 字节数（索引/搜索内容从 BOM 之后开始）。
    pub fn bom_len(&self) -> u64 {
        match self {
            Encoding::Utf8Bom => 3,
            _ => 0,
        }
    }

    /// 是否 UTF-8 系（字节偏移 == 解码后字符串字节偏移，无需映射）。
    pub fn is_utf8_family(&self) -> bool {
        matches!(self, Encoding::Utf8 | Encoding::Utf8Bom | Encoding::Ascii)
    }

    /// 是否 GBK 系。
    pub fn is_gbk(&self) -> bool {
        matches!(self, Encoding::Gbk)
    }

    /// 字节 → 字符串（无效序列按 U+FFFD 损失转换）。
    pub fn decode_str(&self, bytes: &[u8]) -> String {
        match *self {
            Encoding::Gbk => {
                let (cow, _, _) = GBK.decode(bytes);
                cow.into_owned()
            }
            _ => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    /// 字符串 → 字节（GBK 下无法映射的字符替换为 '?'）。
    pub fn encode_str(&self, s: &str) -> Vec<u8> {
        match *self {
            Encoding::Gbk => {
                let (cow, _, had_errors) = GBK.encode(s);
                let mut out = cow.into_owned();
                if had_errors {
                    // encode 默认把无法映射字符写成 &#NNNNN; 数字引用，替换为 '?'
                    let mut clean: Vec<u8> = Vec::with_capacity(out.len());
                    let mut i = 0;
                    while i < out.len() {
                        if out[i] == b'&' && out[i..].starts_with(b"&#") {
                            if let Some(end) = out[i..].iter().position(|&b| b == b';') {
                                clean.push(b'?');
                                i += end + 1;
                                continue;
                            }
                        }
                        clean.push(out[i]);
                        i += 1;
                    }
                    out = clean;
                }
                out
            }
            _ => s.as_bytes().to_vec(),
        }
    }

    /// 单个字符按本编码的字节长度（用于 解码偏移 ↔ 文件偏移 映射）。
    pub fn char_len(&self, c: char) -> usize {
        if self.is_utf8_family() {
            c.len_utf8()
        } else {
            let mut buf = [0u8; 4];
            self.encode_str(c.encode_utf8(&mut buf)).len().max(1)
        }
    }
}

/// GBK 前导字节（0x81–0xFE）。
pub fn is_gbk_lead(b: u8) -> bool {
    (0x81..=0xFE).contains(&b)
}

/// GBK 次字节（0x40–0xFE，不含 0x7F）。
pub fn is_gbk_trail(b: u8) -> bool {
    (0x40..=0x7E).contains(&b) || (0x80..=0xFE).contains(&b)
}

/// 大小写折叠（仅 ASCII 字母）。
///
/// `gbk = true` 时跳过 GBK 双字节序列的次字节——GBK 次字节落在 0x40–0x7E
/// 时与 ASCII 字母重叠，盲目折叠会把两个不同的汉字折叠到同一形式造成误匹配。
pub fn fold_ascii(bytes: &mut [u8], gbk: bool) {
    if !gbk {
        bytes.make_ascii_lowercase();
        return;
    }
    let mut prev_lead = false;
    for b in bytes.iter_mut() {
        let cur_is_ascii = *b < 0x80;
        if cur_is_ascii && !prev_lead {
            b.make_ascii_lowercase();
        }
        prev_lead = is_gbk_lead(*b);
    }
}

/// 采样头部做编码检测。
pub fn detect_encoding(head: &[u8]) -> Encoding {
    // 1. BOM 优先
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Encoding::Utf8Bom;
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return Encoding::Utf16Le;
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Encoding::Utf16Be;
    }

    // 2. 含零字节 → UTF-16 猜测（统计零字节奇偶位置）
    if head.contains(&0) {
        let even_zeros = head.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        let odd_zeros = head.iter().step_by(2).filter(|&&b| b == 0).count();
        return if even_zeros >= odd_zeros {
            Encoding::Utf16Le
        } else {
            Encoding::Utf16Be
        };
    }

    // 3. 纯 ASCII
    if head.iter().all(|&b| b < 0x80) {
        return Encoding::Ascii;
    }

    // 4. UTF-8 校验：用 error_len() 区分"采样边界截断"与"真正非法字节"。
    match std::str::from_utf8(head) {
        Ok(_) => Encoding::Utf8,
        Err(e) => match e.error_len() {
            // None = 错误源于采样截断（多字节序列不完整）→ 仍判 UTF-8
            None => Encoding::Utf8,
            Some(_) => detect_ansi_flavor(head),
        },
    }
}

/// 非 UTF-8 的高位字节内容：区分 GBK 与 Latin1。
///
/// 判据：采样整体是合法 GBK 字节结构（前导+次字节成对），且至少含一个
/// 双字节序列——纯 Latin1 的高位字节是散立的重音字母，极少构成合法 GBK 串。
/// 采样末尾被截断的前导字节视作合法结束（与 UTF-8 检测的截断处理一致）。
fn detect_ansi_flavor(head: &[u8]) -> Encoding {
    let mut double_byte_seqs = 0usize;
    let mut i = 0usize;
    let mut strict_ok = true;
    let mut truncated_tail = false;
    while i < head.len() {
        let b = head[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        if is_gbk_lead(b) {
            if i + 1 >= head.len() {
                // 采样末尾截断的前导字节 → 视作合法结束
                truncated_tail = true;
                break;
            }
            if is_gbk_trail(head[i + 1]) {
                double_byte_seqs += 1;
                i += 2;
                continue;
            }
            strict_ok = false;
            break;
        }
        // 高位字节但非 GBK 前导（0x80–0xA0 区间等）→ 不是 GBK 文本
        strict_ok = false;
        break;
    }
    // GBK 严格解码校验：仅当末尾存在截断的前导字节时裁掉它再解码
    let mut end = head.len();
    if strict_ok && truncated_tail && end > 0 {
        end -= 1;
    }
    if strict_ok && double_byte_seqs > 0 && end > 0 && !GBK.decode(&head[..end]).2 {
        Encoding::Gbk
    } else {
        Encoding::Latin1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert_eq!(detect_encoding(b"hello world"), Encoding::Ascii);
        assert_eq!(detect_encoding(&[0xEF, 0xBB, 0xBF, b'a']), Encoding::Utf8Bom);
        assert_eq!(detect_encoding(&[0xFF, 0xFE, b'a', 0]), Encoding::Utf16Le);
        assert_eq!(detect_encoding(&[0xFE, 0xFF, 0, b'a']), Encoding::Utf16Be);
        // 中文 UTF-8 "你好"
        let zh = "你好世界".as_bytes();
        assert_eq!(detect_encoding(zh), Encoding::Utf8);
        // 截断的多字节序列尾部 → 仍判 UTF-8
        assert_eq!(detect_encoding(&zh[..7]), Encoding::Utf8);
        // GBK："你好世界" = C4 E3 BA C3 CA C0 BD E7
        let gbk = [0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7];
        assert_eq!(detect_encoding(&gbk), Encoding::Gbk);
        // GBK 混合 ASCII："abc你好"
        let mixed = [b'a', b'b', b'c', 0xC4, 0xE3, 0xBA, 0xC3];
        assert_eq!(detect_encoding(&mixed), Encoding::Gbk);
        // Latin1：高位字节非法 GBK（连续 0xFF）
        assert_eq!(detect_encoding(&[b'a', 0xFF, 0xFF, 0xFF]), Encoding::Latin1);
        // 单个高位重音字母（Latin1 é = 0xE9）后跟控制字节 → Latin1
        assert_eq!(detect_encoding(&[b'a', 0xE9, 0x01]), Encoding::Latin1);
    }

    #[test]
    fn gbk_roundtrip() {
        let enc = Encoding::Gbk;
        let s = "你好，世界！Hello 123";
        let bytes = enc.encode_str(s);
        // 4 汉字×2 + 全角逗号/叹号×2 + "Hello 123"×9
        assert_eq!(bytes.len(), 8 + 4 + 9);
        assert_eq!(enc.decode_str(&bytes), s);
        assert!(enc.indexable());
        assert!(!enc.is_utf8_family());
    }

    #[test]
    fn gbk_unmappable_becomes_question_mark() {
        let enc = Encoding::Gbk;
        let bytes = enc.encode_str("ok\u{1F600}"); // emoji 无法映射 GBK
        assert_eq!(enc.decode_str(&bytes), "ok?");
    }

    #[test]
    fn fold_gbk_skips_trail_bytes() {
        // GBK 0xB0 0x41 与 0xB0 0x61 是两个不同的汉字（次字节 A/a）。
        // gbk 折叠不得把次字节 0x41 折成 0x61。
        let mut a = [0xB0u8, 0x41];
        fold_ascii(&mut a, true);
        assert_eq!(a, [0xB0, 0x41]);
        // 纯 ASCII 照常折叠
        let mut b = *b"AbC";
        fold_ascii(&mut b, true);
        assert_eq!(&b, b"abc");
        // 非严格模式（UTF-8）整段折叠
        let mut c = *b"AbC";
        fold_ascii(&mut c, false);
        assert_eq!(&c, b"abc");
    }
}
