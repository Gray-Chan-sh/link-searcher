//! 飞书命令的**零配置可点选辅助**（`docs/LARK_BOT.md` §11）。
//!
//! 背景：官方 FAQ 明确"机器人本身不支持斜杠命令"——打 `/` 不会弹任何列表。
//! 于是我们给"列表类回复"（`/sessions`、删除确认）配上**两条并行的点选路径**：
//!
//! 1. **卡片按钮**（`card.action.trigger` 回调）—— 点击即执行；
//! 2. **回复数字**（本模块）—— 用户直接发 `3`，由 [`MenuMemory`] 里的
//!    "序号 → 动作"映射解析后执行。这条走普通消息链路，**零配置**（不依赖后台订阅）。
//!
//! 两条路共用**同一份序号**（列表渲染时生成）。
//!
//! 为什么记忆在**进程内存**而不是 SQLite：菜单是**一次性的 UI 状态**，且断点续期
//! 没有意义（bot 重启后用户看到的那条列表早已翻篇）。真正的会话状态在
//! `lark_chat_sessions` 表里，不在这里 —— 这里丢了最多是"再发一次 /sessions"。

use std::collections::HashMap;

/// 菜单记忆的有效期（秒）—— 超时后数字回复不再被当作点选，正常走 AI。
pub const MENU_TTL_SECS: i64 = 600;
/// 记忆里保留的私聊数上限（防无限增长；单用户场景远用不到）。
const MAX_CHATS: usize = 32;

/// 一条菜单项要执行的动作（bot 内部的最小指令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// 切换到该会话。
    Switch(String),
}

