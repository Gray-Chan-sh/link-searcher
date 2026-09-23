//! PDF text-layer quality heuristics: garbled/sparse/implausible detection and
//! watermark/repetition analysis.

use std::collections::HashSet;
/// Detect if extracted PDF text is garbled / corrupted.
/// Returns true if >30% of characters are suspicious.
pub fn is_garbled_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let total = text.chars().count() as f64;
    // >30% suspicious chars (replacement char, stray control chars)
    let suspicious = text
        .chars()
        .filter(|c| {
            *c == '\u{FFFD}'
                || (c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
        })
        .count() as f64;
    if suspicious / total > 0.3 {
        return true;
    }
    // <5% non-whitespace → lopdf parsed only spaces (e.g. Quartz PDFs)
    let non_blank = text.chars().filter(|c| !c.is_whitespace()).count() as f64;
    non_blank / total < 0.05
}

/// Normalize page text for watermark comparison: strip variable parts
/// (hex codes ≥30 chars, dates, URLs, whitespace) leaving only stable text.
pub(super) fn normalize_for_watermark(text: &str) -> String {
    let mut out = String::with_capacity(300);
    let chars: Vec<char> = text.chars().take(300).collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Skip URLs
        if c == 'h' && i + 4 <= len && chars[i..i + 4] == ['h', 't', 't', 'p'] {
            while i < len && !chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        // Skip hex blobs (≥30 consecutive hex chars → verification codes, UUIDs)
        if c.is_ascii_hexdigit() {
            let start = i;
            while i < len && chars[i].is_ascii_hexdigit() {
                i += 1;
            }
            if i - start >= 30 {
                continue;
            }
            i = start;
        }
        // Skip date/time patterns: YYYY-MM-DD HH:MM:SS or YYYY.MM.DD
        if c.is_ascii_digit() {
            let start = i;
            while i < len
                && (chars[i].is_ascii_digit()
                    || chars[i] == '-'
                    || chars[i] == '.'
                    || chars[i] == ':')
            {
                i += 1;
            }
            let slice: String = chars[start..i].iter().collect();
            if slice.contains('-') || slice.contains(':')
                || (slice.contains('.') && slice.len() >= 8)
            {
                continue;
            }
            // Short pure-digit sequences (<5 chars) are PDF coordinate
            // garbage, not meaningful content. Skip them.
            if slice.chars().all(|c| c.is_ascii_digit()) && slice.len() <= 4 {
                continue;
            }
            i = start;
        }
        out.push(c);
        i += 1;
    }
    // Strip trailing page numbers like "1.", "12."
    while out.ends_with('.') {
        out.pop();
        while out.chars().next_back().is_some_and(|c| c.is_ascii_digit()) {
            out.pop();
        }
    }
    out
}

/// Detect if text across pages looks like a repeated watermark.
/// Normalizes each page's prefix (strips hex codes, dates, URLs, whitespace)
/// then checks whether adjacent normalized prefixes are identical. Returns
/// true if >80% of consecutive page pairs match.
pub fn is_watermark_text(pages: &[String]) -> bool {
    if pages.len() < 2 {
        return false;
    }
    let normalized: Vec<String> = pages
        .iter()
        .map(|p| normalize_for_watermark(p))
        .filter(|n| n.chars().count() > 2)
        .collect();
    if normalized.len() < 2 {
        return false;
    }
    let mut same = 0usize;
    for i in 1..normalized.len() {
        if normalized[i - 1] == normalized[i] {
            same += 1;
        }
    }
    let total = normalized.len() - 1;
    total > 0 && (same as f64 / total as f64) > 0.8
}

/// Returns true if text is highly repetitive — e.g. a watermark repeated
/// verbatim across lines/pages, which character-set Jaccard misses when
/// pages vary or only one page exists.
pub(super) fn is_repetitive(text: &str) -> bool {
    if text.len() < 100 {
        return false;
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let distinct: HashSet<&str> = lines.iter().copied().collect();
    (lines.len() - distinct.len()) as f64 / lines.len() as f64 > 0.6
}

/// True when the text layer is too sparse to be real document body text.
/// A real body page has far more than 50 non-whitespace chars; 50/page is
/// the conservative threshold.
pub fn is_sparse_text_layer(text: &str, page_count: usize) -> bool {
    if page_count == 0 {
        return false;
    }
    let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
    non_ws < 50 * page_count
}

/// True when the extracted text layer is implausible — garbage/binary junk or
/// physically impossible character density — and must NOT be trusted as clean.
///
/// Two independent signals, either triggers:
///   1. Absurd density: >20 000 non-whitespace chars per page (impossible for
///      real text; scanned PDFs yield 1–3k chars/page via OCR).
///   2. Low printable ratio: reuses `compute_quality` and checks
///      `QualityFlag::LowPrintable` only. We deliberately ignore `LowLexicon`
///      because legal docs legitimately contain rare characters, names, and
///      jargon — triggering OCR on those would over-trigger on healthy files.
pub fn is_implausible_text_layer(text: &str, page_count: usize) -> bool {
    if text.is_empty() || page_count == 0 {
        return false;
    }
    let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
    if non_ws / page_count > 20_000 {
        return true;
    }
    let meta = crate::extractor::quality::ExtractMeta {
        page_count: Some(page_count as u32),
        ..Default::default()
    };
    let quality = crate::extractor::quality::compute_quality(text, &meta, "pdf");
        quality
            .flags
            .contains(&crate::extractor::quality::QualityFlag::LowPrintable)
}

/// True when any page font has no Name-valued `/Encoding` but does have a
/// `/ToUnicode` entry.
///
/// `Document::extract_text` then hits `Font::get_font_encoding`'s `DictKey`
/// fallback ("Could not parse the encoding ... Trying to retrieve ToUnicode"),
/// which silently DROPS characters while the output still looks clean — so the
/// `pdftotext` recovery below never fires. Common in Chinese PDFs (macOS
/// Quartz / WPS subset TrueType fonts such as `AAAAAC+STSongti-SC-Regular`).
pub(super) fn has_unparseable_font_encoding(doc: &lopdf::Document) -> bool {
    for (_, page_id) in doc.get_pages() {
        let Ok(fonts) = doc.get_page_fonts(page_id) else {
            continue;
        };
        for font in fonts.values() {
            let encoding_is_name = font.get(b"Encoding").and_then(lopdf::Object::as_name_str).is_ok();
            if !encoding_is_name && font.has(b"ToUnicode") {
                return true;
            }
        }
    }
    false
}

pub fn prefer_recovered_text(lopdf_text: &str, recovered: &str, page_count: usize) -> bool {
    !is_garbled_text(recovered)
        && !is_implausible_text_layer(recovered, page_count)
        && !is_sparse_text_layer(recovered, page_count)
        && recovered.chars().count() > lopdf_text.chars().count()
}
