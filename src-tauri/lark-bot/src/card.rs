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
///
/// `template`：页眉配色（`blue` / `red` / …）。会话类卡片用 `blue` 让**会话名一眼可辨**
/// （2026-10-17 用户反馈"标题不明显"）；工具类卡片（帮助/列表/搜索）保持无配色的克制样式。
pub fn render_card(
    title: &str,
    body_md: &str,
    footer: Option<&str>,
    template: Option<&str>,
) -> serde_json::Value {
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
    let mut header = json!({ "title": { "tag": "plain_text", "content": title } });
    if let Some(t) = template.map(str::trim).filter(|t| !t.is_empty()) {
        header["template"] = json!(t);
    }
    json!({
        "config": { "wide_screen_mode": true },
        "header": header,
        "elements": elements,
    })
}

/// 会话类卡片的页眉配色（回答/流式/当前会话都用它，形成"蓝色 = 会话"的一致视觉）。
pub const TEMPLATE_SESSION: &str = "blue";
/// 出错卡片的页眉配色（红色 = 失败，与正常回答一眼区分）。
pub const TEMPLATE_ERROR: &str = "red";

/// "检索中"卡片：进度阶段 + 已经生成的部分内容 + **「停止」按钮**。
///
/// `title`：当前会话名（`/rename` 或首问自动生成）—— 回答卡与流式卡都带上它，
/// 用户始终能看到"现在在哪个会话里问答"。空/占位时退回无标题（见 [`headline`]）。
///
/// `session_id`：非空时挂「停止」按钮（`stop` 动作，见 [`parse_card_action`]）——
/// 与 `/stop` 命令同一条实现：按会话取消在途生成。流式过程中卡片会被反复 patch，
/// 按钮随每次渲染重建（patch 是整体替换，不会丢）。
pub fn render_working(
    progress: &str,
    partial: &str,
    title: Option<&str>,
    session_id: &str,
) -> serde_json::Value {
    let body = if partial.trim().is_empty() {
        format!("⏳ {progress}")
    } else {
        format!("{partial}\n\n---\n⏳ {progress}")
    };
    let mut card = render_card(
        &headline("正在检索…", title),
        &body,
        None,
        Some(TEMPLATE_SESSION),
    );
    let sid = session_id.trim();
    if !sid.is_empty()
        && let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut())
    {
        elements.push(json!({
            "tag": "action",
            "actions": [{
                "tag": "button",
                "text": { "tag": "plain_text", "content": "停止" },
                "type": "danger",
                "value": action_payload("stop", sid),
            }],
        }));
    }
    card
}

/// 卡片标题的组合规则：有会话名 → **`会话名 · 基础`**（名字在前，最醒目）；
/// 否则就用基础标题。
///
/// "新会话"是**占位标题**（首轮回答保存时才自动命名）→ 不显示它，
/// 免得每张卡都挂一个没信息量的"新会话"。
fn headline(base: &str, title: Option<&str>) -> String {
    match title.map(str::trim).filter(|t| !t.is_empty() && *t != "新会话") {
        Some(t) => format!("{t} · {base}"),
        None => base.to_string(),
    }
}

/// 渲染**分页**的最终卡片（`page` 从 1 开始）。
///
/// `title` = 会话名（首问后自动生成，或 `/rename` 指定）→ 页眉显示 `会话名 · 回答`，
/// 让用户一眼知道这条回答属于哪个会话（多会话切换时的关键信息）；配色见 [`TEMPLATE_SESSION`]。
///
/// `cancelled` = 这轮曾被**停止**（`/stop` 或卡片按钮）→ 页眉换成"已停止"，
/// 明确告诉用户"看到的是被打断的部分内容"，而不是一个看起来完整却莫名的短回答。
///
/// 证据条数与耗时**只标在最后一张**：否则一张长回答会连着刷三遍同样的脚注，
/// 反而把"读到第几段"这个真正有用的信息淹掉。
pub fn render_answer_card(
    body: &str,
    title: Option<&str>,
    page: usize,
    pages: usize,
    evidence_count: usize,
    took_ms: u64,
    cancelled: bool,
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
    let base = if cancelled {
        "已停止".to_string()
    } else if pages > 1 {
        format!("回答（{page} / {pages}）")
    } else {
        "回答".to_string()
    };
    let footer = if meta.is_empty() { None } else { Some(meta.join(" · ")) };
    render_card(&headline(&base, title), body, footer.as_deref(), Some(TEMPLATE_SESSION))
}

