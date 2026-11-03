//! 答案依据校验（`answer_verify`）：把答案里带 `[N]` 的陈述拿回原文核对。
//!
//! 分两段跑，为的是「能用 LLM 判断，但不为每条陈述付一次调用」：
//!
//! 1. **机械预筛**（毫秒级、纯字符串）：句内数值归一化后若能在被引片段里找到，
//!    直接判 `supported`，不再送模型。历史数据里绝大多数日期/金额都走这条路。
//! 2. **LLM 批量裁定**（一次调用）：只把预筛挑出来的可疑条目 + 去重后的原文片段
//!    一起发给模型，由它判断 `supported / contradicted / absent / derived`。
//!
//! 判定的时机是**作答之后**（`ai-done` 已发），所以不占用用户等待的时间；
//! 结论通过 `answer_verify` 事件推给前端，并落 `ai_events`。
//!
//! 一条铁律：机械层「归一化后完全找不到」是确定性证据，即使模型说 `supported`
//! 也按矛盾记录 —— 两者冲突时以确定性证据为准。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::EvidenceItem;

/// 每条原文片段送进 LLM 的字符上限。
const MAX_EXCERPT_CHARS: usize = 700;
/// 整份核验请求的字符上限（超过就截断尾部片段，陈述清单优先保留）。
const MAX_TOTAL_PROMPT_CHARS: usize = 6_000;
/// 单轮送 LLM 裁定的条目上限（超出部分保持 `unchecked`）。
const MAX_PENDING: usize = 40;
/// LLM 裁定的超时（它是作答之后的旁路，不值得等太久）。
const LLM_TIMEOUT_SECS: u64 = 30;
/// 事件里最多带多少条明细（避免大表格把 payload 撑爆）。
const MAX_EVENT_ITEMS: usize = 60;

/// 单条核验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// 原文明确支持（含改写、换算、中文数字等价）。
    Supported,
    /// 原文有这项事实，但数值或表述不符。
    Contradicted,
    /// 原文里根本没有这项事实。
    Absent,
    /// 由原文事实推算得出（如年龄），推算依据本身在原文中。
    Derived,
    /// 无法核验：没有数值、引用材料不在本轮上下文、LLM 不可用或超上限。
    Unchecked,
}

/// 一条待核验陈述：答案里带 `[N]` 的一个句子/表格行。
#[derive(Debug, Clone)]
pub struct Claim {
    pub id: usize,
    pub sentence: String,
    pub cites: Vec<usize>,
    /// 句内数值 token 的**归一化**形式（全角/中文数字/日期分隔符已处理）。
    pub values: Vec<String>,
}

/// 核验结论（事件与日志用）。
#[derive(Debug, Clone, Serialize)]
pub struct VerifiedClaim {
    pub id: usize,
    pub sentence: String,
    pub cites: Vec<usize>,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correct: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `prepare` 的产物：已判定项 + 待裁定项 + 预拼好的请求体。
#[derive(Default)]
struct Plan {
    decided: Vec<VerifiedClaim>,
    pending: Vec<VerifiedClaim>,
    prompt: Option<String>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.decided.is_empty() && self.pending.is_empty()
    }

    fn into_all(self) -> Vec<VerifiedClaim> {
        let mut all = self.decided;
        all.extend(self.pending);
        all.sort_by_key(|c| c.id);
        all
    }
}

// ── 入口 ────────────────────────────────────────────────────────────────

