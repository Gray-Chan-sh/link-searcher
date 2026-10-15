//! 飞书**交互卡片**的渲染、**节流**与 **SSE 帧解析**（`docs/LARK_BOT.md` §8 / §8.1）。
//!
//! 这三块都刻意做成**纯逻辑**（不含任何 IO）：节流是最容易"看着对、实际发多或发漏"的地方，
//! SSE 分帧则是最容易被换行/分块坑到的地方 —— 都不该只能靠"真飞书上试一次"来验证。
//!
//! ## 实测依据（2026-10-15 探针，§8.1）
//!
//! | 事实 | 结论 |
//! |---|---|
//! | app 身份 `PATCH /open-apis/im/v1/messages/{id}` | **没有"最多 2 次"限制**（实测 80/80 成功） |
//! | 卡 1.0 与 2.0 schema | **都被接受** → 这里用更保守的 1.0 |
//!
//! "最多更新 2 次"属 **`card.action.trigger` 回调 token** 那条路，与这里无关。
//! 因此节流按 **"最小间隔 + 次数上限"** 设计，而不是按"只能发 N 次"。

use std::time::{Duration, Instant};

use serde_json::json;

/// 中间更新的**最小间隔**。实测 250ms 也安全；1.5s 既留足 QPS 余量，又符合人眼阅读节奏。
pub const MIN_INTERVAL: Duration = Duration::from_millis(1500);
/// 中间更新**次数上限**（**不含**最后那次收尾）。
pub const MAX_UPDATES: usize = 12;
/// 卡片正文允许的**字节**上限。
///
/// 为什么按**字节**而不是字符：飞书的硬约束是**卡片 JSON ≤ 30KB**（字节），
/// 中文一个字 3 字节 —— 按"字符数"设限极容易在长中文答案上算错（2026-10-15 实测踩过：
/// 先前按 2500 **字符**截断，而卡片本可承载约 9000 字）。
/// 取 20KB 给页眉/页脚/JSON 语法留足余量。
pub const MAX_BODY_BYTES: usize = 20_000;

// ───────────────────────────── 节流 ─────────────────────────────

/// 节流器：回答"这一次更新要不要真的发出去"。
///
/// **收尾那一次永远发**（`finished = true`）—— 否则用户会停在一张"处理中"的卡片上，
/// 那比不显示进度还糟。
#[derive(Debug)]
pub struct Throttle {
    min_interval: Duration,
    max_updates: usize,
    last_sent: Option<Instant>,
    sent: usize,
}

impl Throttle {
    pub fn new(min_interval: Duration, max_updates: usize) -> Self {
        Self { min_interval, max_updates, last_sent: None, sent: 0 }
    }

    /// 生产参数（§8.1 实测推断）。
    pub fn standard() -> Self {
        Self::new(MIN_INTERVAL, MAX_UPDATES)
    }

    /// `now` 由调用方传入 —— 单测因此**不需要 sleep**（避免把测试挂到真实时间上）。
    pub fn should_send(&mut self, now: Instant, finished: bool) -> bool {
        if finished {
            self.last_sent = Some(now);
            self.sent += 1;
            return true;
        }
        if self.sent >= self.max_updates {
            return false;
        }
        match self.last_sent {
            Some(t) if now.duration_since(t) < self.min_interval => false,
            _ => {
                self.last_sent = Some(now);
                self.sent += 1;
                true
            }
        }
    }

    /// 已发出的更新次数（含收尾）。
    pub fn sent(&self) -> usize {
        self.sent
    }
}

// ───────────────────────────── 渲染 ─────────────────────────────

/// 按**字节**预算截断（回退到字符边界，绝不切出半个 UTF-8 字符），并说明已截断。///
/// 按字节而不是字符：飞书的限制是卡片 JSON ≤ 30KB **字节**，而中文 1 字 = 3 字节
/// （2026-10-15 实测踩过：按"字符"设限会把长中文答案截得比纯文本还短）。
pub fn clip_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n…（内容过长已截断；完整内容请到桌面端 / WebUI 查看）",
        &text[..end]
    )
}

/// 单条回答最多占几张卡片。超过就把尾部合并进最后一张并给 WebUI 指引 ——
/// 极端长的答案不该把聊天刷成十几条（用户要的是"看得到"，不是"被淹没"）。
pub const MAX_CARDS: usize = 6;

