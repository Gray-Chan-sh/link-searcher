use serde::Serialize;
use tauri::State;

use crate::extractor::ocr;
use crate::state::AppState;

#[derive(Serialize)]
pub struct OcrEngineStatus {
    pub engine_type: String,
    pub name: String,
    pub available: bool,
    pub platforms: Vec<String>,
    pub install_guide: String,
    /// Extra status detail (e.g. which Windows OCR language packs are
    /// installed, or the OCR model dir). Empty when nothing to add.
    pub detail: String,
}

#[derive(Serialize)]
pub struct OcrTestResult {
    pub success: bool,
    pub text: String,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Legacy command — kept for backward compatibility.
/// Returns whether Tesseract is available on the system.
#[tauri::command]
pub fn check_tesseract() -> Result<bool, String> {
    Ok(crate::extractor::ocr::is_tesseract_available())
}

/// List all available OCR engines with status and install guides.
#[tauri::command]
pub fn list_ocr_engines() -> Result<Vec<OcrEngineStatus>, String> {
    let engines = ocr::detect_available_engines();
    let mut result = Vec::new();
    for engine in engines {
        let (name, platforms, install_guide) = match &engine {
            ocr::OcrEngineType::PaddleOCR => (
                "PaddleOCR（内置）".to_string(),
                vec!["macOS".to_string(), "Windows".to_string(), "Linux".to_string()],
                "在首次启动引导或设置页「依赖中心」下载 OCR 模型（约 20MB）".to_string(),
            ),
            ocr::OcrEngineType::AppleVision => (
                "Apple Vision (macOS 内置)".to_string(),
                vec!["macOS".to_string()],
                "内置在 macOS 10.15+，无需安装".to_string(),
            ),
            ocr::OcrEngineType::WindowsOcr => (
                "Windows OCR".to_string(),
                vec!["Windows".to_string()],
                "内置在 Windows 10+；中文识别需安装中文语言包\n设置 → 时间和语言 → 语言 → 添加中文 → 选项 → 手写/OCR".to_string(),
            ),
            ocr::OcrEngineType::Tesseract => (
                "Tesseract OCR".to_string(),
                vec!["macOS".to_string(), "Windows".to_string(), "Linux".to_string()],
                "macOS: brew install tesseract tesseract-lang\n\
                 Windows: winget install -e --id UB-Mannheim.TesseractOCR\n\
                 Linux: sudo apt install tesseract-ocr"
                    .to_string(),
            ),
            ocr::OcrEngineType::None => (
                "不启用 OCR".to_string(),
                vec!["*".to_string()],
                "始终可用，跳过图片索引".to_string(),
            ),
        };
        let (available, detail) = match &engine {
            ocr::OcrEngineType::PaddleOCR => {
                let ok = crate::extractor::paddleocr::models_present();
                let detail = if ok {
                    "模型就绪".to_string()
                } else {
                    "模型未安装：请在「依赖中心」下载".to_string()
                };
                (ok, detail)
            }
            ocr::OcrEngineType::Tesseract => (ocr::is_tesseract_available(), String::new()),
            ocr::OcrEngineType::AppleVision => (true, String::new()),
            ocr::OcrEngineType::WindowsOcr => {
                let (ok, langs) = crate::extractor::windows_ocr::availability();
                let detail = if langs.is_empty() {
                    "未检测到已安装的 OCR 语言包".to_string()
                } else {
                    format!("可用语言包: {}", langs.join(", "))
                };
                (ok, detail)
            }
            ocr::OcrEngineType::None => (true, String::new()),
        };
        result.push(OcrEngineStatus {
            engine_type: format!("{:?}", engine),
            name,
            available,
            platforms,
            install_guide,
            detail,
        });
    }
    Ok(result)
}

#[derive(Serialize)]
pub struct DependencyStatus {
    pub name: String,
    pub command: String,
    pub available: bool,
    pub install_guide: String,
}

/// Check the availability of external tools (Tesseract, pdftoppm, etc.)
#[tauri::command]
pub fn check_dependencies(state: State<'_, AppState>) -> Result<Vec<DependencyStatus>, String> {
    Ok(vec![
        DependencyStatus {
            name: "PaddleOCR（图片 OCR）".into(),
            command: "data_dir/models/ppocrv5".into(),
            available: crate::extractor::paddleocr::models_present(),
            install_guide: "在「依赖中心」点击安装（镜像下载，约 20MB）".into(),
        },
        DependencyStatus {
            name: "Tesseract OCR".into(),
            command: "tesseract".into(),
            available: crate::extractor::ocr::is_tesseract_available(),
            install_guide: "macOS: brew install tesseract\nWindows: winget install -e --id UB-Mannheim.TesseractOCR\nLinux: sudo apt install tesseract-ocr".into(),
        },
        DependencyStatus {
            name: "PDF Renderer (pdftoppm)".into(),
            command: "pdftoppm".into(),
            available: crate::extractor::pdf::is_pdftoppm_available(),
            install_guide: "macOS: brew install poppler\nWindows: winget install -e --id oschwartz10612.Poppler\nLinux: sudo apt install poppler-utils".into(),
        },
        DependencyStatus {
            name: "FFmpeg (音频解码)".into(),
            command: "ffmpeg".into(),
            available: crate::extractor::audio::ffmpeg_available(),
            install_guide: "macOS: brew install ffmpeg\nWindows: winget install -e --id Gyan.FFmpeg\nLinux: sudo apt install ffmpeg".into(),
        },
        DependencyStatus {
            name: "FunASR (音频转写)".into(),
            command: "data_dir/models/funasr (sherpa-onnx int8 models)".into(),
            available: crate::extractor::audio::funasr_model_ready(&state.data_dir),
            install_guide: "在「依赖中心」点击下载 FunASR 模型（约 850MB）".into(),
        },
    ])
}

#[derive(Serialize)]
pub struct FileTypeInfo {
    pub extension: String,
    pub name: String,
    pub dependency_met: bool,
    pub install_guide: String,
    pub count_in_dirs: u64,
}

#[tauri::command]
pub fn get_file_type_support(state: State<'_, AppState>) -> Result<Vec<FileTypeInfo>, String> {
    let exts = crate::extractor::get_supported_extensions();

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
    drop(conn);

    let ocr_available = !crate::extractor::ocr::detect_available_engines().is_empty();

    let mut result = Vec::new();
    for ext in exts {
        let (name, dep_met, guide) = match ext {
            "txt"|"md"|"csv"|"json"|"xml"|"yaml"|"yml"|"toml"|"ini"|"cfg"|"log" => ("Plain text", true, ""),
            "py"|"rs"|"ts"|"js"|"html"|"css"|"sql"|"sh"|"bat"|"ps1"|"env"|"conf"|"properties" => ("Code", true, ""),
            "pdf" => ("PDF", true, ""),
            "docx"|"doc" => ("Word", true, ""),
            "xlsx"|"xls" => ("Excel", true, ""),
            "pptx"|"ppt" => ("PowerPoint", true, ""),
            "png"|"jpg"|"jpeg"|"gif"|"bmp"|"webp"|"tiff"|"tif" => ("Image", ocr_available, if ocr_available { "" } else { "需安装 OCR 引擎" }),
            _ => ("", true, ""),
        };

        let mut count = 0u64;
        for dir in &dirs {
            if let Ok(entries) = std::fs::read_dir(&dir.path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
                        count += 1;
                    }
                    if p.is_dir()
                        && let Ok(sub) = std::fs::read_dir(&p) {
                            for sub_e in sub.flatten() {
                                if sub_e.path().extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
                                    count += 1;
                                }
                            }
                        }
                }
            }
        }

        result.push(FileTypeInfo { extension: ext.to_string(), name: name.to_string(), dependency_met: dep_met, install_guide: guide.to_string(), count_in_dirs: count });
    }
    Ok(result)
}

