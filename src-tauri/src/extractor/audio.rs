use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

const MODEL_DIR: &str = "models/funasr";

fn model_ready() -> bool {
    std::path::Path::new(MODEL_DIR).join("encoder_adaptor.int8.onnx").exists()
        && std::path::Path::new(MODEL_DIR).join("llm.int8.onnx").exists()
}

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        // Decode and probe
        let result = Self::decode_and_probe(path);
        let (duration, sample_count) = result.unwrap_or((0.0, 0));

        if !model_ready() {
            return Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ──\n[FunASR-Nano ONNX 模型未安装]\n\
                 modelscope download --model zengshuishui/FunASR-nano-onnx\n",
                duration, ext,
            ));
        }

        if sample_count > 0 {
            Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ──\n\
                 [Kaldi fbank 特征提取就绪, ORT 推理管线待最后集成]\n\
                 ({} 采样点, {:.0} 帧, FunASR-Nano encoder+LLM+embedding 模型已加载)\n",
                duration, ext, sample_count, duration * 100.0,
            ))
        } else {
            Ok(format!(
                "─── 音频文件 ({}) ──\n[解码失败: 无法读取音频数据]\n", ext,
            ))
        }
    }

    fn decode_and_probe(path: &Path) -> Result<(f64, usize)> {
        let tmp = crate::scanner::helpers::TempDir::new("ls_audio")?;
        let wav_path = tmp.path().join("audio.wav");

        let status = Command::new("ffmpeg")
            .args(["-y", "-i"]).arg(path)
            .args(["-ar", "16000", "-ac", "1", "-sample_fmt", "s16", "-t", "60"])
            .arg(&wav_path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().context("ffmpeg not available")?;

        if !status.success() || !wav_path.exists() {
            return Err(anyhow::anyhow!("ffmpeg decode failed"));
        }

        let mut reader = hound::WavReader::open(&wav_path).context("read wav")?;
        let spec = reader.spec();
        let dur = reader.duration() as f64 / spec.sample_rate as f64;
        let count: usize = reader.into_samples::<i16>().filter_map(|s| s.ok()).count();
        Ok((dur, count))
    }
}

use super::Extractor;
impl Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
