//! Ignored benchmark: remote embedding throughput, serial vs adaptive.
//!
//! `#[ignore]`d so CI never needs network or credentials. Run manually against
//! a real gateway and compare the printed items/s between the two modes:
//!
//! ```text
//! LS_CONFIG_DIR=/tmp/ls-bench-a LINK_SEARCHER_EMBED_ADAPTIVE=0 \
//!   BENCH_BASE_URL=http://192.168.1.100:8081 BENCH_KEY=... BENCH_MODEL=bge-m3 \
//!   BENCH_ITEMS=512 BENCH_CALL=256 \
//!   cargo test --test remote_embed_adaptive_bench -- --ignored --nocapture
//! ```
//!
//! Then repeat with `LINK_SEARCHER_EMBED_ADAPTIVE=1` and a fresh
//! `LS_CONFIG_DIR` (so the learned plan does not carry over).
use std::time::Instant;

#[test]
#[ignore = "manual network benchmark"]
fn remote_embed_serial_vs_adaptive() {
    let dir = std::env::var("LS_CONFIG_DIR").expect("set LS_CONFIG_DIR to a temp dir");
    let base = std::env::var("BENCH_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8081".into());
    let key = std::env::var("BENCH_KEY").unwrap_or_default();
    let model = std::env::var("BENCH_MODEL").unwrap_or_else(|_| "bge-m3".into());
    let items: usize = std::env::var("BENCH_ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(512);
    let call: usize = std::env::var("BENCH_CALL").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let mode = std::env::var("LINK_SEARCHER_EMBED_ADAPTIVE").unwrap_or_else(|_| "default".into());

    std::fs::create_dir_all(&dir).unwrap();
    let cfg = serde_json::json!({
        "data_dir": dir.clone(),
        "providers": [{
            "id": "bench",
            "name": "bench",
            "base_url": base,
            "api_key": key,
            "models": [{ "id": model, "model_type": "Embedding", "enabled": true }],
        }],
        "active_embedding_model_id": format!("bench:{model}"),
    });
    std::fs::write(
        std::path::Path::new(&dir).join("config.json"),
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();

    // Realistic Chinese document text (~700 chars), same for every input so the
    // gateway cost per item is stable across the two runs.
    let seed = "本系统用于本地文件的全文与语义检索，支持多种文档格式的解析、OCR 与向量化。";
    let text: String = seed.repeat(20);
    assert!(text.chars().count() > 500, "benchmark text too short");

    let mut done = 0usize;
    let started = Instant::now();
    while done < items {
        let n = call.min(items - done);
        let batch: Vec<String> = (0..n).map(|_| text.clone()).collect();
        let out = link_searcher_lib::ai::embed_batched(&batch, 64);
        assert_eq!(out.len(), n, "embed_batched must return one slot per input");
        let ok = out.iter().filter(|v| v.is_some()).count();
        assert_eq!(ok, n, "every input must embed (got {ok}/{n})");
        done += n;
    }
    let secs = started.elapsed().as_secs_f64().max(0.001);
    println!(
        "[BENCH] mode={mode} items={items} elapsed={secs:.1}s rate={:.2} items/s{}",
        items as f64 / secs,
        link_searcher_lib::ai::embed_plan_summary()
            .map(|p| format!("  ({p})"))
            .unwrap_or_default()
    );
}
