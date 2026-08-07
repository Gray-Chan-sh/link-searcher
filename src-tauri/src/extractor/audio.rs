use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

const MODEL_SUBDIR: &str = "models/funasr";

/// Resolve the FunASR model directory across dev and bundled runs.
///
/// The venv lives at `src-tauri/models/funasr/.venv` during development,
/// where the process cwd is `src-tauri/`. A bundled build starts from an
/// arbitrary cwd, so relative paths fail — probe fixed candidates instead.
fn funasr_dir() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Ok(d) = std::env::var("LINK_SEARCHER_FUNASR_DIR") {
            v.push(PathBuf::from(d));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                v.push(parent.join(MODEL_SUBDIR));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join(MODEL_SUBDIR));
        }
        v
    };
    candidates
        .into_iter()
        .find(|d| d.join(".venv").join("bin").join("python").exists())
}

fn venv_python() -> Option<PathBuf> {
    funasr_dir().map(|d| d.join(".venv").join("bin").join("python"))
}

fn model_ready() -> bool {
    venv_python().is_some()
}

fn python_cmd() -> Option<Command> {
    let dir = funasr_dir()?;
    let py = dir.join(".venv").join("bin").join("python");
    let mut c = Command::new(py);
    c.arg(dir.join("infer.py"));
    Some(c)
}

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        if !model_ready() {
            return Ok(format!(
                "─── 音频文件 ({}, {:.1}MB) ──\n[ASR 环境未安装]\n安装参考: models/funasr/README.md（可设 LINK_SEARCHER_FUNASR_DIR 指向模型目录）\n",
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

        let mut cmd = python_cmd().ok_or_else(|| anyhow::anyhow!("FunASR venv not found"))?;
        match cmd.arg(&wav_path).output() {
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
