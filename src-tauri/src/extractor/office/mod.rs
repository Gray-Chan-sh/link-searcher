use std::io::BufReader;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use calamine::Reader as CalamineReader;
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;

use super::Extractor;
use crate::config::load_config;
use crate::scanner::helpers::TempDir;

pub fn is_libreoffice_available() -> bool {
    if let Ok(lo_bin) = std::env::var("LO_BINARY") {
        if !lo_bin.is_empty() && check_binary(&lo_bin) {
            return true;
        }
    }
    let config = load_config();
    if !config.lo_binary_path.is_empty() && check_binary(&config.lo_binary_path) {
        return true;
    }
    for path in common_lo_paths() {
        if check_binary(&path) {
            return true;
        }
    }
    check_binary("soffice")
}

fn check_binary(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    // Only check file existence for absolute/relative paths, not for bare commands (like "soffice") that rely on PATH resolution
    let is_bare_name = !path.contains('/') && !path.contains('\\');
    if !is_bare_name && !Path::new(path).exists() {
        return false;
    }
    std::process::Command::new(path)
        .arg("--headless")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn common_lo_paths() -> Vec<String> {
    let mut paths = Vec::new();
    if cfg!(target_os = "macos") {
        paths.push("/Applications/LibreOffice.app/Contents/MacOS/soffice".into());
        paths.push("/opt/homebrew/bin/soffice".into());
        paths.push("/usr/local/bin/soffice".into());
    } else if cfg!(target_os = "windows") {
        paths.push("C:\\Program Files\\LibreOffice\\program\\soffice.exe".into());
        paths.push("C:\\Program Files (x86)\\LibreOffice\\program\\soffice.exe".into());
    } else if cfg!(target_os = "linux") {
        paths.push("/usr/lib/libreoffice/program/soffice".into());
        paths.push("/usr/bin/soffice".into());
        paths.push("/snap/bin/soffice".into());
    }
    paths
}

fn determine_lo_binary() -> String {
    if let Ok(lo_bin) = std::env::var("LO_BINARY") {
        if !lo_bin.is_empty() {
            return lo_bin;
        }
    }
    let config = load_config();
    if !config.lo_binary_path.is_empty() {
        return config.lo_binary_path;
    }
    for path in common_lo_paths() {
        if Path::new(&path).exists() {
            return path;
        }
    }
    "soffice".to_string()
}

#[cfg(target_os = "macos")]
struct LsuiElementGuard;

#[cfg(target_os = "macos")]
impl LsuiElementGuard {
    fn suppress() -> Self {
        let _ = std::process::Command::new("defaults")
            .args(["write", "org.libreoffice.script", "LSUIElement", "1"])
            .status();
        LsuiElementGuard
    }
}

#[cfg(target_os = "macos")]
impl Drop for LsuiElementGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("defaults")
            .args(["delete", "org.libreoffice.script", "LSUIElement"])
            .status();
    }
}

pub fn extract_via_libreoffice(path: &Path) -> Result<String> {
    let binary = determine_lo_binary();
    if binary.is_empty() {
        return Err(anyhow::anyhow!("LibreOffice 未配置且不可用"));
    }

    // macOS only: hide LibreOffice Dock icon for headless conversion
    #[cfg(target_os = "macos")]
    let _lsu_guard = if binary.contains("LibreOffice") {
        Some(LsuiElementGuard::suppress())
    } else {
        None
    };

    let tmp_dir = TempDir::new("ls_lo").context("failed to create LO temp dir")?;

    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    let out_dir = tmp_dir.path().to_path_buf();

    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let output = std::process::Command::new(&binary)
                .env("SAL_USE_VCLPLUGIN", "svp")
                .args(["--headless", "--convert-to", "txt:Text"])
                .arg("--outdir")
                .arg(&out_dir)
                .arg(&path)
                .output()
                .map_err(|e| anyhow::anyhow!("{} not available: {e}", binary))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("{} --convert-to txt failed (code {:?}): {}", binary, output.status.code(), stderr.trim());
            }

            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
            let out_path = out_dir.join(format!("{}.txt", stem));

            if !out_path.exists() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("LibreOffice 转换失败: {}", if stderr.trim().is_empty() { "未生成输出文件" } else { stderr.trim() });
            }

            let text = std::fs::read_to_string(&out_path)
                .map_err(|e| anyhow::anyhow!("failed to read LO output: {e}"))?;

            Ok(text)
        })();
        let _ = tx.send(result);
    });

    let result = rx
        .recv_timeout(Duration::from_secs(60))
        .map_err(|_| anyhow::anyhow!("LibreOffice timed out (60s)"))??;

    Ok(result)
}

pub struct OfficeExtractor;

