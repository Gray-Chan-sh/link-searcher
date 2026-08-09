use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::extractor::Extractor;
use crate::scanner::helpers::TempDir;

const MAX_TOTAL_SIZE: u64 = 100 * 1024 * 1024;
const MAX_FILES: usize = 1000;
const MAX_SINGLE_FILE: u64 = 50 * 1024 * 1024;
const TEXT_FORMATS: &[&str] = &[
    "txt", "md", "csv", "json", "xml", "yaml", "yml", "toml", "ini", "cfg",
    "log", "py", "rs", "ts", "js", "html", "css", "sql", "sh", "bat",
    "ps1", "env", "conf", "properties",
];

/// Reject entry names that could escape the extraction temp dir (zip-slip):
/// absolute paths or any path segment of `..` (both `/` and `\` separators).
fn is_safe_archive_name(name: &str) -> bool {
    !Path::new(name).is_absolute()
        && !name
            .replace('\\', "/")
            .split('/')
            .any(|seg| seg == "..")
}

/// Read at most `cap` bytes; `over` is true when the input exceeded the cap
/// (a zip entry's declared size may lie).
fn read_capped<R: Read>(reader: R, cap: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let n = reader.take(cap + 1).read_to_end(&mut buf)? as u64;
    Ok((buf, n > cap))
}

pub struct ArchiveExtractor;

impl ArchiveExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_archive(&self, path: &Path, lang: &str) -> Result<String> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("archive")
            .to_lowercase();

        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return self.extract_tar_compressed(path, lang, "gz");
        }
        if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
            return self.extract_tar_compressed(path, lang, "bz2");
        }
        if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            return self.extract_tar_compressed(path, lang, "xz");
        }
        if name.ends_with(".zip") {
            return self.extract_zip(path, lang);
        }
        if name.ends_with(".tar") {
            let f = fs::File::open(path).context("open tar")?;
            return self.process_tar_entries(tar::Archive::new(BufReader::new(f)), path, lang);
        }
        if name.ends_with(".gz") {
            return self.extract_single_compressed(path, lang, "gz");
        }
        if name.ends_with(".bz2") {
            return self.extract_single_compressed(path, lang, "bz2");
        }
        if name.ends_with(".xz") {
            return self.extract_single_compressed(path, lang, "xz");
        }
        Err(anyhow::anyhow!("不支持的压缩格式"))
    }

    fn extract_zip(&self, path: &Path, lang: &str) -> Result<String> {
        let started = Instant::now();
        let file = fs::File::open(path).context("open zip")?;
        let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("read zip")?;

        let mut output = String::new();
        let mut total_size: u64 = 0;
        let mut file_count: usize = 0;

        for i in 0..archive.len() {
            let entry = archive.by_index(i).context("read zip entry")?;
            let entry_name = entry.name().to_owned();
            let entry_size = entry.size();

            if entry.is_dir() || entry_size == 0 {
                continue;
            }
            if file_count >= MAX_FILES || MAX_TOTAL_SIZE.saturating_sub(total_size) == 0 {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str("─── [跳过: 达到上限] ... ───\n");
                break;
            }
            if entry_size > MAX_SINGLE_FILE {
                append_skip(&mut output, &entry_name, "文件过大");
                continue;
            }

            let cap = MAX_TOTAL_SIZE.saturating_sub(total_size).min(MAX_SINGLE_FILE);
            let (buf, over) = read_capped(entry, cap).context("read zip entry data")?;
            if over {
                append_skip(&mut output, &entry_name, "解压超过上限");
                continue;
            }
            total_size += buf.len() as u64;
            file_count += 1;

            append_entry(&mut output, &entry_name, &buf, lang)?;
        }
        log::info!(
            "[ARCHIVE] zip: {} entries extracted in {:.1}s",
            file_count,
            started.elapsed().as_secs_f64(),
        );
        Ok(output)
    }

    fn extract_tar_compressed(&self, path: &Path, lang: &str, compression: &str) -> Result<String> {
        let file = fs::File::open(path).context("open compressed tar")?;
        let reader: Box<dyn Read> = match compression {
            "gz" => Box::new(flate2::read::GzDecoder::new(BufReader::new(file))),
            "bz2" => Box::new(bzip2::read::BzDecoder::new(BufReader::new(file))),
            "xz" => Box::new(xz2::read::XzDecoder::new(BufReader::new(file))),
            _ => return Err(anyhow::anyhow!("unsupported compression: {compression}")),
        };
        self.process_tar_entries(tar::Archive::new(reader), path, lang)
    }

    fn process_tar_entries<R: Read>(
        &self,
        mut archive: tar::Archive<R>,
        _path: &Path,
        lang: &str,
    ) -> Result<String> {
        let started = Instant::now();
        let mut output = String::new();
        let mut total_size: u64 = 0;
        let mut file_count: usize = 0;

        for entry in archive.entries().context("iterate tar entries")? {
            let mut entry = entry.context("read tar entry")?;
            let entry_name = entry
                .path()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "?".to_string());
            let entry_size = entry.header().size().context("tar entry size")?;
            let entry_type = entry.header().entry_type();

            if entry_type.is_dir() || entry_size == 0 {
                continue;
            }
            if file_count >= MAX_FILES || MAX_TOTAL_SIZE.saturating_sub(total_size) == 0 {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str("─── [跳过: 达到上限] ... ───\n");
                break;
            }
            if entry_size > MAX_SINGLE_FILE {
                append_skip(&mut output, &entry_name, "文件过大");
                continue;
            }

            let cap = MAX_TOTAL_SIZE.saturating_sub(total_size).min(MAX_SINGLE_FILE);
            let (buf, over) = read_capped(&mut entry, cap).context("read tar entry")?;
            if over {
                append_skip(&mut output, &entry_name, "解压超过上限");
                continue;
            }
            total_size += buf.len() as u64;
            file_count += 1;

            append_entry(&mut output, &entry_name, &buf, lang)?;
        }
        log::info!(
            "[ARCHIVE] tar: {} entries ({:.1}MB) extracted in {:.1}s",
            file_count,
            total_size as f64 / 1_048_576.0,
            started.elapsed().as_secs_f64(),
        );
        Ok(output)
    }

    fn extract_single_compressed(&self, path: &Path, lang: &str, compression: &str) -> Result<String> {
        let file = fs::File::open(path).context("open compressed")?;
        let reader: Box<dyn Read> = match compression {
            "gz" => Box::new(flate2::read::GzDecoder::new(BufReader::new(file))),
            "bz2" => Box::new(bzip2::read::BzDecoder::new(BufReader::new(file))),
            "xz" => Box::new(xz2::read::XzDecoder::new(BufReader::new(file))),
            _ => return Err(anyhow::anyhow!("unsupported compression: {compression}")),
        };

        let (buf, over) = read_capped(reader, MAX_SINGLE_FILE).context("decompress")?;
        if over {
            return Err(anyhow::anyhow!("解压后文件过大 ({} 字节)", buf.len()));
        }

        let stem = path
            .file_stem()
            .and_then(|n| n.to_str())
            .map(|s| {
                // Strip .tar if present (for .tar.gz → stem is .tar, get real stem)
                if s.to_lowercase().ends_with(".tar") {
                    &s[..s.len() - 4]
                } else {
                    s
                }
            })
            .unwrap_or("decompressed");
        let display_name = format!("{stem}");

        let mut output = String::new();
        append_entry(&mut output, &display_name, &buf, lang)?;
        Ok(output)
    }
}

