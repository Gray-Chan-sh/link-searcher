//! 飞书私聊里的**斜杠命令解析**（`docs/LARK_BOT.md` §11）。
//!
//! 背景（2026-10-16 查证）：飞书官方 FAQ 明确"机器人**不支持** `/help` 这类斜杠命令"
//! —— 客户端不会在输入 `/` 时弹出任何列表。所以命令只能是**纯文本消息**，
//! 由本模块解析；另配「机器人自定义菜单」（开发者后台配置，`application.bot.menu_v6`
//! 事件）与「回复数字点选」两条辅助路径，见 `main.rs`。
//!
//! 设计约束：
//! - **未命中一律返回 `Ai`**：任何不认识的输入都当作正常提问走 RAG —— 命令的引入
//!   绝不能吃掉"用户真的想问一个以 / 开头的东西"这种边角（宁可它走问答）。
//! - 解析是**纯函数**：路由错了会变成"打 /new 结果问了个问题"，而这条链路没法
//!   靠肉眼在飞书里穷举，所以它是本模块最该被单测盯住的一层。
//!
//! 命令集（全英文，2026-10-16 与用户确认）：

use serde_json::json;

/// 命令帮助正文（`/help` 的回复，也是菜单项「帮助」的内容）。
pub const HELP_TEXT: &str = "**可用命令**\n\
- `/help` — 显示本帮助\n\
- `/current` — 查看当前会话（名字 / 消息数）\n\
- `/new` — 新建会话并切换过去\n\
- `/sessions` — 查看会话列表\n\
- `/switch 3` — 切换到第 3 个会话（也可用标题前缀，如 `/switch 合同`）\n\
- `/rename 新名字` — 给当前会话改个名字（侧栏/列表显示的就是它）\n\
- `/stop` — 停止正在生成的回答\n\
- `/delete 3` — 删除第 3 个会话（会先让你确认）\n\
- `/find 关键词` — 搜索本地库（`/find 关键词 2` 翻到第 2 页）\n\
\n\
**提示**：直接发数字（如 `3`）可选择上一条列表里的对应项；直接提问即进入 AI 问答。";

/// 搜索结果每页条数（`/find` 用；与 `main::FIND_PAGE_SIZE` 一致）。
pub const MAX_FIND_PAGE: usize = 99;

/// 一条解析后的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    /// 查看当前会话（名字/消息数）。
    Current,
    New,
    List,
    Switch(String),
    /// 给**当前活动会话**改标题（新名字含空格时整体保留）。
    Rename(String),
    /// 停止当前正在生成的回答。
    Stop,
    Delete(String),
    Find { query: String, page: usize },
    /// 不是命令（空正文 / 未命中前缀）或未识别的命令 → 走 AI 问答。
    ///
    /// 🙋 未识别的命令（如 `/foo`）**按普通提问处理**是刻意的：命令表之外的东西
    /// 没有"正确答案"可言，而 RAG 至少可能给出用户想找的内容。
    Ai,
}

/// 解析一条私聊正文：命令 → 对应动作；否则 → [`Command::Ai`]。
pub fn parse_command(text: &str) -> Command {
    let t = text.trim();
    // 单独的 `/`：用户想"看看有什么命令" → 帮助（最常见的手势，别回"没听懂"）
    if t == "/" {
        return Command::Help;
    }
    let Some(rest) = t.strip_prefix('/') else {
        return Command::Ai;
    };
    let mut parts = rest.split_whitespace();
    let head = parts.next().unwrap_or("").to_ascii_lowercase();
    let args: Vec<&str> = parts.collect();
    match head.as_str() {
        "help" => Command::Help,
        "current" | "session" => Command::Current,
        "new" => Command::New,
        "sessions" | "list" => Command::List,
        "stop" | "cancel" => Command::Stop,
        "switch" => match args.first() {
            Some(a) if !a.is_empty() => Command::Switch((*a).to_string()),
            // 缺参数：当作列出会话（用户多半想看"能切到哪几个"）
            _ => Command::List,
        },
        "rename" => {
            // 名字可含空格 → 把剩余整段当标题（而不是只取第一个词）
            let name: Vec<&str> = args.clone();
            if name.is_empty() {
                Command::Rename(String::new()) // 缺参数 → 上层回用法提示
            } else {
                Command::Rename(name.join(" "))
            }
        }
        "delete" => match args.first() {
            Some(a) if !a.is_empty() => Command::Delete((*a).to_string()),
            _ => Command::List,
        },
        "find" | "search" => {
            if args.first().is_none_or(|s| s.is_empty()) {
                return Command::Find {
                    query: String::new(),
                    page: 1,
                };
            }
            let (query, page) = split_trailing_page(&args);
            Command::Find { query, page }
        }
        _ => Command::Ai,
    }
}

/// `/find 关键词 [页码]` 的参数拆分：末位参数是**纯正整数且不超过 [`MAX_FIND_PAGE`]**
/// 时才当作页码。
///
/// 为什么要这个上限：`判决书 2024` 与"翻到第 2024 页"字面上无法区分，而搜索场景下
/// 前者常见得多（年份、编号都是数字）。页码上限取 99（每页 5 条 ≈ 495 条以内；
/// 真要看更深的页，用更具体的关键词缩小命中是更实际的做法）。
fn split_trailing_page(args: &[&str]) -> (String, usize) {
    let mut words: Vec<&str> = args.to_vec();
    let mut page = 1usize;
    if words.len() > 1
        && let Some(last) = words.last()
        && let Ok(n) = last.parse::<usize>()
        && (1..=MAX_FIND_PAGE).contains(&n)
    {
        page = n;
        words.pop();
    }
    (words.join(" "), page)
}

