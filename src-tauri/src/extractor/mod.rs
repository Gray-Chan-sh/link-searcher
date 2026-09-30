pub mod apple_vision;
pub mod archive;
pub mod audio;
mod image;
pub mod ocr;
pub mod office;
pub mod paddleocr;
pub mod pdf;
mod preprocess;
pub mod quality;
pub mod windows_ocr;
mod text;

use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

use anyhow::Result;

pub fn sanitize_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    if text.contains('\0') {
        return String::new();
    }

    let mut cleaned: String = text
        .nfkc()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
        .collect();

    let replacement_count = cleaned.chars().filter(|&c| c == '\u{FFFD}').count();
    let total_chars = cleaned.chars().count();

    if total_chars > 0 && (replacement_count as f64 / total_chars as f64) > 0.15 {
        cleaned = cleaned.replace('\u{FFFD}', " ");
    }

    cleaned = squeeze_cjk_spaces(&cleaned);
    cleaned.trim().to_string()
}

/// Whether `c` is an ideograph/kana that is never written with intra-word spaces.
/// Kana are included because Japanese scans show the same one-glyph-per-run
/// layout; Hangul is excluded on purpose (Korean *does* space its words).
fn is_spaceless_script(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
            | '\u{3400}'..='\u{4DBF}'   // CJK Extension A
            | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
            | '\u{3040}'..='\u{309F}'   // Hiragana
            | '\u{30A0}'..='\u{30FF}'   // Katakana
            | '\u{FF00}'..='\u{FFEF}'   // Fullwidth forms (，。（）…)
    )
}

/// Punctuation that never needs a surrounding space when it abuts an ideograph.
/// Scans of Chinese documents routinely carry **half-width** ASCII punctuation
/// (`上海机场 ( 集团 ) 有限公`) even though the prose is Chinese; leaving those
/// spaces in place splits the surrounding words just as badly as ideograph
/// spaces do. Latin letters/digits are deliberately **not** included: a space
/// next to them is a genuine word boundary (`hello world`, `编号 12345`).
fn is_cjk_adjacent_punct(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | ':' | '!'
            | '?' | '/' | '\\' | '"' | '\'' | '<' | '>' | '|' | '~'
            | '-' | '_' | '+' | '=' | '*' | '&' | '%' | '#' | '@' | '$'
    )
}

/// Characters that may sit directly against an ideograph with no space.
fn is_glue_safe(c: char) -> bool {
    is_spaceless_script(c) || is_cjk_adjacent_punct(c)
}

