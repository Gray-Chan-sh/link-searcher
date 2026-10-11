//! PDF watermark detection and removal via word geometry.
//!
//! A diagonal "signature" watermark (e.g. a law-firm stamp) that was baked into
//! a page image and later OCR'd into the text layer leaves a very specific
//! fingerprint: short tokens, each alone on its own tiny line, laid out along a
//! **negative-slope diagonal** and repeated at fixed offsets. Real body text is
//! (a) on horizontal lines, and (b) not a sequence of isolated 1–3 character
//! fragments. We exploit both facts.
//!
//! Detection is deliberately multi-signal so it never mistakes real content for
//! a watermark:
//!   1. candidate = a word alone in a very short line (`<= MAX_LINE_CHARS`);
//!   2. those candidates must form a diagonal chain (negative slope, consistent,
//!      spanning at least `MIN_SPAN` points) with at least `MIN_CHAIN` inliers;
//!   3. optionally reinforced by a corpus-wide fragment dictionary.
//!
//! Watermarked words are then removed from the reconstructed text. Callers must
//! never let removal empty out a document — see `pdf.rs` for the escalation and
//! fallback policy (watermark removal is a cleaning step, not a terminal one).

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::poppler::pdftotext_path;

/// A word whose whole line has at most this many non-whitespace characters is a
/// watermark *candidate* (real body lines are far longer).
const MAX_LINE_CHARS: usize = 4;
/// Minimum number of collinear candidates for a diagonal chain.
const MIN_CHAIN: usize = 4;
/// Allowed slope (dy/dx) range. Negative = text rising to the right. Body text is
/// horizontal (slope ≈ 0) and vertical CJK is steep (|slope| ≫ 2), both excluded.
const SLOPE_MIN: f64 = -2.0;
const SLOPE_MAX: f64 = -0.12;
/// The chain must span at least this many points along x (rejects tiny clusters).
const MIN_SPAN: f64 = 40.0;
/// Point-to-line tolerance, in PDF points.
const INLIER_TOL: f64 = 6.0;

// ── Data model ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WordBox {
    pub text: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Default)]
pub struct LineBox {
    pub words: Vec<WordBox>,
}

#[derive(Debug, Clone, Default)]
pub struct PageBoxes {
    pub lines: Vec<LineBox>,
    /// Page width/height in PDF points (0 when unknown). Used to normalize word
    /// positions so the same watermark can be compared across pages.
    pub width: f64,
    pub height: f64,
}

/// A text fragment that repeats at (approximately) the same normalized
/// position across pages. This is the signal a **plain text-layer** watermark
/// (one with no geometric anomaly) leaves behind; it can only be evaluated
/// with the whole document.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossPageCluster {
    pub text: String,
    /// Number of distinct pages the fragment appears on.
    pub pages: usize,
    /// Total pages examined.
    pub total_pages: usize,
    /// Mean normalized position.
    pub nx: f64,
    pub ny: f64,
}

/// Result of scanning one PDF for a geometric watermark.
#[derive(Debug, Clone, Default)]
pub struct WatermarkScan {
    pub found: bool,
    /// Text reconstructed from ALL words (`\n`-joined lines).
    pub raw_text: String,
    /// Text reconstructed with watermark words removed.
    pub cleaned_text: String,
    /// Non-whitespace characters attributed to watermark words.
    pub removed_chars: usize,
    /// Non-whitespace characters in `raw_text`.
    pub total_chars: usize,
    /// The removed fragments (for OCR post-filtering and dictionary growth).
    pub tokens: Vec<String>,
    /// Fragments repeated at the same normalized position across pages — the
    /// primary signal for a plain text-layer watermark (Phase 2, diagnostic).
    pub clusters: Vec<CrossPageCluster>,
}

impl WatermarkScan {
    /// Removed fraction of the text layer (0.0–1.0).
    pub fn removed_ratio(&self) -> f64 {
        if self.total_chars == 0 {
            0.0
        } else {
            self.removed_chars as f64 / self.total_chars as f64
        }
    }
}

