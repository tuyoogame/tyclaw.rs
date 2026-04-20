//! JSON 修复工具 —— 修复 LLM 输出中常见的 JSON 格式错误。
//!
//! LLM 在工具调用时可能生成格式不完整的 JSON，
//! 常见问题包括：末尾逗号、未闭合的括号、markdown 代码块包裹等。

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// 全局缓存的末尾逗号正则表达式（编译一次，之后复用）。
fn trailing_comma_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r",\s*([}\]])").unwrap())
}

/// 尝试修复畸形 JSON 字符串并解析为 Value。
///
/// 修复策略（按顺序尝试）：
/// 1. 直接解析
/// 2. 去除 markdown 代码块包裹
/// 3. 去除末尾逗号
/// 4. 补齐未闭合的括号
/// 5. 去除控制字符
pub fn repair_json(input: &str) -> Result<Value, serde_json::Error> {
    // 1. 直接尝试解析
    if let Ok(v) = serde_json::from_str(input) {
        return Ok(v);
    }

    let mut s = input.trim().to_string();

    // 2. 去除 markdown 代码块
    if s.starts_with("```") {
        // 去掉第一行 (```json 或 ```)
        if let Some(pos) = s.find('\n') {
            s = s[pos + 1..].to_string();
        }
        // 去掉末尾的 ```
        if s.ends_with("```") {
            s.truncate(s.len() - 3);
        }
        s = s.trim().to_string();
        if let Ok(v) = serde_json::from_str(&s) {
            return Ok(v);
        }
    }

    // 3. 去除末尾逗号 (在 } 或 ] 之前)
    let cleaned = trailing_comma_re().replace_all(&s, "$1").to_string();
    if let Ok(v) = serde_json::from_str(&cleaned) {
        return Ok(v);
    }
    s = cleaned;

    // 4. 去除控制字符（保留换行和制表符）
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t' || *c == '\r')
        .collect();
    if cleaned != s {
        if let Ok(v) = serde_json::from_str(&cleaned) {
            return Ok(v);
        }
        s = cleaned;
    }

    // 5. 补齐未闭合的括号
    let mut braces = 0i32;
    let mut brackets = 0i32;
    let mut in_string = false;
    let mut prev_char = ' ';
    for c in s.chars() {
        if c == '"' && prev_char != '\\' {
            in_string = !in_string;
        } else if !in_string {
            match c {
                '{' => braces += 1,
                '}' => braces -= 1,
                '[' => brackets += 1,
                ']' => brackets -= 1,
                _ => {}
            }
        }
        prev_char = c;
    }
    // 如果在字符串内，先关闭字符串
    if in_string {
        s.push('"');
    }
    for _ in 0..brackets {
        s.push(']');
    }
    for _ in 0..braces {
        s.push('}');
    }

    // 最终尝试
    serde_json::from_str(&s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_valid_json() {
        let result = repair_json(r#"{"key": "value"}"#).unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_trailing_comma() {
        let result = repair_json(r#"{"key": "value",}"#).unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_markdown_fence() {
        let result = repair_json("```json\n{\"key\": \"value\"}\n```").unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_unclosed_brace() {
        let result = repair_json(r#"{"key": "value""#).unwrap();
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_array_trailing_comma() {
        let result = repair_json(r#"[1, 2, 3,]"#).unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }
}