/// 飞书卡片 markdown 支持的是**子集**，这里把不支持的写法降级成等价表达。
///
/// **实测能力矩阵（2026-10-15，用户逐项核对，见 §8.3）**
///
/// | 支持 ✅ | 不支持 ❌ |
/// |---|---|
/// | `**粗体**`、`*斜体*`、`~~删除~~`、`-` 列表、`1.` 列表、**嵌套列表（缩进）**、代码块、链接、`---`、空行 | **`#` 标题**、**行内代码**、**`>` 引用**、**表格** |
///
/// 降级规则（**只改"怎么摆"，不改内容**）：
///
/// 1. `### X` → `**X**`（独立加粗行，前后补空行）—— 这是最要紧的一条：模型的回答里标题极多，
///    不降级就会满屏裸 `###`
/// 2. `* 项目` → `- 项目`（`*` 会被当斜体起始；`-` 实测稳定）——**缩进一律原样保留**，
///    因为**嵌套列表实测是支持的**（拍平会白丢层级）
/// 3. 行内代码的反引号去掉（`x`）—— 飞书不渲染行内代码，留着就是裸反引号
/// 4. `> 引用` → 去掉 `>`，保留文字
/// 5. **表格** → 用代码块包起来（保住对齐；飞书不渲染表格）
///
/// 安全边界：**代码块内部一律原样保留**（`#`、反引号、`|` 在那里都是内容，不是标记）。
pub fn normalize_for_card(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut in_table = false;
    let close_table = |out: &mut Vec<String>, in_table: &mut bool| {
        if *in_table {
            out.push("```".to_string());
            *in_table = false;
        }
    };

    for raw in text.split('\n') {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // ── 代码块：原样进出（不解析内部任何标记）──
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            close_table(&mut out, &mut in_table);
            in_fence = !in_fence;
            out.push(line.to_string());
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }

        // ── 表格行：连续包进代码块 ──
        if is_table_row(trimmed) {
            if !in_table {
                out.push("```".to_string());
                in_table = true;
            }
            out.push(line.to_string());
            continue;
        }
        close_table(&mut out, &mut in_table);

        // 行内代码：去掉反引号（非代码块行才处理）
        let line = line.replace('`', "");
        let trimmed = line.trim_start();
        let indent = " ".repeat(line.len() - trimmed.len());

        // ① 标题 → 加粗独立行
        if let Some(title) = heading_text(trimmed) {
            if !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                out.push(String::new());
            }
            out.push(format!("**{title}**"));
            continue;
        }
        // ② `* 项目` → `- 项目`
        if let Some(rest) = bullet_rest(trimmed) {
            out.push(format!("{indent}- {rest}"));
            continue;
        }
        // ④ 引用 → 去掉 `>`，保留文字
        if let Some(rest) = trimmed.strip_prefix('>') {
            out.push(format!("{indent}{}", rest.trim_start()));
            continue;
        }
        out.push(line.to_string());
    }
    close_table(&mut out, &mut in_table);
    out.join("\n")
}

/// `### 标题` → `Some("标题")`；`#` 数量 1–6 且后面有内容才算标题。
fn heading_text(line: &str) -> Option<String> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line[hashes..].trim();
    if rest.is_empty() { None } else { Some(rest.to_string()) }
}

/// `* 项目` / `*\t项目` → `Some("项目")`。
///
/// **必须要求 `*` 后面是空白**：否则 `*斜体*` 会被误当列表项。
fn bullet_rest(line: &str) -> Option<String> {
    let rest = line.strip_prefix('*')?;
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    if rest.starts_with("**") {
        return None; // `**粗体**` 不是列表
    }
    Some(rest.trim_start().to_string())
}

/// 形如 `| a | b |` 的行（飞书不渲染表格 → 我们用代码块保住对齐）。
fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2
}

/// 渲染一张卡片（**1.0 schema**：`elements` 在顶层）。
///
/// `body_md` 会先过 [`normalize_for_card`]：飞书的卡片 markdown 是**子集**，
/// 不支持的写法要降级，否则用户会看到裸标记（`### 一、…`）。
///
/// `footer` 走 `note` 元素（灰色小字），用于"耗时 / 引用条数"这类元信息。
pub fn render_card(title: &str, body_md: &str, footer: Option<&str>) -> serde_json::Value {
    let mut elements = vec![json!({
        "tag": "markdown",
        "content": clip_bytes(&normalize_for_card(body_md), MAX_BODY_BYTES),
    })];
    if let Some(f) = footer.map(str::trim).filter(|f| !f.is_empty()) {
        elements.push(json!({ "tag": "hr" }));
        elements.push(json!({
            "tag": "note",
            "elements": [{ "tag": "plain_text", "content": f }],
        }));
    }
    json!({
        "config": { "wide_screen_mode": true },
        "header": { "title": { "tag": "plain_text", "content": title } },
        "elements": elements,
    })
}

/// "检索中"卡片：进度阶段 + 已经生成的部分内容。
pub fn render_working(progress: &str, partial: &str) -> serde_json::Value {
    let body = if partial.trim().is_empty() {
        format!("⏳ {progress}")
    } else {
        format!("{partial}\n\n---\n⏳ {progress}")
    };
    render_card("正在检索…", &body, None)
}

