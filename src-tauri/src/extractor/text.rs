use std::io::Read;
use std::path::Path;

use anyhow::Result;

use super::Extractor;

const BINARY_CHECK_SIZE: usize = 8_192;
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

pub struct TextExtractor;

impl TextExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for TextExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        // P1-2: limit to 10MB to avoid OOM on huge text files
        let mut data = Vec::with_capacity(10 * 1024 * 1024);
        std::fs::File::open(path)?
            .take(10 * 1024 * 1024)
            .read_to_end(&mut data)?;

        if data.is_empty() {
            return Ok(String::new());
        }

        // BOM detection for UTF-16 encoded files (common on Windows)
        if data.starts_with(UTF16LE_BOM) {
            let (cow, _) = encoding_rs::UTF_16LE.decode_without_bom_handling(&data[2..]);
            return Ok(cow.into_owned());
        }
        if data.starts_with(UTF16BE_BOM) {
            let (cow, _) = encoding_rs::UTF_16BE.decode_without_bom_handling(&data[2..]);
            return Ok(cow.into_owned());
        }

        // Strip UTF-8 BOM if present
        let body = if data.starts_with(UTF8_BOM) {
            &data[3..]
        } else {
            &data[..]
        };

        // Try fast UTF-8 path first
        match std::str::from_utf8(body) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => {
                // Check for binary content (null bytes in first 8 KB)
                if body[..body.len().min(BINARY_CHECK_SIZE)].contains(&0x00) {
                    return Ok(String::new());
                }
                // Lossy fallback for text files with encoding issues
                Ok(String::from_utf8_lossy(body).into_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_txt_extract() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_text");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("test.txt");
        std::fs::write(&path, "Hello, World!")?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "Hello, World!");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_md_extract() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_md");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("test.md");
        std::fs::write(&path, "# Heading\n\nSome *markdown* text.")?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "# Heading\n\nSome *markdown* text.");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_binary_file_returns_empty() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_binary");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("binary.bin");
        // Write binary content with null bytes
        let mut f = std::fs::File::create(&path)?;
        f.write_all(&[0x00, 0x01, 0x02, 0x03, 0xFF])?;
        drop(f);

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_utf16le_bom() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_utf16");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("utf16.txt");
        // Encode "Hello" as UTF-16LE with BOM
        let mut bom_data = Vec::from(UTF16LE_BOM);
        let mut encoded = Vec::new();
        for byte in "Hello".encode_utf16().flat_map(|c| c.to_le_bytes()) {
            encoded.push(byte);
        }
        bom_data.extend_from_slice(&encoded);
        std::fs::write(&path, &bom_data)?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "Hello");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_utf8_bom() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_utf8bom");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("utf8bom.txt");
        let mut data = Vec::from(UTF8_BOM);
        data.extend_from_slice(b"Hello with BOM");
        std::fs::write(&path, &data)?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "Hello with BOM");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_empty_file() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_empty");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("empty.txt");
        std::fs::write(&path, "")?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_lossy_fallback() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_lossy");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("lossy.txt");
        // Invalid UTF-8 sequence (0xFF is not valid in UTF-8)
        std::fs::write(&path, &[0x48, 0x65, 0x6C, 0x6C, 0x6F, 0xFF])?;

        let extractor = TextExtractor::new();
        let result = extractor.extract(&path)?;
        // Should get lossy replacement
        assert!(result.starts_with("Hello"));
        assert_eq!(result.chars().count(), 6); // 5 chars + replacement

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