impl Extractor for ArchiveExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_archive(path, "eng")
    }
}

/// Extract text from a buffer that may be a text file, office doc, PDF, or image.
fn append_entry(output: &mut String, name: &str, buf: &[u8], lang: &str) -> Result<()> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if !output.is_empty() {
        output.push('\n');
    }

    if TEXT_FORMATS.contains(&ext.as_str()) {
        let text = String::from_utf8_lossy(buf);
        output.push_str(&format!("─── {name} ───\n"));
        output.push_str(text.trim());
        output.push('\n');
        return Ok(());
    }

    if is_supported_ext(&ext) {
        if !is_safe_archive_name(name) {
            append_skip(output, name, "危险路径");
            return Ok(());
        }
        let tmp = TempDir::new("ls_arc")?;
        let tmp_path = tmp.path().join(name);
        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent).context("create archive temp dir")?;
        }
        fs::write(&tmp_path, buf).context("write archive temp file")?;

        match crate::extractor::extract_text(&tmp_path, lang, None) {
            Ok(text) if !text.trim().is_empty() => {
                output.push_str(&format!("─── {name} ───\n"));
                output.push_str(text.trim());
                output.push('\n');
            }
            Ok(_) => {
                append_skip(output, name, "无内容");
            }
            Err(e) => {
                let reason = classify_extract_error(&e);
                append_skip(output, name, &reason);
            }
        }
        return Ok(());
    }

    append_skip(output, name, "格式不支持");
    Ok(())
}

fn append_skip(output: &mut String, name: &str, reason: &str) {
    output.push_str(&format!("─── [跳过: {reason}] {name} ───\n"));
}

fn classify_extract_error(e: &anyhow::Error) -> String {
    let msg = e.to_string();
    if msg.contains("加密") || msg.contains("Encrypted") {
        "加密".to_string()
    } else if msg.contains("无法读取") || msg.contains("损坏") || msg.contains("corrupted") {
        "损坏".to_string()
    } else {
        "提取失败".to_string()
    }
}

fn is_supported_ext(ext: &str) -> bool {
    matches!(
        ext,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
            | "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif"
            | "odt" | "ods" | "odp" | "rtf" | "epub"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn archive_name_safety() {
        assert!(is_safe_archive_name("docs/report.pdf"));
        assert!(is_safe_archive_name("sub/dir/file.txt"));
        assert!(!is_safe_archive_name("../evil.pdf"));
        assert!(!is_safe_archive_name("a/../../evil.pdf"));
        assert!(!is_safe_archive_name("/etc/passwd"));
        assert!(!is_safe_archive_name("..\\..\\evil.pdf"));
        assert!(!is_safe_archive_name(".."));
    }

    #[test]
    fn read_capped_limits_actual_bytes() {
        let data = b"0123456789";
        let (buf, over) = read_capped(Cursor::new(data), 5).unwrap();
        assert_eq!(buf.len(), 6);
        assert!(over);
        let (buf, over) = read_capped(Cursor::new(data), 10).unwrap();
        assert_eq!(buf.len(), 10);
        assert!(!over);
    }
}