/// 渲染**分页**的最终卡片（`page` 从 1 开始）。
///
/// 证据条数与耗时**只标在最后一张**：否则一张长回答会连着刷三遍同样的脚注，
/// 反而把"读到第几段"这个真正有用的信息淹掉。
pub fn render_answer_card(
    body: &str,
    page: usize,
    pages: usize,
    evidence_count: usize,
    took_ms: u64,
) -> serde_json::Value {
    let mut meta: Vec<String> = Vec::new();
    if pages > 1 {
        meta.push(format!("第 {page} / {pages} 段"));
    }
    if page == pages {
        if evidence_count > 0 {
            meta.push(format!("共用 {evidence_count} 条证据"));
        }
        if took_ms > 0 {
            meta.push(format!("{}s", took_ms / 1000));
        }
    }
    let title = if pages > 1 { format!("回答（{page} / {pages}）") } else { "回答".to_string() };
    let footer = if meta.is_empty() { None } else { Some(meta.join(" · ")) };
    render_card(&title, body, footer.as_deref())
}

/// 把长回答切成**多张卡片**（每张 ≤ `max_bytes`），最多 `max_cards` 张。
///
/// 切分规则与理由：
/// - **优先在行边界切**（`\n`）→ 不会把一段话拦腰截断；某一行本身超预算时才硬切；
/// - 硬切一律**回退到字符边界** → 中文不会被切出半个 UTF-8 字符；
/// - 切分**不丢任何字符**（各段拼回去就是原文）；
/// - 超过 `max_cards` 时把尾部**合并进最后一张**并加"完整内容请看桌面端/WebUI"指引。
///
/// 这段是纯函数 —— "分页会不会把答案弄丢/弄错"正是最该被测试盯住的地方。
pub fn split_for_cards(text: &str, max_bytes: usize, max_cards: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();

    for line in text.split_inclusive('\n') {
        // 单行自身超预算：先按字符边界切出若干满预算段
        if line.len() > max_bytes {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            let mut rest = line;
            while rest.len() > max_bytes {
                let mut end = max_bytes;
                while end > 0 && !rest.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 {
                    break; // 理论不可达（预算远大于单字符字节数）
                }
                chunks.push(rest[..end].to_string());
                rest = &rest[end..];
            }
            cur.push_str(rest);
            continue;
        }
        if !cur.is_empty() && cur.len() + line.len() > max_bytes {
            chunks.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }

    if chunks.len() > max_cards {
        let keep = max_cards.max(1);
        let mut merged = chunks[keep - 1].clone();
        for extra in chunks.drain(keep..) {
            merged.push_str(&extra);
        }
        chunks[keep - 1] = clip_bytes(&merged, max_bytes);
    }
    chunks
}

/// 失败卡片（**在"卡已发出"之后**出错时用）：不再另发文本，避免答案重复出现两次。
pub fn render_error(detail: &str) -> serde_json::Value {
    render_card("出错了", &format!("未能完成这次回答：\n\n```\n{detail}\n```"), None)
}

// ───────────────────────────── SSE 分帧 ─────────────────────────────

/// SSE 帧累加器。
///
/// axum 的 `Sse` 编码是 `event: <name>\n` + `data: <json>\n` + **空行**分隔，
/// 且 `keep_alive` 会插入 `: ping` 注释行。**必须按帧累加**，不能"一行当一帧"
/// （数据里含换行或分块到达时会拆错）。
#[derive(Debug, Default)]
pub struct SseAccum {
    event: String,
    data: String,
}

impl SseAccum {
    /// 喂一行；当这一行构成**完整帧**时返回 `(event, data)`。
    pub fn push_line(&mut self, line: &str) -> Option<(String, String)> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if self.data.is_empty() {
                self.event.clear();
                return None;
            }
            return Some((std::mem::take(&mut self.event), std::mem::take(&mut self.data)));
        }
        // `:` 开头是注释（keep-alive），其余字段（id/retry）我们不用
        if line.starts_with(':') {
            return None;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
        None
    }
}

// ───────────────────────────── 卡片回调（D：交互按钮）─────────────────────────────

/// 卡片回调事件（`card.action.trigger`）里我们真正用到的字段（§2 有实测样本）。
///
/// ⚠️ **实测约束（§2 实测发现 ①②④）**：
///
/// - `card_content` 会被飞书**重写**（不是发送原文的回显）→ **绝不能**靠它判断卡片状态，
///   路由只能看 `action_value` / `custom_action_id`；
/// - `action_value` 往往是**一个 JSON 字符串**，需要**二次 parse**；
/// - 卡片事件的 `timestamp` 是**微秒**（消息事件是毫秒）→ 两者不可按同一精度解析
///   （这里不解析它，只用 `message_id` 等稳定字段）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CardActionEvent {
    /// 事件类型，应为 `card.action.trigger`。
    #[serde(rename = "type", default)]
    pub event_type: String,
    /// 按钮上带的值（通常是 JSON 字符串）。
    #[serde(default)]
    pub action_value: String,
    /// 卡片所在的消息 id。
    #[serde(default)]
    pub message_id: String,
    /// 会话 id。
    #[serde(default)]
    pub chat_id: String,
    /// 点击者的 `open_id`（仍需过白名单 —— 回调是**另一条入口**，不能只靠消息入口把关）。
    #[serde(default)]
    pub operator_id: String,
}

