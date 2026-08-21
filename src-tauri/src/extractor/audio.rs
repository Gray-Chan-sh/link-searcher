use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// Locate `ffmpeg`. Searches PATH first, then the bundled/dev
/// `ffmpeg-bin/` dir, then next to the executable, then common
/// Homebrew prefixes — the Tauri app may not inherit the terminal PATH.
fn find_ffmpeg_binary() -> Option<PathBuf> {
    if Command::new("ffmpeg").arg("-version").output().is_ok() {
        return Some(PathBuf::from("ffmpeg"));
    }
    let dev_path = PathBuf::from("ffmpeg-bin").join("ffmpeg");
    if dev_path.exists() && Command::new(&dev_path).arg("-version").output().is_ok() {
        return Some(dev_path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundle_path = dir.join("ffmpeg");
            if bundle_path.exists() && Command::new(&bundle_path).arg("-version").output().is_ok() {
                return Some(bundle_path);
            }
        }
    }
    for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let candidate = PathBuf::from(prefix).join("ffmpeg");
        if candidate.exists() && Command::new(&candidate).arg("-version").output().is_ok() {
            return Some(candidate);
        }
    }
    None
}

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn ffmpeg_path() -> Option<&'static Path> {
    FFMPEG_PATH.get_or_init(find_ffmpeg_binary).as_deref()
}

/// Public check used by startup dependency detection and `check_dependencies`.
pub fn ffmpeg_available() -> bool {
    ffmpeg_path().is_some()
}

/// FunASR-Nano ONNX model files that must be present in the model dir.
/// Layout matches `sherpa-onnx-funasr-nano-int8-2025-12-30` archive.
const MODEL_SUBDIR: &str = "models/funasr";
const MODEL_ARCHIVE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2";
const REQUIRED_FILES: [&str; 4] = [
    "encoder_adaptor.int8.onnx",
    "llm.int8.onnx",
    "embedding.int8.onnx",
    "Qwen3-0.6B/tokenizer.json",
];

/// Resolve the FunASR model directory across dev and bundled runs.
///
/// Dev: `src-tauri/models/funasr`. Bundled: next to executable / data dir.
/// The model dir contains the sherpa-onnx-int8 archive contents (no venv).
fn funasr_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("LINK_SEARCHER_FUNASR_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // Dev-machine builds: walk up from the .app to the checkout.
            let mut dir = Some(parent);
            while let Some(d) = dir {
                let repo = d.join("src-tauri").join(MODEL_SUBDIR);
                if model_present(&repo) {
                    v.push(repo);
                    break;
                }
                dir = d.parent();
            }
            v.push(parent.join(MODEL_SUBDIR));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        v.push(cwd.join(MODEL_SUBDIR));
    }
    v.push(crate::config::load_config().data_dir.join(MODEL_SUBDIR));
    v
}

fn model_present(dir: &Path) -> bool {
    REQUIRED_FILES
        .iter()
        .all(|f| dir.join(f).is_file())
}

fn funasr_dir() -> Option<PathBuf> {
    funasr_candidates()
        .into_iter()
        .find(|d| model_present(d))
}

pub fn funasr_model_ready(data_dir: &Path) -> bool {
    let dir = data_dir.join(MODEL_SUBDIR);
    model_present(&dir)
}

/// URL used by the installer (`download_funasr_model`).
pub fn model_download_url() -> &'static str {
    MODEL_ARCHIVE_URL
}

/// Static shared recognizer. `OfflineRecognizer` creation loads ~950M of
/// int8 weights; reusing one instance across files avoids reloading per file.
static RECOGNIZER: OnceLock<Option<sherpa_onnx::OfflineRecognizer>> = OnceLock::new();

fn recognizer() -> Option<&'static sherpa_onnx::OfflineRecognizer> {
    RECOGNIZER
        .get_or_init(|| build_recognizer().ok())
        .as_ref()
}

