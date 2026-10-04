//! 插件与宿主共用的名称和 MBID 规范化函数。

use std::sync::LazyLock;

use simplecc::dicts;
use unicode_normalization::UnicodeNormalization;

static T2S: LazyLock<simplecc::Dict> = LazyLock::new(|| dicts::T2S.clone());

/// 规范化名称：NFKC、trim、空白折叠、lowercase、去首尾标点、繁体转简体。
pub fn normalize_name(s: &str) -> String {
    let nfkc: String = s.nfkc().collect();
    let trimmed = trim_edges(&nfkc);
    let collapsed = collapse_whitespace(&trimmed);
    let lower = collapsed.to_lowercase();
    let stripped = trim_punctuation_edges(&lower);
    if stripped.chars().any(is_cjk) {
        T2S.replace_all(&stripped)
    } else {
        stripped
    }
}

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2A6DF}'
            | '\u{2A700}'..='\u{2B73F}'
            | '\u{2B740}'..='\u{2B81F}'
            | '\u{2B820}'..='\u{2CEAF}'
            | '\u{30000}'..='\u{3134F}'
    )
}

/// 校验并规范化 MBID（小写 UUID，去花括号与空白）。
pub fn normalize_mbid(s: &str) -> Option<String> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '{' && *c != '}')
        .collect();
    if cleaned.len() != 36 {
        return None;
    }
    let lower = cleaned.to_lowercase();
    if lower.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Some(lower)
    } else {
        None
    }
}

fn trim_edges(s: &str) -> String {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{3000}' || c == '\u{00A0}')
        .to_string()
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() || c == '\u{3000}' || c == '\u{00A0}' {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn trim_punctuation_edges(s: &str) -> String {
    s.trim_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | '-' | '_' | '/' | '\\' | '|' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_nfkc_and_whitespace() {
        assert_eq!(normalize_name("  Hello\u{3000}World  "), "hello world");
        assert_eq!(normalize_name("ＡＢＣ"), "abc");
    }

    #[test]
    fn normalize_name_unifies_traditional_and_simplified() {
        assert_eq!(normalize_name("陳小春"), "陈小春");
        assert_eq!(normalize_name("陳奐仁"), "陈奂仁");
    }
}
