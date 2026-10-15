//! 飞书入站事件的**准入判定**（`docs/LARK_BOT.md` §7.1 的规格实现）。
//!
//! 这里是"仅私聊 + 用户白名单"落地的地方。之所以必须放在我们自己的代码里：
//! 实测（2026-10-15）**飞书侧拦不住"把机器人拉进群"**，控制台也没有那个开关
//! （§7.1 ⑨ ③），所以**唯一的防线就是这段判定**。
//!
//! 判定顺序（§7.1 ①，**严格按序，不要合并**）：
//! ```text
//! L0 sender_type != "user"                → 忽略（防机器人自问自答）
//! L2 chat_type != "p2p" 且要求私聊        → 拒绝（**回固定提示**）
//! L1 白名单为空 / 解析失败 / 不含 sender  → 拒绝（**静默**）
//! 通过                                     → 进入问答流程
//! ```

/// 飞书 `im.message.receive_v1` 中我们真正用到的字段。
///
/// **全部可选、宽松解析**：字段集随场景变化（群事件多出 `update_time` / `mentions`，
/// 私聊事件没有），且将来飞书加字段不该让解析失败。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct LarkMessageEvent {
    /// 事件类型，例如 `im.message.receive_v1`。
    #[serde(rename = "type", default)]
    pub event_type: String,
    /// 去重键（**不是** `event_id` —— 官方明言它重投会变，§8）。
    #[serde(default)]
    pub message_id: String,
    /// p2p 会话 id（`oc_…`），per-user 而非 per-device。
    #[serde(default)]
    pub chat_id: String,
    /// `"p2p"` / `"group"` —— 唯一稳定的类型判别。
    #[serde(default)]
    pub chat_type: String,
    #[serde(default)]
    pub message_type: String,
    /// 发送者 `open_id`（`ou_…`）—— 白名单的键。
    #[serde(default)]
    pub sender_id: String,
    /// `"user"` 才是真人；机器人/应用发出的消息不是这个值。
    #[serde(default)]
    pub sender_type: String,
    /// 已**预渲染**的正文（实测 text 消息给的是裸文本，不是 `{"text":…}` 信封）。
    #[serde(default)]
    pub content: String,
}

/// 准入判定所需的配置（来自 `app_settings`，见 §7.1 ⑤）。
#[derive(Debug, Clone)]
pub struct LarkGateConfig {
    /// L2：是否要求私聊。默认 `true`。
    pub require_p2p: bool,
    /// L1：允许的 `open_id`。**空 ⇒ 拒绝全部**（fail-closed，§7.1 ②）。
    pub allowed_users: Vec<String>,
}

impl Default for LarkGateConfig {
    fn default() -> Self {
        // 安全默认：要求私聊 + 空名单（＝谁都不许）。
        Self { require_p2p: true, allowed_users: Vec::new() }
    }
}

/// 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// 放行，可进入问答流程。
    Pass,
    /// 非用户来源（机器人/应用自己发的）—— 忽略，**且不回复**。
    DenyNonUser,
    /// 非私聊 —— 拒绝，**回固定提示**（群里多半是我自己误用，需要反馈）。
    DenyNonP2p,
    /// 不在白名单（含名单为空）—— 拒绝，**静默**（不向未知用户确认机器人存在）。
    DenyNotAllowed,
}

/// 非私聊时的固定提示（§7.1 ③）。
pub const NON_P2P_NOTICE: &str = "本机器人仅支持私聊";

