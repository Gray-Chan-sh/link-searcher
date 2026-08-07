use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

const MODEL_DIR: &str = "models/funasr";

fn model_ready() -> bool {
    let venv_py = Path::new(MODEL_DIR).join(".venv").join("bin").join("python");
    venv_py.exists()
}

fn python_cmd() -> Command {
    let venv_py = Path::new(MODEL_DIR).join(".venv").join("bin").join("python");
    let mut c = if venv_py.exists() {
        Command::new(venv_py)
    } else {
        Command::new("python3")
    };
    c.arg("models/funasr/infer.py");
    c
}

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        if !model_ready() {
            return Ok(format!(
                "─── 音频文件 ({}, {:.1}MB) ──\n[ASR 环境未安装]\n安装参考: src-tauri/models/funasr/README.md\n",
                ext, meta.len() as f64 / 1_048_576.0,
            ));
        }

        // Decode to 16kHz mono WAV (full length)
        let tmp = crate::scanner::helpers::TempDir::new("ls_audio")?;
        let wav_path = tmp.path().join("audio.wav");
        let status = Command::new("ffmpeg")
            .args(["-y", "-i"]).arg(path)
            .args(["-ar", "16000", "-ac", "1", "-sample_fmt", "s16"])
            .arg(&wav_path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().context("ffmpeg not available")?;

        if !status.success() || !wav_path.exists() {
            return Err(anyhow::anyhow!("ffmpeg decode failed"));
        }

        let dur = match hound::WavReader::open(&wav_path) {
            Ok(r) => r.duration() as f64 / r.spec().sample_rate as f64,
            Err(_) => 0.0,
        };

// Run FunASR inference via the project venv python.
        match python_cmd()
            .arg(&wav_path)
            .output()
        {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if text.is_empty() {
                    return Ok(format!("─── 音频文件 ({:.0}s) ──\n[FunASR 推理完成，无识别结果]\n", dur));
                }
                Ok(format!("─── 音频文件 ({:.0}s) ──\n{}\n", dur, text))
            }
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                Ok(format!("─── 音频文件 ({:.0}s, {}) ──\n[ASR 推理失败: {}]\n", dur, ext, err.trim()))
            }
            Err(e) => Ok(format!("─── 音频文件 ({:.0}s, {}) ──\n[Python 环境不可用: {e}]\n", dur, ext)),
        }
    }
}

use super::Extractor;
impl Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}
