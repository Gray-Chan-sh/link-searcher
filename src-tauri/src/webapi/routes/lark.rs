//! 飞书 bot 专用的持久化接口（`docs/LARK_BOT.md` §5.5 阶段三）。
//!
//! bot 是**独立进程**，只经 webapi 访问本库（§4.1 的数据流），因此这几个接口是它落地
//! "会话延续"与"事件幂等"的唯一入口：
//!
//! - `GET/PUT/DELETE /api/lark/binding` —— p2p `chat_id` ↔ 本库聊天会话
//! - `GET /api/lark/bindings`           —— 全量（诊断 / 将来的统一管理视图）
//! - `GET/POST /api/lark/chat-sessions` —— 某私聊的**多会话**列表 / 新建（§11）
//! - `PUT /api/lark/chat-sessions/active` —— 切换活动会话（§11）
//! - `POST /api/lark/chat-sessions/delete` —— 删除一条会话映射（§11）
//! - `POST /api/lark/seen`              —— 按 `message_id` 去重，返回是否首次
//! - `GET /api/integrations/lark`       —— **结构化**读飞书策略（`config.json`）
//! - `PUT /api/integrations/lark`       —— 结构化写策略（写后**重启 bot**）
//! - `GET /api/integrations/lark/status` —— 策略 + **运行态**（设置页 / CLI 诊断）
//! - `POST /api/integrations/lark/restart` —— 手动重启 bot
//! - `POST /api/integrations/lark/pairing/start|cancel` —— 进入 / 退出配对模式
//! - `POST /api/integrations/lark/pairing/consume` —— 配对消费（**白名单写入口**，
//!   必须先校验配对有效，见 `docs/LARK_BOT.md` §7.1 ⑥）
//! - `GET  /api/integrations/lark/self-check` —— 接入自检（App ID / 身份联网校验 / 卡片订阅）
//! - `POST /api/integrations/lark/bind/start|cancel|dismiss` —— 绑定飞书应用（`config init`）
//! - `GET  /api/integrations/lark/bind/status` —— 绑定会话进度（前端每秒拉）
//!
//! ⚠️ 绑定接口**不是完全对称**的：`start` 的 `existing` 模式要传 App Secret，
//! 只允许桌面端（Web 走自签证书的局域网通道，别把应用密钥往网络上递）。
//!
//! **传输对称**：这几个端点的实现与 Tauri 命令**共用** `commands/lark.rs` 的 `pub fn`，
//! 因此桌面与 Web 端行为一致（设置页两种模式都能用）。
//!
//! `created_at` / `updated_at` 一律**服务端**生成，bot 不需要（也不应该）自己算。

use axum::{
    extract::{Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::sessions::lark::{self, LarkBinding, DEFAULT_SEEN_CAP};
use crate::config::LarkIntegration;
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/lark/binding", get(get_binding).put(upsert_binding).delete(delete_binding))
        .route("/api/lark/bindings", get(list_bindings))
        .route("/api/lark/seen", post(mark_seen))
        .route(
            "/api/lark/chat-sessions",
            get(list_chat_sessions).post(create_chat_session),
        )
        .route("/api/lark/chat-sessions/active", put(set_active_session))
        .route(
            "/api/lark/chat-sessions/delete",
            post(delete_chat_session),
        )
        .route(
            "/api/integrations/lark",
            get(get_lark_config).put(put_lark_config),
        )
        .route("/api/integrations/lark/status", get(get_lark_status))
        .route("/api/integrations/lark/restart", post(restart_bot))
        .route(
            "/api/integrations/lark/pairing/start",
            post(start_pairing),
        )
        .route(
            "/api/integrations/lark/pairing/cancel",
            post(cancel_pairing),
        )
        .route(
            "/api/integrations/lark/pairing/consume",
            post(consume_pairing),
        )
        .route("/api/integrations/lark/self-check", get(self_check))
        .route("/api/integrations/lark/bind/start", post(bind_start))
        .route("/api/integrations/lark/bind/status", get(bind_status))
        .route("/api/integrations/lark/bind/cancel", post(bind_cancel))
        .route("/api/integrations/lark/bind/dismiss", post(bind_dismiss))
}

/// 结构化读飞书集成配置（`config.json` 的 `integrations.lark`）。
///
/// 为什么不复用 `/api/settings`：那是 DB `app_settings` 的**平铺字符串**表 —— 把
/// bool 塞成 `"true"`、把数组塞成 JSON 字符串，让每个消费者各自剥壳。飞书策略
/// 已迁到 `config.json`（`docs/LARK_BOT.md` §5.6），形状由**服务端**保证，
/// bot 只消费（这也是将来接入第二个平台时能复用的接缝）。
async fn get_lark_config() -> Result<Json<serde_json::Value>, ApiError> {
    let lark = crate::config::lark_integration();
    let mut value = serde_json::to_value(&lark).map_err(|e| ApiError { error: e.to_string() })?;
    // 由**服务端**把"配对是否此刻有效"裁决后再下发：bot 不碰时钟，那条 fail-closed
    // 规则（`0` / 已过去都算失效）只存在一份实现（`LarkIntegration::pairing_active`）。
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "pairing_active".to_string(),
            serde_json::json!(lark.pairing_active(chrono::Utc::now().timestamp())),
        );
    }
    Ok(Json(value))
}