/// 旁路核验：永远不阻塞作答流程，任何失败都只记日志。
pub async fn run(
    app: AppHandle,
    db: Pool<SqliteConnectionManager>,
    session_id: String,
    turn_number: usize,
    trace_id: String,
    answer: String,
    evidence: Vec<EvidenceItem>,
) {
    if answer.trim().is_empty() {
        return;
    }
    let started = Instant::now();
    let prepare_db = db.clone();
    let plan = match tokio::task::spawn_blocking(move || prepare(&prepare_db, &answer, &evidence)).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[AI] verify: prepare task failed: {e}");
            return;
        }
    };
    if plan.is_empty() {
        log::info!("[AI] verify: no citable claim, skipped");
        return;
    }

    let mut plan = plan;
    if let Some(prompt) = plan.prompt.take() {
        if crate::ai::llm_enabled() {
            let fut = tokio::task::spawn_blocking(move || crate::ai::chat(SYSTEM, &prompt));
            match tokio::time::timeout(Duration::from_secs(LLM_TIMEOUT_SECS), fut).await {
                Ok(Ok(Some(raw))) => match parse_verdicts(&raw) {
                    Some(parsed) => apply_verdicts(&mut plan.pending, &parsed),
                    None => log::warn!(
                        "[AI] verify: LLM 输出无法解析为裁定数组，{} 条保持 unchecked",
                        plan.pending.len()
                    ),
                },
                Ok(Ok(None)) => log::warn!("[AI] verify: LLM 不可用，待裁定项保持 unchecked"),
                Ok(Err(e)) => log::warn!("[AI] verify: LLM task failed: {e}"),
                Err(_) => log::warn!("[AI] verify: LLM 超时 {LLM_TIMEOUT_SECS}s"),
            }
        } else {
            log::info!("[AI] verify: LLM 未配置，待裁定项保持 unchecked");
        }
    }

    let all = plan.into_all();
    let counts = counts_of(&all);
    let took_ms = started.elapsed().as_millis() as u64;
    log::info!(
        "[AI] verify: checked={} supported={} contradicted={} absent={} derived={} unchecked={} took={}ms",
        all.len(),
        counts.supported,
        counts.contradicted,
        counts.absent,
        counts.derived,
        counts.unchecked,
        took_ms
    );

    let items: Vec<VerifiedClaim> = all
        .iter()
        .filter(|c| c.verdict != Verdict::Supported)
        .take(MAX_EVENT_ITEMS)
        .cloned()
        .collect();
    let payload = serde_json::json!({
        "session_id": session_id,
        "trace_id": trace_id,
        "checked": all.len(),
        "supported": counts.supported,
        "contradicted": counts.contradicted,
        "absent": counts.absent,
        "derived": counts.derived,
        "unchecked": counts.unchecked,
        "took_ms": took_ms,
        "items": items,
    });
    if let Err(e) = app.emit("answer_verify", &payload) {
        log::warn!("[AI] verify: emit failed: {e}");
    }
    // `db.get()` 在连接池耗尽时会阻塞最长 30s —— 不能占着 tokio worker 线程。
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(conn) = db.get() else {
            log::warn!("[AI] verify: DB unavailable; answer_verify event dropped");
            return;
        };
        if let Err(e) = crate::db::ai_events::record_event(
            &conn,
            &session_id,
            turn_number,
            VERIFY_EVENT_SEQ,
            "answer_verify",
            &payload,
        ) {
            log::warn!("[AI] verify: record_event failed: {e}");
        }
    })
    .await;
}

/// `answer_verify` 在 `ai_events` 里的序号：排在常规事件（1..n）之后，
/// 因为它可能晚于 `turn_complete` 若干秒才落库。
const VERIFY_EVENT_SEQ: u32 = 90;

#[derive(Default)]
struct Counts {
    supported: usize,
    contradicted: usize,
    absent: usize,
    derived: usize,
    unchecked: usize,
}

fn counts_of(all: &[VerifiedClaim]) -> Counts {
    let mut c = Counts::default();
    for item in all {
        match item.verdict {
            Verdict::Supported => c.supported += 1,
            Verdict::Contradicted => c.contradicted += 1,
            Verdict::Absent => c.absent += 1,
            Verdict::Derived => c.derived += 1,
            Verdict::Unchecked => c.unchecked += 1,
        }
    }
    c
}

// ── 准备阶段（同步、可在 spawn_blocking 里跑）──────────────────────────

fn prepare(
    db: &Pool<SqliteConnectionManager>,
    answer: &str,
    evidence: &[EvidenceItem],
) -> Plan {
    let claims = extract_claims(answer);
    if claims.is_empty() {
        return Plan::default();
    }
    let spans = resolve_spans(db, evidence);
    let (decided, pending) = prefilter(&claims, &spans);
    let prompt = build_llm_input(&pending, &spans);
    let pending = pending
        .into_iter()
        .map(|c| VerifiedClaim {
            id: c.id,
            sentence: c.sentence,
            cites: c.cites,
            verdict: Verdict::Unchecked,
            correct: None,
            note: Some("未能自动核验".into()),
        })
        .collect();
    Plan {
        decided,
        pending,
        prompt,
    }
}