#[derive(Serialize)]
pub struct UnsupportedExtInfo {
    pub extension: String,
    pub count: u64,
    pub dir_id: String,
    /// True when the format could be indexed if a known dependency were
    /// installed (e.g. LibreOffice for `.wps`/`.et`/`.dps`); false for
    /// formats with no extractor path at all.
    pub rescusable: bool,
    /// Human-readable hint shown in the UI (install guide or "no support").
    pub hint: String,
}

/// Return file extensions seen on disk during scans that are NOT in the
/// extractor whitelist, with occurrence counts. Lets users see *why* some
/// files never appear in search results instead of silently dropping them.
#[tauri::command]
pub fn get_unsupported_ext_stats(state: State<'_, AppState>) -> Result<Vec<UnsupportedExtInfo>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let stats = crate::db::tracker::get_unsupported_ext_stats(&conn)
        .map_err(|e| format!("{e}"))?;

    Ok(stats
        .into_iter()
        .map(|s| {
            UnsupportedExtInfo {
                extension: s.ext,
                count: s.count,
                dir_id: s.dir_id,
                rescusable: false,
                hint: "暂无提取器支持".into(),
            }
        })
        .collect())
}

/// Test an OCR engine by running it against a generated test image.
#[tauri::command]
pub fn test_ocr_engine(engine_type: String) -> Result<OcrTestResult, String> {
    let engine = match engine_type.as_str() {
        "PaddleOCR" => ocr::OcrEngineType::PaddleOCR,
        "Tesseract" => ocr::OcrEngineType::Tesseract,
        "AppleVision" => ocr::OcrEngineType::AppleVision,
        "WindowsOcr" => ocr::OcrEngineType::WindowsOcr,
        "None" => ocr::OcrEngineType::None,
        _ => return Err("unknown engine type".to_string()),
    };

    let png_data =
        ocr::create_test_image().map_err(|e| format!("failed to create test image: {e}"))?;
    let tmp_path = std::env::temp_dir().join("ls_ocr_test.png");
    std::fs::write(&tmp_path, &png_data).map_err(|e| format!("failed to write test image: {e}"))?;

    let start = std::time::Instant::now();
    match ocr::ocr_image_with_engine(&tmp_path, &engine, "eng") {
        Ok(text) => {
            let elapsed = start.elapsed().as_millis() as u64;
            let _ = std::fs::remove_file(&tmp_path);
            Ok(OcrTestResult {
                success: !text.trim().is_empty(),
                text: text.trim().to_string(),
                duration_ms: elapsed,
                error: if text.trim().is_empty() {
                    Some("未识别到文字".to_string())
                } else {
                    None
                },
            })
        }
        Err(e) => {
            let elapsed = start.elapsed().as_millis() as u64;
            let _ = std::fs::remove_file(&tmp_path);
            Ok(OcrTestResult {
                success: false,
                text: String::new(),
                duration_ms: elapsed,
                error: Some(format!("{e}")),
            })
        }
    }
}