/// 结构化**写**飞书集成配置（CLI / 远程客户端用；桌面设置页走 Tauri 命令）。
///
/// 与 `PUT /api/settings` 的区别：键名与类型由**服务端**保证，且**只碰
/// `integrations.lark` 这一块**（不是"整份重建"，不会顺带抹掉别人的字段）。
/// 写后**重启 bot** —— 策略是 bot 启动时读一次，不重启就等于没改。
async fn put_lark_config(
    State(state): State<ApiState>,
    Json(body): Json<LarkIntegration>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 与 Tauri 命令 `set_lark_config` **同一条实现**（含"写后广播 config-changed"与"重启 bot"）。
    crate::commands::lark::save_config(&state.app_handle, &body)
        .map_err(|e| ApiError { error: e })?;
    let saved = crate::config::lark_integration();
    serde_json::to_value(saved)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

#[derive(Deserialize)]
struct PairingConsumeBody {
    open_id: String,
}

#[derive(Deserialize)]
struct PairingStartBody {
    /// 配对时长（分钟）；缺省用 `DEFAULT_PAIRING_MINUTES`，服务端会夹到 `1..=60`。
    #[serde(default)]
    minutes: Option<i64>,
}

/// 策略 + **运行态**快照（`running` / 配对是否有效 / 剩余秒数 / 绑定数）。
///
/// 与 Tauri 命令 `get_lark_status` **同一实现**（`commands::lark::status_of`）——
/// 设置页在 Web 模式下也拿得到同样的形状。
async fn get_lark_status(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let status =
        crate::commands::lark::status_of(&app_state).map_err(|e| ApiError { error: e })?;
    serde_json::to_value(status)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

/// 手动重启 bot（手改 `config.json` 由热重载自动处理；这里是兜底入口）。
async fn restart_bot() -> Result<Json<serde_json::Value>, ApiError> {
    crate::commands::lark::restart();
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 进入配对模式（截止时刻由**服务端**算，不靠 UI 计时）。
async fn start_pairing(
    State(state): State<ApiState>,
    Json(body): Json<PairingStartBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::commands::lark::pairing_start(&state.app_handle, body.minutes)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 退出配对模式。
async fn cancel_pairing(State(state): State<ApiState>) -> Result<Json<serde_json::Value>, ApiError> {
    crate::commands::lark::pairing_cancel(&state.app_handle).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 配对消费：把发送者写入白名单，并**自动关闭**配对（§7.1 ⑥）。
///
/// ⚠️ **这是白名单写入口** —— 校验（配对确实开着且未过期）只有一份实现，在
/// [`crate::commands::lark::pairing_consume`] → [`crate::commands::lark::consume_pairing_into`]
/// （纯函数，有单测）。本端点**只做薄包装**，绝不自己判一遍：
/// 漏判就是"凭一个 token 往白名单里塞任意 `open_id`"的口子（等于绕过 L1 准入）。
///
/// 是否私聊由**闸门**判定（bot 只在 p2p 配对时才调这里）—— 服务端看不到 `chat_type`，
/// 因此不重复判定，但正因如此才更要把"配对是否有效"守死。
async fn consume_pairing(
    State(state): State<ApiState>,
    Json(body): Json<PairingConsumeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let allowed_users = crate::commands::lark::pairing_consume(&state.app_handle, &body.open_id)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(
        serde_json::json!({ "ok": true, "allowed_users": allowed_users }),
    ))
}

// ───────────────── 接入自检 / 绑定（实现见 `crate::lark_setup`） ─────────────────

/// 接入自检（**只读**）：`lark-cli` 在哪、绑了哪个 App、身份能否联网校验、卡片回调订阅。
///
/// 与 Tauri 命令 `lark_self_check` 同一条实现 —— 它要起子进程（`doctor` 约 2s、含联网），
/// 所以放进 `spawn_blocking`，别把只有几个 worker 的异步运行时堵住。
async fn self_check() -> Result<Json<serde_json::Value>, ApiError> {
    let check = tokio::task::spawn_blocking(crate::lark_setup::self_check)
        .await
        .map_err(|e| ApiError { error: e.to_string() })?;
    serde_json::to_value(check)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

/// 开始绑定飞书应用；**不阻塞**（进度靠 `GET …/bind/status` 拉）。
///
/// ⚠️ **`existing` 模式（要传 App Secret）只允许桌面端**：Web API 是给局域网 / 远程客户端用的
/// （自签证书），把应用密钥经网络送进来是**新增**的暴露面，而桌面端就在本机、走 Tauri IPC。
/// 这里显式拒绝 —— "传输对称"是为了**行为一致**，不是为了扩大攻击面。
async fn bind_start(
    Json(body): Json<crate::lark_setup::BindRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.mode.trim().eq_ignore_ascii_case("existing") {
        return Err(ApiError {
            error: "绑定已有应用（需要 App Secret）只能在桌面端设置页操作".into(),
        });
    }
    tokio::task::spawn_blocking(move || crate::lark_setup::bind_start(&body))
        .await
        .map_err(|e| ApiError { error: e.to_string() })?
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn bind_status() -> Result<Json<serde_json::Value>, ApiError> {
    serde_json::to_value(crate::lark_setup::bind_status())
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

async fn bind_cancel() -> Result<Json<serde_json::Value>, ApiError> {
    let cancelled = tokio::task::spawn_blocking(crate::lark_setup::bind_cancel)
        .await
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "ok": true, "cancelled": cancelled })))
}

async fn bind_dismiss() -> Result<Json<serde_json::Value>, ApiError> {
    crate::lark_setup::bind_dismiss();
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChatIdQuery {
    chat_id: String,
}

#[derive(Deserialize)]
struct BindingBody {
    chat_id: String,
    session_id: String,
    #[serde(default)]
    user_open_id: String,
}

#[derive(Deserialize)]
struct SeenBody {
    message_id: String,
    /// 去重环容量；缺省用 [`DEFAULT_SEEN_CAP`]。
    #[serde(default)]
    cap: Option<u64>,
}

async fn get_binding(
    State(state): State<ApiState>,
    Query(q): Query<ChatIdQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let found = lark::get_binding(&conn, &q.chat_id).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "binding": found })))
}

