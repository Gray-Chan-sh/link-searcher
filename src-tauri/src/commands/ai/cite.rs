//! Citation post-processing: supplement, sanitize and merge `[N]` markers in
//! LLM answers based on the evidence materials provided this turn.

use super::EvidenceItem;

/// Post-process LLM response: supplement [N] citations for sentences that
/// match evidence snippets but weren't tagged by the LLM.
///
/// Algorithm:
/// 1. Split by 。！？!? into sentences (`.` excluded — it appears in
///    decimals/versions/URLs; Chinese sentences end with 。！？)
/// 2. Skip sentences that already have [N]
/// 3. For remaining sentences, compute keyword overlap with each evidence snippet
/// 4. If overlap > threshold, add [N] to sentence end
/// 5. Merge consecutive sentences that cite the same source
pub fn auto_cite(answer: &str, evidence: &[EvidenceItem]) -> String {
    if evidence.is_empty() || answer.trim().is_empty() {
        return answer.to_string();
    }

    let code_block_re = regex::Regex::new(r"```[\s\S]*?```").unwrap();
    let mut placeholders: Vec<String> = Vec::new();
    let protected = code_block_re.replace_all(answer, |caps: &regex::Captures| {
        let idx = placeholders.len();
        placeholders.push(caps[0].to_string());
        format!("\x00CODE{idx}\x00")
    });

    let labels: Vec<(usize, &str)> = evidence.iter().enumerate().map(|(i, e)| {
        let no = if e.material_no == 0 { i + 1 } else { e.material_no };
        (no, e.snippet.as_str())
    }).collect();

    let sent_re = regex::Regex::new(r"[^。！？!?\n]*[。！？!?]").unwrap();
    // 已有引用的句子直接跳过，不再追加 [N]（避免 [3][1] 重叠）
    let has_cite_re = regex::Regex::new(r"\[\d+\]").unwrap();
    let mut result = String::with_capacity(protected.len() + 64);
    let mut last_end = 0;
    let mut prev_cite: Option<usize> = None;

    for m in sent_re.find_iter(&protected) {
        let sent = m.as_str();
        let trimmed = sent.trim();

        result.push_str(&protected[last_end..m.start()]);
        result.push_str(sent);

        last_end = m.end();

        if trimmed.is_empty() || trimmed.starts_with("\x00CODE") || has_cite_re.is_match(trimmed) {
            prev_cite = None;
            continue;
        }

        let mut best: Option<usize> = None;
        let mut best_score = 0.0;
        for (n, snippet) in &labels {
            let score = keyword_overlap(sent, snippet);
            if score > 0.15 && score > best_score {
                best_score = score;
                best = Some(*n);
            }
        }

        if let Some(n) = best {
            if prev_cite == Some(n) {
                let pos = result.rfind(&format!("[{n}]")).unwrap_or(result.len());
                result.replace_range(pos..pos + format!("[{n}]").len(), "");
            }
            result.push_str(&format!("[{n}]"));
            prev_cite = Some(n);
        } else {
            prev_cite = None;
        }
    }
    result.push_str(&protected[last_end..]);

    for (idx, code) in placeholders.iter().enumerate() {
        result = result.replace(&format!("\x00CODE{idx}\x00"), code);
    }

    result
}

/// Strip out-of-range citation markers (`[N]` where N > evidence_len) that
/// the LLM fabricated, *before* [`auto_cite`] re-cites uncited sentences.
/// Out-of-range brackets become plain text (the number is kept, brackets
/// dropped) so no dangling `[99]` points at a nonexistent source.
pub fn sanitize_citations(answer: &str, evidence_len: usize) -> String {
    if evidence_len == 0 || answer.trim().is_empty() {
        return answer.to_string();
    }
    let cite_re = regex::Regex::new(r"\[(\d+)\]").unwrap();
    cite_re
        .replace_all(answer, |caps: &regex::Captures| {
            let n: usize = caps[1].parse().unwrap_or(0);
            if (1..=evidence_len).contains(&n) {
                caps[0].to_string() // valid: keep as-is
            } else {
                caps[1].to_string() // fabricated: drop the brackets, keep the text
            }
        })
        .into_owned()
}

/// Set-based variant of [`sanitize_citations`]: strips any `[N]` whose number is
/// not among the materials actually provided this turn (session-stable numbers).
pub fn sanitize_citations_set(answer: &str, valid: &std::collections::BTreeSet<usize>) -> String {
    if valid.is_empty() || answer.trim().is_empty() {
        return answer.to_string();
    }
    let cite_re = regex::Regex::new(r"\[(\d+)\]").unwrap();
    cite_re
        .replace_all(answer, |caps: &regex::Captures| {
            let n: usize = caps[1].parse().unwrap_or(0);
            if valid.contains(&n) {
                caps[0].to_string()
            } else {
                caps[1].to_string()
            }
        })
        .into_owned()
}

/// Jaccard overlap (`|A∩B| / |A∪B|`) between two strings' content words.
fn keyword_overlap(a: &str, b: &str) -> f64 {
    let jieba = crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner());
    let a_words: std::collections::HashSet<String> = jieba
        .cut(a, true)
        .iter()
        .map(|w| w.word.to_lowercase())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let b_words: std::collections::HashSet<String> = jieba
        .cut(b, true)
        .iter()
        .map(|w| w.word.to_lowercase())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    if a_words.is_empty() || b_words.is_empty() { return 0.0; }
    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.len() + b_words.len() - intersection;
    intersection as f64 / union.max(1) as f64
}