// ── Scanning ───────────────────────────────────────────────────────────────

/// Scan a PDF for a geometric watermark. Returns `None` when poppler is
/// unavailable, the bbox pass fails, or the PDF has no extractable words —
/// in which case callers keep their existing behaviour unchanged.
pub fn scan(path: &Path) -> Option<WatermarkScan> {
    let bin = pdftotext_path()?;
    let xml = run_bbox_layout(bin, path)?;
    let pages = parse_bbox(&xml);
    if pages.is_empty() {
        return None;
    }
    let mut result = analyze(&pages);
    result.clusters = cross_page_clusters(&pages);
    Some(result)
}

/// Detect fragments that repeat at the same normalized position across pages.
///
/// This is the primary signal for a **plain text-layer watermark** (e.g. a
/// horizontal `上海小城律师事务所`): it carries no geometric anomaly, so only
/// document-wide repetition distinguishes it from body text. Detection only —
/// callers decide whether to remove anything.
///
/// A fragment qualifies when it appears on at least `max(2, ceil(0.6×pages))`
/// distinct pages and its normalized position stays within a small tolerance on
/// every occurrence.
pub fn cross_page_clusters(pages: &[PageBoxes]) -> Vec<CrossPageCluster> {
    use std::collections::HashMap;

    /// Normalized position tolerance (fraction of page width/height).
    const POS_TOL: f64 = 0.03;
    /// Minimum fraction of pages a fragment must appear on.
    const MIN_PAGE_RATIO: f64 = 0.6;
    /// Fragments longer than this are body lines, not watermarks.
    const MAX_CHARS: usize = 24;

    let total = pages.len();
    if total < 2 {
        return Vec::new();
    }

    let mut by_text: HashMap<String, Vec<(usize, f64, f64)>> = HashMap::new();
    for (page_index, page) in pages.iter().enumerate() {
        let (w, h) = (page.width, page.height);
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        for line in &page.lines {
            let text: String = line
                .words
                .iter()
                .map(|word| word.text.trim())
                .collect::<Vec<_>>()
                .join("");
            let text = text.trim();
            if text.is_empty() || text.chars().count() > MAX_CHARS {
                continue;
            }
            let Some(first) = line.words.first() else {
                continue;
            };
            by_text
                .entry(text.to_string())
                .or_default()
                .push((page_index, first.x / w, first.y / h));
        }
    }

    let min_pages = ((total as f64 * MIN_PAGE_RATIO).ceil() as usize).max(2);
    let mut out = Vec::new();
    for (text, occ) in by_text {
        let distinct: HashSet<usize> = occ.iter().map(|(p, _, _)| *p).collect();
        if distinct.len() < min_pages {
            continue;
        }
        let n = occ.len() as f64;
        let nx = occ.iter().map(|(_, x, _)| *x).sum::<f64>() / n;
        let ny = occ.iter().map(|(_, _, y)| *y).sum::<f64>() / n;
        let stable = occ
            .iter()
            .all(|(_, x, y)| (x - nx).abs() <= POS_TOL && (y - ny).abs() <= POS_TOL);
        if stable {
            out.push(CrossPageCluster {
                text,
                pages: distinct.len(),
                total_pages: total,
                nx,
                ny,
            });
        }
    }
    out.sort_by(|a, b| b.pages.cmp(&a.pages).then(a.text.cmp(&b.text)));
    out
}