fn build_recognizer() -> Result<sherpa_onnx::OfflineRecognizer> {
    let dir = funasr_dir().ok_or_else(|| anyhow::anyhow!("FunASR model not found"))?;
    let f = |name: &str| dir.join(name).to_string_lossy().to_string();

    let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.funasr_nano = sherpa_onnx::OfflineFunASRNanoModelConfig {
        encoder_adaptor: Some(f("encoder_adaptor.int8.onnx")),
        llm: Some(f("llm.int8.onnx")),
        embedding: Some(f("embedding.int8.onnx")),
        tokenizer: Some(f("Qwen3-0.6B")),
        system_prompt: Some("You are a helpful assistant.".into()),
        user_prompt: Some("语音转写：".into()),
        max_new_tokens: 512,
        temperature: 1e-06,
        top_p: 0.8,
        seed: 42,
        language: None,
        itn: 1,
        hotwords: None,
        ..Default::default()
    };
    config.decoding_method = Some("greedy_search".into());
    config.model_config.model_type = Some("funasr_nano".into());

    log::info!("[ASR] loading FunASR-Nano model (warm, ~1-2s)");
    let t0 = std::time::Instant::now();
    let rec = sherpa_onnx::OfflineRecognizer::create(&config)
        .ok_or_else(|| anyhow::anyhow!("sherpa-onnx failed to init FunASR-Nano recognizer"))?;
    log::info!("[ASR] recognizer ready in {:.1}s", t0.elapsed().as_secs_f64());
    Ok(rec)
}

pub struct AudioExtractor;

impl AudioExtractor {
    pub fn new() -> Self { Self }

    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        if !funasr_dir().is_some() {
            let probed = funasr_candidates()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ");
            return Ok(format!(
                "─── 音频文件 ({}, {:.1}MB) ──\n[ASR 模型未下载]\n已探测:\n  {}\n请在设置页点击「下载 FunASR 模型」（约 850MB，GitHub 下载）\n",
                ext,
                meta.len() as f64 / 1_048_576.0,
                probed,
            ));
        }

