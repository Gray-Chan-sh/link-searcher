//! Web API HTTP 层冒烟测试：真实起 axum（含 `ConnectInfo`），跑通
//! 鉴权 + 单用户会话的完整链路。
//!
//! 为何必须有这一层测试：`session_guard` 依赖
//! `into_make_service_with_connect_info::<SocketAddr>()` 注入的来源 IP，
//! 一旦接线漏了/接错，**每个 API 请求都会 500**（ConnectInfo 抽取失败），
//! 而单元测试完全覆盖不到。这里同时验证：
//! - 中间件顺序：无/错 token → 401，且**不得创建会话**（auth 在外层）
//! - 正确 token → 200 并首次授予会话（证明来源 IP 抽取成功）
//! - `GET /api/session` 的 JSON 形状（is_owner / port / bind / lan_ip）
//! - `ping` 续租、`logout` 清会话、退出后可重新登入
//!
//! 注意：单用户会话是**进程级全局状态**，因此所有断言集中在同一个
//! `main()` 里串行执行，避免并行测试互相干扰。
//!
//! 另外本文件用 `harness = false`（见 `Cargo.toml` 的 `[[test]]`）：我们要构造
//! 真实 `AppHandle<Wry>`（`ApiState.app_handle` 是具体类型，MockRuntime 的
//! handle 不匹配），而 `tao` 在 Windows 上**禁止在非主线程创建事件循环**，
//! libtest 默认在线程里跑测试 → 必须让测试体跑在进程主线程上。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, atomic::AtomicBool};

use serde_json::Value;

use link_searcher_lib::db;
use link_searcher_lib::indexer::IndexerService;
use link_searcher_lib::scanner::Scanner;
use link_searcher_lib::search::IndexManager;
use link_searcher_lib::state::{AppState, ScanDelta};
use link_searcher_lib::webapi::state::ApiState;

// ---------------------------------------------------------------------------
// Temp dir with automatic cleanup
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ls_webapi_{prefix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// 鉴权中间件从 `app_settings['web_api_token']` 读取（生产由 `resolve_token`
/// 在启动时写入），因此测试也必须把它写进 DB。
const TEST_TOKEN: &str = "test-token-abc123";

/// 起一台只监听 127.0.0.1 的 axum 服务器（明文 HTTP，测试无需 TLS），
/// 返回 (地址, 关停信号, 线程句柄)。路由与生产环境完全一致。
fn spawn_api_server(tmp: &TempDir) -> (SocketAddr, tokio::sync::oneshot::Sender<()>, std::thread::JoinHandle<()>) {
    let db_path = tmp.path().join("test.db");
    let db_str = db_path.to_str().unwrap();
    let conn = rusqlite::Connection::open(db_str).unwrap();
    db::init_db(&conn).unwrap();
    drop(conn);
    let pool = db::get_pool(db_str).unwrap();
    {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params!["web_api_token", TEST_TOKEN],
        )
        .unwrap();
    }

    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir).unwrap();

    let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
    let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
    let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
    let (watcher_tx, _watcher_rx) = std::sync::mpsc::channel();

    let app_state = AppState::new(
        pool,
        im,
        indexer,
        scanner,
        Arc::new(AtomicBool::new(false)),           // is_scanning
        Arc::new(AtomicBool::new(false)),           // is_rebuilding
        Arc::new(AtomicBool::new(false)),           // cancel_scan
        Arc::new(AtomicBool::new(false)),           // is_restoring
        Arc::new(Mutex::new(ScanDelta::default())), // scan_delta
        data_dir,
        index_dir,
        db_path,
        watcher_tx,
        None,
    );

    // 用真实 Wry 运行时 + mock context（无窗口）：`ApiState.app_handle` 是
    // 具体类型 `AppHandle<Wry>`，MockRuntime 的 handle 类型不匹配。
    let app = tauri::Builder::default()
        .manage(app_state)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("failed to build mock tauri app");

    let token = TEST_TOKEN.to_string();
    let (event_tx, _) = tokio::sync::broadcast::channel::<(String, String)>(8);
    let api_state = ApiState {
        app_handle: app.handle().clone(),
        auth_token: Arc::new(token),
        cancel_token: Arc::new(tokio_util::sync::CancellationToken::new()),
        event_tx,
        dev_mode: false,
    };
    let router = link_searcher_lib::webapi::routes::build_router(api_state);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();

    let join = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind ephemeral port");
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("axum serve");
        });
    });

    let addr = addr_rx.recv().expect("server address");
    // `app` 可安全 drop：AppHandle 与 App 都是 Arc 支撑的。
    drop(app);
    (addr, shutdown_tx, join)
}

/// 发一个 HTTP 请求；4xx/5xx 也返回 (状态码, body) 而不 panic。
fn request(addr: SocketAddr, method: &str, path: &str, token: Option<&str>) -> (u16, String) {
    request_with_body(addr, method, path, token, None)
}

/// 同上，但可带 JSON body。
fn request_with_body(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let url = format!("http://{addr}{path}");
    let agent = ureq::AgentBuilder::new().build();
    let mut req = agent.request(method, &url).set("Connection", "close");
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let result = match body {
        Some(b) => req.set("Content-Type", "application/json").send_string(b),
        None => req.call(),
    };
    match result {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => panic!("request {method} {path} failed: {e}"),
    }
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("body is not JSON: {e}\n{body}"))
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

