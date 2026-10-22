use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::transcript::AudioSegment;

/// Whether `ffmpeg` is installed: just run `ffmpeg -version` once and see if it
/// resolves. On Windows the process PATH is refreshed from the registry at
/// startup and after package installs (see [`crate::process::refresh_path`]),
/// so a winget/choco/scoop install is picked up with no directory enumeration.
/// On macOS/Linux a GUI app may not inherit the shell PATH, so the common
/// install prefixes remain as a fallback.
fn find_ffmpeg_binary() -> Option<PathBuf> {
    if crate::process::probe_ok("ffmpeg", &["-version"]) {
        return Some(PathBuf::from("ffmpeg"));
    }
    #[cfg(not(target_os = "windows"))]
    for prefix in crate::process::UNIX_BIN_PREFIXES {
        let candidate = PathBuf::from(prefix).join("ffmpeg");
        if candidate.exists() && crate::process::probe_ok(&candidate, &["-version"]) {
            return Some(candidate);
        }
    }
    None
}

/// Caches only *positive* results: a "not found" is re-probed on the next call,
/// so installing ffmpeg while the app is running flips the status to ready
/// without a restart.
static FFMPEG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn ffmpeg_path() -> Option<&'static Path> {
    if let Some(p) = FFMPEG_PATH.get() {
        return Some(p.as_path());
    }
    let found = find_ffmpeg_binary()?;
    Some(FFMPEG_PATH.get_or_init(|| found).as_path())
}

/// Public check used by startup dependency detection and `check_dependencies`.
pub fn ffmpeg_available() -> bool {
    ffmpeg_path().is_some()
}

/// Resolved ffmpeg binary, if any. Used to inject `LS_FFMPEG` into the bot
/// sidecar (it deliberately does not link this crate, so it cannot reuse the
/// discovery logic itself).
pub fn ffmpeg_binary() -> Option<PathBuf> {
    ffmpeg_path().map(|p| p.to_path_buf())
}

/// FunASR-Nano ONNX model files that must be present in the model dir.
/// Layout matches `sherpa-onnx-funasr-nano-int8-2025-12-30` archive.
const MODEL_SUBDIR: &str = "models/funasr";
const MODEL_ARCHIVE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2";
const REQUIRED_FILES: [&str; 6] = [
    "encoder_adaptor.int8.onnx",
    "llm.int8.onnx",
    "embedding.int8.onnx",
    "Qwen3-0.6B/tokenizer.json",
    // sherpa-onnx 的 FunASR-Nano tokenizer 同时需要 vocab.json / merges.txt：
    // 缺了它们 recognizer 会**初始化失败**（`vocab.json does not exist`），
    // 于是整篇音频只剩一句"ASR 初始化失败"占位文本。2026-10-23 实测：
    // 依赖中心旧版本只下了 tokenizer.json，用户库里 86 个音频全部因此失败。
    "Qwen3-0.6B/vocab.json",
    "Qwen3-0.6B/merges.txt",
];

/// Speaker diarization model files (downloaded separately).
const SEGMENTATION_MODEL_FILE: &str = "seg.onnx";
const EMBEDDING_MODEL_FILE: &str = "emb.onnx";

/// Speaker diarization pipeline: segmentation + embedding extraction + clustering.
///
/// Only caches **successful** builds; the "models missing" case is decided before
/// touching this cell (see `diarized_slices`), so installing the models from the
/// dependency center takes effect without an app restart.
static SPEAKER_DIARIZER: OnceLock<Option<sherpa_onnx::OfflineSpeakerDiarization>> = OnceLock::new();