/// 把本轮真正进入 prompt 的原文片段取回来（按 `material_no` 归并）。
///
/// 只用 `injected_spans`（送进 prompt 的字符区间），而不是整篇文档 —— 这才是
/// 「模型当时看到的内容」，也是模型该被追责的范围。
fn resolve_spans(
    db: &Pool<SqliteConnectionManager>,
    evidence: &[EvidenceItem],
) -> HashMap<usize, String> {
    let mut out: HashMap<usize, String> = HashMap::new();
    let usable: Vec<&EvidenceItem> = evidence
        .iter()
        .filter(|e| e.material_no > 0 && !e.injected_spans.is_empty())
        .collect();
    if usable.is_empty() {
        return out;
    }
    let Ok(conn) = db.get() else {
        log::warn!("[AI] verify: DB unavailable; 无法取回原文片段");
        return out;
    };
    let ids: Vec<String> = usable.iter().map(|e| e.file_id.clone()).collect();
    let recs = crate::db::tracker::get_files_by_ids(&conn, &ids).unwrap_or_default();
    let mut id_to_md5: HashMap<&str, &str> = HashMap::new();
    let mut md5s: Vec<String> = Vec::new();
    for (id, rec) in ids.iter().zip(recs.iter()) {
        if let Some(rec) = rec {
            if let Some(md5) = &rec.md5 {
                id_to_md5.insert(id.as_str(), md5.as_str());
                md5s.push(md5.clone());
            }
        }
    }
    if md5s.is_empty() {
        return out;
    }
    md5s.sort();
    md5s.dedup();
    let contents = crate::db::tracker::get_contents(&conn, &md5s).unwrap_or_default();
    let mut md5_to_text: HashMap<&str, &str> = HashMap::new();
    for (m, c) in md5s.iter().zip(contents.iter()) {
        if let Some(text) = c {
            md5_to_text.insert(m.as_str(), text.as_str());
        }
    }
    for item in usable {
        let Some(md5) = id_to_md5.get(item.file_id.as_str()) else {
            continue;
        };
        let Some(text) = md5_to_text.get(md5) else {
            continue;
        };
        let mut joined = String::new();
        for (start, end) in &item.injected_spans {
            let seg = char_slice(text, *start, *end);
            if seg.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(seg);
        }
        if !joined.trim().is_empty() {
            out.insert(item.material_no, joined);
        }
    }
    out
}

/// 按**字符**下标切片（`injected_spans` 是字符区间，不是字节区间）。
fn char_slice(s: &str, start: usize, end: usize) -> &str {
    if start >= end {
        return "";
    }
    let mut b_start: Option<usize> = None;
    let mut b_end: Option<usize> = None;
    for (n, (i, _)) in s.char_indices().enumerate() {
        if n == start {
            b_start = Some(i);
        }
        if n == end {
            b_end = Some(i);
            break;
        }
    }
    let a = b_start.unwrap_or(s.len());
    let b = b_end.unwrap_or(s.len());
    if a >= b || a >= s.len() {
        ""
    } else {
        &s[a..b]
    }
}

// ── 陈述抽取 ────────────────────────────────────────────────────────────

/// 从答案里抽出所有**带引用**的句子/表格行。
pub fn extract_claims(answer: &str) -> Vec<Claim> {
    let cite_re = regex::Regex::new(r"\[(\d+)\]").unwrap();
    let chunk_re = regex::Regex::new(r"[^。！？!?\n]+").unwrap();
    let mut out: Vec<Claim> = Vec::new();
    for m in chunk_re.find_iter(answer) {
        let sentence = m.as_str().trim();
        // 纯 markdown 分隔行（|---|）没有信息量
        if sentence.is_empty() || sentence.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }
        let mut cites: Vec<usize> = cite_re
            .captures_iter(sentence)
            .filter_map(|c| c.get(1))
            .filter_map(|g| g.as_str().parse::<usize>().ok())
            .collect();
        if cites.is_empty() {
            continue;
        }
        cites.sort_unstable();
        cites.dedup();
        // 引用编号本身是数字，抽取数值前必须先剥掉，否则 `[35]` 会被当成年份。
        let body = cite_re.replace_all(sentence, " ");
        let values = extract_values(&body);
        out.push(Claim {
            id: out.len() + 1,
            sentence: sentence.to_string(),
            cites,
            values,
        });
    }
    out
}

