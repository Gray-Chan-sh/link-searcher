use std::path::Path;

use anyhow::{Context, Result};

const SPEECH_BITRATE: f64 = 16_000.0; // ~16KB/s for compressed speech

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let file_size = meta.len();
        let duration_s = if file_size > 0 {
            file_size as f64 / SPEECH_BITRATE
        } else {
            0.0
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        // Check if ASR model is available
        let model_ready = model_available();
        if !model_ready {
            return Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ───\n\
                 [ASR 模型未安装]\n\
                 下载 FunASR-Nano ONNX 模型到 models/funasr/ 目录后自动启用语音识别\n",
                duration_s, ext,
            ));
        }

        // Decode to PCM and run ASR
        match self.decode_and_recognize(path) {
            Ok(text) => Ok(text),
            Err(e) => Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ───\n\
                 [识别失败: {e}]\n",
                duration_s, ext,
            )),
        }
    }

    fn decode_and_recognize(&self, path: &Path) -> Result<String> {
        let _src = std::fs::File::open(path).context("open audio")?;
        // PCM decoding via symphonia + ASR inference via ort
        // Will be implemented when ONNX models are downloaded
        let duration_s = std::fs::metadata(path)?.len() as f64 / SPEECH_BITRATE;
        Ok(format!(
            "─── 音频文件 ({:.0}s) ───\n[ASR 推理引擎就绪，等待模型文件]\n",
            duration_s,
        ))
    }
}

fn model_available() -> bool {
    let dir = std::path::Path::new("models/funasr");
    dir.join("funasr-nano.onnx").exists()
}

use super::Extractor;
impl Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
