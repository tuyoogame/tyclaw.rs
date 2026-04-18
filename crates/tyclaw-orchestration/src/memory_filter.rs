//! Memory 段落相关性过滤。
//!
//! 按段落过滤 MEMORY.md，只保留与当前用户请求相关的段落。

use std::collections::HashSet;
use tracing::info;

/// 按段落过滤 MEMORY.md，只保留与当前用户请求相关的段落。
///
/// 规则：
/// - 结构性段落（Skills路径、工作规则等）始终保留
/// - 事实性段落（天气、股价、查询结果等）只在与 user_message 有关键词重叠时保留
/// - 保留段落的顺序不变
pub(crate) fn filter_memory_by_relevance(memory: &str, user_message: &str) -> String {
    // 将用户消息分词为关键词集合（中文按字符，英文按空格）
    let query_tokens = extract_keywords(user_message);
    if query_tokens.is_empty() {
        return memory.to_string();
    }

    // 按 ## 标题拆分段落
    let sections = split_memory_sections(memory);
    if sections.is_empty() {
        return memory.to_string();
    }

    let mut kept = Vec::new();
    let mut filtered_count = 0usize;

    for (header, body) in &sections {
        let full = format!("{}\n{}", header, body);

        // 结构性段落始终保留：包含 skill、path、规则、工作、项目、config 等关键词
        if is_structural_section(header) {
            kept.push(full);
            continue;
        }

        // 事实性段落：检查关键词重叠
        let section_tokens = extract_keywords(&format!("{} {}", header, body));
        let overlap: usize = query_tokens
            .iter()
            .filter(|t| section_tokens.contains(*t))
            .count();

        if overlap > 0 {
            kept.push(full);
        } else {
            filtered_count += 1;
        }
    }

    if filtered_count > 0 {
        info!(
            filtered = filtered_count,
            kept = kept.len(),
            "Filtered irrelevant memory sections"
        );
    }

    kept.join("\n\n")
}

/// 判断是否为结构性段落（始终保留）。
pub(crate) fn is_structural_section(header: &str) -> bool {
    let h = header.to_lowercase();
    // 这些段落是关于工作方式/工具/项目结构的元信息，始终有用
    let structural_keywords = [
        "skill", "path", "location", "规则", "工作", "项目", "config",
        "important", "注意", "workspace", "instability", "note",
        "known", "issue", "bug", "logic", "spec", "结构",
    ];
    structural_keywords.iter().any(|kw| h.contains(kw))
}

/// 将 MEMORY.md 按 `## ` 标题分段。
pub(crate) fn split_memory_sections(memory: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_header = String::new();
    let mut current_body = String::new();
    let mut in_section = false;

    for line in memory.lines() {
        if line.starts_with("## ") {
            if in_section {
                sections.push((current_header.clone(), current_body.trim().to_string()));
            }
            current_header = line.to_string();
            current_body = String::new();
            in_section = true;
        } else if in_section {
            current_body.push_str(line);
            current_body.push('\n');
        } else {
            // 顶层内容（## 之前），始终保留
            if !line.trim().is_empty() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }
    }

    // 处理最后一个段落
    if in_section {
        sections.push((current_header, current_body.trim().to_string()));
    } else if !current_body.trim().is_empty() {
        // 没有任何 ## 标题，整体保留
        sections.push(("".to_string(), current_body.trim().to_string()));
    }

    sections
}

