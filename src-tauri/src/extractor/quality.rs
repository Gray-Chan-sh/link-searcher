use std::collections::HashSet;
use std::sync::LazyLock;

#[derive(Debug, Clone, Default)]
pub struct ExtractMeta {
    pub ocr_used: bool,
    pub mean_confidence: Option<f32>,
    pub page_count: Option<u32>,
    pub image_dims: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityFlag {
    LowPrintable,
    HighFffd,
    LowConfidence,
    LowDensity,
    LowLexicon,
    Exhausted,
    MaxReextract,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QualityResult {
    pub score: f32,
    pub printable_ratio: f32,
    pub fffd_ratio: f32,
    pub confidence: Option<f32>,
    pub density_norm: f32,
    pub lexicon_hit_rate: f32,
    pub flags: Vec<QualityFlag>,
}

static CJK_LEXICON: LazyLock<HashSet<char>> = LazyLock::new(|| {
    include_str!("lexicons/top3500_cjk.txt")
        .lines()
        .filter_map(|l| l.trim().chars().next())
        .collect()
});

static EN_LEXICON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("lexicons/top3000_en.txt")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect()
});

pub fn lexicon_sizes() -> (usize, usize) {
    (CJK_LEXICON.len(), EN_LEXICON.len())
}

#[inline]
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

// sanitize_text already removes control chars before scoring, so printable metric's
// job is to catch genuinely non-text (replacement + private-use + control) content,
// not to whitelist one script's punctuation.
#[inline]
fn is_printable_allowed(c: char) -> bool {
    if c == '\u{FFFD}' || c.is_control() {
        return false;
    }
    if matches!(
        c,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
    ) {
        return false;
    }
    true
}

/// Compute lexicon hit rate: CJK chars from CJK set + whole Latin words in EN set.
fn compute_lexicon_hit_rate(text: &str) -> f32 {
    let non_ws_count = text.chars().filter(|c| !c.is_whitespace()).count();
    if non_ws_count == 0 {
        return 1.0;
    }

    let mut hit_count = 0usize;
    let mut latin_run: Vec<char> = Vec::new();

    let flush_latin = |run: &mut Vec<char>, hits: &mut usize| {
        if !run.is_empty() {
            let word: String = run.iter().collect::<String>().to_lowercase();
            if EN_LEXICON.contains(word.as_str()) {
                *hits += run.len();
            }
            run.clear();
        }
    };

    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            latin_run.push(c);
        } else {
            flush_latin(&mut latin_run, &mut hit_count);
            if is_cjk(c) && CJK_LEXICON.contains(&c) {
                hit_count += 1;
            }
        }
    }
    flush_latin(&mut latin_run, &mut hit_count);

    hit_count as f32 / non_ws_count as f32
}

pub fn compute_quality(text: &str, meta: &ExtractMeta, file_ext: &str) -> QualityResult {
    let total_chars = text.chars().count();
    if total_chars == 0 {
        return QualityResult {
            score: 0.0,
            printable_ratio: 0.0,
            fffd_ratio: 0.0,
            confidence: meta.mean_confidence,
            density_norm: 0.0,
            lexicon_hit_rate: 0.0,
            flags: vec![QualityFlag::LowPrintable, QualityFlag::LowDensity],
        };
    }

    let printable_count = text.chars().filter(|&c| is_printable_allowed(c)).count();
    let printable_ratio = printable_count as f32 / total_chars as f32;

    let fffd_count = text.chars().filter(|&c| c == '\u{FFFD}').count();
    let fffd_ratio = fffd_count as f32 / total_chars as f32;

    let confidence = meta.mean_confidence;
    let conf_component = if meta.ocr_used {
        confidence.unwrap_or(1.0)
    } else {
        1.0
    };

    let ext_lower = file_ext.to_lowercase();
    let density = if ext_lower == "pdf" {
        let pages = meta.page_count.unwrap_or(1) as f32;
        (total_chars as f32) / pages.max(1.0)
    } else if matches!(
        ext_lower.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif"
    ) {
        if let Some((w, h)) = meta.image_dims {
            let area = (w * h) as f32 / 10000.0;
            (total_chars as f32) / area.max(1.0)
        } else {
            (total_chars as f32) / 1.0
        }
    } else {
        (total_chars as f32) / 1.0
    };

    let density_norm = (density / 200.0).clamp(0.0, 1.0);
    let lexicon_hit_rate = compute_lexicon_hit_rate(text);

    let composite = (0.20 * printable_ratio
        + 0.15 * (1.0 - fffd_ratio)
        + 0.25 * conf_component
        + 0.15 * density_norm
        + 0.25 * lexicon_hit_rate)
        .clamp(0.0, 1.0);

    let mut flags = Vec::new();
    if printable_ratio < 0.7 {
        flags.push(QualityFlag::LowPrintable);
    }
    if fffd_ratio > 0.15 {
        flags.push(QualityFlag::HighFffd);
    }
    if meta.ocr_used && confidence.is_some_and(|c| c < 0.6) {
        flags.push(QualityFlag::LowConfidence);
    }
    if density_norm < 0.25 {
        flags.push(QualityFlag::LowDensity);
    }
    if lexicon_hit_rate < 0.4 {
        flags.push(QualityFlag::LowLexicon);
    }

    QualityResult {
        score: composite,
        printable_ratio,
        fffd_ratio,
        confidence,
        density_norm,
        lexicon_hit_rate,
        flags,
    }
}

