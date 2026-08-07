use std::path::Path;

use anyhow::{Context, Result};

use super::Extractor;

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let duration_s = if meta.len() > 0 {
            // Approximate: most compressed audio is ~16KB/s at speech quality
            meta.len() as f64 / 16000.0
        } else {
            0.0
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
        Ok(format!(
            "─── 音频文件 ({:.0}s, {}) ───\n[ASR 待集成: ORT + Fun-ASR-Nano + CAM++ 说话人分离]\n",
            duration_s, ext,
        ))
    }
}

impl Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
