mod image;
pub mod ocr;
pub mod office;
pub mod paddleocr;
pub mod pdf;
mod text;

use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;

/// Trait for extracting plain text from files.
pub trait Extractor: Send + Sync {
    fn extract(&self, path: &Path) -> Result<String>;
}

static TEXT_EXTRACTOR: LazyLock<text::TextExtractor> = LazyLock::new(text::TextExtractor::new);
static PDF_EXTRACTOR: LazyLock<pdf::PdfExtractor> = LazyLock::new(pdf::PdfExtractor::new);
static OFFICE_EXTRACTOR: LazyLock<office::OfficeExtractor> =
    LazyLock::new(office::OfficeExtractor::new);
static IMAGE_EXTRACTOR: LazyLock<image::ImageExtractor> = LazyLock::new(image::ImageExtractor::new);

/// Dispatch text extraction based on file extension.
/// `lang` is the OCR language for PDF/image extraction (from directory config
/// or global settings). When the extracted text is watermark/garbage, PDFs
/// fall through to OCR and image files always go through OCR.
pub fn extract_text(path: &Path, lang: &str) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        // Text formats
        "txt" | "md" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "log" | "py" | "rs" | "ts" | "js" | "html" | "css" | "sql" | "sh" | "bat"
        | "ps1" | "env" | "conf" | "properties" => TEXT_EXTRACTOR.extract(path),
        // Document formats
        "pdf" => PDF_EXTRACTOR.extract_with_lang(path, lang),
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => OFFICE_EXTRACTOR.extract(path),
        // Image formats (OCR placeholder)
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" => {
            IMAGE_EXTRACTOR.extract(path)
        }
        // Unknown format: try reading as plain text
        _ => match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => Ok(text),
            Ok(_) => Err(anyhow::anyhow!("empty file or binary content: {ext}")),
            Err(e) => Err(anyhow::anyhow!("unsupported format '{ext}' and cannot read as text: {e}")),
        },
    }
}

/// Classify a file extension into a high-level type string.
pub fn classify_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" => "image",
        "pdf" => "pdf",
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => "office",
        "txt" | "md" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini"
        | "cfg" | "log" | "py" | "rs" | "ts" | "js" | "html" | "css" | "sql"
        | "sh" | "bat" | "ps1" | "env" | "conf" | "properties" => "text",
        _ => "unknown",
    }
}

/// Return all supported file extensions (without leading dot).
pub fn get_supported_extensions() -> Vec<&'static str> {
    vec![
        "txt", "md", "csv", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "log", "py",
        "rs", "ts", "js", "html", "css", "sql", "sh", "bat", "ps1", "env", "conf", "properties",
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif",
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
            let result = extract_text(&path, "eng");
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
        let result = extract_text(&path, "eng");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