/// 会话**历史召回**卡片（`/history`）：与回答卡同款会话配色，页眉带页码。
///
/// `footer` 由调用方给（如"共 N 条消息"）；分页文案放页眉（`历史记录（1 / 3）`），
/// 因为它决定"这段是第几页"——用户点开就要知道从哪读起。
pub fn render_history_card(
    body: &str,
    title: Option<&str>,
    page: usize,
    pages: usize,
    footer: Option<&str>,
) -> serde_json::Value {
    let base = if pages > 1 {
        format!("历史记录（{page} / {pages}）")
    } else {
        "历史记录".to_string()
    };
    render_card(&headline(&base, title), body, footer, Some(TEMPLATE_SESSION))
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

/// 会话列表卡片：每行一个「**切换 N**」按钮（序号与文本列表一致）。
pub fn render_session_list_card(
    title: &str,
    items: &[ListItem],
    session_ids: &[String],
    footer: Option<&str>,
) -> serde_json::Value {
    render_list_card(title, items, footer, Some(("切换", "switch")), session_ids)
}

/// 删除确认卡片：每行一个「**删除 N**」按钮（二次确认，防误删）。
pub fn render_delete_list_card(
    title: &str,
    items: &[ListItem],
    session_ids: &[String],
    footer: Option<&str>,
) -> serde_json::Value {
    render_list_card(title, items, footer, Some(("删除", "remove_confirm")), session_ids)
}

/// 飞书**单条纯文本**的安全长度（按字符计）：超长会被拒/截断，宁可我们自己裁。
const MAX_MENU_LINE_CHARS: usize = 200;
/// 菜单列表的**纯文本**行数上限（渲染与动作序号共用同一常量）。
pub const MAX_MENU_LINES: usize = 5;

/// 一条列表项：飞书侧**同时**通过卡片与纯文本展示。
///
/// 为什么两套都要：卡片上按钮的 `card.action.trigger` 回调依赖长连接与后台菜单订阅，
/// 而"回复数字"走的是普通消息（现有链路，零配置）。两者共用同一份序号。
#[derive(Debug, Clone)]
pub struct ListItem {
    /// 展示标题（会话标题 / 文件名）。
    pub title: String,
    /// 次要说明（更新时间 / 路径等），可为空。
    pub detail: String,
}

/// 单行列表文本（**纯文本路径**用的格式；卡片侧用同一序号）。
fn menu_line(i: usize, item: &ListItem) -> String {
    let mut line = format!("{}. {}", i + 1, item.title.trim());
    let detail = item.detail.trim();
    if !detail.is_empty() {
        line.push_str("（");
        line.push_str(detail);
        line.push('）');
    }
    clip_bytes(&line, MAX_MENU_LINE_CHARS)
}

/// 列表的**纯文本**形态（发给飞书，供"回复数字"使用）。
pub fn menu_text(items: &[ListItem]) -> String {
    items
        .iter()
        .take(MAX_MENU_LINES)
        .enumerate()
        .map(|(i, item)| menu_line(i, item))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 会话/搜索列表卡片：`1.` 开头的 markdown 列表 + 可选说明 + 可选按钮。
///
/// `actions` 与 `items` 一一对应（第 i 个按钮作用于第 i 项）。**没有 items 时不渲染**
/// 调用方据此走别的分支。
pub fn render_list_card(
    title: &str,
    items: &[ListItem],
    footer: Option<&str>,
    button: Option<(&str, &str)>, // (label, action) —— 每项同一种动作（序号随 payload 传递）
    session_ids: &[String],
) -> serde_json::Value {
    let body = menu_text(items);
    let mut card = render_card(title, &body, footer, None);
    let Some((label, action)) = button else {
        return card;
    };
    let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut()) else {
        return card;
    };
    let buttons: Vec<serde_json::Value> = items
        .iter()
        .take(MAX_MENU_LINES)
        .enumerate()
        .filter_map(|(i, _)| {
            let sid = session_ids.get(i)?;
            if sid.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "tag": "button",
                "text": { "tag": "plain_text", "content": format!("{label} {}", i + 1) },
                "type": "default",
                "value": action_payload_indexed(action, sid, i + 1),
            }))
        })
        .collect();
    if buttons.is_empty() {
        return card;
    }
    elements.push(serde_json::json!({ "tag": "action", "actions": buttons }));
    card
}

/// 搜索结果卡片：每条 = 文件名（粗体）+ 路径 + 命中片段；按钮两行 ——
/// 「发送 N」（把原文件发到飞书）与「上一页 / 下一页 / ✦ 语义」。
///
/// 之前**不打按钮**是因为"打开 WebUI 预览"要带 LAN 地址、设备与网络拿不准；
/// 现在的按钮要么是**发送原文件到飞书**（字节由 bot 上传到飞书云，不需要客户端
/// 能访问本机），要么是**重新搜索**（同样由 bot 完成）—— 这个顾虑不成立
/// （见 `docs/LARK_BOT.md` §5.13）。
///
/// `view` 把"按钮要用的状态"收成一个结构体（query/page/pages/semantic…），
/// 避免 8 个位置参数；`items` 是正文条目。
pub struct SearchCardView<'a> {
    /// 原始查询词：翻页/语义按钮要用它重跑同一次搜索。
    pub query: &'a str,
    /// 页脚（页数、命中数、提示等）。
    pub footer: Option<&'a str>,
    /// 与 `items` 一一对应；空 id 的位置不出现「发送」按钮（取不到就少一个按钮，
    /// 而不是发错文件）。
    pub file_ids: &'a [String],
    /// 当前页 / 总页数（1-based）。`pages <= 1` 时不出现翻页按钮。
    pub page: usize,
    pub pages: usize,
    /// 当前是否语义模式（决定按钮文案与下一次点击的目标状态）。
    pub semantic: bool,
    /// Embedding 是否可用；不可用时**不显示**「✦ 语义」按钮 ——
    /// 点了没变化等于误导（与 WebUI 置灰同一个道理）。
    pub semantic_available: bool,
}