/// Remove spaces that sit **between two spaceless-script characters**.
///
/// Copier-scanner PDFs (and IntSig/Foxit re-exports) store text one glyph per
/// run, so poppler faithfully emits `"上 海 机 场"`. That breaks the index two
/// ways: jieba only recognises words across contiguous ideographs, so the text
/// tokenises into single characters — which both the query builder and
/// `extract_retrieval_keywords` then **discard** (`chars().count() < 2`) — and
/// the embeddings are computed over the spaced text, costing ~0.03–0.05 cosine
/// against a normal query. Measured on a real library: 1838 files were ≥30%
/// inter-character spaces, 1604 of them ≥70%, and a scanned exhibit volume
/// containing the literal phrase "证据卷七十七：隐瞒境外存款952余万" was
/// **unreachable by any query**.
///
/// Only spaces *between two* such characters are dropped, so a space adjacent to
/// Latin/digits/newlines survives: `"hello world 上海 机场"` → `"hello world 上海机场"`.
///
/// A space is dropped when both neighbours are [`is_glue_safe`] **and at least
/// one of them is an ideograph/kana** — the latter guard keeps `"( 1 )"` and
/// `"a - b"` (pure ASCII punctuation sequences) intact.
pub fn squeeze_cjk_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' || c == '\u{3000}' {
            // Walk the whole run of spaces; keep it unless both neighbours are
            // glue-safe *and* at least one is ideographic/kana.
            let start = i;
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\u{3000}') {
                i += 1;
            }
            let before = start.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i).copied();
            let drop = match (before, after) {
                (Some(b), Some(a)) => {
                    is_glue_safe(b)
                        && is_glue_safe(a)
                        && (is_spaceless_script(b) || is_spaceless_script(a))
                }
                _ => false,
            };
            if !drop {
                for &s in &chars[start..i] {
                    out.push(s);
                }
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Trait for extracting plain text from files.
pub trait Extractor: Send + Sync {
    fn extract(&self, path: &Path) -> Result<String>;
}

static TEXT_EXTRACTOR: LazyLock<text::TextExtractor> = LazyLock::new(text::TextExtractor::new);
static PDF_EXTRACTOR: LazyLock<pdf::PdfExtractor> = LazyLock::new(pdf::PdfExtractor::new);
static OFFICE_EXTRACTOR: LazyLock<office::OfficeExtractor> =
    LazyLock::new(office::OfficeExtractor::new);
static ARCHIVE_EXTRACTOR: LazyLock<archive::ArchiveExtractor> = LazyLock::new(archive::ArchiveExtractor::new);
static AUDIO_EXTRACTOR: LazyLock<audio::AudioExtractor> = LazyLock::new(audio::AudioExtractor::new);

/// Dispatch text extraction based on file extension, returning both the
/// extracted text and quality metadata. [`extract_text`] is a thin wrapper
/// that discards the metadata.
///
/// `lang` is the OCR language for PDF/image extraction (from directory config
/// or global settings). When the extracted text is watermark/garbage, PDFs
/// fall through to OCR and image files always go through OCR. An unusable or
/// missing configured OCR engine resolves via [`ocr::preferred_engine`] to the
/// best engine available on this platform.
pub fn extract_text_with_meta(
    path: &Path,
    lang: &str,
    engine: Option<ocr::OcrEngineType>,
) -> Result<(String, quality::ExtractMeta)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let (raw_res, meta) = match ext.as_str() {
        // Text formats
        "txt" | "md" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "log" | "py" | "rs" | "ts" | "js" | "html" | "css" | "sql" | "sh" | "bat"
        | "ps1" | "env" | "conf" | "properties" => {
            (TEXT_EXTRACTOR.extract(path), quality::ExtractMeta::default())
        }
        // Document formats
        "pdf" => {
            let (text, meta) = PDF_EXTRACTOR.extract_with_meta(path, lang, engine)?;
            (Ok(text), meta)
        }
        "doc" | "docx" | "docm" | "xls" | "xlsx" | "xlsm" | "xlsb" | "ppt" | "pptx"
        | "pptm" | "ppsm" | "ppsx" | "pps" | "pot" | "odt" | "ods" | "odp" | "rtf"
        | "epub" => {
            (OFFICE_EXTRACTOR.extract(path), quality::ExtractMeta::default())
        }
        // Image formats (OCR)
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" => {
            let e = ocr::preferred_engine(engine);
            match ocr::ocr_image_with_stats(path, &e, lang) {
                Ok((text, stats)) => {
                    let dims = ::image::image_dimensions(path).ok();
                    let meta = quality::ExtractMeta {
                        ocr_used: true,
                        mean_confidence: stats.mean_confidence,
                        page_count: None,
                        image_dims: dims,
                        pre_sanitize_fffd_ratio: None,
                        file_size: std::fs::metadata(path).ok().map(|m| m.len()),
                    };
                    (Ok(text), meta)
                }
                Err(e) => (Err(e), quality::ExtractMeta::default()),
            }
        }
        // Archives
        "zip" | "tar" | "tgz" | "tbz2" | "txz" | "gz" | "bz2" | "xz" => {
            (ARCHIVE_EXTRACTOR.extract_archive(path, lang), quality::ExtractMeta::default())
        }
        // Audio
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "wma" => {
            (AUDIO_EXTRACTOR.extract_audio(path), quality::ExtractMeta::default())
        }
        // Unknown format: try reading as plain text (capped — a 50GB video
        // or image file must not be read fully into memory).
        _ => {
            let mut buf = Vec::new();
            let raw_res = std::fs::File::open(path)
                .and_then(|f| f.take(10 * 1024 * 1024).read_to_end(&mut buf))
                .map_err(|e| anyhow::anyhow!("unsupported format '{ext}' and cannot read as text: {e}"));
            let result = raw_res.and_then(|_| match std::str::from_utf8(&buf) {
                Ok(text) if !text.trim().is_empty() => Ok(text.to_string()),
                Ok(_) => Err(anyhow::anyhow!("empty file or binary content: {ext}")),
                Err(_) => Err(anyhow::anyhow!("unsupported format '{ext}': binary content, cannot read as text")),
            });
            (result, quality::ExtractMeta::default())
        }
    };

    let pre_sanitize_fffd_ratio = raw_res.as_ref().ok().map(|t| {
        let total = t.chars().count();
        if total > 0 {
            t.chars().filter(|&c| c == '\u{FFFD}').count() as f32 / total as f32
        } else {
            0.0
        }
    });

    let mut meta = meta;
    meta.pre_sanitize_fffd_ratio = pre_sanitize_fffd_ratio;

    raw_res.map(|t| sanitize_text(&t)).map(|t| (t, meta))
}