/// 解析 `lark_allowed_users`（`app_settings` 里的 JSON 数组字符串）。
///
/// **任何异常都返回空**（＝拒绝全部）：解析失败、不是数组、元素非字符串。
/// 宁可把自己锁在外面，也不能因为"读不懂"就放行（§7.1 ②）。
pub fn parse_allowed_users(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// 按 §7.1 ① 的顺序判定。**输入顺序即优先级**，不要重排。
pub fn gate(ev: &LarkMessageEvent, cfg: &LarkGateConfig) -> GateVerdict {
    // L0：来源必须是真人。机器人/应用自己发的消息会被忽略，防止自问自答死循环
    //    （群样本里 `mentions[0].id` 就是机器人自己的 open_id，见 §7.1 ⑩）。
    if ev.sender_type.trim() != "user" {
        return GateVerdict::DenyNonUser;
    }
    // L2：类型闸门。放在 L1 之前 —— 否则群消息会被误记成"某用户不在白名单"，
    //    排查时两类拒绝原因就混了。
    if cfg.require_p2p && ev.chat_type.trim() != "p2p" {
        return GateVerdict::DenyNonP2p;
    }
    // L1：白名单（空 ⇒ 拒绝全部）。
    let sender = ev.sender_id.trim();
    if sender.is_empty() || !cfg.allowed_users.iter().any(|u| u == sender) {
        return GateVerdict::DenyNotAllowed;
    }
    GateVerdict::Pass
}

/// 剥掉正文里的 `@机器人名` 前缀（§7.1 ⑩ 实测：预渲染把 `@_user_1` 换成了机器人名，
/// 直接送检索会带噪音；`mentions[].key` 与实际文本不一致，故按文本剥）。
///
/// 规则保守：只剥**行首**的一个 `@…`（到第一个空白或全角空格为止），
/// 避免误伤正文里本来就有的 `@`。
pub fn strip_leading_mention(content: &str) -> String {
    let s = content.trim_start();
    if !s.starts_with('@') {
        return content.trim().to_string();
    }
    match s.find(|c: char| c.is_whitespace() || c == '\u{3000}') {
        Some(idx) => s[idx..].trim().to_string(),
        // 整条只有 `@某人`，没有正文
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(chat_type: &str, sender_type: &str, sender: &str) -> LarkMessageEvent {
        LarkMessageEvent {
            event_type: "im.message.receive_v1".into(),
            message_id: "om_1".into(),
            chat_id: "oc_1".into(),
            chat_type: chat_type.into(),
            message_type: "text".into(),
            sender_id: sender.into(),
            sender_type: sender_type.into(),
            content: "hello".into(),
        }
    }

    fn cfg(require_p2p: bool, users: &[&str]) -> LarkGateConfig {
        LarkGateConfig {
            require_p2p,
            allowed_users: users.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// §7.1 ⑦ #5：白名单命中 + p2p → 放行。
    #[test]
    fn p2p_allowlisted_user_passes() {
        assert_eq!(
            gate(&ev("p2p", "user", "ou_me"), &cfg(true, &["ou_me"])),
            GateVerdict::Pass
        );
    }

    /// §7.1 ⑦ #1：群消息必须被类型闸门拦下（**回提示**，不是静默）。
    #[test]
    fn group_message_is_denied_by_type_gate() {
        assert_eq!(
            gate(&ev("group", "user", "ou_me"), &cfg(true, &["ou_me"])),
            GateVerdict::DenyNonP2p
        );
    }

    /// §7.1 ⑦ #2：p2p 但不在名单 → 静默拒绝。
    #[test]
    fn p2p_stranger_is_denied_silently() {
        assert_eq!(
            gate(&ev("p2p", "user", "ou_stranger"), &cfg(true, &["ou_me"])),
            GateVerdict::DenyNotAllowed
        );
    }

    /// §7.1 ⑦ #3：**空白名单 ⇒ 拒绝全部**（fail-closed 回归）。
    #[test]
    fn empty_whitelist_denies_everyone() {
        assert_eq!(
            gate(&ev("p2p", "user", "ou_me"), &cfg(true, &[])),
            GateVerdict::DenyNotAllowed,
            "空白名单必须拒绝全部（fail-closed）"
        );
    }

    /// §7.1 ⑦ #7：机器人/应用自己发的消息必须被忽略（防自问自答）。
    #[test]
    fn non_user_sender_is_ignored_before_everything_else() {
        // 即使 chat_type 是 group、且 sender 不在名单，也应先归到 L0
        assert_eq!(
            gate(&ev("group", "app", "ou_bot"), &cfg(true, &[])),
            GateVerdict::DenyNonUser,
            "L0 必须先于 L2/L1"
        );
        assert_eq!(
            gate(&ev("p2p", "bot", "ou_me"), &cfg(true, &["ou_me"])),
            GateVerdict::DenyNonUser
        );
    }

    /// 群消息的拒绝原因必须是"类型"，不能被记成"不在白名单"（否则排查会误导）。
    #[test]
    fn group_denial_reason_is_type_not_identity() {
        assert_eq!(
            gate(&ev("group", "user", "ou_stranger"), &cfg(true, &["ou_me"])),
            GateVerdict::DenyNonP2p
        );
    }

    /// 关掉 `require_p2p` 后群消息可进入白名单判定 —— 保留这个开关，
    /// 但不改默认（默认始终要求私聊）。
    #[test]
    fn require_p2p_can_be_disabled_explicitly() {
        assert_eq!(
            gate(&ev("group", "user", "ou_me"), &cfg(false, &["ou_me"])),
            GateVerdict::Pass
        );
    }

    /// §7.1 ⑦ #4：名单**解析失败 ⇒ 拒绝全部**（不能因为读不懂就放行）。
    #[test]
    fn unparseable_whitelist_denies_everyone() {
        for raw in ["{not json", "\"ou_me\"", "123", "null", "[1,2]"] {
            assert!(
                parse_allowed_users(raw).is_empty(),
                "非法输入 {raw:?} 必须解析为空名单"
            );
        }
        assert!(parse_allowed_users("").is_empty(), "空串 = 空名单");
        assert_eq!(
            parse_allowed_users(r#"["ou_a", " ou_b ", ""]"#),
            vec!["ou_a".to_string(), "ou_b".to_string()],
            "应去空白并丢掉空元素"
        );
    }

    #[test]
    fn leading_mention_is_stripped() {
        assert_eq!(strip_leading_mention("@二飞 hello"), "hello");
        assert_eq!(strip_leading_mention("@二飞　hello"), "hello", "全角空格也算分隔");
        assert_eq!(strip_leading_mention("  @二飞 hello  "), "hello");
        assert_eq!(strip_leading_mention("@二飞"), "", "只有 @ 没有正文 → 空");
        assert_eq!(strip_leading_mention("hello"), "hello", "没有 @ 时只做 trim");
        assert_eq!(
            strip_leading_mention("价格是 a@b 吗"),
            "价格是 a@b 吗",
            "行内的 @ 不能被误伤"
        );
    }
}