/// 从一条消息里解析"纯数字点选"：`"2"` / `"２"`（全角）/ `"2 3"`。
///
/// 全角数字是中文输入法下的常见误打，顺手归一化（只影响这一处，成本极低）。
pub fn parse_selection(text: &str) -> Option<usize> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let mut n = 0usize;
    for c in t.chars() {
        let d = match c {
            '0'..='9' => c as u8 - b'0',
            '０'..='９' => (c as u32 - '０' as u32) as u8,
            _ => return None,
        };
        n = n.checked_mul(10)?.checked_add(d as usize)?;
    }
    (n >= 1).then_some(n)
}

/// 渲染 `/help` 回复的卡片（正文同 [`HELP_TEXT`]）。
pub fn help_card() -> serde_json::Value {
    let mut card = crate::card::render_card("命令帮助", HELP_TEXT, None, None);
    // 帮助卡上挂一个「新建会话」按钮：零配置下最常用的动作，一步可达
    if let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut()) {
        elements.push(json!({
            "tag": "action",
            "actions": [{
                "tag": "button",
                "text": { "tag": "plain_text", "content": "新建会话" },
                "type": "primary",
                "value": crate::card::action_payload("new_session", ""),
            }],
        }));
    }
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slash_and_help_both_show_the_command_list() {
        assert_eq!(parse_command("/"), Command::Help);
        assert_eq!(parse_command("/help"), Command::Help);
        assert_eq!(parse_command("  /HELP  "), Command::Help, "命令大小写不敏感、允许空白");
    }

    #[test]
    fn session_commands_parse_with_and_without_args() {
        assert_eq!(parse_command("/new"), Command::New);
        assert_eq!(parse_command("/sessions"), Command::List);
        assert_eq!(parse_command("/list"), Command::List, "list 是 sessions 的别名");
        assert_eq!(parse_command("/current"), Command::Current);
        assert_eq!(parse_command("/session"), Command::Current, "单数别名校验");
        assert_eq!(parse_command("/stop"), Command::Stop);
        assert_eq!(parse_command("/cancel"), Command::Stop, "cancel 是 stop 的别名");
        assert_eq!(parse_command("/switch 3"), Command::Switch("3".into()));
        assert_eq!(parse_command("/switch 合同"), Command::Switch("合同".into()));
        // 缺参数的 switch/delete → 列出来让用户看着选，而不是报错
        assert_eq!(parse_command("/switch"), Command::List);
        assert_eq!(parse_command("/delete"), Command::List);
    }

    #[test]
    fn rename_keeps_full_title_with_spaces() {
        assert_eq!(parse_command("/rename 合同审查"), Command::Rename("合同审查".into()));
        assert_eq!(
            parse_command("/rename 汪均益 案卷 整理"),
            Command::Rename("汪均益 案卷 整理".into()),
            "标题里的空格要保留（整段都是名字）"
        );
        assert_eq!(parse_command("/rename"), Command::Rename(String::new()), "缺参数 → 空（上层给用法提示）");
        assert_eq!(parse_command("/rename   "), Command::Rename(String::new()));
    }

    #[test]
    fn find_parses_query_and_optional_page() {
        assert_eq!(
            parse_command("/find 汪均益"),
            Command::Find { query: "汪均益".into(), page: 1 }
        );
        assert_eq!(
            parse_command("/find 汪均益 2"),
            Command::Find { query: "汪均益".into(), page: 2 }
        );
        assert_eq!(
            parse_command("/find 2024 年度"),
            Command::Find { query: "2024 年度".into(), page: 1 },
            "非末位的数字是关键词的一部分"
        );
        assert_eq!(
            parse_command("/find 判决书 2024"),
            Command::Find { query: "判决书 2024".into(), page: 1 },
            "超上限的数字（2024）只能是关键词，不是页码"
        );
        assert_eq!(parse_command("/find"), Command::Find { query: String::new(), page: 1 });
        assert_eq!(parse_command("/search 合同"), Command::Find { query: "合同".into(), page: 1 });
    }

    /// **核心安全边界**：一切非命令输入都必须走 AI —— 命令解析绝不能吞掉正常提问。
    #[test]
    fn non_commands_and_unknown_slashes_fall_through_to_ai() {
        for input in [
            "汪均益案的最终结果是什么",
            "",
            "   ",
            "/foo",
            "/newx",
            "/ 空格后不是命令",
            "1+1 等于几",
            "价格是 a/b 吗",
        ] {
            assert_eq!(parse_command(input), Command::Ai, "必须走 AI：{input:?}");
        }
    }

    #[test]
    fn numeric_selection_accepts_plain_and_fullwidth_digits() {
        assert_eq!(parse_selection("3"), Some(3));
        assert_eq!(parse_selection(" 12 "), Some(12));
        assert_eq!(parse_selection("２"), Some(2), "全角数字归一化");
        assert_eq!(parse_selection("０"), None, "0 不是有效序号");
        assert_eq!(parse_selection(""), None);
        assert_eq!(parse_selection("3 3"), None, "只接受单个数字");
        assert_eq!(parse_selection("3个"), None);
        assert_eq!(parse_selection("-1"), None);
    }
}