/// Character Error Rate: normalized Levenshtein distance after stripping ASCII whitespace.
pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    let ref_clean: Vec<char> = reference
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let hyp_clean: Vec<char> = hypothesis
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();

    if ref_clean.is_empty() {
        return if hyp_clean.is_empty() { 0.0 } else { 1.0 };
    }

    let m = ref_clean.len();
    let n = hyp_clean.len();

    // Standard O(n*m) DP Levenshtein with two-row optimization.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(ref_clean[i - 1] != hyp_clean[j - 1]);
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n] as f64 / m as f64
}

pub fn flags_to_json(flags: &[QualityFlag]) -> String {
    serde_json::to_string(flags).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexicon_sizes() {
        let (cjk, en) = lexicon_sizes();
        assert!(
            cjk >= 3000 && cjk <= 4000,
            "cjk size expected ~3500, got {}",
            cjk
        );
        assert!(
            en >= 2500 && en <= 3500,
            "en size expected ~3000, got {}",
            en
        );
    }

    #[test]
    fn test_empty_string_quality() {
        let meta = ExtractMeta::default();
        let res = compute_quality("", &meta, "txt");
        assert_eq!(res.score, 0.0);
        assert_eq!(res.printable_ratio, 0.0);
        assert_eq!(res.density_norm, 0.0);
        assert!(res.flags.contains(&QualityFlag::LowPrintable));
        assert!(res.flags.contains(&QualityFlag::LowDensity));
    }

    #[test]
    fn test_clean_chinese_english_text() {
        let text = "Hello World 这是一个测试文本合同编号 1234567890 这是一个比较长的测试文本样例包含各种常用字符";
        let meta = ExtractMeta {
            ocr_used: false,
            mean_confidence: None,
            page_count: None,
            image_dims: None,
        };
        let res = compute_quality(text, &meta, "txt");
        assert!(
            res.score >= 0.75,
            "clean text score should be >= 0.75, got {}",
            res.score
        );
        assert!(
            res.flags.is_empty(),
            "flags should be empty, got {:?}",
            res.flags
        );
    }

    #[test]
    fn test_heavy_fffd_text() {
        let text = "xx \u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD} yy";
        let meta = ExtractMeta::default();
        let res = compute_quality(text, &meta, "txt");
        assert!(
            res.score < 0.5,
            "heavy fffd score should be < 0.5, got {}",
            res.score
        );
        assert!(
            res.flags.contains(&QualityFlag::HighFffd),
            "flags should contain HighFffd, got {:?}",
            res.flags
        );
    }

    #[test]
    fn test_ocr_confidence_flags() {
        let text = "Hello World 123";
        let meta_high = ExtractMeta {
            ocr_used: true,
            mean_confidence: Some(0.9),
            page_count: None,
            image_dims: None,
        };
        let res_high = compute_quality(text, &meta_high, "png");
        assert!(
            !res_high.flags.contains(&QualityFlag::LowConfidence),
            "high confidence should not set LowConfidence flag"
        );

        let meta_low = ExtractMeta {
            ocr_used: true,
            mean_confidence: Some(0.3),
            page_count: None,
            image_dims: None,
        };
        let res_low = compute_quality(text, &meta_low, "png");
        assert!(
            res_low.flags.contains(&QualityFlag::LowConfidence),
            "low confidence should set LowConfidence flag"
        );
    }

    #[test]
    fn test_pdf_page_count_no_panic() {
        let text = "Test PDF document text content";
        let meta_some = ExtractMeta {
            ocr_used: false,
            mean_confidence: None,
            page_count: Some(1),
            image_dims: None,
        };
        let res_some = compute_quality(text, &meta_some, "pdf");
        assert!(res_some.score > 0.0);

        let meta_none = ExtractMeta {
            ocr_used: false,
            mean_confidence: None,
            page_count: None,
            image_dims: None,
        };
        let res_none = compute_quality(text, &meta_none, "pdf");
        assert!(res_none.score > 0.0);
    }

    #[test]
    fn test_cer_identical() {
        assert_eq!(cer("Hello OCR 123", "Hello OCR 123"), 0.0);
        assert_eq!(cer("测试文本 ABC", "测试 文本 ABC"), 0.0);
    }

    #[test]
    fn test_cer_disjoint() {
        assert_eq!(cer("ABC", "XYZ"), 1.0);
    }

    #[test]
    fn test_cer_empty_ref() {
        assert_eq!(cer("", ""), 0.0);
        assert_eq!(cer("", "non empty"), 1.0);
    }

    #[test]
    fn test_flags_to_json() {
        let flags = vec![QualityFlag::LowPrintable, QualityFlag::HighFffd];
        let json = flags_to_json(&flags);
        assert!(json.contains("low_printable"));
        assert!(json.contains("high_fffd"));
    }

    #[test]
    fn test_chinese_full_width_punctuation_printable() {
        let text = "浙江传化江南大地发展有限公司 变更记录共24条数据：【新增】项目！；（测试）《规范》、，？";
        let meta = ExtractMeta::default();
        let res = compute_quality(text, &meta, "docx");
        assert!(
            res.printable_ratio >= 0.95,
            "chinese text with full-width punctuation should have high printable_ratio, got {}",
            res.printable_ratio
        );
        assert!(
            !res.flags.contains(&QualityFlag::LowPrintable),
            "should not set LowPrintable flag for legitimate Chinese text with full-width punctuation"
        );
    }

    #[test]
    fn test_private_use_area_non_printable() {
        let pua_text = "\u{E000}\u{E001}\u{F8FF}\u{F0000}\u{100000}";
        let meta = ExtractMeta::default();
        let res = compute_quality(pua_text, &meta, "txt");
        assert_eq!(res.printable_ratio, 0.0);
        assert!(res.flags.contains(&QualityFlag::LowPrintable));
    }

    #[test]
    fn test_is_printable_allowed_blocks_replacement_control_pua() {
        assert!(!is_printable_allowed('\u{FFFD}'), "replacement char");
        assert!(!is_printable_allowed('\u{0000}'), "null control");
        assert!(!is_printable_allowed('\u{0007}'), "BEL control");
        assert!(!is_printable_allowed('\u{001B}'), "ESC control");
        assert!(!is_printable_allowed('\u{E000}'), "PUA BMP start");
        assert!(!is_printable_allowed('\u{F8FF}'), "PUA BMP end");
        assert!(!is_printable_allowed('\u{F0000}'), "PUA Plane 15 start");
        assert!(!is_printable_allowed('\u{FFFFD}'), "PUA Plane 15 end");
        assert!(!is_printable_allowed('\u{100000}'), "PUA Plane 16 start");
        assert!(!is_printable_allowed('\u{10FFFD}'), "PUA Plane 16 end");
    }

    #[test]
    fn test_is_printable_allowed_accepts_all_scripts() {
        assert!(is_printable_allowed('中'));
        assert!(is_printable_allowed('，'));
        assert!(is_printable_allowed('：'));
        assert!(is_printable_allowed('；'));
        assert!(is_printable_allowed('！'));
        assert!(is_printable_allowed('？'));
        assert!(is_printable_allowed('（'));
        assert!(is_printable_allowed('）'));
        assert!(is_printable_allowed('【'));
        assert!(is_printable_allowed('】'));
        assert!(is_printable_allowed('《'));
        assert!(is_printable_allowed('》'));
        assert!(is_printable_allowed('、'));
        assert!(is_printable_allowed('a'));
        assert!(is_printable_allowed('Z'));
        assert!(is_printable_allowed('0'));
        assert!(is_printable_allowed(' '));
        assert!(!is_printable_allowed('\n'), "newline is a control char");
        assert!(is_printable_allowed('Я'));
        assert!(is_printable_allowed('α'));
        assert!(is_printable_allowed('→'));
    }
}
