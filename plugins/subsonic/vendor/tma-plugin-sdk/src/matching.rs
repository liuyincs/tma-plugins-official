//! 插件与宿主共用的名称相似度和匹配置信度函数。

use crate::normalize_name;

/// MusicBrainz 匹配分数组成权重。
pub const WEIGHT_NAME_SIM: f32 = 0.5;
pub const WEIGHT_MB_SCORE: f32 = 0.4;
pub const WEIGHT_YEAR_MATCH: f32 = 0.1;

/// 归一化名称相似度，范围为 [0, 1]。
pub fn name_similarity(local: &str, candidate: &str) -> f32 {
    let local_norm = normalize_name(local);
    let candidate_norm = normalize_name(candidate);
    if local_norm.is_empty() && candidate_norm.is_empty() {
        return 1.0;
    }
    if local_norm.is_empty() || candidate_norm.is_empty() {
        return 0.0;
    }
    if local_norm == candidate_norm {
        return 1.0;
    }
    let dist = levenshtein(&local_norm, &candidate_norm) as f32;
    let max_len = local_norm
        .chars()
        .count()
        .max(candidate_norm.chars().count()) as f32;
    let sim = 1.0 - (dist / max_len).clamp(0.0, 1.0);
    let containment_boost =
        if local_norm.contains(&candidate_norm) || candidate_norm.contains(&local_norm) {
            0.85_f32
        } else {
            0.0_f32
        };
    sim.max(containment_boost)
}

/// Levenshtein 字符级编辑距离。
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// 把 MusicBrainz 搜索证据转为宿主置信度。
pub fn fuzzy_confidence(name_sim: f32, mb_score: f32, year_match: bool) -> f32 {
    let mb_norm = (mb_score / 100.0).clamp(0.0, 1.0);
    let name_c = name_sim.clamp(0.0, 1.0);
    let year_c = if year_match { 1.0 } else { 0.0 };
    (WEIGHT_NAME_SIM * name_c + WEIGHT_MB_SCORE * mb_norm + WEIGHT_YEAR_MATCH * year_c).min(0.99)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_similarity_cases() {
        assert!((name_similarity("the beatles", "the beatles") - 1.0).abs() < f32::EPSILON);
        assert!(name_similarity("the beatles", "beatles") >= 0.85);
        assert!(name_similarity("radiohead", "radiohed") > 0.8);
        assert!(name_similarity("abba", "queen") < 0.5);
        assert_eq!(name_similarity("", ""), 1.0);
        assert_eq!(name_similarity("x", ""), 0.0);
        assert!((name_similarity("春天来了", "春天来吗") - 0.75).abs() < 0.01);
        assert_eq!(name_similarity("陳小春", "陈小春"), 1.0);
    }

    #[test]
    fn levenshtein_cases() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("你好", "你好吗"), 1);
    }

    #[test]
    fn confidence_is_bounded() {
        assert_eq!(fuzzy_confidence(1.0, 100.0, true), 0.99);
    }
}