/// 从文本中提取关键词集合。
/// 中文：提取连续的 2 字符 bigram（覆盖中文词汇）
/// 英文/数字：提取连续的 ASCII 词，转小写
pub(crate) fn extract_keywords(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();

    // 先分离 ASCII 词和中文字符
    let mut ascii_buf = String::new();
    let mut cjk_chars: Vec<char> = Vec::new();

    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            ascii_buf.push(c);
            // CJK 序列断开，flush
            if cjk_chars.len() >= 2 {
                for window in cjk_chars.windows(2) {
                    tokens.insert(window.iter().collect::<String>());
                }
            }
            cjk_chars.clear();
        } else if c >= '\u{4E00}' && c <= '\u{9FFF}' {
            // CJK 字符
            cjk_chars.push(c);
            // ASCII 序列断开，flush
            if ascii_buf.len() >= 2 {
                tokens.insert(ascii_buf.to_lowercase());
            }
            ascii_buf.clear();
        } else {
            // 其他字符（标点、空格等），flush 两个 buffer
            if ascii_buf.len() >= 2 {
                tokens.insert(ascii_buf.to_lowercase());
            }
            ascii_buf.clear();
            if cjk_chars.len() >= 2 {
                for window in cjk_chars.windows(2) {
                    tokens.insert(window.iter().collect::<String>());
                }
            }
            cjk_chars.clear();
        }
    }

    // flush 剩余
    if ascii_buf.len() >= 2 {
        tokens.insert(ascii_buf.to_lowercase());
    }
    if cjk_chars.len() >= 2 {
        for window in cjk_chars.windows(2) {
            tokens.insert(window.iter().collect::<String>());
        }
    }

    tokens
}

// ---------------------------------------------------------------------------
// Memory 过滤测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod memory_filter_tests {
    use super::*;

    #[test]
    fn test_filter_removes_irrelevant_weather() {
        let memory = "\
## Skills paths and locations
- Weather Checker: _personal/skills/weather-checker/tool.py
- LTV Excel Generator: _personal/skills/ltv-excel-generator/tool.py

## Weather facts previously obtained
- Beijing 2026-04-10: 晴 22.3°C
- Tokyo 2026-04-11: 阴 12.6~24.2°C
- Hong Kong 2026-04-09: Partly cloudy

## US Stock price facts
- AAPL $258.90 (+$5.40, +2.13%)

## Gold price facts
- Gold spot ~$4,715.74/oz";

        // 用户请求写小游戏 → 天气/股价/金价段落应该被过滤，Skills路径保留
        let filtered = filter_memory_by_relevance(memory, "帮我写一个H5小游戏");
        assert!(filtered.contains("Skills paths"), "structural section should be kept");
        assert!(!filtered.contains("Beijing"), "weather facts should be filtered");
        assert!(!filtered.contains("AAPL"), "stock facts should be filtered");
        assert!(!filtered.contains("Gold spot"), "gold facts should be filtered");
    }

    #[test]
    fn test_filter_keeps_relevant_weather() {
        let memory = "\
## Weather facts previously obtained
- 北京 2026-04-10: 晴 22.3°C, 天气预报数据

## Skills paths and locations
- Weather Checker: tool.py";

        // 用户请求查天气 → 天气段落应该保留（"天气" bigram 重叠）
        let filtered = filter_memory_by_relevance(memory, "帮我查一下北京近3天的天气");
        assert!(filtered.contains("北京"), "weather should be kept when relevant");
    }

    #[test]
    fn test_filter_keeps_all_structural() {
        let memory = "\
## IMPORTANT: Skills workspace notes
- Personal skills directory has been deleted

## Known data quality issues
- input.xlsx contains extrapolated pay data";

        let filtered = filter_memory_by_relevance(memory, "随便什么请求");
        assert!(filtered.contains("IMPORTANT"), "important sections kept");
        assert!(filtered.contains("Known data"), "known issues kept");
    }

    #[test]
    fn test_extract_keywords_chinese() {
        let kws = extract_keywords("帮我写一个H5小游戏");
        assert!(kws.contains("h5"));
        // 应该有中文 bigram
        assert!(kws.contains("游戏") || kws.contains("小游"));
    }

    #[test]
    fn test_split_sections() {
        let memory = "# Top\nsome intro\n\n## Section A\ncontent a\n\n## Section B\ncontent b";
        let sections = split_memory_sections(memory);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].0.contains("Section A"));
        assert!(sections[1].0.contains("Section B"));
    }
}