/// Resolve the FunASR model directory across dev and bundled runs.
///
/// Dev: `src-tauri/models/funasr`. Bundled: next to executable / data dir.
/// The model dir contains the sherpa-onnx-int8 archive contents (no venv).
fn funasr_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("LINK_SEARCHER_FUNASR_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent() {
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

/// Check a candidate dir AND its immediate subdirectories for required files.
/// The installer keeps files in `sherpa-onnx-funasr-nano-int8-2025-12-30/`
/// while dev/bundled copies may be flat.
fn find_model_dir(candidate: &Path) -> Option<PathBuf> {
    if model_present(candidate) {
        return Some(candidate.to_path_buf());
    }
    // Check immediate subdirectories (installed layout keeps files in subdir).
    if let Ok(entries) = std::fs::read_dir(candidate) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let p = e.path();
                if model_present(&p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn funasr_dir() -> Option<PathBuf> {
    funasr_candidates()
        .into_iter()
        .find_map(|d| find_model_dir(&d))
}

pub fn funasr_model_ready(data_dir: &Path) -> bool {
    let dir = data_dir.join(MODEL_SUBDIR);
    find_model_dir(&dir).is_some()
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

/// Speaker diarization model directory.
fn diarization_dir() -> Option<PathBuf> {
    funasr_dir().map(|d| d.join("models").join("diarization"))
}

fn diarization_model_present() -> bool {
    let dir = match diarization_dir() {
        Some(d) => d,
        None => return false,
    };
    dir.join(SEGMENTATION_MODEL_FILE).is_file()
        && dir.join(EMBEDDING_MODEL_FILE).is_file()
}

fn build_diarizer() -> Option<sherpa_onnx::OfflineSpeakerDiarization> {
    let dir = diarization_dir()?;
    let seg_path = dir.join(SEGMENTATION_MODEL_FILE).to_string_lossy().to_string();
    let emb_path = dir.join(EMBEDDING_MODEL_FILE).to_string_lossy().to_string();
    log::info!("[ASR] loading speaker diarization models (segmentation={}, embedding={})", seg_path, emb_path);
    let config = sherpa_onnx::OfflineSpeakerDiarizationConfig {
        segmentation: sherpa_onnx::OfflineSpeakerSegmentationModelConfig {
            pyannote: sherpa_onnx::OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(seg_path),
            },
            num_threads: 1,
            debug: false,
            provider: Some("cpu".into()),
        },
        embedding: sherpa_onnx::SpeakerEmbeddingExtractorConfig {
            model: Some(emb_path),
            ..Default::default()
        },
        clustering: sherpa_onnx::FastClusteringConfig {
            num_clusters: -1,  // auto-detect via threshold
            threshold: 0.5,
        },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };
    sherpa_onnx::OfflineSpeakerDiarization::create(&config)
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
        max_new_tokens: 1024,
        temperature: 1e-06,
        top_p: 0.8,
        seed: 42,
        language: None,
        itn: 0,
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

/// 转写结果：**纯正文** + **段级时间轴**。
///
/// 正文刻意不含任何 `[说话人N]` / `[00:00:12]` 标签：标签会进 tantivy /
/// `doc_chunks` / 向量，污染全文检索（`[说话人` 会被 jieba 切成词、`12`/`34`
/// 这类数字会变成检索词）。标签只在输出（预览/导出）时由
/// [`crate::transcript::render_tagged`] 拼出来。
#[derive(Debug, Clone, Default)]
pub struct AudioTranscript {
    pub text: String,
    pub segments: Vec<AudioSegment>,
}

impl Default for AudioExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioExtractor {
    pub fn new() -> Self { Self }

    /// 兼容入口：只要纯文本。
    pub fn extract_audio(&self, path: &Path) -> Result<String> {
        Ok(self.transcribe(path)?.text)
    }

    /// 转写音频，返回纯正文 + 段级时间轴。
    pub fn transcribe(&self, path: &Path) -> Result<AudioTranscript> {
        let meta = std::fs::metadata(path).context("audio stat")?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        if funasr_dir().is_none() {
            let probed = funasr_candidates()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ");
            return Ok(AudioTranscript {
                text: format!(
                    "─── 音频文件 ({}, {:.1}MB) ──\n[ASR 模型未就绪]\n已探测:\n  {}\n请在「设置 → 依赖中心」安装/修复 FunASR 模型（约 850MB）；\n若曾下载过，点一次「立即安装」即可补齐缺失文件（Qwen3-0.6B 下需 tokenizer.json + vocab.json + merges.txt）\n",
                    ext,
                    meta.len() as f64 / 1_048_576.0,
                    probed,
                ),
                segments: Vec::new(),
            });
        }

        // Decode to 16kHz mono WAV (full length; no truncation).
        let tmp = crate::scanner::helpers::TempDir::new("ls_audio")?;
        let wav_path = tmp.path().join("audio.wav");
        let ffmpeg = ffmpeg_path()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg not available. Install ffmpeg (brew install ffmpeg)"))?;
        // 先把音频"洗"一遍：高通去低频轰鸣 + FFT 降噪 + 动态归一化。
        //
        // 2026-10-23 实测（一段嘈杂仲裁录音，全片底噪 -27dB、无静音段）：
        // 不加滤镜只识别出 17 段/435 字（大量"PEACE.反益"、"一颗星的星星"之类
        // 幻觉）；加滤镜后 102 段/3867 字，句子连贯通顺。底噪被压掉后，VAD 与
        // 说话人分离也不再被噪声淹没（同一段音频分离轮次 107 → 正常量级）。
        //
        // 老 ffmpeg（缺 afftdn/dynaudnorm）会让解码直接失败 → 自动回退到不加
        // 滤镜的原样解码，保证"能跑"优先于"更好"。
        let decoded = decode_wav(ffmpeg, path, &wav_path, true).and_then(|ok| {
            if ok && wav_path.exists() {
                return Ok(());
            }
            log::warn!("[ASR] 降噪解码失败，回退到原样解码（可能缺少 afftdn/dynaudnorm 滤镜）");
            let _ = std::fs::remove_file(&wav_path);
            decode_wav(ffmpeg, path, &wav_path, false).map(|_| ())
        });
        if let Err(e) = decoded {
            return Err(e);
        }
        if !wav_path.exists() {
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
                return Ok(AudioTranscript {
                    text: format!(
                        "─── 音频文件 ({:.0}s, {}) ──\n[ASR 初始化失败：模型文件缺失或损坏，请重新下载模型]\n",
                        dur, ext
                    ),
                    segments: Vec::new(),
                });
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
        // split before feeding. `recognize_slices` always yields bounded
        // slices: global speaker diarization when its models exist, Silero VAD
        // next, fixed 28s hard-split otherwise. Never feed a long file whole
        // (OOM: an 18.7GB RSS blowup was measured on a 34-minute recording).
        //
        // Diarization runs **once on the whole waveform** (not per slice):
        // per-slice clustering restarts its speaker numbering every chunk, so
        // "说话人0" in chunk 1 and chunk 5 could be different people.
        let slices = recognize_slices(&samples);
        let mut parts: Vec<String> = Vec::with_capacity(slices.len());
        let mut segments: Vec<AudioSegment> = Vec::with_capacity(slices.len());
        for slice in &slices {
            let Some(raw) = transcribe(rec, &samples[slice.start..slice.end]) else {
                continue;
            };
            // Sanitize **per segment** so the timeline text and the joined body
            // stay byte-identical: `sanitize_text` is idempotent, so the outer
            // pass in `extract_text_with_meta` becomes a no-op for audio.
            // 先截断复读退化（见 transcript::collapse_repetition），再按切片时长
            // 兜底限字：复读会漂移到"几乎不成周期"的形态（实测 811 字里是
            // "osuoro…" + "ususus…" 两截不同的循环），单靠 n-gram 压不住。
            let text = cap_by_duration(
                &crate::extractor::sanitize_text(&crate::transcript::collapse_repetition(&raw)),
                slice.end - slice.start,
            );
            if text.is_empty() {
                continue;
            }
            parts.push(text.clone());
            segments.push(AudioSegment {
                start_ms: samples_to_ms(slice.start),
                end_ms: samples_to_ms(slice.end),
                speaker: slice.speaker,
                text,
            });
        }
        let text = parts.join("\n");

        if text.trim().is_empty() {
            return Ok(AudioTranscript {
                text: format!(
                    "─── 音频文件 ({:.0}s, {}) ──\n[FunASR 推理完成，无识别结果]\n",
                    dur, ext
                ),
                segments: Vec::new(),
            });
        }
        log::info!(
            "[ASR] {:?}: {} chars / {} segments in {:.1}s",
            path.file_name(),
            text.len(),
            segments.len(),
            t0.elapsed().as_secs_f64()
        );
        // 正文保持纯文本（无 `─── 音频文件 ──` 头、无 `[说话人N]`）：
        // 头部/标签都会进索引，搜「音频文件」就能命中全部音频。
        Ok(AudioTranscript { text, segments })
    }
}

/// 一个待转写的切片（绝对采样点偏移）+ 它的说话人。
#[derive(Debug, Clone, Copy)]
struct Slice {
    start: usize,
    end: usize,
    speaker: Option<i32>,
}

/// 一条说话人轮次（绝对采样点）。
#[derive(Debug, Clone, Copy)]
struct Turn {
    start: usize,
    end: usize,
    speaker: i32,
}

/// 16kHz 采样点 → 毫秒。
fn samples_to_ms(sample: usize) -> i64 {
    (sample as f64 * 1000.0 / 16000.0).round() as i64
}

/// ASR 前的音频清洗链（见 `transcribe` 里的实测记录）。
const DENOISE_FILTER: &str = "highpass=f=80,afftdn=nr=12:nf=-40,dynaudnorm=f=200:g=15";

/// 用 ffmpeg 把音频解码成 16kHz 单声道 WAV；`denoise` 决定是否套 [`DENOISE_FILTER`]。
///
/// 返回 `Ok(false)` 表示 ffmpeg 非零退出（调用方据此决定是否回退到不降噪）。
/// 超时 600s（单文件音轨再长也不该编码超过这个时间），超时直接报错。
fn decode_wav(
    ffmpeg: &Path,
    src: &Path,
    dst: &Path,
    denoise: bool,
) -> Result<bool> {
    let mut cmd = crate::process::new(ffmpeg);
    cmd.args(["-y", "-i"]).arg(src);
    if denoise {
        cmd.args(["-af", DENOISE_FILTER]);
    }
    let mut child = cmd
        .args(["-ar", "16000", "-ac", "1", "-sample_fmt", "s16"])
        .arg(dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("ffmpeg failed to run")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("ffmpeg decode timed out after 600s");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    };
    Ok(status.success())
}

/// 按切片时长给转写文本设上限（LLM-ASR 复读兜底）。
///
/// 正常语速上限约 12 字/秒（中文极快也就 8~9 字/秒），再加 30 字余量。
/// 超过这个量只能是复读/幻觉 —— 实测一段 3.2 秒的切片吐了 277 字。
fn cap_by_duration(text: &str, sample_count: usize) -> String {
    let secs = sample_count as f64 / 16000.0;
    let max_chars = (secs * 12.0).ceil() as usize + 30;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let mut out: String = chars[..max_chars].iter().collect();
    out.push('…');
    log::debug!(
        "[ASR] capped degenerate segment: {} -> {} chars ({:.1}s)",
        chars.len(),
        max_chars,
        secs
    );
    out
}

/// 把整段音频切成有界切片，尽量带上说话人。
///
/// **转写切片**沿用既有证明可用的口径：Silero VAD（≤30s）→ 固定 28s 硬切。
/// 说话人**不参与切分**，只按时间重叠给每个切片打「主导说话人」。
///
/// 为什么不用说话人轮次当转写切片（2026-10-23 实测）：FunASR-Nano 是 0.6B 的
/// LLM，喂 1~2 秒的短轮次会大量幻觉/复读 —— 同一段 100 秒通话，按轮次切出 34 段
/// 得到 6284 字（其中一段 7 秒吐了 2082 字 "your thoughts on your thoughts…"），
/// 而按 VAD 切只有 4 段 417 字。所以：**切分用 VAD，说话人靠时间对齐**。
fn recognize_slices(samples: &[f32]) -> Vec<Slice> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut slices = match vad_slices(samples) {
        Some(s) => s,
        None => hard_split_slices(samples),
    };
    if let Some(turns) = diarization_turns(samples) {
        for slice in &mut slices {
            slice.speaker = dominant_speaker(&turns, slice.start, slice.end);
        }
    }
    slices
}

/// 全量跑一次 diarizer，得到**全局一致**的说话人时间线（只算时间，不切转写）。
///
/// 关键：不能逐 VAD 段调用 `process()` —— 每次调用内部聚类独立，说话人编号
/// 会按段重置（第 1 段的"说话人0"和第 5 段的"说话人0"可能是两个人）。
fn diarization_turns(samples: &[f32]) -> Option<Vec<Turn>> {
    // 先查模型在不在，**再**碰 OnceLock：模型是「依赖中心」里可随时补装的，
    // 若把"缺失"缓存进 OnceLock，装完模型后必须重启应用才生效。
    if !diarization_model_present() {
        return None;
    }
    let diarizer = SPEAKER_DIARIZER.get_or_init(build_diarizer).as_ref()?;

    let t0 = std::time::Instant::now();
    let result = diarizer.process(samples)?;
    let raw = result.sort_by_start_time();
    let turns: Vec<Turn> = raw
        .iter()
        .filter_map(|t| {
            let start = (t.start.max(0.0) * 16000.0).round() as usize;
            let end = ((t.end * 16000.0).round() as usize).min(samples.len());
            (start < end).then_some(Turn { start, end, speaker: t.speaker })
        })
        .collect();
    log::info!(
        "[ASR] diarization: {} speaker turns in {:.1}s ({:.0}s audio)",
        turns.len(),
        t0.elapsed().as_secs_f64(),
        samples.len() as f64 / 16000.0
    );
    if turns.is_empty() { None } else { Some(turns) }
}

/// 给一个转写切片挑「主导说话人」：重叠时长最多的那个。
///
/// 切片与轮次没有重叠（整段静音/噪声）时退化为**时间上最近**的轮次，
/// 避免这些段落到"无说话人"而让输出忽有忽无。
fn dominant_speaker(turns: &[Turn], start: usize, end: usize) -> Option<i32> {
    let mut best: Option<(i32, usize)> = None;
    for t in turns {
        let overlap = end.min(t.end).saturating_sub(start.max(t.start));
        if overlap > 0 && best.is_none_or(|(_, b)| overlap > b) {
            best = Some((t.speaker, overlap));
        }
    }
    if let Some((speaker, _)) = best {
        return Some(speaker);
    }
    turns
        .iter()
        .min_by_key(|t| {
            if t.start >= end {
                t.start - end
            } else {
                start.saturating_sub(t.end)
            }
        })
        .map(|t| t.speaker)
}

/// 把 `[start, end)` 再切成 ≤28s 的片段（带 1s 重叠，避免切断词）。
///
/// 说话人轮次通常只有几秒，但一段连续发言可能远超 30s —— 那种情况必须再切，
/// 否则整段喂进 30s 窗口的模型会静默返回空文本。
fn split_long(start: usize, end: usize, limit: usize) -> Vec<(usize, usize)> {
    const CHUNK: usize = 28 * 16000;
    const OVERLAP: usize = 16000;
    let end = end.min(limit);
    if start >= end {
        return Vec::new();
    }
    if end - start <= CHUNK {
        return vec![(start, end)];
    }
    let mut out = Vec::new();
    let mut s = start;
    while s < end {
        let e = (s + CHUNK).min(end);
        out.push((s, e));
        if e == end {
            break;
        }
        s = e.saturating_sub(OVERLAP).max(s + 1);
    }
    out
}

fn vad_available() -> bool {
    funasr_dir()
        .map(|d| d.join("silero_vad.onnx").is_file())
        .unwrap_or(false)
}

fn vad_slices(samples: &[f32]) -> Option<Vec<Slice>> {
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
    let mut slices = Vec::new();
    let mut i = 0;
    while i + window <= samples.len() {
        vad.accept_waveform(&samples[i..i + window]);
        drain_vad(&vad, &mut slices, i + window);
        i += window;
    }
    if i < samples.len() {
        vad.accept_waveform(&samples[i..]);
        drain_vad(&vad, &mut slices, samples.len());
    }
    vad.flush();
    drain_vad(&vad, &mut slices, samples.len());
    if slices.is_empty() { None } else { Some(slices) }
}

/// `SpeechSegment::start()` 是**相对于截至目前喂入的全部输入**的采样点偏移，
/// 因此可以直接当绝对时间用；这里仍按「已喂入长度 `fed`」做一次边界钳制，
/// 防止异常值越界到 `samples` 之外。
fn drain_vad(vad: &sherpa_onnx::VoiceActivityDetector, slices: &mut Vec<Slice>, fed: usize) {
    while !vad.is_empty() {
        if let Some(seg) = vad.front() {
            let n = seg.n().max(0) as usize;
            let start = (seg.start().max(0) as usize).min(fed);
            let end = start.saturating_add(n).min(fed);
            if start < end {
                slices.push(Slice { start, end, speaker: None });
            }
            vad.pop();
        }
    }
}

/// 兜底：VAD 与分离模型都不可用时的固定 28s 硬切（1s 重叠）。
fn hard_split_slices(samples: &[f32]) -> Vec<Slice> {
    split_long(0, samples.len(), samples.len())
        .into_iter()
        .map(|(start, end)| Slice { start, end, speaker: None })
        .collect()
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
    fn samples_to_ms_matches_16khz() {
        assert_eq!(samples_to_ms(0), 0);
        assert_eq!(samples_to_ms(8_000), 500);
        assert_eq!(samples_to_ms(16_000), 1_000);
        assert_eq!(samples_to_ms(16_001), 1_000);
    }

    #[test]
    fn split_long_keeps_short_turns_whole() {
        // 5 秒的说话人轮次不该被再切
        assert_eq!(split_long(0, 5 * 16_000, 5 * 16_000), vec![(0, 5 * 16_000)]);
    }

    #[test]
    fn split_long_bounds_every_chunk_to_28s() {
        let total = 61 * 16_000;
        let out = split_long(0, total, total);
        assert!(out.len() >= 3, "61s 应切成 3 段以上: {}", out.len());
        for (s, e) in &out {
            assert!(*e > *s);
            assert!(e - s <= 28 * 16_000, "单段超过 28s: {}", e - s);
            assert!(*e <= total);
        }
        assert_eq!(out.first().map(|x| x.0), Some(0));
        assert_eq!(out.last().map(|x| x.1), Some(total));
        // 相邻段有重叠（避免切断词）
        assert!(out[1].0 < out[0].1);
    }

    #[test]
    fn split_long_rejects_empty_range() {
        assert!(split_long(100, 100, 1000).is_empty());
        assert!(split_long(200, 100, 1000).is_empty());
    }

    #[test]
    fn hard_split_covers_whole_buffer_without_speaker() {
        let samples = vec![0.0f32; 30 * 16_000];
        let slices = hard_split_slices(&samples);
        assert_eq!(slices.len(), 2, "30s 缓冲按 28s 切 → 2 段");
        assert!(slices.iter().all(|s| s.speaker.is_none()));
        assert_eq!(slices[0].start, 0);
        assert_eq!(slices.last().unwrap().end, samples.len());
        assert_eq!(samples_to_ms(slices[1].start), 27_000, "1s 重叠");
    }

    #[test]
    fn recognize_slices_handles_empty_audio() {
        assert!(recognize_slices(&[]).is_empty());
    }

    #[test]
    fn cap_by_duration_limits_degenerate_text() {
        // 实测：3.2 秒切片吐出 277 字 → 上限 12*3.2≈39 + 30 = 69
        let text = "都".repeat(277);
        let out = cap_by_duration(&text, 3 * 16_000 + 3_200);
        assert_eq!(out.chars().count(), 70, "69 字 + 省略号");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn cap_by_duration_keeps_normal_text() {
        // 10 秒 60 字（正常语速）→ 上限 150，不该动
        let text = "这是一个正常长度的转写片段".repeat(5);
        assert_eq!(cap_by_duration(&text, 10 * 16_000), text);
    }

    #[test]
    fn dominant_speaker_picks_largest_overlap() {
        let turns = [
            Turn { start: 0, end: 16_000, speaker: 0 },
            Turn { start: 16_000, end: 48_000, speaker: 1 },
        ];
        // 跨两个轮次 → 重叠更长的那个
        assert_eq!(dominant_speaker(&turns, 8_000, 40_000), Some(1));
        // 完全落在说话人0
        assert_eq!(dominant_speaker(&turns, 0, 16_000), Some(0));
        // 没有任何重叠 → 时间上最近的轮次（避免输出忽有忽无）
        assert_eq!(dominant_speaker(&turns, 60_000, 70_000), Some(1));
        assert_eq!(dominant_speaker(&[], 0, 10), None);
    }

    #[test]
    fn ffmpeg_detection_finds_system_binary() {
        if ffmpeg_available() {
            let p = ffmpeg_path().unwrap();
            assert!(p.exists() || p.as_os_str() == "ffmpeg");
        }
        // No assert on absence — this is an environment probe, not a
        // contract; the chain must simply not panic when ffmpeg is missing.
    }

    #[test]
    fn diarization_models_resolve_from_funasr_dir() {
        // 说话人分离模型必须能从 funasr_dir()/models/diarization 找到。
        // CI / 未下载模型的环境：funasr_dir 为空时软跳过（下方对模型缺失同样 soft-fail）。
        let Some(_funasr) = funasr_dir() else {
            eprintln!("FunASR model dir not found — skipping (CI env)");
            return;
        };
        let diar = diarization_dir().expect("diarization_dir must resolve once funasr_dir exists");
        let seg = diar.join(SEGMENTATION_MODEL_FILE);
        let emb = diar.join(EMBEDDING_MODEL_FILE);
        if !seg.is_file() || !emb.is_file() {
            eprintln!("MISSING diarization models — expected at {diar:?}");
            eprintln!("  seg: {}", seg.display());
            eprintln!("  emb: {}", emb.display());
            return; // soft-fail: models may not be downloaded in CI
        }
        assert!(diarization_model_present(), "diarization_model_present should be true");
        assert!(build_diarizer().is_some(), "diarizer should initialize");
    }
}