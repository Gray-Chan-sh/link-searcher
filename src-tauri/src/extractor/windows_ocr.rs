//! Windows OCR engine (Windows 10+).
//!
//! Uses `Windows.Media.Ocr.OcrEngine` via the official `windows` crate.
//! Calls block synchronously with `.get()` — the same pattern as the
//! Apple Vision `performRequests_error:` path.

use std::path::Path;

/// Map application language codes to BCP-47 tags for Windows OCR.
#[cfg(target_os = "windows")]
const WIN_LANG_MAP: &[(&str, &[&str])] = &[
    ("eng",     &["en-US"]),
    ("chi_sim", &["zh-Hans", "zh-CN"]),
    ("jpn",     &["ja-JP", "ja"]),
    ("kor",     &["ko-KR", "ko"]),
];

/// Windows: report whether the OS OCR engine is usable at all, and which
/// app language codes (eng/chi_sim/jpn/kor) have a matching installed
/// language pack. Non-Windows: always unavailable.
#[cfg(target_os = "windows")]
pub fn availability() -> (bool, Vec<String>) {
    use windows::Globalization::Language;
    use windows::Media::Ocr::OcrEngine;

    let fallback_ok = OcrEngine::TryCreateFromUserProfileLanguages().is_ok();
    let mut usable = Vec::new();
    for (app_code, tags) in WIN_LANG_MAP {
        let ok = tags.iter().any(|tag| {
            Language::CreateLanguage(&windows::core::HSTRING::from(*tag))
                .ok()
                .and_then(|l| OcrEngine::TryCreateFromLanguage(&l).ok())
                .is_some()
        });
        if ok {
            usable.push(app_code.to_string());
        }
    }
    (fallback_ok || !usable.is_empty(), usable)
}

#[cfg(not(target_os = "windows"))]
pub fn availability() -> (bool, Vec<String>) {
    (false, Vec::new())
}

#[cfg(target_os = "windows")]
fn map_lang(lang: &str) -> &[&str] {
    WIN_LANG_MAP
        .iter()
        .find(|(k, _)| *k == lang)
        .map(|(_, v)| *v)
        .unwrap_or(&["en-US"])
}

#[cfg(target_os = "windows")]
fn create_engine(lang: &str) -> Result<windows::Media::Ocr::OcrEngine, String> {
    use windows::core::HSTRING;
    use windows::Globalization::Language;
    use windows::Media::Ocr::OcrEngine;

    for tag in map_lang(lang) {
        let htag = HSTRING::from(*tag);
        if let Ok(language) = Language::CreateLanguage(&htag) {
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                return Ok(engine);
            }
        }
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| format!(
            "No Windows OCR language pack found for {:?}. \
             Install one in Settings → Time & Language → Language → Optional features → OCR",
            lang,
        ))
}

#[cfg(target_os = "windows")]
fn recognize_from_path_inner(path: &Path, lang: &str) -> Result<(String, usize), String> {
    use windows::core::HSTRING;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Storage::FileAccessMode;
    use windows::Storage::StorageFile;

    let path_str = path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve path: {e}"))?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();

    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(&path_str))
        .map_err(|e| format!("Failed to open file: {e}"))?
        .get()
        .map_err(|e| format!("GetFileFromPathAsync failed: {e}"))?;

    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .map_err(|e| format!("Failed to open stream: {e}"))?
        .get()
        .map_err(|e| format!("OpenAsync failed: {e}"))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|e| format!("Failed to create bitmap decoder: {e}"))?
        .get()
        .map_err(|e| format!("BitmapDecoder::CreateAsync failed: {e}"))?;

    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| format!("Failed to get software bitmap: {e}"))?
        .get()
        .map_err(|e| format!("GetSoftwareBitmapAsync failed: {e}"))?;

    let engine = create_engine(lang)?;

    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("RecognizeAsync failed: {e}"))?
        .get()
        .map_err(|e| format!("OCR recognition failed: {e}"))?;

    let lines = result
        .Lines()
        .map_err(|e| format!("Failed to read OCR lines: {e}"))?;

    let mut text = String::new();
    let mut region_count = 0usize;
    for line in lines {
        let line_text = line
            .Text()
            .map_err(|e| format!("Failed to read line text: {e}"))?;
        let trimmed = line_text.to_string_lossy();
        if !trimmed.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&trimmed);
            region_count += 1;
        }
    }

    Ok((text, region_count))
}

#[cfg(target_os = "windows")]
pub fn recognize_from_path(path: &Path, lang: &str) -> Result<String, String> {
    recognize_from_path_inner(path, lang).map(|(text, _)| text)
}

#[cfg(target_os = "windows")]
pub fn recognize_from_path_with_regions(
    path: &Path,
    lang: &str,
) -> Result<(String, usize), String> {
    recognize_from_path_inner(path, lang)
}

// ── Non-Windows stub ──

#[cfg(not(target_os = "windows"))]
pub fn recognize_from_path(_path: &Path, _lang: &str) -> Result<String, String> {
    Err("Windows OCR is only available on Windows 10+".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn recognize_from_path_with_regions(
    _path: &Path,
    _lang: &str,
) -> Result<(String, usize), String> {
    Err("Windows OCR is only available on Windows 10+".to_string())
}