/// 从一段**纯文本**（非卡片）里解析出菜单项 → 动作。
///
/// 只认 `1. xxx` / `1) xxx` / `1、xxx` 形式的行（bot 自己的列表就是这个格式），
/// 且**必须至少两条**才认为这是一个菜单 —— 单行数字开头的普通文本（如
/// "2024 年的计划…"）不能变成点选目标（误伤的代价是正常提问被吞掉）。
///
/// 序号即 `actions` 的下标（`1 → actions[0]`）：列表本来就是按这个顺序展示的。
/// 超出 `actions` 长度的序号忽略。
pub fn resolve_text_menu(text: &str, actions: &[MenuAction]) -> HashMap<usize, MenuAction> {
    let mut out = HashMap::new();
    if actions.is_empty() {
        return out;
    }
    for line in text.lines() {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars().peekable();
        let mut num = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_digit() {
                num.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        if num.is_empty() {
            continue;
        }
        // 数字后必须紧跟 `.` / `)` / `、`（且再往后是空白或内容）
        match chars.next() {
            Some('.') | Some(')') | Some('、') => {}
            _ => continue,
        }
        let Ok(idx) = num.parse::<usize>() else { continue };
        if idx == 0 || idx > actions.len() {
            continue;
        }
        out.insert(idx, actions[idx - 1].clone());
    }
    // 只有一个数字行 ⇒ 大概率是普通文本，不当作菜单
    if out.len() < 2 {
        return HashMap::new();
    }
    out
}

/// 上次菜单的记忆（per-chat）。
#[derive(Debug, Default)]
pub struct MenuMemory {
    /// `chat_id → (存续截止时刻, 序号 → 动作)`
    entries: HashMap<String, (i64, HashMap<usize, MenuAction>)>,
}

impl MenuMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记下某个私聊最近一次列表的（序号 → 动作）映射。
    pub fn remember(&mut self, chat_id: &str, now: i64, items: HashMap<usize, MenuAction>) {
        if items.is_empty() {
            self.entries.remove(chat_id);
            return;
        }
        self.entries.insert(chat_id.to_string(), (now + MENU_TTL_SECS, items));
        self.prune(now);
    }

    /// 解析一次数字点选：命中（菜单仍有效且序号存在）→ 对应动作。
    ///
    /// 命中的条目**保留**（用户可以连点不同序号）；过期条目顺手清掉。
    pub fn resolve(&mut self, chat_id: &str, now: i64, index: usize) -> Option<MenuAction> {
        let (expires_at, items) = self.entries.get(chat_id)?;
        if now >= *expires_at {
            self.entries.remove(chat_id);
            return None;
        }
        items.get(&index).cloned()
    }

    /// 清掉某个私聊的记忆（例如它刚被 `/new` 打破上下文）。
    pub fn forget(&mut self, chat_id: &str) {
        self.entries.remove(chat_id);
    }
    /// 过期清理 + 容量上限（超限丢最旧的）。
    fn prune(&mut self, now: i64) {
        self.entries.retain(|_, (expires_at, _)| now < *expires_at);
        while self.entries.len() > MAX_CHATS {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (expires_at, _))| *expires_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与生产一致：列表正文由 `card::menu_text` 渲染，序号动作由会话顺序展开 ——
    /// 结果必须**逐位对齐**（`1 → 第一项`）。
    #[test]
    fn text_menu_maps_indices_to_actions_in_rendered_order() {
        let items = vec![
            crate::card::ListItem { title: "合同审查".into(), detail: String::new() },
            crate::card::ListItem { title: "汪均益案".into(), detail: String::new() },
            crate::card::ListItem { title: "发票整理".into(), detail: String::new() },
        ];
        let body = crate::card::menu_text(&items);
        let actions = vec![
            MenuAction::Switch("s-1".into()),
            MenuAction::Switch("s-2".into()),
            MenuAction::Switch("s-3".into()),
        ];
        let map = resolve_text_menu(&body, &actions);
        assert_eq!(map.get(&1), Some(&MenuAction::Switch("s-1".into())));
        assert_eq!(map.get(&2), Some(&MenuAction::Switch("s-2".into())));
        assert_eq!(map.get(&3), Some(&MenuAction::Switch("s-3".into())));
        assert_eq!(map.get(&4), None, "越界序号不映射");
    }

    /// 普通正文不能变成"点选菜单"：单行数字开头、或数字散在正文里都不算。
    #[test]
    fn ordinary_text_is_not_a_menu() {
        let actions = vec![MenuAction::Switch("s-1".into()), MenuAction::Switch("s-2".into())];
        assert!(resolve_text_menu("2024 年的计划如下", &actions).is_empty());
        assert!(
            resolve_text_menu("1. 只有一条，不算列表", &actions).is_empty(),
            "单条列表不算菜单（普通文本误伤）"
        );
        assert!(resolve_text_menu("随便一段话。\n没有数字行。", &actions).is_empty());
        // 数字后不是 . ) 、 的也不认
        assert!(resolve_text_menu("1 第一条\n2 第二条", &actions).is_empty());
        assert!(resolve_text_menu("1. 第一条", &[]).is_empty(), "没有动作就不映射");
    }

    #[test]
    fn memory_resolves_only_within_ttl_and_clears_on_miss() {
        let mut mem = MenuMemory::new();
        let mut items = HashMap::new();
        items.insert(1usize, MenuAction::Switch("s-1".into()));
        items.insert(2usize, MenuAction::Switch("s-2".into()));
        mem.remember("oc_1", 1_000, items);

        assert_eq!(
            mem.resolve("oc_1", 1_000, 2),
            Some(MenuAction::Switch("s-2".into()))
        );
        // 未记录的序号 → None（调用方据此走 AI）
        assert_eq!(mem.resolve("oc_1", 1_000, 9), None);
        // 别的私聊互不影响
        assert_eq!(mem.resolve("oc_2", 1_000, 1), None);

        // 过期后失效
        assert_eq!(mem.resolve("oc_1", 1_000 + MENU_TTL_SECS, 1), None);
        assert_eq!(
            mem.resolve("oc_1", 1_000 + MENU_TTL_SECS + 1, 1),
            None,
            "过期条目必须清掉"
        );
    }

    #[test]
    fn empty_remember_clears_previous_menu() {
        let mut mem = MenuMemory::new();
        let mut items = HashMap::new();
        items.insert(1usize, MenuAction::Switch("s".into()));
        items.insert(2usize, MenuAction::Switch("s2".into()));
        mem.remember("oc_1", 0, items);
        mem.remember("oc_1", 0, HashMap::new());
        assert_eq!(mem.resolve("oc_1", 0, 1), None);
    }

    #[test]
    fn forget_wins_over_remember() {
        let mut mem = MenuMemory::new();
        let mut items = HashMap::new();
        items.insert(1usize, MenuAction::Switch("s".into()));
        items.insert(2usize, MenuAction::Switch("s2".into()));
        mem.remember("oc_1", 0, items);
        mem.forget("oc_1");
        assert_eq!(mem.resolve("oc_1", 0, 1), None);
    }
}
