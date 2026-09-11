/// 文本编码（采样检测）。
///
/// v0.1 完整支持 Utf8 / Ascii（索引与搜索直接在原始字节上做）；
/// Utf16 / Latin1 检测后标记为降级路径，索引与搜索拒绝并给出明确错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 带 BOM
    Utf8Bom,
    /// UTF-8 无 BOM
    Utf8,
    /// 纯 ASCII
    Ascii,
    Utf16Le,
    Utf16Be,
    /// 非 UTF-8 的 8-bit 编码（ANSI/Latin1 等一律归此类）
    Latin1,
}

impl Encoding {
    pub fn name(&self) -> &'static str {
        match self {
            Encoding::Utf8Bom => "UTF-8 (BOM)",
            Encoding::Utf8 => "UTF-8",
            Encoding::Ascii => "ASCII",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "ANSI/Latin1",
        }
    }

    /// v0.1 可建索引的编码。
    pub fn indexable(&self) -> bool {
        matches!(self, Encoding::Utf8 | Encoding::Utf8Bom | Encoding::Ascii)
    }

    /// BOM 字节数（索引/搜索内容从 BOM 之后开始）。
    pub fn bom_len(&self) -> u64 {
        match self {
            Encoding::Utf8Bom => 3,
            _ => 0,
        }
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
            Some(_) => Encoding::Latin1,
        },
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
        // Latin1：高位字节非法 UTF-8
        assert_eq!(detect_encoding(&[b'a', 0xFF, 0xFF, 0xFF]), Encoding::Latin1);
    }

    #[test]
    fn bom_len() {
        assert_eq!(Encoding::Utf8Bom.bom_len(), 3);
        assert_eq!(Encoding::Ascii.bom_len(), 0);
    }
}