/// 我们支持的按钮动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardAction {
    /// 「打开详情」：给出会话在 WebUI 里的入口。
    OpenDetail { session_id: String },
    /// 「换一批证据」：用同一个问题重跑一轮。
    Retry { session_id: String },
}

/// 从 `action_value` 解析出动作。返回 `None` = **不认识/不完整 → 直接忽略**。
///
/// 纯函数：路由错了会变成"点 A 触发 B"，而这条链路**没法靠肉眼在飞书里穷举**，
/// 所以它是 D 最该被单测盯住的一层。
pub fn parse_card_action(ev: &CardActionEvent) -> Option<CardAction> {
    // 二次 parse：飞书给的是 JSON **字符串**（§2 实测发现 ①）
    let v: serde_json::Value = serde_json::from_str(ev.action_value.trim()).ok()?;
    let action = v.get("action")?.as_str()?.trim();
    let session_id = v
        .get("session_id")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // 没有 session_id 的动作无法安全执行（不知道对哪个会话做事）→ 忽略
    if session_id.is_empty() {
        return None;
    }
    match action {
        "open_detail" => Some(CardAction::OpenDetail { session_id }),
        "retry" => Some(CardAction::Retry { session_id }),
        _ => None,
    }
}

/// 按钮的 `action_value` 构造器（**发送侧与解析侧必须成对**，故放同一个文件）。
///
/// 保留成函数而不是散落的字面量：两边一旦漂移，测试里的往返（round-trip）断言会立刻红。
pub fn action_value(action: &str, session_id: &str) -> String {
    action_payload(action, session_id).to_string()
}

/// 按钮 `value` 的**对象**形式（飞书按钮的 `value` 字段要对象）。
///
/// 飞书回调时会把它的**字符串化**结果放进 `action_value`（§2 实测样本正是如此：
/// `"action_value":"{\"probe\":\"card-action-20261015\"}"`），所以 [`parse_card_action`]
/// 走"二次 parse"那条路 —— 两边靠 `action_payload` 单点定义 + 往返测试保持一致。
pub fn action_payload(action: &str, session_id: &str) -> serde_json::Value {
    serde_json::json!({ "action": action, "session_id": session_id })
}

/// WebUI 的入口 URL（给「打开详情」按钮用）。
///
/// 必须用 **LAN 地址**、不能用 `127.0.0.1` —— 用户多半是在**手机/别的机器**上点这个按钮，
/// 给回环地址等于点不开。纯函数：这是"点开到底能不能打开"的唯一决定因素，也是唯一值得单测的部分。
pub fn webui_url(lan_ip: &str, port: u16) -> String {
    format!("https://{}:{}/", lan_ip.trim(), port)
}

/// 给卡片**追加**动作按钮（只有"最后一张"该用）。
///
/// - **只在最后一张**：多段回答时中间页挂按钮会让"点哪个"变含糊；
/// - **没有 session_id 就不加**：动作都作用于某个会话，匿名按钮点不动反而是噪音；
/// - **只加真的能用的按钮**：`换一批证据` 需要"在同一张卡上重跑一轮"的接线（D-4b），
///   在它落地之前**不放出来** —— 一个点了没反应的按钮比没有按钮更糟。
pub fn append_actions(card: &mut serde_json::Value, session_id: &str) {
    let sid = session_id.trim();
    if sid.is_empty() {
        return;
    }
    let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut()) else {
        return;
    };
    elements.push(serde_json::json!({
        "tag": "action",
        "actions": [
            {
                "tag": "button",
                "text": { "tag": "plain_text", "content": "打开详情" },
                "type": "default",
                "value": action_payload("open_detail", sid),
            },
        ],
    }));
}