fn value_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"\d{4}\s*[-年/.]\s*\d{1,2}(?:\s*[-月/.]\s*\d{1,2})?|\d+(?:\.\d+)?\s*(?:万|亿)?\s*(?:元|岁|年|个月|%|％|人|户|份|页|次|天)|\d{2,}|[一二三四五六七八九十百千万〇零两]{2,}",
        )
        .unwrap()
    })
}

/// 句内数值 token（归一化后去重）。
fn extract_values(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in value_re().find_iter(body) {
        let n = normalize_for_match(m.as_str());
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

// ── 归一化 ──────────────────────────────────────────────────────────────

/// 归一化到可比对的形式：全角→半角、中文数字→阿拉伯数字、去掉日期分隔符与空白、
/// 去掉数字串的前导零。
///
/// 目标只有一个：让 `1928年5月19日`、`1928-05-19`、`１９２８年５月１９日`、
/// `一九二八年五月十九日` 都能互相比中。**不做语义判断**。
pub fn normalize_for_match(s: &str) -> String {
    let mut wide = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = match ch {
            '\u{FF10}'..='\u{FF19}' => char::from(b'0' + (ch as u32 - 0xFF10) as u8),
            '\u{FF21}'..='\u{FF3A}' => char::from(b'A' + (ch as u32 - 0xFF21) as u8),
            '\u{FF41}'..='\u{FF5A}' => char::from(b'a' + (ch as u32 - 0xFF41) as u8),
            '\u{3002}' => '.',
            '\u{FF0F}' => '/',
            '\u{FF0D}' => '-',
            _ => ch,
        };
        wide.push(c);
    }
    let cn = cn_numerals_to_digits(&wide);
    // ⚠️ 顺序不能反：先去前导零、再去分隔符。
    // 反了的话 `1928-05-19` 会先被拼成 `19280519`（一整段），前导零无从去除，
    // 于是和 `1928年5月19日` 的 `1928519` 对不上 —— 2026-11-03 实测踩到。
    let trimmed = strip_leading_zeros(&cn);
    trimmed
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '年' | '月' | '日' | '-' | '/' | '.' | ' ' | '\t' | '，' | ',' | '\u{FF0C}' | '\u{3001}'
            )
        })
        .collect()
}

fn strip_leading_zeros(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            flush_digit_run(&mut run, &mut out);
            out.push(c);
        }
    }
    flush_digit_run(&mut run, &mut out);
    out
}

fn flush_digit_run(run: &mut String, out: &mut String) {
    if run.is_empty() {
        return;
    }
    let trimmed = run.trim_start_matches('0');
    if trimmed.is_empty() {
        out.push('0');
    } else {
        out.push_str(trimmed);
    }
    run.clear();
}

fn is_cn_numeral(c: char) -> bool {
    cn_digit(c).is_some() || matches!(c, '十' | '百' | '千' | '万' | '亿')
}

