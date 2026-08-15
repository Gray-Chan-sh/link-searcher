//! chat_stream wire-level tests against a local mock SSE gateway.
//!
//! `LS_CONFIG_DIR` points the global config at a throwaway dir so these
//! tests never touch the user's real config.json.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use link_searcher_lib::ai::chat_stream;

// cargo runs #[test]s in parallel; the global config + LS_CONFIG_DIR are
// process-wide, so all gateway tests serialize on this lock.
static CFG_LOCK: Mutex<()> = Mutex::new(());

static PORT_SEQ: AtomicUsize = AtomicUsize::new(48000);

struct MockGateway {
    port: u16,
    addr: String,
    handle: thread::JoinHandle<()>,
    // 用 channel 让 handler 知道已收到请求（便于验证请求体）
    req_body: std::sync::mpsc::Receiver<String>,
}

impl MockGateway {
    /// Start a one-shot server: for each accepted connection, read the HTTP
    /// request, capture its body, then respond per `make_body`.
    fn start(make_body: impl Fn(&str) -> Vec<u8> + Send + 'static) -> Self {
        let port = PORT_SEQ.fetch_add(1, Ordering::SeqCst) as u16;
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind mock gateway");
        listener.set_nonblocking(true).expect("nonblocking");
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            // 单连接足够：chat_stream 只发一个请求。
            for _ in 0..30 {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        // 读到 \r\n\r\n 即请求头结束；随后读 body（Content-Length）。
                        loop {
                            let n = sock.read(&mut chunk).unwrap_or(0);
                            if n == 0 { break; }
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = find_header_end(&buf) {
                                let head = String::from_utf8_lossy(&buf[..pos]);
                                let len = content_length(&head);
                                while buf.len() < pos + 4 + len {
                                    let n = sock.read(&mut chunk).unwrap_or(0);
                                    if n == 0 { break; }
                                    buf.extend_from_slice(&chunk[..n]);
                                }
                                break;
                            }
                        }
                        if let Some(pos) = find_header_end(&buf) {
                            let _ = tx.send(String::from_utf8_lossy(&buf[pos + 4..]).into_owned());
                        }
                        let body = make_body(&String::from_utf8_lossy(&buf));
                        let _ = sock.write_all(&body);
                        let _ = sock.flush();
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
        });
        Self {
            port,
            addr: format!("http://127.0.0.1:{port}/v1"),
            handle,
            req_body: rx,
        }
    }

    fn req_body(&self, timeout: std::time::Duration) -> Option<String> {
        self.req_body.recv_timeout(timeout).ok()
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Write a config with one LLM provider pointing at the gateway.
fn write_mock_config(dir: &std::path::Path, base_url: &str) {
    std::fs::create_dir_all(dir).expect("create cfg dir");
    let cfg = serde_json::json!({
        "providers": [{
            "id": "mock1",
            "name": "mock",
            "base_url": base_url,
            "api_key": "test-key",
            "models": [{ "id": "mock-llm", "model_type": "Llm", "enabled": true }]
        }],
        "active_llm_model_id": "mock1:mock-llm",
        "active_embedding_model_id": "",
        "semantic_weight": 0.3
    });
    std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap())
        .expect("write config.json");
}

fn sse_response(chunks: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for c in chunks {
        body.push_str(c);
        body.push_str("\n\n");
    }
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    resp.push_str(&body);
    resp.into_bytes()
}

fn sse_delta(text: &str) -> String {
    serde_json::json!({"choices":[{"delta":{"content":text}}]}).to_string()
}

