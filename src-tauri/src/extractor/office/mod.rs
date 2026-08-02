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
    // First-run profile creation can take a while, so don't block forever.
    let mut child = match std::process::Command::new(path)
        .arg("--headless")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match wait_timeout(&mut child, Duration::from_secs(15)) {
        Ok(Some(status)) => status.success(),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
        Err(_) => false,
    }
}

fn wait_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                if start.elapsed() >= timeout {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
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

pub fn extract_via_libreoffice(path: &Path) -> Result<String> {
    let binary = determine_lo_binary();
    if binary.is_empty() {
        return Err(anyhow::anyhow!("LibreOffice 未配置且不可用"));
    }

    let tmp_dir = TempDir::new("ls_lo").context("failed to create LO temp dir")?;
    let out_dir = tmp_dir.path().to_path_buf();

    // Isolated profile per invocation: concurrent soffice processes (Rayon
    // par_iter) would otherwise contend on the shared default profile .lock.
    let profile_dir = out_dir.join("lo_profile");
    std::fs::create_dir_all(&profile_dir).context("failed to create LO profile dir")?;
    let profile_display = profile_dir.display().to_string();
    let profile_uri = if profile_display.starts_with('/') {
        format!("file://{profile_display}")
    } else {
        format!("file:///{profile_display}")
    };

    let stderr_log = out_dir.join("lo_stderr.log");
    let stderr_file = std::fs::File::create(&stderr_log).context("failed to create LO stderr log")?;

    let child = std::process::Command::new(&binary)
        .env("SAL_USE_VCLPLUGIN", "svp")
        .arg(format!("-env:UserInstallation={profile_uri}"))
        .args([
            "--headless",
            "--nologo",
            "--nodefault",
            "--norestore",
            "--nolockcheck",
            "--nofirststartwizard",
        ])
        .arg("--convert-to")
        .arg("txt:Text")
        .arg("--outdir")
        .arg(&out_dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| anyhow::anyhow!("{binary} not available: {e}"))?;

    let child_handle = std::sync::Arc::new(std::sync::Mutex::new(child));
    let child_for_thread = std::sync::Arc::clone(&child_handle);
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        // Poll without holding the lock while sleeping so the timeout
        // branch below can acquire it to kill an orphaned process.
        let result = loop {
            let mut child = child_for_thread.lock().unwrap_or_else(|e| e.into_inner());
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    drop(child);
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => break Err(e),
            }
        };
        let _ = tx.send(result);
    });

    let status = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(anyhow::anyhow!("LibreOffice wait failed: {e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let mut child = child_handle.lock().unwrap_or_else(|e| e.into_inner());
            log::warn!("[OFFICE] killed soffice after 60s timeout");
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!(
                "LibreOffice timed out (60s) and was killed (pid {})",
                child.id()
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(anyhow::anyhow!("LibreOffice thread panicked"));
        }
    };

    let stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();

    if !status.success() {
        anyhow::bail!("{binary} --convert-to txt failed (code {:?}): {}", status.code(), stderr.trim());
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let out_path = out_dir.join(format!("{stem}.txt"));

    if !out_path.exists() {
        anyhow::bail!("LibreOffice 转换失败: {}", if stderr.trim().is_empty() { "未生成输出文件" } else { stderr.trim() });
    }

    let text = std::fs::read_to_string(&out_path)
        .map_err(|e| anyhow::anyhow!("failed to read LO output: {e}"))?;

    Ok(text)
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