/// 中文数字 → 阿拉伯数字。
///
/// 单个字符（`一`、`五`）**不转换**，除非紧跟 `年/月/日/号/岁` —— 否则
/// `统一`、`一起`、`十分` 这类词会被写坏，反而制造误匹配。
fn cn_numerals_to_digits(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_cn_numeral(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_cn_numeral(chars[i]) {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let followed_by_unit = chars
            .get(i)
            .map(|c| matches!(c, '年' | '月' | '日' | '号' | '岁'))
            .unwrap_or(false);
        let single = run.chars().count() == 1;
        if single && !followed_by_unit {
            out.push_str(&run);
            continue;
        }
        match parse_cn_number(&run) {
            Some(n) => out.push_str(&n.to_string()),
            None => out.push_str(&run),
        }
    }
    out
}

fn parse_cn_number(s: &str) -> Option<u64> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return None;
    }
    // 逐位写法（一九二八 / 二〇二六）
    if chars.iter().all(|c| cn_digit(*c).is_some()) {
        let mut v: u64 = 0;
        for c in &chars {
            v = v.checked_mul(10)?.checked_add(cn_digit(*c)?)?;
        }
        return Some(v);
    }
    let mut total: u64 = 0;
    let mut section: u64 = 0;
    let mut number: u64 = 0;
    for c in chars {
        if let Some(d) = cn_digit(c) {
            number = d;
            continue;
        }
        match c {
            '十' => {
                if number == 0 {
                    number = 1;
                }
                section = section.checked_add(number.checked_mul(10)?)?;
                number = 0;
            }
            '百' => {
                if number == 0 {
                    number = 1;
                }
                section = section.checked_add(number.checked_mul(100)?)?;
                number = 0;
            }
            '千' => {
                if number == 0 {
                    number = 1;
                }
                section = section.checked_add(number.checked_mul(1000)?)?;
                number = 0;
            }
            '万' => {
                let cur = section.checked_add(number)?;
                total = total.checked_add(cur.checked_mul(10_000)?)?;
                section = 0;
                number = 0;
            }
            '亿' => {
                let cur = total.checked_add(section)?.checked_add(number)?;
                total = cur.checked_mul(100_000_000)?;
                section = 0;
                number = 0;
            }
            _ => return None,
        }
    }
    Some(total.checked_add(section)?.checked_add(number)?)
}

fn cn_digit(c: char) -> Option<u64> {
    Some(match c {
        '零' | '〇' => 0,
        '一' => 1,
        '二' | '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        _ => return None,
    })
}

// ── 预筛 ────────────────────────────────────────────────────────────────

/// 机械预筛：能确定的直接下结论，剩下的交给 LLM。
///
/// `spans` 的 key 是 `material_no`，值是**原始**片段（内部会归一化）。
pub fn prefilter(
    claims: &[Claim],
    spans: &HashMap<usize, String>,
) -> (Vec<VerifiedClaim>, Vec<Claim>) {
    let norm: HashMap<usize, String> = spans
        .iter()
        .map(|(k, v)| (*k, normalize_for_match(v)))
        .collect();
    let mut decided: Vec<VerifiedClaim> = Vec::new();
    let mut pending: Vec<Claim> = Vec::new();
    for claim in claims {
        let available: Vec<&String> = claim
            .cites
            .iter()
            .filter_map(|n| norm.get(n))
            .collect();
        if available.is_empty() {
            decided.push(VerifiedClaim {
                id: claim.id,
                sentence: claim.sentence.clone(),
                cites: claim.cites.clone(),
                verdict: Verdict::Unchecked,
                correct: None,
                note: Some("引用材料不在本轮上下文".into()),
            });
            continue;
        }
        if claim.values.is_empty() {
            decided.push(VerifiedClaim {
                id: claim.id,
                sentence: claim.sentence.clone(),
                cites: claim.cites.clone(),
                verdict: Verdict::Unchecked,
                correct: None,
                note: Some("陈述中没有可核验的数值".into()),
            });
            continue;
        }
        let hit = claim
            .values
            .iter()
            .any(|v| available.iter().any(|s| s.contains(v.as_str())));
        if hit {
            decided.push(VerifiedClaim {
                id: claim.id,
                sentence: claim.sentence.clone(),
                cites: claim.cites.clone(),
                verdict: Verdict::Supported,
                correct: None,
                note: None,
            });
        } else {
            pending.push(claim.clone());
        }
    }
    (decided, pending)
}

const SYSTEM: &str = "你是事实核验助手。用户会给你若干条**待核验陈述**和它们引用的**原文片段**。\
你的任务：只依据给定原文片段，判断每条陈述是否被支持。\
结论只能是下面四种之一：\
- supported：原文明确支持（允许改写、单位换算、中文数字与阿拉伯数字等价）；\
- contradicted：原文里有这项事实，但数值或表述不符（此时必须给出 correct 字段，填原文的值）；\
- absent：原文片段里根本没有这项事实；\
- derived：陈述里的数字是由原文事实推算得出（例如由出生日期算出年龄），且推算依据在原文中。\
只输出 JSON 数组，不要解释、不要加代码块标记。格式：\
[{\"id\":1,\"verdict\":\"supported\"},{\"id\":2,\"verdict\":\"contradicted\",\"correct\":\"1923\"}]";