async fn upsert_binding(
    State(state): State<ApiState>,
    Json(body): Json<BindingBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.chat_id.trim().is_empty() || body.session_id.trim().is_empty() {
        return Err(ApiError {
            error: "chat_id and session_id are required".into(),
        });
    }
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let now = chrono::Utc::now().timestamp();
    let binding = LarkBinding {
        chat_id: body.chat_id,
        session_id: body.session_id,
        user_open_id: body.user_open_id,
        is_active: true,
        created_at: now,
        updated_at: now,
    };
    lark::upsert_binding(&conn, &binding).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true, "binding": binding })))
}

async fn delete_binding(
    State(state): State<ApiState>,
    Query(q): Query<ChatIdQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let deleted = lark::delete_binding(&conn, &q.chat_id).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn list_bindings(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let bindings = lark::list_bindings(&conn).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "bindings": bindings })))
}

/// 按 `message_id` 去重。`first_time = true` 表示**首次见到，应当处理**；
/// `false` 表示重投，**应当忽略**（§8 幂等）。
async fn mark_seen(
    State(state): State<ApiState>,
    Json(body): Json<SeenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.message_id.trim().is_empty() {
        return Err(ApiError {
            error: "message_id is required".into(),
        });
    }
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let now = chrono::Utc::now().timestamp();
    let cap = body.cap.unwrap_or(DEFAULT_SEEN_CAP);
    let first_time = lark::mark_seen(&conn, &body.message_id, now, cap)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "first_time": first_time })))
}