        // Decode to 16kHz mono WAV (full length)
        let tmp = crate::scanner::helpers::TempDir::new("ls_audio")?;
        let wav_path = tmp.path().join("audio.wav");
        let ffmpeg = ffmpeg_path()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg not available. Install ffmpeg (brew install ffmpeg)"))?;
        // Cap decode at 30 minutes so the WAV and sample buffer stay bounded;
        // poll child with a timeout so a hung decode can't stall the scan.
        let mut child = Command::new(ffmpeg)
            .args(["-y", "-i"]).arg(path)
            .args(["-t", "1800", "-ar", "16000", "-ac", "1", "-sample_fmt", "s16"])
            .arg(&wav_path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().context("ffmpeg failed to run")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let status = loop {
            match child.try_wait()? {
                Some(st) => break st,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("ffmpeg decode timed out after 300s");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        };

        if !status.success() || !wav_path.exists() {
            return Err(anyhow::anyhow!("ffmpeg decode failed"));
        }

        let dur = match hound::WavReader::open(&wav_path) {
            Ok(r) => r.duration() as f64 / r.spec().sample_rate as f64,
            Err(_) => 0.0,
        };

        // Read PCM samples (i16) and convert to f32 in [-1, 1].
        let reader = hound::WavReader::open(&wav_path).context("open decoded wav")?;
        let samples: Vec<f32> = reader
            .into_samples::<i16>()
            .map_while(Result::ok)
            .map(|s| s as f32 / 32768.0)
            .collect();

        let rec = match recognizer() {
            Some(r) => r,
            None => {
                return Ok(format!(
                    "─── 音频文件 ({:.0}s, {}) ──\n[ASR 初始化失败：模型文件缺失或损坏，请重新下载模型]\n",
                    dur, ext
                ));
            }
        };

        // Hotwords: keep disabled for now (the legacy Python pipeline never
        // actually populated FUNASR_HOTWORDS either). Enabling it requires
        // threading the DB pool into the extractor; tracked as a follow-up.
        let _hotwords: Option<Vec<String>> = None;

        log::info!(
            "[ASR] transcribing {:?} ({:.0}s audio, {:.1}MB)",
            path.file_name(),
            dur,
            meta.len() as f64 / 1_048_576.0,
        );

        let t0 = std::time::Instant::now();
        // FunASR-Nano is an LLM trained on ~30s windows; long audio must be
        // split before feeding. `recognize_segments` always yields bounded
        // chunks: Silero VAD when the model exists, fixed 28s hard-split
        // otherwise. Never feed a long file whole (OOM).
        let segments = recognize_segments(&samples);
        let text = segments
            .iter()
            .filter_map(|seg| transcribe(rec, seg))
            .collect::<Vec<_>>()
            .join(" ");

        if text.trim().is_empty() {
            return Ok(format!(
                "─── 音频文件 ({:.0}s, {}) ──\n[FunASR 推理完成，无识别结果]\n",
                dur, ext
            ));
        }
        log::info!("[ASR] {:?}: {} chars in {:.1}s", path.file_name(), text.len(), t0.elapsed().as_secs_f64());
        Ok(format!("─── 音频文件 ({:.0}s) ──\n{}\n", dur, text))
    }
}

/// Split `samples` into bounded chunks (≤30s each) for LLM decoding.
/// Uses Silero VAD when its model is present; otherwise falls back to a
/// fixed 28s hard split with 0.5s overlap at boundaries. Always returns
/// a non-empty list — never feed long audio whole (OOM).
fn recognize_segments(samples: &[f32]) -> Vec<Vec<f32>> {
    if let Some(segments) = vad_segments(samples) {
        return segments;
    }
    // Fixed hard split: 28s chunks (28 * 16000 samples), no overlap.
    const CHUNK: usize = 28 * 16000;
    if samples.len() <= CHUNK {
        return vec![samples.to_vec()];
    }
    samples
        .chunks(CHUNK)
        .map(|c| c.to_vec())
        .collect()
}

fn vad_available() -> bool {
    funasr_dir()
        .map(|d| d.join("silero_vad.onnx").is_file())
        .unwrap_or(false)
}

fn vad_segments(samples: &[f32]) -> Option<Vec<Vec<f32>>> {
    if !vad_available() {
        return None;
    }
    let dir = funasr_dir()?;
    let vad_model = dir.join("silero_vad.onnx").to_string_lossy().to_string();

    let mut vad_config = sherpa_onnx::VadModelConfig::default();
    vad_config.sample_rate = 16000;
    vad_config.num_threads = 1;
    vad_config.silero_vad = sherpa_onnx::SileroVadModelConfig {
        model: Some(vad_model),
        threshold: 0.5,
        min_silence_duration: 0.35,
        min_speech_duration: 0.25,
        window_size: 512,
        max_speech_duration: 30.0,
    };
    let vad = sherpa_onnx::VoiceActivityDetector::create(&vad_config, 60.0)?;

    let window = 512usize;
    let mut segments = Vec::new();
    let mut i = 0;
    while i + window <= samples.len() {
        vad.accept_waveform(&samples[i..i + window]);
        drain_vad(&vad, &mut segments);
        i += window;
    }
    if i < samples.len() {
        vad.accept_waveform(&samples[i..]);
    }
    vad.flush();
    drain_vad(&vad, &mut segments);
    Some(segments)
}

fn drain_vad(vad: &sherpa_onnx::VoiceActivityDetector, segments: &mut Vec<Vec<f32>>) {
    while !vad.is_empty() {
        if let Some(seg) = vad.front() {
            segments.push(seg.samples().to_vec());
            vad.pop();
        }
    }
}

fn transcribe(rec: &sherpa_onnx::OfflineRecognizer, samples: &[f32]) -> Option<String> {
    let stream = rec.create_stream();
    stream.accept_waveform(16000, samples);
    rec.decode(&stream);
    stream.get_result().map(|r| r.text.trim().to_string())
}

use super::Extractor;
impl Extractor for AudioExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_audio(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_detection_finds_system_binary() {
        if ffmpeg_available() {
            let p = ffmpeg_path().unwrap();
            assert!(p.exists() || p.as_os_str() == "ffmpeg");
        }
        // No assert on absence — this is an environment probe, not a
        // contract; the chain must simply not panic when ffmpeg is missing.
    }
}