/// 拼一次批量裁定的请求体。没有待裁定项时返回 `None`。
pub fn build_llm_input(pending: &[Claim], spans: &HashMap<usize, String>) -> Option<String> {
    if pending.is_empty() || spans.is_empty() {
        return None;
    }
    let capped = &pending[..pending.len().min(MAX_PENDING)];
    let mut cites: Vec<usize> = capped.iter().flat_map(|c| c.cites.iter().copied()).collect();
    cites.sort_unstable();
    cites.dedup();

    let mut s = String::from("待核验陈述：\n");
    for c in capped {
        let nums: Vec<String> = c.cites.iter().map(|n| n.to_string()).collect();
        s.push_str(&format!(
            "{}. {}（引用材料：{}）\n",
            c.id,
            c.sentence,
            nums.join("、")
        ));
    }
    s.push_str("\n原文片段：\n");
    for n in cites {
        if let Some(text) = spans.get(&n) {
            s.push_str(&format!("【{n}】{}\n", super::truncate_text(text, MAX_EXCERPT_CHARS)));
        }
    }
    if s.chars().count() > MAX_TOTAL_PROMPT_CHARS {
        s = s.chars().take(MAX_TOTAL_PROMPT_CHARS).collect();
    }
    Some(s)
}

/// 解析裁定结果。取第一个 `[` 到最后一个 `]`，容忍模型加代码块或解释文字。
pub fn parse_verdicts(raw: &str) -> Option<Vec<(usize, Verdict, Option<String>)>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(&raw[start..=end]).ok()?;
    let mut out: Vec<(usize, Verdict, Option<String>)> = Vec::new();
    for v in arr {
        let Some(id) = v.get("id").and_then(|x| x.as_u64()) else {
            continue;
        };
        let Some(raw_verdict) = v.get("verdict").and_then(|x| x.as_str()) else {
            continue;
        };
        let verdict = match raw_verdict.trim().to_lowercase().as_str() {
            "supported" | "ok" | "true" | "supported." => Verdict::Supported,
            "contradicted" | "wrong" | "conflict" => Verdict::Contradicted,
            "absent" | "missing" | "none" => Verdict::Absent,
            "derived" | "inferred" => Verdict::Derived,
            _ => Verdict::Unchecked,
        };
        let correct = v
            .get("correct")
            .and_then(|x| x.as_str())
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty());
        out.push((id as usize, verdict, correct));
    }
    if out.is_empty() { None } else { Some(out) }
}