/// 翻页/语义按钮里携带的 query 上限（字符）。
///
/// 超过就不挂这些按钮、退回页脚文字提示：卡片 JSON 有 30KB 硬上限，
/// 超长 query 会把按钮 payload 顶爆，宁可少两个按钮也不能让整张卡发不出去。
pub const MAX_QUERY_BUTTON_CHARS: usize = 200;

/// 搜索卡上的导航按钮（翻页 / 语义）需要重跑搜索 —— query 必须能塞进 payload。
fn query_fits_in_button(query: &str) -> bool {
    let q = query.trim();
    !q.is_empty() && q.chars().count() <= MAX_QUERY_BUTTON_CHARS
}

/// 一个小按钮（导航行用；`kind` 是飞书按钮类型：default / primary）。
fn nav_button(label: &str, value: serde_json::Value, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "tag": "button",
        "text": { "tag": "plain_text", "content": label },
        "type": kind,
        "value": value,
    })
}

pub fn render_search_card(view: &SearchCardView<'_>, items: &[ListItem]) -> serde_json::Value {
    let mut lines: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        lines.push(format!("**{}. {}**", i + 1, item.title.trim()));
        if !item.detail.trim().is_empty() {
            lines.push(format!("   {}", item.detail.trim()));
        }
        lines.push(String::new());
    }
    let body = lines.join("\n");
    let mut card = render_card(
        &format!("搜索「{}」", view.query.trim()),
        &body,
        view.footer,
        None,
    );
    let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut()) else {
        return card;
    };

    // 第一行：发送原文件（有 file_id 的条目才有按钮）
    let send_buttons: Vec<serde_json::Value> = items
        .iter()
        .take(MAX_MENU_LINES)
        .enumerate()
        .filter_map(|(i, _)| {
            let fid = view.file_ids.get(i)?.trim();
            if fid.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "tag": "button",
                "text": { "tag": "plain_text", "content": format!("发送 {}", i + 1) },
                "type": "default",
                "value": action_payload_file("send_file", fid),
            }))
        })
        .collect();
    if !send_buttons.is_empty() {
        elements.push(serde_json::json!({ "tag": "action", "actions": send_buttons }));
    }

    // 第二行：翻页 + 语义开关。query 超长时整行不出现（退回页脚文字提示）。
    let mut nav_buttons: Vec<serde_json::Value> = Vec::new();
    if query_fits_in_button(view.query) {
        if view.page > 1 {
            nav_buttons.push(nav_button(
                "上一页",
                action_payload_search(view.query, view.page - 1, view.semantic),
                "default",
            ));
        }
        if view.page < view.pages {
            nav_buttons.push(nav_button(
                "下一页",
                action_payload_search(view.query, view.page + 1, view.semantic),
                "default",
            ));
        }
        if view.semantic_available {
            let (label, kind) = if view.semantic {
                ("✦ 语义：开", "primary")
            } else {
                ("✦ 语义：关", "default")
            };
            nav_buttons.push(nav_button(
                label,
                action_payload_search(view.query, view.page, !view.semantic),
                kind,
            ));
        }
    }
    if !nav_buttons.is_empty() {
        elements.push(serde_json::json!({ "tag": "action", "actions": nav_buttons }));
    }
    card
}

/// 失败卡片（**在"卡已发出"之后**出错时用）：不再另发文本，避免答案重复出现两次。
pub fn render_error(detail: &str) -> serde_json::Value {
    render_card(
        "出错了",
        &format!("未能完成这次回答：\n\n```\n{detail}\n```"),
        None,
        Some(TEMPLATE_ERROR),
    )
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
    ///
    /// ⚠️ 准入判定实际走 [`operator_open_id`]（直接读**原始 JSON**，兼容多种形状）——
    /// 这个字段只作反序列化的文档化：真实样本的字段形状尚未确认，宁可多留一条读原始
    /// 事件的路径，也不把安全性押在一种形状上。
    #[allow(dead_code)]
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
    /// 「新建会话」（帮助卡 / 会话列表卡）：**不需要** session_id。
    NewSession,
    /// 「切换会话」：把按钮对应的会话置为活动。
    Switch { session_id: String },
    /// 「删除会话」：先回一本确认卡（真正的删除走 [`CardAction::RemoveConfirm`]）。
    Remove { session_id: String, index: usize },
    /// 「确认删除」：经由确认卡上的按钮到这里才真删。
    RemoveConfirm { session_id: String, index: usize },
    /// 「停止」：取消该会话**正在生成**的回答（与 `/stop` 同一实现）。
    Stop { session_id: String },
    /// 「发送文件」：把搜索结果里的第 N 个文件（`file_id`）作为媒体消息发到飞书。
    ///
    /// 与其它动作不同：它**不需要** session_id（与"对哪个会话做事"无关），
    /// 所以解析时走单独一条分支（缺 `file_id` 才忽略）。
    SendFile { file_id: String },
    /// 「重新搜索」：翻页或切换语义模式 —— 用同一 `query` 重跑并**原地更新**卡片。
    ///
    /// 一个动作覆盖两种按钮：翻页 = 换 `page`；语义开关 = 换 `semantic`。
    /// 同样不需要 session_id。
    SearchAgain { query: String, page: usize, semantic: bool },
}

