use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
        let duration_s = meta.len() as f64 / 16000.0;
        let file_size_mb = meta.len() as f64 / 1_048_576.0;

        // Decode to get actual duration via ffmpeg probe
        let actual_dur = probe_duration(path).unwrap_or(duration_s);

        Ok(format!(
            "─── 音频文件 ({:.1}s, {}, {:.1}MB) ───\n\
             加载 whisper 模型以启用语音识别:\n\
             curl -L -o models/funasr/ggml-tiny.bin \\\n\
             https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin\n",
            actual_dur, ext, file_size_mb,
        ))
    }
}

fn probe_duration(path: &Path) -> Result<f64> {
    let output = Command::new("ffprobe")
        .args(["-v", "quiet", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .context("ffprobe not available")?;
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().parse::<f64>().context("parse duration")
}

impl super::Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