/// Run `pdftotext -bbox-layout` and return its stdout.
///
/// `poppler::run_with_timeout` reads stdout only *after* the child exits, which
/// deadlocks when the output exceeds the OS pipe buffer (a 17-page bbox-layout
/// dump is ~360 KB ≫ 64 KB): the child blocks on write and never exits. Here a
/// reader thread drains stdout concurrently while we enforce the timeout.
fn run_bbox_layout(bin: &Path, path: &Path) -> Option<String> {
    let mut cmd = crate::process::new(bin);
    cmd.arg("-bbox-layout").arg(path).arg("-");
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut so = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut ok = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                ok = status.success();
                break;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let out = reader.join().ok()?;
    if out.is_empty() {
        return None;
    }
    // If the process was killed we still may have partial output; only accept
    // output from a clean exit.
    if !ok {
        return None;
    }
    Some(String::from_utf8_lossy(&out).to_string())
}

/// Analyze parsed word boxes using only geometry from this PDF. Pure function
/// (no I/O) — unit-testable and deliberately independent of other documents.
pub fn analyze(pages: &[PageBoxes]) -> WatermarkScan {
    analyze_with(pages)
}

fn analyze_with(pages: &[PageBoxes]) -> WatermarkScan {
    let mut tokens: Vec<String> = Vec::new();
    let mut token_seen: HashSet<String> = HashSet::new();
    let mut removed_chars = 0usize;
    let mut raw_lines: Vec<String> = Vec::new();
    let mut clean_lines: Vec<String> = Vec::new();
    let mut total_chars = 0usize;
    let mut found = false;

    for page in pages {
        // Flatten words with a stable index, remembering which line each is on.
        let mut flat: Vec<WordBox> = Vec::new();
        let mut line_of: Vec<usize> = Vec::new();
        let mut line_char_counts: Vec<usize> = Vec::new();
        for (li, line) in page.lines.iter().enumerate() {
            let mut chars = 0usize;
            for w in &line.words {
                chars += w.text.chars().filter(|c| !c.is_whitespace()).count();
                line_of.push(li);
                flat.push(w.clone());
            }
            line_char_counts.push(chars);
        }
        if flat.is_empty() {
            continue;
        }

        let removed = detect_watermark_words(&flat, &line_of, &line_char_counts);

        // Reconstruct raw / cleaned text (one line per bbox line).
        let mut raw_here = String::new();
        let mut clean_here = String::new();
        for (li, line) in page.lines.iter().enumerate() {
            let mut raw_words: Vec<&str> = Vec::new();
            let mut clean_words: Vec<&str> = Vec::new();
            for w in &line.words {
                raw_words.push(w.text.trim());
            }
            for (wi, w) in flat.iter().enumerate() {
                if line_of[wi] != li {
                    continue;
                }
                if !removed[wi] {
                    clean_words.push(w.text.trim());
                }
            }
            if !raw_words.is_empty() {
                if !raw_here.is_empty() {
                    raw_here.push('\n');
                }
                raw_here.push_str(&raw_words.join(" "));
            }
            if !clean_words.is_empty() {
                if !clean_here.is_empty() {
                    clean_here.push('\n');
                }
                clean_here.push_str(&clean_words.join(" "));
            }
        }

        // Account the removed characters.
        for (wi, w) in flat.iter().enumerate() {
            let c = w.text.chars().filter(|c| !c.is_whitespace()).count();
            if removed[wi] {
                removed_chars += c;
                let t = w.text.trim().to_string();
                if !t.is_empty() && token_seen.insert(t.clone()) {
                    tokens.push(t);
                }
            }
        }
        total_chars += raw_here.chars().filter(|c| !c.is_whitespace()).count();
        if removed_chars > 0 {
            found = true;
        }
        raw_lines.push(raw_here);
        clean_lines.push(clean_here);
    }

    WatermarkScan {
        found,
        raw_text: raw_lines.join("\n"),
        cleaned_text: clean_lines.join("\n"),
        removed_chars,
        total_chars,
        tokens,
        clusters: Vec::new(),
    }
}

