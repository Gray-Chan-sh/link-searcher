//! 飞书 bot 专用的持久化接口（`docs/LARK_BOT.md` §5.5 阶段三）。
//!
//! bot 是**独立进程**，只经 webapi 访问本库（§4.1 的数据流），因此这几个接口是它落地
//! "会话延续"与"事件幂等"的唯一入口：
//!
//! - `GET/PUT/DELETE /api/lark/binding` —— p2p `chat_id` ↔ 本库聊天会话
//! - `GET /api/lark/bindings`           —— 全量（诊断 / 将来的统一管理视图）
//! - `POST /api/lark/seen`              —— 按 `message_id` 去重，返回是否首次
//! - `GET /api/integrations/lark`       —— **结构化**读飞书策略（`config.json`）
//! - `PUT /api/integrations/lark`       —— 结构化写策略（写后**重启 bot**）
//! - `POST /api/integrations/lark/pairing/consume` —— 配对消费（**白名单写入口**，
//!   必须先校验配对有效，见 `docs/LARK_BOT.md` §7.1 ⑥）
//!
//! `created_at` / `updated_at` 一律**服务端**生成，bot 不需要（也不应该）自己算。

use axum::{
    extract::{Query, State},
    routing::{get, post},
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
        .route(
            "/api/lark/binding",
            get(get_binding).put(upsert_binding).delete(delete_binding),
        )
        .route("/api/lark/bindings", get(list_bindings))
        .route("/api/lark/seen", post(mark_seen))
        .route(
            "/api/integrations/lark",
            get(get_lark_config).put(put_lark_config),
        )
        .route(
            "/api/integrations/lark/pairing/consume",
            post(consume_pairing),
        )
}

/// 结构化读飞书集成配置（`config.json` 的 `integrations.lark`）。
///
/// 为什么不复用 `/api/settings`：那是 DB `app_settings` 的**平铺字符串**表 —— 把
/// bool 塞成 `"true"`、把数组塞成 JSON 字符串，让每个消费者各自剥壳。飞书策略
/// 已迁到 `config.json`（`docs/LARK_BOT.md` §5.6），形状由**服务端**保证，
/// bot 只消费（这也是将来接入第二个平台时能复用的接缝）。
async fn get_lark_config() -> Result<Json<serde_json::Value>, ApiError> {
    let lark = crate::config::lark_integration();
    serde_json::to_value(lark)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

/// 结构化**写**飞书集成配置（CLI / 远程客户端用；桌面设置页走 Tauri 命令）。
///
/// 与 `PUT /api/settings` 的区别：键名与类型由**服务端**保证，且**只碰
/// `integrations.lark` 这一块**（不是"整份重建"，不会顺带抹掉别人的字段）。
/// 写后**重启 bot** —— 策略是 bot 启动时读一次，不重启就等于没改。
async fn put_lark_config(
    Json(body): Json<LarkIntegration>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::config::set_lark_integration(&body).map_err(|e| ApiError { error: e })?;
    crate::lark_bot::restart();
    let saved = crate::config::lark_integration();
    serde_json::to_value(saved)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

#[derive(Deserialize)]
struct PairingConsumeBody {
    open_id: String,
}

/// 配对消费：把发送者写入白名单，并**自动关闭**配对（§7.1 ⑥）。
///
/// ⚠️ **这是白名单写入口** —— 必须先校验配对**确实开着且未过期**，否则它就成了
/// "凭一个 token 往白名单里塞任意 `open_id`"的口子（等于绕过 L1 准入）。校验不过一律拒绝。
///
/// 是否私聊由**闸门**判定（bot 只在 p2p 配对时才调这里）—— 服务端看不到 `chat_type`，
/// 因此不重复判定，但正因如此才更要把"配对是否有效"守死。
async fn consume_pairing(
    Json(body): Json<PairingConsumeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let open_id = body.open_id.trim().to_string();
    if open_id.is_empty() {
        return Err(ApiError {
            error: "open_id is required".into(),
        });
    }
    let now = chrono::Utc::now().timestamp();
    let mut lark = crate::config::lark_integration();
    if !lark.pairing_active(now) {
        return Err(ApiError {
            error: "pairing is not active（未开启或已过期）".into(),
        });
    }
    if !lark.allowed_users.iter().any(|u| u == &open_id) {
        lark.allowed_users.push(open_id.clone());
    }
    // 配对是"一次性"的：消费即关闭（留着等于没有白名单）。
    lark.pairing = false;
    lark.pairing_expires_at = 0;
    crate::config::set_lark_integration(&lark).map_err(|e| ApiError { error: e })?;
    // 白名单变了 → 让 bot 重读，否则新配的用户仍会被拒。
    crate::lark_bot::restart();

    let saved = crate::config::lark_integration();
    log::info!("[LARK] 配对成功：{open_id} 已加入白名单，配对模式自动关闭");
    Ok(Json(
        serde_json::json!({ "ok": true, "allowed_users": saved.allowed_users }),
    ))
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
