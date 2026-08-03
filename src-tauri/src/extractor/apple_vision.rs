//! Apple Vision OCR engine (macOS 10.15+).
//!
//! Uses `VNRecognizeTextRequest` with the Accurate recognition level,
//! running on the ANE (Neural Engine) when available. Wraps every call
//! in `autoreleasepool` to prevent Vision's temporary ObjC allocations
//! from accumulating (documented leak in long-running processes).

use std::path::Path;

#[cfg(target_os = "macos")]
use objc2::rc::autoreleasepool;

#[cfg(target_os = "macos")]
use objc2::AllocAnyThread;

#[cfg(target_os = "macos")]
use objc2::rc::Retained;

#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};

#[cfg(target_os = "macos")]
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequestTextRecognitionLevel,
};

/// Map application language codes to Apple Vision's BCP-47 language tags.
#[cfg(target_os = "macos")]
const VISION_LANG_MAP: &[(&str, &str)] = &[
    ("eng", "en-US"),
    ("chi_sim", "zh-Hans"),
    ("jpn", "ja-JP"),
    ("kor", "ko-KR"),
];

/// Run Apple Vision OCR on a single image file.
///
/// Falls back to `en-US` if `lang` is not in the map.
/// The Vision framework's Accurate mode supports CJK; Fast mode does not,
/// so Accurate is hard-coded here.
#[cfg(target_os = "macos")]
pub fn recognize_from_path(path: &Path, lang: &str) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Path contains non-UTF-8 characters".to_string())?;

    let vision_lang = VISION_LANG_MAP
        .iter()
        .find(|(k, _)| *k == lang)
        .map(|(_, v)| *v)
        .unwrap_or("en-US");

    autoreleasepool(|_| unsafe {
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);
        let handler = VNImageRequestHandler::initWithURL_options(
            VNImageRequestHandler::alloc(),
            &url,
            &NSDictionary::new(),
        );

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);

        let ns_lang = NSString::from_str(vision_lang);
        let langs = NSArray::from_retained_slice(&[ns_lang]);
        request.setRecognitionLanguages(&langs);

        let request_vn: Retained<_> = Retained::into_super(Retained::into_super(request.clone()));
        let requests = NSArray::from_retained_slice(&[request_vn]);

        handler
            .performRequests_error(&requests)
            .map_err(|e| format!("Apple Vision OCR failed: {e}"))?;

        let mut text = String::new();
        if let Some(results) = request.results() {
            for obs in results.iter() {
                let candidates = obs.topCandidates(1);
                if let Some(best) = candidates.firstObject() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&best.string().to_string());
                }
            }
        }
        Ok(text)
    })
}

/// Stub for non-macOS platforms — returns a descriptive error.
#[cfg(not(target_os = "macos"))]
pub fn recognize_from_path(_path: &Path, _lang: &str) -> Result<String, String> {
    Err("Apple Vision OCR is only available on macOS 10.15+".to_string())
}
