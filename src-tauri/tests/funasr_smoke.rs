//! End-to-end smoke test for FunASR-Nano via sherpa-onnx.
//!
//! Uses the real model files from `src-tauri/models/funasr` and a test wav
//! from `LINK_SEARCHER_TEST_WAV` (set by the run). Skips cleanly when either
//! is absent, so CI without the 950MB model download stays green.
use std::path::PathBuf;

fn model_present() -> bool {
    ["encoder_adaptor.int8.onnx", "llm.int8.onnx", "embedding.int8.onnx", "Qwen3-0.6B/tokenizer.json"]
        .iter()
        .all(|f| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("models/funasr")
                .join(f)
                .is_file()
        })
}

#[test]
fn funasr_nano_transcribes_smoke() {
    if !model_present() {
        eprintln!("SKIP: FunASR model not downloaded at src-tauri/models/funasr");
        return;
    }
    let wav = match std::env::var("LINK_SEARCHER_TEST_WAV") {
        Ok(p) if PathBuf::from(&p).is_file() => PathBuf::from(p),
        _ => {
            eprintln!("SKIP: set LINK_SEARCHER_TEST_WAV to an audio file");
            return;
        }
    };

    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/funasr");
    let p = |name: &str| model_dir.join(name).to_string_lossy().to_string();
    let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.funasr_nano = sherpa_onnx::OfflineFunASRNanoModelConfig {
        encoder_adaptor: Some(p("encoder_adaptor.int8.onnx")),
        llm: Some(p("llm.int8.onnx")),
        embedding: Some(p("embedding.int8.onnx")),
        tokenizer: Some(p("Qwen3-0.6B")),
        system_prompt: Some("You are a helpful assistant.".into()),
        user_prompt: Some("语音转写：".into()),
        max_new_tokens: 512,
        temperature: 1e-06,
        top_p: 0.8,
        seed: 42,
        ..Default::default()
    };
    config.decoding_method = Some("greedy_search".into());

    let rec = sherpa_onnx::OfflineRecognizer::create(&config)
        .expect("recognizer creation should succeed with real model files");

    let wave = sherpa_onnx::Wave::read(&wav.to_string_lossy()).expect("read test wav");
    let stream = rec.create_stream();
    stream.accept_waveform(wave.sample_rate(), wave.samples());
    rec.decode(&stream);
    let text = stream.get_result().map(|r| r.text).unwrap_or_default();

    assert!(!text.trim().is_empty(), "expected non-empty transcription, got empty");
    eprintln!("[SMOKE] {} -> {text}", wav.file_name().unwrap_or_default().to_string_lossy());
}

/// Full pipeline: ffmpeg decode -> sherpa-onnx recognition, through the
/// actual `AudioExtractor::extract_audio` used by the indexer.
#[test]
fn audio_extractor_full_pipeline() {
    if !model_present() {
        eprintln!("SKIP: FunASR model not downloaded at src-tauri/models/funasr");
        return;
    }
    let mp3 = match std::env::var("LINK_SEARCHER_TEST_MP3") {
        Ok(p) if PathBuf::from(&p).is_file() => PathBuf::from(p),
        _ => {
            eprintln!("SKIP: set LINK_SEARCHER_TEST_MP3 to an audio file");
            return;
        }
    };

    let extractor = link_searcher_lib::extractor::audio::AudioExtractor::new();
    let text = extractor.extract_audio(&mp3).expect("extract should not fail");
    assert!(
        text.contains("─── 音频文件"),
        "expected formatted audio header in output, got: {text:?}"
    );
    eprintln!("[PIPELINE] extract_audio output head: {}", &text[..text.len().min(200)]);
}

/// Long audio (VAD-segmented): a >1min file must not silently produce empty
/// text. Set LINK_SEARCHER_TEST_LONG_MP3 to a long audio file.
#[test]
fn audio_extractor_long_audio() {
    if !model_present() {
        eprintln!("SKIP: FunASR model not downloaded at src-tauri/models/funasr");
        return;
    }
    let mp3 = match std::env::var("LINK_SEARCHER_TEST_LONG_MP3") {
        Ok(p) if PathBuf::from(&p).is_file() => PathBuf::from(p),
        _ => {
            eprintln!("SKIP: set LINK_SEARCHER_TEST_LONG_MP3 to a long audio file");
            return;
        }
    };

    let extractor = link_searcher_lib::extractor::audio::AudioExtractor::new();
    let text = extractor.extract_audio(&mp3).expect("extract should not fail");
    let body = text.replace("─── 音频文件", "").trim().to_string();
    assert!(
        !body.contains("无识别结果"),
        "long audio should transcribe via VAD segments, got: {text:?}"
    );
    eprintln!("[LONG] extract_audio (long) output head: {}", &text[..text.floor_char_boundary(text.len().min(300))]);
}