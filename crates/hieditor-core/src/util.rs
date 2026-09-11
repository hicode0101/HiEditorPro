/// 人类可读的大小表示。
pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

/// 解析 "1GB" / "512MB" / "64KB" / 纯字节数。
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("GB").or_else(|| s.strip_suffix("gb")) {
        (n, 1u64 << 30)
    } else if let Some(n) = s.strip_suffix("MB").or_else(|| s.strip_suffix("mb")) {
        (n, 1 << 20)
    } else if let Some(n) = s.strip_suffix("KB").or_else(|| s.strip_suffix("kb")) {
        (n, 1 << 10)
    } else if let Some(n) = s.strip_suffix('B').or_else(|| s.strip_suffix('b')) {
        (n, 1)
    } else {
        (s, 1)
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1GB"), Some(1 << 30));
        assert_eq!(parse_size("512MB"), Some(512 << 20));
        assert_eq!(parse_size("64kb"), Some(64 << 10));
        assert_eq!(parse_size("123"), Some(123));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1 << 30), "1.00 GB");
    }
}