/// 从回调事件的原始 JSON 里取出**操作者 `open_id`**。
///
/// ⚠️ 该字段的**准确形状尚未用真实样本确认**（§2 那份样本以 `card_content` / `token` 为主），
/// 因此这里**兼容几种已知形状**，并且**fail-closed**：取不到就返回 `None`，上层据此**拒绝**。
/// 宁可"按钮暂时点不动"，也绝不"看不懂就放行" —— 回调是**另一条入口**，
/// 消息入口那套闸门**不会**自动保护它。
///
/// 兼容形状：`operator_id` 为字符串；或对象里的 `open_id`/`id`/`user_id`；
/// 或外层叫 `operator` / `operator_open_id`。
pub fn operator_open_id(v: &serde_json::Value) -> Option<String> {
    fn pick(x: &serde_json::Value) -> Option<String> {
        if let Some(s) = x.as_str() {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        for k in ["open_id", "id", "user_id"] {
            if let Some(s) = x.get(k).and_then(|y| y.as_str()) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }
    for k in ["operator_id", "operator", "operator_open_id"] {
        if let Some(id) = v.get(k).and_then(|x| if x.is_null() { None } else { pick(x) }) {
            return Some(id);
        }
    }
    None
}

/// 回调的准入判定：**与消息入口共用同一份白名单**（§7.1 ②：空名单 = 拒绝全部，fail-closed）。
///
/// 回调里没有 `chat_type`（它不是消息事件）→ 这里**只判身份**；
/// "仅私聊"由消息入口保证（卡片只会发在 p2p 会话里）。
/// 取不到操作者（`None`）→ **拒绝**。
pub fn card_action_allowed(operator: Option<&str>, allowed_users: &[String]) -> bool {
    match operator.map(str::trim).filter(|s| !s.is_empty()) {
        Some(op) => allowed_users.iter().any(|u| u == op),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 节流：全部用注入的 `now`，不 sleep ──

    #[test]
    fn first_update_goes_out_immediately() {
        let mut t = Throttle::standard();
        let t0 = Instant::now();
        assert!(t.should_send(t0, false), "第一次必须立刻发（否则用户一直看不到卡片内容）");
    }

    #[test]
    fn updates_within_interval_are_coalesced_away() {
        let mut t = Throttle::new(Duration::from_millis(1000), 5);
        let t0 = Instant::now();
        assert!(t.should_send(t0, false));
        assert!(!t.should_send(t0 + Duration::from_millis(300), false), "间隔内必须丢弃（合并）");
        assert!(!t.should_send(t0 + Duration::from_millis(999), false));
        assert!(t.should_send(t0 + Duration::from_millis(1000), false), "满一个间隔后应放行");
    }

    #[test]
    fn middle_updates_stop_at_the_cap() {
        let mut t = Throttle::new(Duration::from_millis(1), 3);
        let t0 = Instant::now();
        for i in 0..3 {
            assert!(t.should_send(t0 + Duration::from_millis(i * 10), false), "第 {i} 次应在上限内");
        }
        assert!(!t.should_send(t0 + Duration::from_secs(60), false), "超过上限后中间更新必须停");
        assert_eq!(t.sent(), 3);
    }

    #[test]
    fn final_update_always_goes_out_even_past_the_cap() {
        let mut t = Throttle::new(Duration::from_secs(3600), 1);
        let t0 = Instant::now();
        assert!(t.should_send(t0, false));
        // 既超过次数上限、也远没到间隔 —— 但收尾必须发
        assert!(t.should_send(t0 + Duration::from_millis(1), true));
        assert_eq!(t.sent(), 2);
    }

    // ── 截断 ──

    #[test]
    fn clip_bytes_never_splits_a_multibyte_char() {
        let s = "字".repeat(10); // 每个汉字 3 字节 → 共 30 字节
        assert_eq!(clip_bytes(&s, 30), s, "刚好到预算不该截断");
        let out = clip_bytes(&s, 10); // 10 不是 3 的倍数 → 必须回退到 9 字节 = 3 个字
        assert!(out.starts_with("字字字"), "应回退到字符边界：{out}");
        assert!(out.contains("已截断"));
        assert_eq!(out.matches('字').count(), 3, "绝不能切出半个 UTF-8 字符");
    }

    /// 回归：正文预算必须让**整张卡片**（含页眉/页脚/JSON 语法）留在飞书 30KB 硬限内。
    /// 2026-10-15 实测踩过相反方向的坑：按"字符"设限把长中文答案截得比纯文本还短。
    #[test]
    fn whole_card_json_stays_under_feishu_30kb_limit() {
        let long = "字".repeat(20_000); // 60KB，必被截断
        let card = render_answer_card(&long, 1, 1, 3, 1000);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.len() < 30_000, "整张卡片 JSON 必须 < 30KB，实际 {} 字节", json.len());
        assert!(json.contains("已截断"));
        // 中文长答案的真实收益：能装下远超纯文本(3000 字)的量
        assert!(
            card["elements"][0]["content"].as_str().unwrap_or("").matches('字').count() > 5000,
            "20KB 预算应能装下 5000 字以上中文"
        );
    }

    // ── SSE 分帧 ──

    #[test]
    fn sse_frame_spanning_lines_is_reassembled() {
        let mut a = SseAccum::default();
        assert_eq!(a.push_line("event: ai-chunk"), None);
        assert_eq!(a.push_line("data: {\"a\":1}"), None);
        assert_eq!(
            a.push_line(""),
            Some(("ai-chunk".to_string(), "{\"a\":1}".to_string())),
            "空行才结束一帧"
        );
    }

    #[test]
    fn sse_keepalive_and_stray_blank_lines_are_ignored() {
        let mut a = SseAccum::default();
        assert_eq!(a.push_line(": ping"), None, "keep-alive 注释行不能当事件");
        assert_eq!(a.push_line(""), None, "没有 data 的空行不产生帧");
        let mut b = SseAccum::default();
        b.push_line("event: ai-done");
        b.push_line("data: {}");
        assert_eq!(b.push_line(""), Some(("ai-done".to_string(), "{}".to_string())));
    }

    #[test]
    fn sse_data_leading_space_is_stripped_but_inner_newlines_kept() {
        let mut a = SseAccum::default();
        a.push_line("event: x");
        a.push_line("data: line1");
        a.push_line("data: line2");
        assert_eq!(a.push_line(""), Some(("x".to_string(), "line1\nline2".to_string())));
    }

    // ── 卡片渲染 ──

    #[test]
    fn card_shape_is_valid_1_0() {
        let c = render_answer_card("答案 [1]", 1, 1, 2, 3400);
        assert!(c.get("config").is_some());
        assert_eq!(c["header"]["title"]["content"], "回答");
        assert_eq!(c["elements"][0]["tag"], "markdown");
        assert_eq!(c["elements"][0]["content"], "答案 [1]");
        // footer 走 note 元素，含 hr 分隔
        let tags: Vec<&str> = c["elements"].as_array().unwrap().iter().map(|e| e["tag"].as_str().unwrap_or("")).collect();
        assert_eq!(tags, vec!["markdown", "hr", "note"]);
        assert!(c["elements"][2]["elements"][0]["content"].as_str().unwrap_or("").contains("2 条证据"));
    }

    #[test]
    fn working_card_shows_progress_and_marks_partial() {
        let c = render_working("正在检索文档…", "");
        assert!(c["elements"][0]["content"].as_str().unwrap_or("").contains("⏳"));
        let c2 = render_working("生成回答中…", "已经写了一半");
        let body = c2["elements"][0]["content"].as_str().unwrap_or("");
        assert!(body.contains("已经写了一半") && body.contains("生成回答中"));
        assert_eq!(c2["elements"].as_array().unwrap().len(), 1, "没有 footer 时不该多出元素");
    }

    #[test]
    fn error_card_is_marked_and_not_mistaken_for_an_answer() {
        let c = render_error("HTTP 500");
        assert_eq!(c["header"]["title"]["content"], "出错了");
        assert!(c["elements"][0]["content"].as_str().unwrap_or("").contains("HTTP 500"));
    }

    // ── 分页（超长回答切成多张卡片）──

    #[test]
    fn short_answer_stays_one_card() {
        let chunks = split_for_cards("较短的回答 [1]", 1000, 6);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "较短的回答 [1]");
    }

    /// **最重要的一条**：分页绝不能丢内容 —— 各段拼回去必须与原文一字不差。
    #[test]
    fn split_never_loses_a_character() {
        let original: String = (0..400).map(|i| format!("第{i}行，这是一段用来测试分页的中文内容。\n")).collect();
        let chunks = split_for_cards(&original, 500, 99);
        assert!(chunks.len() > 1, "这段应当被切成多张");
        assert_eq!(chunks.concat(), original, "拼回去必须与原文完全一致");
    }

    #[test]
    fn every_chunk_fits_the_budget_and_breaks_on_line_boundaries() {
        let original: String = (0..200).map(|i| format!("行{i}：内容内容内容内容内容\n")).collect();
        let chunks = split_for_cards(&original, 300, 99);
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.len() <= 300, "第 {i} 段超出预算：{} 字节", c.len());
        }
        // 除最后一段外，都应在行边界结束（不会把一行话拦腰截断）
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.ends_with('\n'), "应在行边界切分：{:?}", &c[c.len().saturating_sub(12)..]);
        }
    }

    #[test]
    fn single_huge_line_is_hard_split_on_char_boundary() {
        let one_line = "字".repeat(100); // 300 字节，且**没有换行**
        let chunks = split_for_cards(&one_line, 100, 9);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), one_line, "硬切同样不能丢字符");
        for c in &chunks {
            assert!(c.len() <= 100, "硬切也要守预算");
            assert!(c.chars().all(|ch| ch == '字'), "绝不能切出半个 UTF-8 字符");
        }
    }

    #[test]
    fn over_the_card_cap_merges_tail_and_points_to_the_desktop_app() {
        let original: String = (0..200).map(|i| format!("行{i}：内容\n")).collect();
        let chunks = split_for_cards(&original, 200, 3);
        assert_eq!(chunks.len(), 3, "超过上限应合并到最后一张");
        let tail = chunks.last().unwrap();
        assert!(tail.contains("完整内容请到桌面端"), "被合并截断时要给出口：{tail}");
        assert!(tail.contains("已截断"));
        // 前 max_cards-1 张不受影响，仍是原文的前缀
        assert!(original.starts_with(chunks[0].as_str()));
    }

    #[test]
    fn paged_card_marks_pages_and_only_last_shows_evidence() {
        let c1 = render_answer_card("第一段", 1, 3, 19, 67000);
        assert_eq!(c1["header"]["title"]["content"], "回答（1 / 3）");
        let f1 = &c1["elements"][2]["elements"][0]["content"];
        assert_eq!(f1, "第 1 / 3 段", "非最后一张不该标证据/耗时");
        let c3 = render_answer_card("末段", 3, 3, 19, 67000);
        let f3 = c3["elements"][2]["elements"][0]["content"].as_str().unwrap_or("");
        assert!(f3.contains("第 3 / 3 段") && f3.contains("19 条证据") && f3.contains("67s"), "实际：{f3}");
    }

    #[test]
    fn single_page_card_has_no_page_marker() {
        let c = render_answer_card("就一段", 1, 1, 2, 3400);
        assert_eq!(c["header"]["title"]["content"], "回答");
        let f = c["elements"][2]["elements"][0]["content"].as_str().unwrap_or("");
        assert!(!f.contains("段"), "只有一段时不该出现分页标记：{f}");
        assert!(f.contains("2 条证据"));
    }

    // ── markdown 子集降级（§8.3 实测矩阵：这些写法飞书**不渲染**）──

    #[test]
    fn headings_become_bold_standalone_lines() {
        let out = normalize_for_card("前文\n### 一、公司设立\n正文");
        assert_eq!(out, "前文\n\n**一、公司设立**\n正文", "标题要变加粗行并补空行");
        assert!(!out.contains('#'), "不能残留裸 #：{out}");
    }

    #[test]
    fn asterisk_bullets_become_dash_but_italics_are_untouched() {
        assert_eq!(normalize_for_card("* 项目 A"), "- 项目 A");
        assert_eq!(normalize_for_card("  * 嵌套项"), "  - 嵌套项", "缩进要保留");
        assert_eq!(normalize_for_card("*斜体* 不是列表"), "*斜体* 不是列表");
        assert_eq!(normalize_for_card("**粗体** 也不是列表"), "**粗体** 也不是列表");
    }

    #[test]
    fn inline_backticks_and_blockquote_marker_are_removed() {
        assert_eq!(normalize_for_card("调用 `foo()` 即可"), "调用 foo() 即可");
        assert_eq!(normalize_for_card("> 引用一句"), "引用一句");
    }

    #[test]
    fn tables_are_wrapped_in_a_code_block() {
        let out = normalize_for_card("| a | b |\n| --- | --- |\n| 1 | 2 |\n普通行");
        assert!(out.starts_with("```\n| a | b |"), "表格前要有代码块起点：{out}");
        assert!(out.contains("| 1 | 2 |\n```"), "表格后要闭合：{out}");
        assert!(out.ends_with("普通行"), "表格后的正文不受影响：{out}");
    }

    /// **安全边界**：代码块内部是内容、不是标记 —— 一个字都不能改。
    #[test]
    fn code_block_content_is_left_verbatim() {
        let src = "```\n### 标题\n`x`\n| a | b |\n```";
        assert_eq!(normalize_for_card(src), src);
    }

    #[test]
    fn normalization_is_idempotent() {
        let src = "### 标题\n* 项\n| a | b |\n| - | - |\n`code`\n> 引";
        let once = normalize_for_card(src);
        assert_eq!(normalize_for_card(&once), once, "二次归一化不该再变化");
    }

    // ── 卡片回调（D）：解析与路由 ──

    #[test]
    fn card_action_round_trips_with_its_builder() {
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: action_value("open_detail", "sess-1"),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::OpenDetail { session_id: "sess-1".into() })
        );
        let ev2 = CardActionEvent {
            action_value: action_value("retry", "sess-2"),
            ..Default::default()
        };
        assert_eq!(parse_card_action(&ev2), Some(CardAction::Retry { session_id: "sess-2".into() }));
    }

    /// 陌生/不完整的动作必须**忽略**，绝不能"猜着执行" —— 按钮是**另一条入口**。
    #[test]
    fn unknown_or_incomplete_actions_are_ignored_not_executed() {
        for bad in [
            r#"{"action":"delete_everything","session_id":"s"}"#,
            r#"{"action":"open_detail"}"#,                    // 缺 session_id
            r#"{"session_id":"s"}"#,                          // 缺 action
            r#"{"action":"open_detail","session_id":"  "}"#,  // session_id 是空白
            "not json at all",
            "",
        ] {
            let ev = CardActionEvent { action_value: bad.to_string(), ..Default::default() };
            assert_eq!(parse_card_action(&ev), None, "必须忽略：{bad}");
        }
    }

    #[test]
    fn action_value_survives_surrounding_whitespace() {
        let ev = CardActionEvent {
            action_value: format!("  {}  ", action_value("retry", "s3")),
            ..Default::default()
        };
        assert_eq!(parse_card_action(&ev), Some(CardAction::Retry { session_id: "s3".into() }));
    }

    /// 真实事件字段更多（`timestamp` 是**微秒**、还有 `token`/`card_content`…）→ 必须**宽松解析**。
    #[test]
    fn card_event_json_tolerates_extra_fields() {
        let raw = r#"{"type":"card.action.trigger","timestamp":"1792054882978640","token":"c-x",
          "action_value":"{\"action\":\"open_detail\",\"session_id\":\"s4\"}",
          "card_content":"{\"config\":{}}","message_id":"om_1","chat_id":"oc_1","operator_id":"ou_1"}"#;
        let ev: CardActionEvent = serde_json::from_str(raw).expect("多字段不应导致解析失败");
        assert_eq!(ev.message_id, "om_1");
        assert_eq!(ev.operator_id, "ou_1");
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::OpenDetail { session_id: "s4".into() })
        );
    }

    // ── 回调的身份与准入（**fail-closed**；回调是另一条入口）──

    #[test]
    fn operator_open_id_accepts_the_known_shapes() {
        let s = |j: &str| operator_open_id(&serde_json::from_str::<serde_json::Value>(j).unwrap());
        assert_eq!(s(r#"{"operator_id":"ou_a"}"#), Some("ou_a".to_string()));
        assert_eq!(s(r#"{"operator_id":{"open_id":"ou_b"}}"#), Some("ou_b".to_string()));
        assert_eq!(s(r#"{"operator":{"open_id":"ou_c"}}"#), Some("ou_c".to_string()));
        assert_eq!(s(r#"{"operator_open_id":"ou_d"}"#), Some("ou_d".to_string()));
    }

    /// 形状未确认 → 取不到就**必须是 None**（上层据此拒绝），绝不猜。
    #[test]
    fn operator_open_id_is_none_when_unavailable() {
        let s = |j: &str| operator_open_id(&serde_json::from_str::<serde_json::Value>(j).unwrap());
        for bad in [
            r#"{}"#,
            r#"{"operator_id":""}"#,
            r#"{"operator_id":null}"#,
            r#"{"operator_id":{}}"#,
        ] {
            assert_eq!(s(bad), None, "取不到操作者必须是 None（fail-closed）：{bad}");
        }
    }

    #[test]
    fn card_action_is_denied_unless_operator_is_whitelisted() {
        let allowed = vec!["ou_me".to_string()];
        assert!(card_action_allowed(Some("ou_me"), &allowed));
        assert!(!card_action_allowed(Some("ou_stranger"), &allowed), "陌生人不许点");
        assert!(!card_action_allowed(None, &allowed), "取不到操作者 → 拒绝");
        assert!(!card_action_allowed(Some(""), &allowed));
        assert!(!card_action_allowed(Some("  "), &allowed));
        assert!(
            !card_action_allowed(Some("ou_me"), &[]),
            "空名单 = 拒绝全部（与 §7.1 ② 一致）"
        );
    }

    /// **发送侧 ↔ 解析侧**必须钉在一起：按钮的 `value`（对象）被飞书**字符串化**成
    /// `action_value` 后，必须能被 `parse_card_action` 认出来。
    /// 它们一旦漂移，症状是"按钮点了没反应"—— 最难查的那种。
    #[test]
    fn action_buttons_round_trip_into_parseable_actions() {
        let mut card = render_answer_card("答", 1, 1, 1, 1000);
        append_actions(&mut card, "sess-9");
        let elems = card["elements"].as_array().unwrap();
        let action = elems.last().expect("应追加 action 元素");
        assert_eq!(action["tag"], "action");
        let buttons = action["actions"].as_array().unwrap();
        // 只放"真的能用"的按钮：`换一批证据` 要等重跑接线（D-4b）落地后再加
        assert_eq!(buttons.len(), 1, "当前只应有『打开详情』");
        assert_eq!(buttons[0]["text"]["content"], "打开详情");

        // 模拟飞书：把 value 对象字符串化后作为 action_value 交回来
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: buttons[0]["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::OpenDetail { session_id: "sess-9".into() }),
            "按钮往返失败"
        );
    }

    #[test]
    fn webui_url_uses_the_lan_address_not_loopback() {
        assert_eq!(webui_url("192.168.1.5", 8443), "https://192.168.1.5:8443/");
        assert_eq!(webui_url("  10.0.0.7  ", 9000), "https://10.0.0.7:9000/", "首尾空白要清掉");
        assert!(
            !webui_url("192.168.1.5", 8443).contains("127.0.0.1"),
            "必须给 LAN 地址 —— 用户在手机上点，回环地址点不开"
        );
    }

    #[test]
    fn actions_are_skipped_without_a_session_id() {
        let mut card = render_answer_card("答", 1, 1, 1, 1000);
        let before = card["elements"].as_array().unwrap().len();
        append_actions(&mut card, "   ");
        assert_eq!(
            card["elements"].as_array().unwrap().len(),
            before,
            "没有 session_id 就不该加按钮（点了也动不了，只会是噪音）"
        );
    }
}