/// Dispatch text extraction based on file extension.
/// Convenience wrapper that discards quality metadata.
pub fn extract_text(path: &Path, lang: &str, engine: Option<ocr::OcrEngineType>) -> Result<String> {
    extract_text_with_meta(path, lang, engine).map(|(text, _meta)| text)
}

/// Classify a file extension into a high-level type string.
pub fn classify_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" => "image",
        "pdf" => "pdf",
        "doc" | "docx" | "docm" | "xls" | "xlsx" | "xlsm" | "xlsb" | "ppt" | "pptx"
        | "pptm" | "ppsm" | "ppsx" | "pps" | "pot" | "odt" | "ods" | "odp" | "rtf"
        | "epub" => "office",
        "txt" | "md" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini"
        | "cfg" | "log" | "py" | "rs" | "ts" | "js" | "html" | "css" | "sql"
        |         "sh" | "bat" | "ps1" | "env" | "conf" | "properties" => "text",
        "zip" | "tar" | "tgz" | "tbz2" | "txz" | "gz" | "bz2" | "xz" => "archive",
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "wma" => "audio",
        _ => "unknown",
    }
}

/// Return all supported file extensions (without leading dot).
pub fn get_supported_extensions() -> Vec<&'static str> {
    vec![
        "txt", "md", "csv", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "log", "py",
        "rs", "ts", "js", "html", "css", "sql", "sh", "bat", "ps1", "env", "conf", "properties",
        "pdf", "doc", "docx", "docm", "xls", "xlsx", "xlsm", "xlsb", "ppt", "pptx", "pptm",
        "ppsm", "ppsx", "pps", "pot", "odt", "ods", "odp", "rtf", "epub",
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif",
        "zip", "tar", "tgz", "tbz2", "txz", "gz", "bz2", "xz",
        "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma",
    ]
}

/// Check if the given path has a supported file extension.
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| get_supported_extensions().contains(&e.to_lowercase().as_str()))
}