/// Return `removed[i]` for each flattened word.
fn detect_watermark_words(
    flat: &[WordBox],
    line_of: &[usize],
    line_char_counts: &[usize],
) -> Vec<bool> {
    let mut removed = vec![false; flat.len()];

    // Candidates: words on a tiny line. This alone excludes almost all body
    // text, whose lines are long.
    let cands: Vec<usize> = (0..flat.len())
        .filter(|&i| line_char_counts[line_of[i]] <= MAX_LINE_CHARS)
        .collect();

    // 1) Geometric diagonal chains among the candidates.
    let chain_hits = ransac_diagonals(flat, &cands);
    for i in chain_hits {
        removed[i] = true;
    }

    removed
}

/// Iteratively extract negative-slope diagonal lines from the candidate set.
fn ransac_diagonals(flat: &[WordBox], cands: &[usize]) -> Vec<usize> {
    let mut remaining: Vec<usize> = cands.to_vec();
    let mut hits: HashSet<usize> = HashSet::new();

    loop {
        if remaining.len() < MIN_CHAIN {
            break;
        }
        let mut best: Option<Vec<usize>> = None;

        for a in 0..remaining.len() {
            for b in (a + 1)..remaining.len() {
                let ia = remaining[a];
                let ib = remaining[b];
                let dx = flat[ib].x - flat[ia].x;
                if dx.abs() < 1.0 {
                    continue;
                }
                let slope = (flat[ib].y - flat[ia].y) / dx;
                if !(SLOPE_MIN..=SLOPE_MAX).contains(&slope) {
                    continue;
                }
                let intercept = flat[ia].y - slope * flat[ia].x;
                let inliers: Vec<usize> = remaining
                    .iter()
                    .copied()
                    .filter(|&i| {
                        (flat[i].y - (slope * flat[i].x + intercept)).abs() <= INLIER_TOL
                    })
                    .collect();
                if inliers.len() < MIN_CHAIN {
                    continue;
                }
                let mut minx = f64::MAX;
                let mut maxx = f64::MIN;
                for &i in &inliers {
                    minx = minx.min(flat[i].x);
                    maxx = maxx.max(flat[i].x);
                }
                if maxx - minx < MIN_SPAN {
                    continue;
                }
                if best.as_ref().map_or(true, |v| inliers.len() > v.len()) {
                    best = Some(inliers);
                }
            }
        }

        match best {
            Some(inliers) if inliers.len() >= MIN_CHAIN => {
                let set: HashSet<usize> = inliers.iter().copied().collect();
                for &i in &inliers {
                    hits.insert(i);
                }
                remaining.retain(|i| !set.contains(i));
            }
            _ => break,
        }
    }

    hits.into_iter().collect()
}

/// Drop lines from OCR output that are exactly a detected watermark fragment.
///
/// Deliberately conservative: only **non-CJK** fragments (ASCII letters, digits,
/// punctuation such as `X`, `3`, `**`, `6-0`) are dropped. A fragment containing
/// a CJK ideograph is kept even if it was detected in the text layer, because the
/// same string (e.g. `上海`, `律师`, `陈骥`) is legitimate body text elsewhere and
/// OCR output carries no coordinates to disambiguate it.
pub fn filter_text(text: &str, tokens: &[String]) -> String {
    if tokens.is_empty() {
        return text.to_string();
    }
    let set: HashSet<&str> = tokens
        .iter()
        .filter(|t| t.chars().all(|c| c.is_ascii() || !c.is_alphabetic()))
        .map(|s| s.as_str())
        .collect();
    if set.is_empty() {
        return text.to_string();
    }
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.chars().count() <= 6 && set.contains(t))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── bbox parsing ───────────────────────────────────────────────────────────