fn apply_verdicts(pending: &mut [VerifiedClaim], parsed: &[(usize, Verdict, Option<String>)]) {
    let by_id: HashMap<usize, (Verdict, Option<String>)> = parsed
        .iter()
        .map(|(id, v, c)| (*id, (*v, c.clone())))
        .collect();
    for item in pending.iter_mut() {
        if let Some((verdict, correct)) = by_id.get(&item.id) {
            item.verdict = *verdict;
            item.correct = correct.clone();
            item.note = if *verdict == Verdict::Unchecked {
                Some("模型未能判定".into())
            } else {
                None
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(pairs: &[(usize, &str)]) -> HashMap<usize, String> {
        pairs.iter().map(|(n, t)| (*n, (*t).to_string())).collect()
    }

    #[test]
    fn normalizes_dates_across_notations() {
        let a = normalize_for_match("1928年5月19日");
        let b = normalize_for_match("1928-05-19");
        let c = normalize_for_match("１９２８年５月１９日");
        let d = normalize_for_match("一九二八年五月十九日");
        assert_eq!(a, "1928519");
        assert_eq!(a, b, "连字符写法必须等价");
        assert_eq!(a, c, "全角写法必须等价");
        assert_eq!(a, d, "中文数字必须等价");
    }

    #[test]
    fn does_not_mangle_ordinary_words_with_one() {
        // 「统一」「一起」里的「一」不是数字，不能被改写成 1
        assert_eq!(normalize_for_match("统一管理"), "统一管理");
        assert_eq!(normalize_for_match("一起处理，十分清楚"), "一起处理十分清楚");
    }

    #[test]
    fn converts_single_numeral_before_unit_word() {
        assert_eq!(normalize_for_match("五岁"), "5岁");
        // 「年」是日期分隔符，归一化时会被去掉
        assert_eq!(normalize_for_match("十五年"), "15");
    }

    #[test]
    fn strips_citation_numbers_from_values() {
        let claims = extract_claims("陈贤文（1928年5月19日出生）：98周岁 [9][35]。");
        assert_eq!(claims.len(), 1);
        let c = &claims[0];
        assert_eq!(c.cites, vec![9, 35]);
        assert!(c.values.contains(&"1928519".to_string()), "{:?}", c.values);
        assert!(c.values.contains(&"98".to_string()), "{:?}", c.values);
        // 引用编号不能被当成年份
        assert!(!c.values.contains(&"35".to_string()), "{:?}", c.values);
    }

    #[test]
    fn ignores_sentences_without_citation() {
        let claims = extract_claims("这是一段没有引用的说明。\n第二句也没有。");
        assert!(claims.is_empty());
    }

    #[test]
    fn splits_table_rows_into_claims() {
        let answer = "| 陈贤文 | 98周岁 [9] |\n| 丁玉英 | 103周岁 [18] |\n|---|---|";
        let claims = extract_claims(answer);
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].cites, vec![9]);
        assert_eq!(claims[1].cites, vec![18]);
    }

    #[test]
    fn prefilter_accepts_value_found_in_span() {
        let claims = extract_claims("陈贤文（1928年5月19日出生）[9]。");
        let sp = spans(&[(9, "陈贤文，男，1928年5月19日出生，住上海市……")]);
        let (decided, pending) = prefilter(&claims, &sp);
        assert!(pending.is_empty());
        assert_eq!(decided.len(), 1);
        assert_eq!(decided[0].verdict, Verdict::Supported);
    }

    #[test]
    fn prefilter_escalates_when_value_absent() {
        let claims = extract_claims("丁玉英（1938年出生）[18]。");
        let sp = spans(&[(18, "丁玉英，女，一九二三年出生。")]);
        let (decided, pending) = prefilter(&claims, &sp);
        assert!(decided.is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn prefilter_marks_missing_material_as_unchecked() {
        let claims = extract_claims("陈贤文 1928年出生 [42]。");
        let (decided, pending) = prefilter(&claims, &spans(&[]));
        assert!(pending.is_empty());
        assert_eq!(decided.len(), 1);
        assert_eq!(decided[0].verdict, Verdict::Unchecked);
    }

    #[test]
    fn prefilter_marks_prose_only_claim_as_unchecked() {
        let claims = extract_claims("法院认为双方感情确已破裂 [3]。");
        let sp = spans(&[(3, "本院认为，双方感情确已破裂。")]);
        let (decided, pending) = prefilter(&claims, &sp);
        assert!(pending.is_empty());
        assert_eq!(decided[0].verdict, Verdict::Unchecked);
    }

    #[test]
    fn builds_prompt_with_deduped_materials() {
        let claims = extract_claims("甲 1990年出生 [2]。");
        let pending = vec![claims[0].clone(), claims[0].clone()];
        let sp = spans(&[(2, "甲，一九九〇年出生。")]);
        let p = build_llm_input(&pending, &sp).expect("有内容");
        assert_eq!(p.matches("【2】").count(), 1, "同一材料只应出现一次");
    }

    #[test]
    fn parses_plain_and_fenced_json() {
        let plain = r#"[{"id":1,"verdict":"supported"},{"id":2,"verdict":"contradicted","correct":"1923"}]"#;
        let parsed = parse_verdicts(plain).expect("可解析");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].1, Verdict::Contradicted);
        assert_eq!(parsed[1].2.as_deref(), Some("1923"));

        let fenced = "```json\n[{\"id\":3,\"verdict\":\"absent\"}]\n```";
        let parsed = parse_verdicts(fenced).expect("容忍代码块");
        assert_eq!(parsed[0].1, Verdict::Absent);

        assert!(parse_verdicts("模型没有按格式回").is_none());
    }

    #[test]
    fn char_slice_uses_char_not_byte_offsets() {
        let s = "陈贤文，男，1928年出生";
        assert_eq!(char_slice(s, 0, 3), "陈贤文");
        assert_eq!(char_slice(s, 3, 5), "，男");
        assert_eq!(char_slice(s, 5, 9), "，192");
        assert_eq!(char_slice(s, 100, 200), "");
    }
}