// ---------------------------------------------------------------------------
// 正常流：多个 delta + [DONE]
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_happy_path_aggregates_deltas_until_done() {
    let _g = CFG_LOCK.lock().unwrap();
    let gw = MockGateway::start(|_| sse_response(&[
        format!("data: {}", sse_delta("你好")).as_str(),
        format!("data: {}", sse_delta("世界")).as_str(),
        "data: [DONE]",
    ]));
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let mut deltas = Vec::new();
    let out = chat_stream("sys", "user", &mut |d: &str| deltas.push(d.to_string()));
    assert_eq!(out.text.as_deref(), Some("你好世界"));
    assert_eq!(deltas, vec!["你好".to_string(), "世界".to_string()]);
    assert!(!out.cancelled);
    assert!(out.took_ms > 0);

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 网关忽略 stream:true → 首行非 data: → 解析为普通 JSON 并作为整段 delta
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_falls_back_to_plain_json_body() {
    let _g = CFG_LOCK.lock().unwrap();
    let body = serde_json::json!({"choices":[{"message":{"role":"assistant","content":"非流式回答"}}]});
    let mut raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.to_string().len()
    );
    raw.push_str(&body.to_string());
    let gw = MockGateway::start(move |_| raw.clone().into_bytes());

    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let mut deltas = Vec::new();
    let out = chat_stream("sys", "user", &mut |d: &str| deltas.push(d.to_string()));
    assert_eq!(out.text.as_deref(), Some("非流式回答"));
    assert_eq!(deltas, vec!["非流式回答".to_string()]);

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 畸形 SSE 帧（非 JSON）被跳过，后续 delta 仍累积
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_skips_malformed_frames_and_keeps_valid_ones() {
    let _g = CFG_LOCK.lock().unwrap();
    let gw = MockGateway::start(|_| sse_response(&[
        "data: not-json{{{",
        format!("data: {}", sse_delta("仍")).as_str(),
        "data: [DONE]",
    ]));
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let mut deltas = Vec::new();
    let out = chat_stream("sys", "user", &mut |d: &str| deltas.push(d.to_string()));
    assert_eq!(out.text.as_deref(), Some("仍"), "畸形帧应被静默跳过");

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 流中断（读不到 [DONE] 即断开）→ 返回已收文本
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_returns_partial_text_on_truncated_stream() {
    let _g = CFG_LOCK.lock().unwrap();
    // 只发一个 delta，不发 [DONE]，随后连接关闭
    let gw = MockGateway::start(|_| sse_response(&[
        format!("data: {}", sse_delta("部分")).as_str(),
    ]));
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let mut deltas = Vec::new();
    let out = chat_stream("sys", "user", &mut |d: &str| deltas.push(d.to_string()));
    assert_eq!(out.text.as_deref(), Some("部分"), "断流应保留已收文本");

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// HTTP 错误（500）→ text=None
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_degrades_to_none_on_http_error() {
    let _g = CFG_LOCK.lock().unwrap();
    let gw = MockGateway::start(|_| b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\n\r\n{}".to_vec());
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let out = chat_stream("sys", "user", &mut |_| {});
    assert!(out.text.is_none(), "500 应返回 None 而非文本");

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 未配置（LS_CONFIG_DIR 指向无有效 provider 的配置）→ text=None
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_degrades_when_unconfigured() {
    let _g = CFG_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join("ls-cfg-empty");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("config.json"), "{}").unwrap();
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let out = chat_stream("sys", "user", &mut |_| {});
    assert!(out.text.is_none(), "空配置应返回 None");

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 请求体校验：model 字段来自 active 模型 id，stream=true
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_sends_correct_request_payload() {
    let _g = CFG_LOCK.lock().unwrap();
    let gw = MockGateway::start(|_| sse_response(&["data: [DONE]"]));
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", gw.port));
    write_mock_config(&tmp, &gw.addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let _ = chat_stream("sys", "user", &mut |_| {});
    let body = gw.req_body(std::time::Duration::from_secs(5)).expect("request captured");
    let json: serde_json::Value = serde_json::from_str(&body).expect("request body is json");
    assert_eq!(json["model"], "mock-llm");
    assert_eq!(json["stream"], true);
    assert_eq!(json["messages"][0]["content"], "sys");
    assert_eq!(json["messages"][1]["content"], "user");

    let _ = gw.handle.join();
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 连接拒绝（端口无人监听）→ text=None，不 panic
// ---------------------------------------------------------------------------
#[test]
fn chat_stream_conn_refused_degrades_gracefully() {
    let _g = CFG_LOCK.lock().unwrap();
    // 绑定后立即释放拿到一个空闲端口，无人监听
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let addr = format!("http://127.0.0.1:{port}/v1");
    let tmp = std::env::temp_dir().join(format!("ls-cfg-{}", port));
    write_mock_config(&tmp, &addr);
    unsafe { std::env::set_var("LS_CONFIG_DIR", &tmp) };

    let out = chat_stream("sys", "user", &mut |_| {});
    assert!(out.text.is_none(), "连接拒绝应返回 None 而非 panic");
    assert_eq!(out.took_ms, 0);

    let _ = std::fs::remove_dir_all(&tmp);
}