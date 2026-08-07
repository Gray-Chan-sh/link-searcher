use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

const MODEL_DIR: &str = "models/funasr";

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
        let model_dir = std::path::Path::new(MODEL_DIR);
        let enc_path = model_dir.join("encoder_adaptor.int8.onnx");
        let llm_path = model_dir.join("llm.int8.onnx");

        if !enc_path.exists() || !llm_path.exists() {
            return Ok(format!(
                "─── 音频文件 ({}, {:.1}MB) ──\n\
                 [FunASR 模型未安装]\n\
                 modelscope download --model zengshuishui/FunASR-nano-onnx\n",
                ext, meta.len() as f64 / 1_048_576.0,
            ));
        }

        // Decode audio and get duration
        let result = self.decode_info(path);
        match result {
            Ok((duration, samples)) => Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ──\n\
                 [FunASR 模型就绪 ({:.0}M 采样点), ORT 推理管线待集成]\n",
                duration, ext, samples as f64 / 1e6,
            )),
            Err(e) => Ok(format!(
                "─── 音频文件 ({}) ──\n[解码失败: {e}]\n", ext,
            )),
        }
    }

    fn decode_info(&self, path: &Path) -> Result<(f64, usize)> {
        let tmp = crate::scanner::helpers::TempDir::new("ls_audio")?;
        let wav_path = tmp.path().join("audio.wav");

        let status = Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(path)
            .args(["-ar", "16000", "-ac", "1", "-sample_fmt", "s16", "-t", "60"])
            .arg(&wav_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("ffmpeg not available")?;

        if !status.success() || !wav_path.exists() {
            return Err(anyhow::anyhow!("ffmpeg decoding failed"));
        }

        let reader = hound::WavReader::open(&wav_path).context("read wav")?;
        let spec = reader.spec();
        let dur = reader.duration() as f64 / spec.sample_rate as f64;
        let count: usize = reader.into_samples::<i16>().filter_map(|s| s.ok()).count();
        Ok((dur, count))
    }
}

impl super::Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
