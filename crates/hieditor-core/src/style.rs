//! 着色数据模型：文本运行 + 样式属性的片段表（StyleSpan）。
//!
//! 编辑区所有着色（搜索高亮 / 语法高亮 / 当前行……）都由各自的"生产者"
//! 向同一个 [`StyleLayer`] 贡献片段，按优先级叠加；渲染端只为可视区解析。
//! CLI（ANSI 颜色）与 GUI（egui 真彩）消费同一模型。

/// RGBA 颜色。core 不依赖任何 GUI 框架。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba([r, g, b, 255])
    }
}

/// 一段文本的样式属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub bold: bool,
}

/// 片段的生产者。数值越大优先级越高（重叠区间高优先级胜出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Producer {
    /// 搜索命中高亮
    Search = 0,
    /// 语法高亮（二期）
    Syntax = 1,
    /// 当前行 / 选区（二期）
    Selection = 2,
}

/// 一个样式片段：字节区间 [start, end) 应用 style。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleSpan {
    pub start: u64,
    pub end: u64,
    pub style: Style,
    pub producer: Producer,
}

/// 样式片段表：按 start 排序；每个生产者的片段整体替换（先清后插），
/// 搜索重跑时旧高亮自然消失，无需逐条 diff。
#[derive(Debug, Default)]
pub struct StyleLayer {
    spans: Vec<StyleSpan>,
}

impl StyleLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 替换某生产者的全部片段。
    pub fn replace_producer(&mut self, producer: Producer, mut spans: Vec<StyleSpan>) {
        self.spans.retain(|s| s.producer != producer);
        for s in &mut spans {
            s.producer = producer;
        }
        spans.extend(self.spans.drain(..));
        self.spans = spans;
        self.spans.sort_by_key(|s| (s.start, s.end));
    }

    /// 清空所有片段。
    pub fn clear(&mut self) {
        self.spans.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// [range_start, range_end) 内的片段（二分定位窗口）。
    pub fn spans_in_range(&self, range_start: u64, range_end: u64) -> &[StyleSpan] {
        let lo = self.spans.partition_point(|s| s.end <= range_start);
        let hi = self.spans.partition_point(|s| s.start < range_end);
        &self.spans[lo..hi.max(lo)]
    }

    pub fn all_spans(&self) -> &[StyleSpan] {
        &self.spans
    }
}

/// 把命中列表（字节偏移 + 长度）转换为 Search 生产者的高亮片段。
pub fn search_hits_to_spans(hits: &[(u64, u32)], style: Style) -> Vec<StyleSpan> {
    hits.iter()
        .map(|&(off, len)| StyleSpan {
            start: off,
            end: off + len as u64,
            style,
            producer: Producer::Search,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_and_query() {
        let mut layer = StyleLayer::new();
        let st = Style {
            bg: Some(Rgba::rgb(255, 200, 0)),
            ..Default::default()
        };
        layer.replace_producer(
            Producer::Search,
            vec![
                StyleSpan { start: 10, end: 15, style: st, producer: Producer::Search },
                StyleSpan { start: 100, end: 105, style: st, producer: Producer::Search },
            ],
        );
        assert_eq!(layer.len(), 2);
        let in_range = layer.spans_in_range(0, 50);
        assert_eq!(in_range.len(), 1);
        assert_eq!(in_range[0].start, 10);
        assert!(layer.spans_in_range(40, 90).is_empty());

        // 替换生产者 → 旧片段消失
        layer.replace_producer(Producer::Search, Vec::new());
        assert!(layer.is_empty());
    }

    #[test]
    fn hits_to_spans() {
        let spans = search_hits_to_spans(&[(5, 3), (20, 3)], Style::default());
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].start, 20);
        assert_eq!(spans[1].end, 23);
        assert_eq!(spans[1].producer, Producer::Search);
    }
}