/// Helper for testing — extract text using a specific extractor.
pub fn extract_text_with_extractor(path: &Path, extractor: &dyn Extractor) -> Result<String> {
    extractor.extract(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squeeze_cjk_spaces_joins_ideographs() {
        // 真实形态：复印机扫描件一个字一个 text run
        assert_eq!(squeeze_cjk_spaces("上 海 机 场"), "上海机场");
        assert_eq!(squeeze_cjk_spaces("吴 建 融 违 反 党 的 纪 律"), "吴建融违反党的纪律");
        assert_eq!(
            squeeze_cjk_spaces("上 海 机 场 ( 集 团 ) 有 限 公 司"),
            "上海机场(集团)有限公司",
            "真实语料用半角括号，与汉字之间的空格同样要去掉"
        );
        assert_eq!(squeeze_cjk_spaces("第 九 纪 检 监 察 室"), "第九纪检监察室");
        // 日文假名同理
        assert_eq!(squeeze_cjk_spaces("これ は テスト"), "これはテスト");
    }

    #[test]
    fn squeeze_cjk_spaces_keeps_pure_ascii_phrases_intact() {
        // 纯 ASCII 的括号/连字符序列不能被动，否则 "( 1 )" 会粘成 "(1)"
        assert_eq!(squeeze_cjk_spaces("( 1 )"), "( 1 )");
        assert_eq!(squeeze_cjk_spaces("a - b"), "a - b");
        assert_eq!(squeeze_cjk_spaces("( hello )"), "( hello )");
    }

    #[test]
    fn squeeze_cjk_spaces_keeps_latin_words_apart() {
        // 英文/数字之间的空格必须保留，否则会粘成一个词
        assert_eq!(squeeze_cjk_spaces("hello world"), "hello world");
        assert_eq!(squeeze_cjk_spaces("foo bar baz"), "foo bar baz");
        // 中英混排：只删"汉字|汉字"之间的空格
        assert_eq!(squeeze_cjk_spaces("hello world 上海 机场"), "hello world 上海机场");
        assert_eq!(squeeze_cjk_spaces("上海 机场 hello world"), "上海机场 hello world");
        assert_eq!(squeeze_cjk_spaces("合同编号 12345 号"), "合同编号 12345 号");
    }

    #[test]
    fn squeeze_cjk_spaces_keeps_newlines() {
        // 换行是结构，不能被吃掉
        assert_eq!(squeeze_cjk_spaces("上 海\n机 场"), "上海\n机场");
        assert_eq!(squeeze_cjk_spaces("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn squeeze_cjk_spaces_handles_edge_cases() {
        assert_eq!(squeeze_cjk_spaces(""), "");
        assert_eq!(squeeze_cjk_spaces(" "), " ");
        assert_eq!(squeeze_cjk_spaces("   "), "   ");
        assert_eq!(squeeze_cjk_spaces("上  "), "上  ", "尾部空格前面是汉字、后面没有字 -> 保留");
        assert_eq!(squeeze_cjk_spaces("  上"), "  上", "开头同理保留");
        assert_eq!(squeeze_cjk_spaces("上  海"), "上海", "连续多个空格一并去掉");
        assert_eq!(squeeze_cjk_spaces("上\u{3000}海"), "上海", "全角空格同样处理");
    }

    #[test]
    fn test_get_supported_extensions() {
        let exts = get_supported_extensions();
        assert!(exts.contains(&"txt"));
        assert!(exts.contains(&"pdf"));
        assert!(exts.contains(&"docx"));
        assert!(exts.contains(&"png"));
        assert!(exts.contains(&"rs"));
        assert!(!exts.contains(&"exe"));
    }

    #[test]
    fn test_is_supported() {
        assert!(is_supported(Path::new("file.txt")));
        assert!(is_supported(Path::new("file.pdf")));
        assert!(is_supported(Path::new("file.docx")));
        assert!(is_supported(Path::new("file.PNG")));
        assert!(!is_supported(Path::new("file.xyz")));
        assert!(!is_supported(Path::new("Makefile")));
    }

    #[test]
    fn test_dispatch_routes_text_files() {
        let dir = std::env::temp_dir().join("extractor_test_dispatch_text");
        let _ = std::fs::create_dir_all(&dir);

        for ext in ["txt", "md", "csv", "json", "rs", "py"] {
            let path = dir.join(format!("test.{}", ext));
            std::fs::write(&path, "hello").unwrap();
            let result = extract_text(&path, "eng", None);
            assert!(
                result.is_ok(),
                "{} should extract ok: {:?}",
                ext,
                result.err()
            );
            assert_eq!(result.unwrap(), "hello");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_dispatch_unsupported_fallback_text() {
        let dir = std::env::temp_dir().join("extractor_test_fallback");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("readme.me");
        std::fs::write(&path, "hello world").unwrap();
        let result = extract_text(&path, "eng", None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