/// 从 `action_value` 解析出动作。返回 `None` = **不认识/不完整 → 直接忽略**。
///
/// 纯函数：路由错了会变成"点 A 触发 B"，而这条链路**没法靠肉眼在飞书里穷举**，
/// 所以它是 D 最该被单测盯住的一层。
pub fn parse_card_action(ev: &CardActionEvent) -> Option<CardAction> {
    // 二次 parse：飞书给的是 JSON **字符串**（§2 实测发现 ①）
    let v: serde_json::Value = serde_json::from_str(ev.action_value.trim()).ok()?;
    let action = v.get("action")?.as_str()?.trim();
    // 唯一**不需要** session_id 的动作：新建会话（没有"对哪个会话做事"的问题）
    if action == "new_session" {
        return Some(CardAction::NewSession);
    }
    // 「发送文件」也不需要 session_id —— 它作用于搜索结果里的 `file_id`。
    if action == "send_file" {
        let file_id = v
            .get("file_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if file_id.is_empty() {
            return None;
        }
        return Some(CardAction::SendFile { file_id });
    }
    // 「重新搜索」（翻页 / 语义开关）同样不需要 session_id。
    if action == "search" {
        let query = v
            .get("query")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let page = v.get("page").and_then(|p| p.as_u64()).unwrap_or(0) as usize;
        let semantic = v.get("semantic").and_then(|s| s.as_bool()).unwrap_or(false);
        // fail-closed：缺 query / 页码越界一律忽略（页码上限与 `/find` 的解析共用同一常量）。
        if query.is_empty() || page < 1 || page > crate::commands::MAX_FIND_PAGE {
            return None;
        }
        return Some(CardAction::SearchAgain { query, page, semantic });
    }
    let session_id = v
        .get("session_id")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // 其余动作都需要 session_id（不知道对哪个会话做事）→ 忽略
    if session_id.is_empty() {
        return None;
    }
    let index = v
        .get("index")
        .and_then(|i| i.as_u64())
        .map(|i| i as usize)
        .unwrap_or(0);
    match action {
        "open_detail" => Some(CardAction::OpenDetail { session_id }),
        "retry" => Some(CardAction::Retry { session_id }),
        "switch" => Some(CardAction::Switch { session_id }),
        "remove" if index >= 1 => Some(CardAction::Remove { session_id, index }),
        "remove_confirm" if index >= 1 => Some(CardAction::RemoveConfirm { session_id, index }),
        "stop" => Some(CardAction::Stop { session_id }),
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

/// 带**序号**的按钮 payload（会话列表卡用）：`index` 是 1-based 的展示序号。
pub fn action_payload_indexed(action: &str, session_id: &str, index: usize) -> serde_json::Value {
    serde_json::json!({ "action": action, "session_id": session_id, "index": index })
}

/// 「发送文件」按钮的 payload：只带 `file_id`（它不作用于任何会话）。
pub fn action_payload_file(action: &str, file_id: &str) -> serde_json::Value {
    serde_json::json!({ "action": action, "file_id": file_id })
}

/// 「重新搜索」按钮的 payload：翻页与语义开关共用（只改其中一个字段）。
pub fn action_payload_search(query: &str, page: usize, semantic: bool) -> serde_json::Value {
    serde_json::json!({
        "action": "search",
        "query": query.trim(),
        "page": page,
        "semantic": semantic,
    })
}

/// WebUI 的**深链**（给「打开详情」按钮用）：直达某条会话，用户可以在浏览器里继续追问。
///
/// 两个要点：
/// - `origin` 必须是**外部设备能访问的基址**（`https://host:port`），不能是 `127.0.0.1`
///   —— 用户多半是在手机 / 别的机器上点这个按钮（地址选择见 `main::webui_address`：
///   自定义基址 > Tailscale > 局域网 IP）；
/// - 必须带上 `origin=lark&session=<id>` —— 应用是 `HashRouter`（故 query 在 `#/chat?` 之后），
///   而会话按端隔离：不带 `origin=lark` 去读飞书会话只会得到 `session not found`。
///
/// 纯函数：这是"点开到底能不能打开到那一条"的唯一决定因素，也是唯一值得单测的部分。
pub fn webui_deep_link(origin: &str, session_id: &str) -> String {
    format!(
        "{}/#/chat?origin=lark&session={}",
        origin.trim().trim_end_matches('/'),
        encode_query_value(session_id.trim())
    )
}

/// query 值的最小百分号编码（会话 id 实际是 UUID，这里只为**防御性收敛**任意输入）。
fn encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 给卡片**追加**动作按钮（只有"最后一张"该用）。
///
/// - **只在最后一张**：多段回答时中间页挂按钮会让"点哪个"变含糊；
/// - **没有 session_id 就不加**：动作都作用于某个会话，匿名按钮点不动反而是噪音；
/// - **只加真的能用的按钮**：两个按钮（`打开详情` / `换一批证据`）都已接线（D-4a / D-4b）。
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
            {
                "tag": "button",
                "text": { "tag": "plain_text", "content": "换一批证据" },
                "type": "default",
                "value": action_payload("retry", sid),
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
        let card = render_answer_card(&long, None, 1, 1, 3, 1000, false);
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

    /// 会话名出现在卡片页眉（**名字在前**，且带会话配色）；占位标题与空值不显示。
    #[test]
    fn card_headline_leads_with_session_title_and_hides_placeholder() {
        let with = render_answer_card("答案", Some("合同审查"), 1, 1, 2, 3400, false);
        assert_eq!(with["header"]["title"]["content"], "合同审查 · 回答");
        assert_eq!(with["header"]["template"], "blue", "会话卡片要配色（标题醒目）");
        let working = render_working("正在检索…", "", Some("合同审查"), "sess-1");
        assert_eq!(working["header"]["title"]["content"], "合同审查 · 正在检索…");
        let paged = render_answer_card("第一段", Some("合同审查"), 1, 3, 0, 0, false);
        assert_eq!(paged["header"]["title"]["content"], "合同审查 · 回答（1 / 3）");

        for placeholder in [None, Some(""), Some("   "), Some("新会话")] {
            let c = render_answer_card("答案", placeholder, 1, 1, 0, 0, false);
            assert_eq!(
                c["header"]["title"]["content"], "回答",
                "占位标题不该显示：{placeholder:?}"
            );
        }
    }

    /// 被停止的回答：页眉明确写"已停止"（不是看起来完整却莫名的短回答）。
    #[test]
    fn cancelled_answer_card_marks_itself_as_stopped() {
        let c = render_answer_card("部分内容", Some("合同审查"), 1, 1, 2, 3400, true);
        assert_eq!(c["header"]["title"]["content"], "合同审查 · 已停止");
        let c2 = render_answer_card("部分内容", None, 1, 1, 0, 0, true);
        assert_eq!(c2["header"]["title"]["content"], "已停止");
    }

    /// 历史召回卡：页眉带页码与会话名、会话配色；footer 走 note。
    #[test]
    fn history_card_marks_page_and_uses_session_style() {
        let c = render_history_card("内容", Some("合同审查"), 1, 2, Some("共 12 条消息"));
        assert_eq!(c["header"]["title"]["content"], "合同审查 · 历史记录（1 / 2）");
        assert_eq!(c["header"]["template"], "blue");
        let tags: Vec<&str> = c["elements"].as_array().unwrap().iter()
            .map(|e| e["tag"].as_str().unwrap_or("")).collect();
        assert_eq!(tags, vec!["markdown", "hr", "note"], "footer 走 note 元素");
        // 单页时不显示页码
        let one = render_history_card("内容", None, 1, 1, None);
        assert_eq!(one["header"]["title"]["content"], "历史记录");
    }

    /// 工作卡带「停止」按钮（有 session_id 时），并往返成 `CardAction::Stop`。
    #[test]
    fn working_card_carries_stop_button() {
        let c = render_working("正在检索…", "", None, "sess-1");
        let last = c["elements"].as_array().unwrap().last().unwrap();
        assert_eq!(last["tag"], "action");
        let btn = &last["actions"][0];
        assert_eq!(btn["text"]["content"], "停止");
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: btn["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::Stop { session_id: "sess-1".into() }),
            "停止按钮往返失败"
        );
        // 没有 session_id（理论上不会发生）→ 不加按钮
        let c2 = render_working("正在检索…", "", None, "  ");
        assert!(
            c2["elements"].as_array().unwrap().iter().all(|e| e["tag"] != "action"),
            "无会话 id 不该挂按钮"
        );
    }

    #[test]
    fn card_shape_is_valid_1_0() {
        let c = render_answer_card("答案 [1]", None, 1, 1, 2, 3400, false);
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
        let c = render_working("正在检索文档…", "", None, "");
        assert!(c["elements"][0]["content"].as_str().unwrap_or("").contains("⏳"));
        let c2 = render_working("生成回答中…", "已经写了一半", None, "");
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
        let c1 = render_answer_card("第一段", None, 1, 3, 19, 67000, false);
        assert_eq!(c1["header"]["title"]["content"], "回答（1 / 3）");
        let f1 = &c1["elements"][2]["elements"][0]["content"];
        assert_eq!(f1, "第 1 / 3 段", "非最后一张不该标证据/耗时");
        let c3 = render_answer_card("末段", None, 3, 3, 19, 67000, false);
        let f3 = c3["elements"][2]["elements"][0]["content"].as_str().unwrap_or("");
        assert!(f3.contains("第 3 / 3 段") && f3.contains("19 条证据") && f3.contains("67s"), "实际：{f3}");
    }

    #[test]
    fn single_page_card_has_no_page_marker() {
        let c = render_answer_card("就一段", None, 1, 1, 2, 3400, false);
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
        let mut card = render_answer_card("答", None, 1, 1, 1, 1000, false);
        append_actions(&mut card, "sess-9");
        let elems = card["elements"].as_array().unwrap();
        let action = elems.last().expect("应追加 action 元素");
        assert_eq!(action["tag"], "action");
        let buttons = action["actions"].as_array().unwrap();
        assert_eq!(buttons.len(), 2, "两个按钮：打开详情 / 换一批证据（都已接线）");
        assert_eq!(buttons[0]["text"]["content"], "打开详情");
        assert_eq!(buttons[1]["text"]["content"], "换一批证据");

        // 模拟飞书：把 value 对象字符串化后作为 action_value 交回来
        for (btn, expect) in [
            (&buttons[0], CardAction::OpenDetail { session_id: "sess-9".into() }),
            (&buttons[1], CardAction::Retry { session_id: "sess-9".into() }),
        ] {
            let ev = CardActionEvent {
                event_type: "card.action.trigger".into(),
                action_value: btn["value"].to_string(),
                ..Default::default()
            };
            assert_eq!(parse_card_action(&ev), Some(expect), "按钮往返失败：{btn}");
        }
    }

    #[test]
    fn webui_deep_link_uses_the_given_origin_not_loopback() {
        assert_eq!(
            webui_deep_link(
                "https://192.168.1.5:8443",
                "3d3a9b1e-8c03-4fe3-a448-142ff70dceeb"
            ),
            "https://192.168.1.5:8443/#/chat?origin=lark&session=3d3a9b1e-8c03-4fe3-a448-142ff70dceeb"
        );
        assert_eq!(
            webui_deep_link("  https://10.0.0.7:9000/  ", "s"),
            "https://10.0.0.7:9000/#/chat?origin=lark&session=s",
            "首尾空白与结尾斜杠都要清掉（避免拼出 //#/chat）"
        );
        assert!(
            !webui_deep_link("https://192.168.1.5:8443", "s").contains("127.0.0.1"),
            "必须给外部可达地址 —— 用户在手机上点，回环地址点不开"
        );
    }

    /// 深链必须**指名到那一条会话**、且带上 `origin=lark`：
    /// 应用是 HashRouter（query 在 `#/chat?` 之后），会话又按端隔离 —— 少了 origin 只会
    /// 得到 `session not found`（2026-10-16 实测：带 origin 18050 字节，不带就是那个错误）。
    #[test]
    fn webui_deep_link_targets_one_session_and_declares_its_origin() {
        let url = webui_deep_link("https://192.168.1.5:8443", "  sess-1  ");
        assert!(url.contains("/#/chat?"), "HashRouter：query 必须在 hash 之后：{url}");
        assert!(url.contains("origin=lark"), "缺 origin 读不到飞书会话：{url}");
        assert!(url.contains("session=sess-1"), "必须指名会话（且去掉空白）：{url}");
    }

    #[test]
    fn deep_link_encodes_session_id_defensively() {
        let url = webui_deep_link("https://10.0.0.7:9000", "a b&c=1");
        assert!(url.contains("session=a%20b%26c%3D1"), "特殊字符必须编码，否则会截断 query：{url}");
        // UUID 这类正常取值不应被改动（真实会话 id 就是这种形状）
        let normal = webui_deep_link("https://10.0.0.7:9000", "3d3a9b1e-8c03-4fe3-a448-142ff70dceeb");
        assert!(normal.contains("session=3d3a9b1e-8c03-4fe3-a448-142ff70dceeb"), "{normal}");
    }

    #[test]
    fn actions_are_skipped_without_a_session_id() {
        let mut card = render_answer_card("答", None, 1, 1, 1, 1000, false);
        let before = card["elements"].as_array().unwrap().len();
        append_actions(&mut card, "   ");
        assert_eq!(
            card["elements"].as_array().unwrap().len(),
            before,
            "没有 session_id 就不该加按钮（点了也动不了，只会是噪音）"
        );
    }

    // ── 多会话/搜索：动作往返（§11）──

    /// 新动作的**发送侧 ↔ 解析侧**往返：按钮 value 被字符串化后必须能被认回来。
    #[test]
    fn session_actions_round_trip_with_their_builders() {
        let cases = [
            (action_payload("switch", "s-1"), CardAction::Switch { session_id: "s-1".into() }),
            (
                action_payload_indexed("remove", "s-2", 2),
                CardAction::Remove { session_id: "s-2".into(), index: 2 },
            ),
            (
                action_payload_indexed("remove_confirm", "s-3", 3),
                CardAction::RemoveConfirm { session_id: "s-3".into(), index: 3 },
            ),
            (action_payload("new_session", ""), CardAction::NewSession),
            (action_payload("stop", "s-5"), CardAction::Stop { session_id: "s-5".into() }),
            (
                action_payload_file("send_file", "f-9"),
                CardAction::SendFile { file_id: "f-9".into() },
            ),
            (
                action_payload_search("合同 2024", 2, true),
                CardAction::SearchAgain { query: "合同 2024".into(), page: 2, semantic: true },
            ),
        ];
        for (payload, expect) in cases {
            let ev = CardActionEvent {
                event_type: "card.action.trigger".into(),
                // 飞书会把 value 对象**字符串化**后放进 action_value（§2 实测样本）
                action_value: payload.to_string(),
                ..Default::default()
            };
            assert_eq!(parse_card_action(&ev), Some(expect), "往返失败：{payload}");
        }
    }

    /// 不完整/未知动作必须**忽略**（fail-closed）：缺 session_id、缺 index、未知 action。
    #[test]
    fn partial_or_unknown_card_actions_are_ignored() {
        for bad in [
            r#"{"action":"switch"}"#,
            r#"{"action":"switch","session_id":"  "}"#,
            r#"{"action":"remove","session_id":"s"}"#,        // 缺 index
            r#"{"action":"remove","session_id":"s","index":0}"#, // index 必须 ≥1
            r#"{"action":"remove_confirm","session_id":"s"}"#,
            r#"{"action":"send_file"}"#,                    // 缺 file_id
            r#"{"action":"send_file","file_id":"  "}"#,     // 空白 file_id
            r#"{"action":"search"}"#,                       // 缺 query/page
            r#"{"action":"search","query":"x","page":0}"#,  // 页码必须 ≥1
            r#"{"action":"search","query":"x","page":100}"#, // 页码上限 99（与 /find 共用）
            r#"{"action":"search","query":"  ","page":1}"#, // 空白 query
            r#"{"action":"cancel"}"#,
            r#"{"action":"__unknown__","session_id":"s"}"#,
        ] {
            let ev = CardActionEvent { action_value: bad.to_string(), ..Default::default() };
            assert_eq!(parse_card_action(&ev), None, "必须忽略：{bad}");
        }
    }

    /// 列表卡：正文序号与按钮序号**一致**；没有 session_id 的位置不出现按钮。
    #[test]
    fn list_card_numbers_text_and_buttons_consistently() {
        let items = vec![
            ListItem { title: "合同审查".into(), detail: "10-16 14:02".into() },
            ListItem { title: "汪均益案".into(), detail: String::new() },
            ListItem { title: "缺 id 的会话".into(), detail: String::new() },
        ];
        let ids = vec!["s-1".to_string(), "s-2".to_string(), String::new()];
        let card = render_session_list_card("你的会话", &items, &ids, Some("回复数字可点选"));
        let body = card["elements"][0]["content"].as_str().unwrap_or("");
        assert!(body.contains("1. 合同审查（10-16 14:02）"), "正文：{body}");
        assert!(body.contains("2. 汪均益案"), "正文：{body}");
        let last = card["elements"].as_array().unwrap().last().unwrap();
        let buttons = last["actions"].as_array().unwrap();
        assert_eq!(buttons.len(), 2, "只应为有 session_id 的前两条加按钮");
        assert_eq!(buttons[0]["text"]["content"], "切换 1");
        assert_eq!(buttons[1]["text"]["content"], "切换 2");
        // 按钮 1 往返：switch s-1 index 1
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: buttons[0]["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::Switch { session_id: "s-1".into() })
        );
        // footer 走 note 元素
        let tags: Vec<&str> = card["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["tag"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(tags, vec!["markdown", "hr", "note", "action"]);
    }

    /// 搜索卡：序号/路径/片段在正文；有 `file_id` 的条目带「发送 N」按钮。
    #[test]
    fn search_card_renders_hits_and_send_buttons_when_ids_exist() {
        let items = vec![
            ListItem { title: "一审判决书.pdf".into(), detail: "合同纠纷/2024\n   …汪均益…".into() },
            ListItem { title: "48号裁定.docx".into(), detail: "上诉材料".into() },
            ListItem { title: "缺 id 的命中.txt".into(), detail: String::new() },
        ];
        let ids = vec!["f-1".to_string(), String::new(), "f-3".to_string()];
        let view = SearchCardView {
            query: "汪均益",
            footer: Some("共 2 条"),
            file_ids: &ids,
            page: 1,
            pages: 1,
            semantic: false,
            semantic_available: false,
        };
        let card = render_search_card(&view, &items);
        let body = card["elements"][0]["content"].as_str().unwrap_or("");
        assert!(body.contains("**1. 一审判决书.pdf**"), "正文：{body}");
        assert!(body.contains("合同纠纷/2024"), "正文：{body}");
        assert!(body.contains("汪均益"), "正文：{body}");

        // 只给有 file_id 的条目加按钮；单页 + 无 Embedding → 没有导航行
        let elements = card["elements"].as_array().unwrap();
        let actions: Vec<&serde_json::Value> = elements.iter().filter(|e| e["tag"] == "action").collect();
        assert_eq!(actions.len(), 1, "只有发送行：{card}");
        let buttons = actions[0]["actions"].as_array().unwrap();
        assert_eq!(buttons.len(), 2, "第 2 条没有 file_id，不应有按钮");
        assert_eq!(buttons[0]["text"]["content"], "发送 1");
        assert_eq!(buttons[1]["text"]["content"], "发送 3");
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: buttons[0]["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::SendFile { file_id: "f-1".into() })
        );

        // 完全没有 file_id → 连发送行都不出现
        let no_ids: Vec<String> = vec![];
        let plain_view = SearchCardView {
            query: "汪均益",
            footer: Some("共 2 条"),
            file_ids: &no_ids,
            page: 1,
            pages: 1,
            semantic: false,
            semantic_available: false,
        };
        let plain = render_search_card(&plain_view, &items);
        assert!(
            !plain["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["tag"] == "action"),
            "没有 file_id 就不该出现空按钮行"
        );
    }

    /// 翻页按钮：第 1 页无「上一页」、末页无「下一页」、单页整行不出现；
    /// 点击的 payload 必须能原样解析回同一次搜索的下一页。
    #[test]
    fn search_card_paginates_only_when_there_are_more_pages() {
        let items = vec![ListItem { title: "a.pdf".into(), detail: String::new() }];
        let ids: Vec<String> = vec![];
        let nav_of = |card: &serde_json::Value| -> Vec<serde_json::Value> {
            card["elements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["tag"] == "action")
                .flat_map(|e| e["actions"].as_array().cloned().unwrap_or_default())
                .collect()
        };
        let view = |page: usize, pages: usize| SearchCardView {
            query: "合同 2024",
            footer: None,
            file_ids: &ids,
            page,
            pages,
            semantic: false,
            semantic_available: false,
        };

        let first = nav_of(&render_search_card(&view(1, 3), &items));
        assert_eq!(first.len(), 1, "第 1 页只有「下一页」：{first:?}");
        assert_eq!(first[0]["text"]["content"], "下一页");
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: first[0]["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::SearchAgain { query: "合同 2024".into(), page: 2, semantic: false })
        );

        let last = nav_of(&render_search_card(&view(3, 3), &items));
        assert_eq!(last.len(), 1, "末页只有「上一页」：{last:?}");
        assert_eq!(last[0]["text"]["content"], "上一页");

        let single = nav_of(&render_search_card(&view(1, 1), &items));
        assert!(single.is_empty(), "单页不出现翻页行：{single:?}");
    }

    /// 语义开关：文案/配色反映当前状态，点击后请求**相反**的状态；
    /// Embedding 不可用时按钮不出现（点了没变化 = 误导）。
    #[test]
    fn search_card_semantic_toggle_reflects_state_and_availability() {
        let items = vec![ListItem { title: "a.pdf".into(), detail: String::new() }];
        let ids: Vec<String> = vec![];
        let semantic_button = |semantic: bool, available: bool| -> Option<serde_json::Value> {
            let view = SearchCardView {
                query: "合同",
                footer: None,
                file_ids: &ids,
                page: 1,
                pages: 1,
                semantic,
                semantic_available: available,
            };
            let card = render_search_card(&view, &items);
            card["elements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["tag"] == "action")
                .flat_map(|e| e["actions"].as_array().cloned().unwrap_or_default())
                .find(|b| b["text"]["content"].as_str().unwrap_or("").contains("语义"))
        };

        let off = semantic_button(false, true).expect("可用时应有语义按钮");
        assert_eq!(off["text"]["content"], "✦ 语义：关");
        assert_eq!(off["type"], "default");
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: off["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::SearchAgain { query: "合同".into(), page: 1, semantic: true }),
            "关闭状态点一下 → 请求开启"
        );

        let on = semantic_button(true, true).expect("可用时应有语义按钮");
        assert_eq!(on["text"]["content"], "✦ 语义：开");
        assert_eq!(on["type"], "primary", "开启时用蓝色表示状态");
        let ev = CardActionEvent {
            event_type: "card.action.trigger".into(),
            action_value: on["value"].to_string(),
            ..Default::default()
        };
        assert_eq!(
            parse_card_action(&ev),
            Some(CardAction::SearchAgain { query: "合同".into(), page: 1, semantic: false }),
            "开启状态点一下 → 请求关闭"
        );

        assert!(
            semantic_button(false, false).is_none(),
            "Embedding 不可用时不显示语义按钮"
        );
    }

    /// query 超过 payload 上限（200 字）：整行导航按钮不出现（不能把卡片顶爆）。
    #[test]
    fn search_card_drops_nav_buttons_for_overlong_query() {
        let items = vec![ListItem { title: "a.pdf".into(), detail: String::new() }];
        let ids: Vec<String> = vec![];
        let long_query = "长".repeat(MAX_QUERY_BUTTON_CHARS + 1);
        let view = SearchCardView {
            query: &long_query,
            footer: Some("下一页：/find …（文字提示保留）"),
            file_ids: &ids,
            page: 1,
            pages: 3,
            semantic: true,
            semantic_available: true,
        };
        let card = render_search_card(&view, &items);
        assert!(
            !card["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["tag"] == "action"),
            "超长 query 不挂任何按钮行：{card}"
        );

        // 刚好等于上限 → 允许
        let edge_query = "长".repeat(MAX_QUERY_BUTTON_CHARS);
        let edge = SearchCardView { query: &edge_query, ..view };
        let card = render_search_card(&edge, &items);
        assert!(
            card["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["tag"] == "action"),
            "等于上限时应正常挂按钮"
        );
    }
}