fn main() {
    let tmp = TempDir::new("http");
    let (addr, shutdown, join) = spawn_api_server(&tmp);
    let good = TEST_TOKEN;

    // 干净起点（会话是进程级全局状态）
    link_searcher_lib::webapi::session::clear(None);

    // ── 1. 无 token → 401，且不得创建会话（auth 必须在外层） ──
    let (status, _) = request(addr, "GET", "/api/version", None);
    assert_eq!(status, 401, "missing token must be 401");
    assert!(
        link_searcher_lib::webapi::session::current().is_none(),
        "an unauthenticated request must NOT create a session"
    );

    // ── 2. 错误 token → 401，同样不创建会话 ──
    let (status, _) = request(addr, "GET", "/api/version", Some("wrong-token"));
    assert_eq!(status, 401, "bad token must be 401");
    assert!(
        link_searcher_lib::webapi::session::current().is_none(),
        "a bad-token request must NOT create a session"
    );

    // ── 3. 正确 token → 200 且首次授予会话（证明 ConnectInfo 接线正确） ──
    let (status, body) = request(addr, "GET", "/api/version", Some(good));
    assert_eq!(status, 200, "valid token must be 200, body={body}");
    assert!(json(&body)["hash"].is_string(), "version payload shape");
    let sess = link_searcher_lib::webapi::session::current()
        .expect("first authenticated request must grant a session");
    assert_eq!(sess.ip, "127.0.0.1", "session IP comes from ConnectInfo (normalized)");

    // ── 4. GET /api/session 的 JSON 形状 ──
    let (status, body) = request(addr, "GET", "/api/session", Some(good));
    assert_eq!(status, 200);
    let v = json(&body);
    assert_eq!(v["active"], Value::Bool(true));
    assert_eq!(v["is_owner"], Value::Bool(true), "requester owns the session");
    assert_eq!(v["ip"], Value::String("127.0.0.1".into()));
    assert!(v["port"].is_number(), "port must be present");
    assert!(v["bind"].is_string(), "bind must be present");
    assert!(v["lan_ip"].is_string(), "lan_ip must be present");
    assert!(v["timeout_secs"].is_number(), "timeout_secs must be present");

    // ── 5. 心跳 ping：续租成功、会话仍在 ──
    let (status, body) = request(addr, "POST", "/api/session/ping", Some(good));
    assert_eq!(status, 200, "ping must succeed for the session owner");
    assert_eq!(json(&body)["active"], Value::Bool(true));
    assert!(link_searcher_lib::webapi::session::current().is_some());

    // ── 6. 主动退出：清空会话 ──
    let (status, body) = request(addr, "POST", "/api/session/logout", Some(good));
    assert_eq!(status, 200);
    assert_eq!(json(&body)["status"], Value::String("logged_out".into()));
    assert!(
        link_searcher_lib::webapi::session::current().is_none(),
        "logout must clear the session"
    );

    // ── 7. 退出后可重新登入（无会话 → 授予），且 is_owner 恢复 true ──
    let (status, body) = request(addr, "GET", "/api/session", Some(good));
    assert_eq!(status, 200, "after logout the same client may log in again");
    let v = json(&body);
    assert_eq!(v["active"], Value::Bool(true));
    assert_eq!(v["is_owner"], Value::Bool(true));

    // ── 8. 依赖中心（方案 A：Web 端可看状态 + 可安装） ──
    let (status, body) = request(addr, "GET", "/api/setup/status", Some(good));
    assert_eq!(status, 200, "setup status must be reachable: {body}");
    let v = json(&body);
    assert!(v["deps"].is_array(), "setup status must carry a deps array");
    assert!(
        v["all_recommended_ready"].is_boolean(),
        "setup status must carry all_recommended_ready"
    );
    assert!(v["data_dir"].is_string(), "setup status must carry data_dir");

    // 刷新后恢复进度条：install-status 必须是合法形状
    let (status, body) = request(addr, "GET", "/api/setup/install-status", Some(good));
    assert_eq!(status, 200);
    let v = json(&body);
    assert_eq!(v["installing"], Value::Bool(false), "nothing installing initially");
    assert!(v["dep"].is_null(), "dep is null when idle");

    // 空 dep / 未知 dep 必须被拒绝（不能静默落空）
    let (status, _) = request_with_body(
        addr,
        "POST",
        "/api/setup/install",
        Some(good),
        Some(r#"{"dep":""}"#),
    );
    assert_eq!(status, 400, "empty dep must be 400");

    let (status, body) = request_with_body(
        addr,
        "POST",
        "/api/setup/install",
        Some(good),
        Some(r#"{"dep":"definitely-not-a-dep"}"#),
    );
    assert_eq!(status, 400, "unknown dep must be 400: {body}");

    // 未安装任何依赖时取消 → 400「没有正在进行的安装」
    let (status, _) = request(addr, "POST", "/api/setup/cancel", Some(good));
    assert_eq!(status, 400, "cancel with no install in flight must be 400");

    // 收尾：清会话 + 关停服务器
    link_searcher_lib::webapi::session::clear(None);
    let _ = shutdown.send(());
    let _ = join.join();

    println!(
        "✓ Web API HTTP smoke test passed: 401(no token) / 401(bad token, no session) / \
         200 + session granted (ConnectInfo) / session JSON shape / ping / logout / re-login / \
         setup status + install-status reachable / install validation / cancel guard"
    );
}