impl OfficeExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for OfficeExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let lo_available = is_libreoffice_available();
        let is_office = matches!(ext.as_str(), "doc"|"docx"|"xls"|"xlsx"|"ppt"|"pptx");

        let lo_error = if is_office && lo_available {
            match extract_via_libreoffice(path) {
                Ok(text) if !text.trim().is_empty() => return Ok(text),
                Ok(_) => Some("LibreOffice 提取结果为空".to_string()),
                Err(e) => Some(format!("LibreOffice 提取失败: {e}")),
            }
        } else {
            None
        };

        match ext.as_str() {
            "docx" => extract_docx(path),
            "xlsx" => extract_xlsx(path),
            "pptx" => extract_pptx(path),
            "doc" | "xls" | "ppt" => {
                if let Some(msg) = lo_error {
                    Err(anyhow::anyhow!("{}。文件可能已损坏或使用了不兼容的格式", msg))
                } else {
                    Err(anyhow::anyhow!(
                        "需要安装 LibreOffice 或使用自定义路径提取此格式。macOS: brew install --cask libreoffice\nLinux: sudo apt install libreoffice\nWindows: winget install LibreOffice。如需指定自定义路径，请在设置中配置 LibreOffice 可执行文件位置"
                    ))
                }
            }
            _ => Err(anyhow::anyhow!("unsupported office format: {}", ext)),
        }
    }
}

fn extract_docx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("failed to open DOCX")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read DOCX as ZIP")?;
    let entry = archive
        .by_name("word/document.xml")
        .context("DOCX missing word/document.xml")?;

    let mut xml_reader = XmlReader::from_reader(BufReader::new(entry));
    xml_reader.config_mut().trim_text(true);

    let mut text = String::new();
    let mut buf = Vec::new();
    let mut in_t = false;
    let mut in_p = false;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                match e.local_name().as_ref() {
                    b"t" => in_t = true,
                    b"p" => in_p = true,
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                match e.local_name().as_ref() {
                    b"t" => in_t = false,
                    b"p" => {
                        if in_p {
                            text.push('\n');
                            in_p = false;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_t {
                    if let Ok(s) = e.unescape() {
                        text.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("XML parse error in DOCX: {e}"),
            _ => {}
        }
        buf.clear();
    }

    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

fn extract_xlsx(path: &Path) -> Result<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut text = String::new();

    if ext == "xls" {
        let mut workbook: calamine::Xls<_> =
            calamine::open_workbook(path).context("failed to open XLS")?;
        let sheets = workbook.sheet_names().to_vec();
        for name in &sheets {
            if let Ok(range) = workbook.worksheet_range(name) {
                for row in range.rows() {
                    let row_text: Vec<String> = row.iter().filter_map(|c| match c {
                        calamine::Data::String(s) => Some(s.clone()),
                        calamine::Data::Float(f) => Some(f.to_string()),
                        calamine::Data::Int(i) => Some(i.to_string()),
                        calamine::Data::Bool(b) => Some(b.to_string()),
                        calamine::Data::DateTime(dt) => Some(dt.to_string()),
                        calamine::Data::DateTimeIso(s) => Some(s.clone()),
                        calamine::Data::DurationIso(s) => Some(s.clone()),
                        _ => None,
                    }).collect();
                    if !row_text.is_empty() {
                        if !text.is_empty() { text.push('\n'); }
                        text.push_str(&row_text.join("\t"));
                    }
                }
            }
        }
    } else {
        let mut workbook: calamine::Xlsx<_> =
            calamine::open_workbook(path).context("failed to open XLSX")?;
        let sheets = workbook.sheet_names().to_vec();
        for name in &sheets {
            if let Ok(range) = workbook.worksheet_range(name) {
                for row in range.rows() {
                    let row_text: Vec<String> = row.iter().filter_map(|c| match c {
                        calamine::Data::String(s) => Some(s.clone()),
                        calamine::Data::Float(f) => Some(f.to_string()),
                        calamine::Data::Int(i) => Some(i.to_string()),
                        calamine::Data::Bool(b) => Some(b.to_string()),
                        calamine::Data::DateTime(dt) => Some(dt.to_string()),
                        calamine::Data::DateTimeIso(s) => Some(s.clone()),
                        calamine::Data::DurationIso(s) => Some(s.clone()),
                        _ => None,
                    }).collect();
                    if !row_text.is_empty() {
                        if !text.is_empty() { text.push('\n'); }
                        text.push_str(&row_text.join("\t"));
                    }
                }
            }
        }
    }

    Ok(text)
}

fn extract_pptx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("failed to open PPTX")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read PPTX as ZIP")?;

    let mut slide_files: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            let name = entry.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    slide_files.sort_by(|a, b| {
        let a_num = extract_slide_number(a);
        let b_num = extract_slide_number(b);
        a_num.cmp(&b_num)
    });

    let mut text = String::new();

    for slide_name in &slide_files {
        let entry = archive
            .by_name(slide_name)
            .with_context(|| format!("failed to read {slide_name}"))?;

        let mut xml_reader = XmlReader::from_reader(BufReader::new(entry));
        xml_reader.config_mut().trim_text(true);

        let mut buf = Vec::new();
        let mut in_t = false;
        let mut slide_text = String::new();

        loop {
            match xml_reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.local_name().as_ref() == b"t" {
                        in_t = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    if e.local_name().as_ref() == b"t" {
                        in_t = false;
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if in_t {
                        if let Ok(s) = e.unescape() {
                            slide_text.push_str(&s);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => anyhow::bail!("XML parse error in PPTX slide: {e}"),
                _ => {}
            }
            buf.clear();
        }

        if !slide_text.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&slide_text);
        }
    }

    Ok(text)
}

fn extract_slide_number(name: &str) -> usize {
    let stem = name.trim_end_matches(".xml");
    stem.rsplit_once("slide")
        .and_then(|(_, num)| num.parse::<usize>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;