fn parse_bbox(xml: &str) -> Vec<PageBoxes> {
    use regex::Regex;
    let Ok(page_re) = Regex::new(r#"(?s)<page\s+([^>]*)>(.*?)</page>"#) else {
        return Vec::new();
    };
    let Ok(attr_re) = Regex::new(r#"(width|height)="([-\d.eE+]+)""#) else {
        return Vec::new();
    };
    let Ok(line_re) = Regex::new(r"(?s)<line\b[^>]*>(.*?)</line>") else {
        return Vec::new();
    };
    let Ok(word_re) = Regex::new(
        r#"(?s)<word\s+xMin="([-\d.eE+]+)"\s+yMin="([-\d.eE+]+)"\s+xMax="([-\d.eE+]+)"\s+yMax="([-\d.eE+]+)">(.*?)</word>"#,
    ) else {
        return Vec::new();
    };

    let mut pages = Vec::new();
    for pm in page_re.captures_iter(xml) {
        let (mut width, mut height) = (0.0f64, 0.0f64);
        for am in attr_re.captures_iter(&pm[1]) {
            let v: f64 = am[2].parse().unwrap_or(0.0);
            match &am[1] {
                "width" => width = v,
                "height" => height = v,
                _ => {}
            }
        }
        let mut lines = Vec::new();
        for lm in line_re.captures_iter(&pm[2]) {
            let mut words = Vec::new();
            for wm in word_re.captures_iter(&lm[1]) {
                let x: f64 = wm[1].parse().unwrap_or(0.0);
                let y: f64 = wm[2].parse().unwrap_or(0.0);
                let text = xml_unescape(&wm[5]);
                if !text.trim().is_empty() {
                    words.push(WordBox { text, x, y });
                }
            }
            if !words.is_empty() {
                lines.push(LineBox { words });
            }
        }
        if !lines.is_empty() {
            pages.push(PageBoxes { lines, width, height });
        }
    }
    pages
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(words: &[(&str, f64, f64)]) -> LineBox {
        LineBox {
            words: words
                .iter()
                .map(|(t, x, y)| WordBox {
                    text: t.to_string(),
                    x: *x,
                    y: *y,
                })
                .collect(),
        }
    }

    /// A synthetic page mixing a diagonal watermark with horizontal body text.
    fn synthetic_page() -> PageBoxes {
        let mut lines = Vec::new();
        // Body: horizontal, long lines.
        lines.push(line(&[
            ("原告", 80.0, 100.0),
            ("常宏", 110.0, 100.0),
            ("男", 140.0, 100.0),
        ]));
        lines.push(line(&[
            ("联系地址", 80.0, 120.0),
            ("上海市浦东新区", 140.0, 120.0),
        ]));
        // Watermark: one short token per tiny line along a negative-slope diagonal.
        let wm = [
            ("X", 367.1, 175.1),
            ("3", 355.1, 182.0),
            ("4", 343.1, 189.0),
            ("**", 321.1, 195.3),
            ("*", 310.1, 208.0),
            ("1", 287.1, 221.3),
            ("32", 263.1, 228.2),
            ("陈骥", 211.2, 250.1),
        ];
        for (t, x, y) in wm {
            lines.push(line(&[(t, x, y)]));
        }
        PageBoxes { lines, width: 595.0, height: 842.0 }
    }

    #[test]
    fn detects_diagonal_watermark_and_keeps_body() {
        let pages = vec![synthetic_page()];
        let scan = analyze(&pages);
        assert!(scan.found, "diagonal watermark should be detected");
        assert!(
            scan.cleaned_text.contains("常宏"),
            "body text must survive: {}",
            scan.cleaned_text
        );
        assert!(
            scan.cleaned_text.contains("上海市浦东新区"),
            "body text must survive: {}",
            scan.cleaned_text
        );
        assert!(
            !scan.cleaned_text.contains("陈骥"),
            "watermark fragment should be removed: {}",
            scan.cleaned_text
        );
        assert!(scan.tokens.iter().any(|t| t == "陈骥"));
        assert!(scan.removed_ratio() > 0.0 && scan.removed_ratio() < 0.9);
    }

    #[test]
    fn horizontal_text_is_not_a_watermark() {
        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(line(&[
                ("这是", 80.0, 100.0 + i as f64 * 12.0),
                ("一行", 120.0, 100.0 + i as f64 * 12.0),
                ("正文", 160.0, 100.0 + i as f64 * 12.0),
            ]));
        }
        let scan = analyze(&[PageBoxes { lines, width: 595.0, height: 842.0 }]);
        assert!(!scan.found, "horizontal body text must not be flagged");
        assert_eq!(scan.removed_chars, 0);
    }

    #[test]
    fn short_body_lines_alone_are_not_enough_without_a_diagonal() {
        // Many tiny lines, but placed horizontally (same y) — e.g. a form.
        let mut lines = Vec::new();
        for i in 0..8 {
            lines.push(line(&[("1", 80.0 + i as f64 * 40.0, 300.0)]));
        }
        let scan = analyze(&[PageBoxes { lines, width: 595.0, height: 842.0 }]);
        assert!(
            !scan.found,
            "collinear-on-a-row tiny tokens must not be treated as a watermark"
        );
    }


    #[test]
    fn parses_poppler_bbox_xml() {
        let xml = r#"<doc><page width="595" height="842">
        <flow><block><line xMin="1" yMin="2" xMax="3" yMax="4">
        <word xMin="179.28" yMin="145.12" xMax="467.45" yMax="181.13">法庭审理笔录</word>
        </line></block></flow>
        <flow><block><line><word xMin="10" yMin="20" xMax="30" yMax="40">X</word></line></block></flow>
        </page></doc>"#;
        let pages = parse_bbox(xml);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines.len(), 2);
        assert_eq!(pages[0].lines[0].words[0].text, "法庭审理笔录");
        assert_eq!(pages[0].lines[1].words[0].text, "X");
    }

    #[test]
    fn filter_text_drops_ascii_watermark_lines_only() {
        let out = filter_text(
            "正文\nX\n3\n**\n陈骥\n更多正文",
            &["X".into(), "3".into(), "**".into(), "陈骥".into()],
        );
        assert!(out.contains("正文"));
        assert!(out.contains("更多正文"));
        assert!(!out.lines().any(|l| l.trim() == "X"));
        assert!(!out.lines().any(|l| l.trim() == "3"));
        assert!(!out.lines().any(|l| l.trim() == "**"));
        // CJK fragments are conservatively kept (could be real content).
        assert!(out.contains("陈骥"), "CJK fragment must not be dropped: {out}");
    }

    /// Phase 2: a fragment repeated at the same normalized position on most
    /// pages is detected; page-varying body text is not.
    #[test]
    fn cross_page_clusters_find_repeated_position() {
        // 5 pages: a horizontal watermark line at the same spot, plus unique body.
        let mut pages = Vec::new();
        for i in 0..5 {
            let mut lines = Vec::new();
            // watermark: horizontal, same normalized position every page
            lines.push(line(&[("上海小城律师事务所", 200.0, 400.0)]));
            // body: unique per page
            lines.push(line(&[("第", 80.0, 100.0), (&format!("{i}页正文内容"), 100.0, 100.0)]));
            pages.push(PageBoxes { lines, width: 595.0, height: 842.0 });
        }
        let clusters = cross_page_clusters(&pages);
        assert!(
            clusters.iter().any(|c| c.text == "上海小城律师事务所"),
            "repeated same-position watermark must be detected: {clusters:?}"
        );
        assert!(
            !clusters.iter().any(|c| c.text.contains("页正文内容")),
            "page-varying body text must not be flagged"
        );
    }

    /// A one-off repeated string at *different* positions on each page is not a
    /// watermark (position consistency is required).
    #[test]
    fn cross_page_clusters_require_position_consistency() {
        let mut pages = Vec::new();
        for i in 0..5 {
            let y = 100.0 + i as f64 * 120.0; // drifts far each page
            pages.push(PageBoxes {
                lines: vec![line(&[("律师事务所", 200.0, y)])],
                width: 595.0,
                height: 842.0,
            });
        }
        assert!(
            cross_page_clusters(&pages).is_empty(),
            "position drift must defeat the cluster"
        );
    }
}