// ───────────────── 多会话（§11：/sessions · /switch · /new · /delete）─────────────────
//
// 语义（数据层见 `commands::sessions::lark`）：
// - 一个私聊**多行**，`is_active = 1` 的那条是当前会话（bot 的问答都落它）；
// - 列表 join `chat_history.json` 的 `origin = "lark"` 会话元数据（给标题），
//   排序以**绑定表的最近使用**为准（会话元数据里没有"被切换过"这个信号）；
// - `create` 顺带把新会话置为活动（bot 的 `/new` 一个请求完成）。

#[derive(Deserialize)]
struct ChatIdOnly {
    chat_id: String,
}

#[derive(Deserialize)]
struct CreateSessionBody {
    chat_id: String,
    #[serde(default)]
    user_open_id: String,
}

#[derive(Deserialize)]
struct SetActiveBody {
    chat_id: String,
    session_id: String,
}

#[derive(Deserialize)]
struct DeleteSessionBody {
    chat_id: String,
    session_id: String,
}

/// 会话列表：绑定行 + 标题（来自会话存储；取不到标题时退化为"新会话"）。
async fn list_chat_sessions(
    State(state): State<ApiState>,
    Query(q): Query<ChatIdOnly>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let rows = lark::list_chat_sessions(&conn, &q.chat_id).map_err(|e| ApiError { error: e })?;
    drop(conn);

    let metas = crate::commands::ai::list_chat_sessions_filtered(
        &app_state.data_dir,
        Some(crate::commands::ai::ORIGIN_LARK),
    );
    let title_of = |id: &str| {
        metas
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| "(找不到对应会话内容)".to_string())
    };
    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .map(|b| {
            serde_json::json!({
                "session_id": b.session_id,
                "title": title_of(&b.session_id),
                "is_active": b.is_active,
                "updated_at": b.updated_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

/// 新建一条 lark 会话并**置为活动**（bot `/new` 的一句话实现）。
async fn create_chat_session(
    State(state): State<ApiState>,
    Json(body): Json<CreateSessionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.chat_id.trim().is_empty() {
        return Err(ApiError { error: "chat_id is required".into() });
    }
    let app_state = state.app_handle.state::<AppState>();
    let session_id = crate::commands::ai::create_chat_session_impl(
        &app_state.data_dir,
        crate::commands::ai::ORIGIN_LARK,
    )
    .map_err(|e| ApiError { error: e })?;
    let now = chrono::Utc::now().timestamp();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    lark::upsert_binding(
        &conn,
        &crate::commands::sessions::lark::LarkBinding {
            chat_id: body.chat_id,
            session_id: session_id.clone(),
            user_open_id: body.user_open_id,
            is_active: true,
            created_at: now,
            updated_at: now,
        },
    )
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true, "session_id": session_id })))
}

/// 切换活动会话。目标不属于该私聊 → 404 语义（`ok: false`，不改动任何状态）。
async fn set_active_session(
    State(state): State<ApiState>,
    Json(body): Json<SetActiveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let now = chrono::Utc::now().timestamp();
    let ok = lark::set_active(&conn, &body.chat_id, &body.session_id, now)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": ok })))
}

/// 删除一条会话**映射**（内容保留在会话存储里）。
async fn delete_chat_session(
    State(state): State<ApiState>,
    Json(body): Json<DeleteSessionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let deleted = lark::delete_session(&conn, &body.chat_id, &body.session_id)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true, "deleted": deleted })))
}
