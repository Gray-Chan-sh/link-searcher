//! Office text extraction — pure native, no LibreOffice subprocess.
//!
//! Format routing:
//! - `.doc` → `rwml` (pure-Rust legacy Word binary)
//! - `.xls/.xlsx/.xlsm/.xlsb` → `calamine` (spreadsheet reader)
//! - `.docx/.ppt/.pptx/.../.odt/.ods/.odp/.rtf/.epub/.csv` → `anydoc`
//!
//! LibreOffice (soffice) was removed as a fallback: it only ever fired on
//! corrupt/garbage files (log evidence) where it also failed, and it dragged
//! in a subprocess batcher, path probing and per-file spawn latency. Corrupt
//! inputs now fail fast with the native error.

use std::path::Path;

use anyhow::Context;
use calamine::Reader as _;

use super::Extractor;

pub struct OfficeExtractor;

impl OfficeExtractor {
    pub fn new() -> Self {
        Self
    }

    fn doc_via_rwml(path: &Path) -> anyhow::Result<String> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read {:?}", path.file_name()))?;
        let text = rwml::extract_text(&bytes)
            .map_err(|e| anyhow::anyhow!("Word 文档解析失败 ({e})"))?;
        if text.trim().is_empty() {
            return Err(anyhow::anyhow!("Word 文档无文本内容（可能损坏或加密）"));
        }
        log::info!("[OFFICE] rwml: {} chars from {:?}", text.len(), path.file_name());
        Ok(text)
    }

    fn xls_via_calamine(path: &Path) -> anyhow::Result<String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let mut text = String::new();
        match ext.as_str() {
            "xls" => {
                let mut workbook: calamine::Xls<_> = calamine::open_workbook(path)
                    .with_context(|| format!("无法打开 XLS: {}", path.display()))?;
                for name in workbook.sheet_names().to_vec() {
                    if let Ok(range) = workbook.worksheet_range(&name) {
                        append_sheet_text(&mut text, range);
                    }
                }
            }
            "xlsb" => {
                let mut workbook: calamine::Xlsb<_> = calamine::open_workbook(path)
                    .with_context(|| format!("无法打开 XLSB: {}", path.display()))?;
                for name in workbook.sheet_names().to_vec() {
                    if let Ok(range) = workbook.worksheet_range(&name) {
                        append_sheet_text(&mut text, range);
                    }
                }
            }
            _ => {
                let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path)
                    .with_context(|| format!("无法打开 XLSX: {}", path.display()))?;
                for name in workbook.sheet_names().to_vec() {
                    if let Ok(range) = workbook.worksheet_range(&name) {
                        append_sheet_text(&mut text, range);
                    }
                }
            }
        }
        if text.trim().is_empty() {
            return Err(anyhow::anyhow!("电子表格无文本内容（可能损坏或加密）"));
        }
        log::info!("[OFFICE] calamine: {} chars from {:?}", text.len(), path.file_name());
        Ok(text)
    }

    fn office_via_anydoc(path: &Path) -> anyhow::Result<String> {
        match anydoc::to_markdown(path) {
            Ok(md) if !md.trim().is_empty() => {
                log::info!("[OFFICE] anydoc: {} chars from {:?}", md.len(), path.file_name());
                Ok(md)
            }
            Ok(_) => Err(anyhow::anyhow!("文档无文本内容（可能损坏或加密）")),
            Err(anydoc::ConvertError::Encrypted) => {
                Err(anyhow::anyhow!("此文件已加密，无法读取内容"))
            }
            Err(e) => Err(anyhow::anyhow!("文档解析失败 ({e})")),
        }
    }
}

fn append_sheet_text(text: &mut String, range: calamine::Range<calamine::Data>) {
    for row in range.rows() {
        let cells: Vec<String> = row
            .iter()
            .filter_map(|c| match c {
                calamine::Data::String(s) => Some(s.clone()),
                calamine::Data::Float(f) => Some(f.to_string()),
                calamine::Data::Int(i) => Some(i.to_string()),
                calamine::Data::Bool(b) => Some(b.to_string()),
                calamine::Data::DateTime(dt) => Some(dt.to_string()),
                calamine::Data::DateTimeIso(s) => Some(s.clone()),
                calamine::Data::DurationIso(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        if !cells.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&cells.join("\t"));
        }
    }
}

impl Extractor for OfficeExtractor {
    fn extract(&self, path: &Path) -> anyhow::Result<String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "doc" => Self::doc_via_rwml(path),
            "xls" | "xlsx" | "xlsm" | "xlsb" => Self::xls_via_calamine(path),
            "docx" | "docm" | "ppt" | "pptx" | "pptm" | "ppsm" | "ppsx" | "pps" | "pot"
            | "odt" | "ods" | "odp" | "rtf" | "epub" | "csv" => Self::office_via_anydoc(path),
            _ => Err(anyhow::anyhow!("unsupported office format: {ext}")),
        }
    }
}

#[cfg(test)]
mod tests;
