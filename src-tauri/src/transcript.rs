//! 音频转写稿的**展示/导出**渲染。
//!
//! 设计要点（见 CHANGELOG「音频时间轴」条）：
//!
//! * 正文 `content_index.text_content` 永远是**纯转写文本** —— tantivy、`doc_chunks`、
//!   向量、RAG、搜索结果片段全部只用它，因此加标签**不会**影响任何检索行为。
//! * 时间轴是**旁路数据**（`content_index.audio_timeline`，JSON），只在这里被读出来
//!   拼成"带标签"文本，用于预览面板与导出。
//! * 每段自带 `text`（而不是只存字符偏移）：`sanitize_text()` 的 NFKC / 挤压 CJK 空格
//!   会改变字符数量，偏移量一旦跨过那一步就会错位；存文本自包含、无耦合。
//!   代价是转写稿在库里多存一份（1 小时音频约 25KB），可忽略。

use serde::{Deserialize, Serialize};

/// 导出格式：纯文本（与 `text_content` 逐字节相同）。
pub const FORMAT_RAW: &str = "raw";
/// 导出格式：带 `[时间 · 说话人]` 标签的可读稿。
pub const FORMAT_TAGGED: &str = "tagged";

/// 一条转写片段：时间范围（毫秒，相对音频起点）+ 说话人 + 文本。
///
/// 时间精度是**段级**（VAD / diarization 边界，通常 500ms 量级），不是逐字对齐
/// —— FunASR-Nano 走 LLM 解码，sherpa-onnx 对它不提供 token 级时间戳。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    /// 说话人编号（0 基）。未做说话人分离时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<i32>,
    pub text: String,
}

/// Serialize a timeline for `content_index.audio_timeline`.
///
/// Returns `None` for an empty timeline so the column stays NULL — that is the
/// signal the UI uses to disable the "tagged" export ("该文件没有时间轴").
pub fn to_json(segments: &[AudioSegment]) -> Option<String> {
    let usable: Vec<&AudioSegment> = segments.iter().filter(|s| !s.text.trim().is_empty()).collect();
    if usable.is_empty() {
        return None;
    }
    serde_json::to_string(segments).ok()
}

/// Parse `content_index.audio_timeline`. Corrupt/legacy values yield an empty
/// timeline rather than an error: the tagged export then falls back to plain
/// text instead of failing the whole request.
pub fn from_json(raw: Option<&str>) -> Vec<AudioSegment> {
    match raw.map(str::trim) {
        Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `HH:MM:SS`（≥100 小时也不会截断，`{:02}` 只是最小宽度）。
///
/// 负数（时钟漂移/异常输入）一律按 0 处理，避免导出出现 `-1:59:59`。
pub fn format_timestamp(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}

/// 段头：`[00:12:34 · 说话人1]`；没有说话人时退化为 `[00:12:34]`。
fn segment_header(seg: &AudioSegment) -> String {
    match seg.speaker {
        Some(sp) => format!("[{} · 说话人{}]", format_timestamp(seg.start_ms), sp),
        None => format!("[{}]", format_timestamp(seg.start_ms)),
    }
}

/// Render the readable ("tagged") transcript, one segment per line.
///
/// Falls back to `text` verbatim when the timeline is empty — callers can then
/// use this function unconditionally without a "has timeline" branch.
pub fn render_tagged(text: &str, segments: &[AudioSegment]) -> String {
    let mut out = String::with_capacity(text.len() + segments.len() * 24);
    for seg in segments {
        let body = seg.text.trim();
        if body.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&segment_header(seg));
        out.push(' ');
        out.push_str(body);
    }
    if out.is_empty() {
        return text.to_string();
    }
    out
}

/// 库里有没有可用时间轴（决定 UI 是否把「带标签」置灰）。
pub fn has_timeline(segments: &[AudioSegment]) -> bool {
    segments.iter().any(|s| !s.text.trim().is_empty())
}

/// 截断 LLM-ASR 的**复读退化**（"都都都都…"、"your thoughts on your thoughts…"）。
///
/// FunASR-Nano 是 0.6B 的 LLM 解码器，遇到噪声/短片段时会陷入重复循环，一段
/// 7 秒的语音能吐出 2000 字（2026-10-23 实测）。
///
/// 判定用**主导 n-gram**而不是严格周期：真实复读会漂移（实测
/// `suorosusuorosusuoro…`，多一个 `s` 就破掉严格周期性）。规则：
///
/// * 取 16/12/8/6/4/3 字窗口中出现次数最多的那个；
/// * 它出现 ≥ 6 次、且覆盖全文 ≥ 50% → 判为复读；
/// * 在它**首次出现之后保留一个最小周期** + 省略号（"都都都"→"都…"，
///   "plusplus"→"plus…"）。
///
/// 正常文本达不到"某个 3~8 字窗口覆盖一半篇幅"这条线；短于 24 字的文本直接跳过，
/// 所以"哈哈哈哈哈""对对对"这类口头重复不会被误伤。
pub fn collapse_repetition(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 24 {
        return text.to_string();
    }
    for len in [16usize, 12, 8, 6, 4, 3] {
        if n < len * 8 {
            continue;
        }
        // gram -> (count, first_start, second_start)
        let mut best: Option<(&[char], usize, usize, usize)> = None;
        let mut i = 0usize;
        while i + len <= n {
            let gram = &chars[i..i + len];
            let mut count = 1usize;
            let mut second: Option<usize> = None;
            let mut j = i + 1;
            while j + len <= n {
                if &chars[j..j + len] == gram {
                    count += 1;
                    if second.is_none() {
                        second = Some(j);
                    }
                    j += len; // 跳过整个重复单元，避免同一处重复计数
                } else {
                    j += 1;
                }
            }
            if count >= 6
                && count * len * 100 >= n * 50
                && best.is_none_or(|(_, c, _, _)| count > c)
            {
                best = Some((gram, count, i, second.unwrap_or(i + len)));
            }
            i += 1;
        }
        if let Some((gram, count, first, _)) = best {
            // 取该 n-gram 的**最小周期**：这样 "都都都都…" 只保留一个「都」，
            // "plusplus…" 只保留一个 "plus"，而不是保留整个 8 字窗口。
            let p = minimal_period(gram);
            let keep = (first + p).min(n);
            let mut out: String = chars[..keep].iter().collect();
            out.push('…');
            log::debug!(
                "[ASR] collapsed repetition: window={} count={} period={} -> {} chars",
                gram.len(),
                count,
                p,
                keep
            );
            return out;
        }
    }
    text.to_string()
}

/// n-gram 的最小重复周期（"都都都都" → 1，"plusplus" → 4，无周期 → 全长）。
fn minimal_period(gram: &[char]) -> usize {
    for p in 1..=gram.len() {
        if gram.iter().enumerate().all(|(i, c)| *c == gram[i % p]) {
            return p;
        }
    }
    gram.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: i64, end_ms: i64, speaker: Option<i32>, text: &str) -> AudioSegment {
        AudioSegment { start_ms, end_ms, speaker, text: text.to_string() }
    }

    #[test]
    fn timestamp_is_zero_padded_hms() {
        assert_eq!(format_timestamp(0), "00:00:00");
        assert_eq!(format_timestamp(999), "00:00:00");
        assert_eq!(format_timestamp(1_000), "00:00:01");
        assert_eq!(format_timestamp(3_723_000), "01:02:03");
        // 超过 100 小时仍完整输出
        assert_eq!(format_timestamp(360_000_000), "100:00:00");
    }

    #[test]
    fn negative_timestamp_clamps_to_zero() {
        assert_eq!(format_timestamp(-5_000), "00:00:00");
    }

    #[test]
    fn tagged_rendering_includes_time_and_speaker() {
        let segs = vec![
            seg(0, 7_000, Some(0), "你好，能听到吗？"),
            seg(7_000, 15_000, Some(1), "听得到。"),
        ];
        assert_eq!(
            render_tagged("你好，能听到吗？\n听得到。", &segs),
            "[00:00:00 · 说话人0] 你好，能听到吗？\n[00:00:07 · 说话人1] 听得到。"
        );
    }

    #[test]
    fn tagged_rendering_omits_speaker_when_absent() {
        let segs = vec![seg(61_000, 70_000, None, "只有时间戳")];
        assert_eq!(render_tagged("只有时间戳", &segs), "[00:01:01] 只有时间戳");
    }

    #[test]
    fn empty_timeline_falls_back_to_plain_text() {
        assert_eq!(render_tagged("纯正文", &[]), "纯正文");
        // 全是空段也算"没有时间轴"
        let blank = vec![seg(0, 1_000, None, "   ")];
        assert_eq!(render_tagged("纯正文", &blank), "纯正文");
    }

    #[test]
    fn blank_segments_are_skipped_without_extra_newlines() {
        let segs = vec![
            seg(0, 1_000, Some(0), "第一句"),
            seg(1_000, 2_000, Some(0), ""),
            seg(2_000, 3_000, Some(0), "第二句"),
        ];
        assert_eq!(
            render_tagged("第一句\n第二句", &segs),
            "[00:00:00 · 说话人0] 第一句\n[00:00:02 · 说话人0] 第二句"
        );
    }

    #[test]
    fn json_roundtrip_preserves_segments() {
        let segs = vec![seg(0, 1_500, Some(2), "甲"), seg(1_500, 3_000, None, "乙")];
        let json = to_json(&segs).expect("non-empty timeline serializes");
        assert_eq!(from_json(Some(&json)), segs);
        // speaker 缺省时不写进 JSON（老数据兼容 + 体积）
        assert!(!json.contains("speaker\":null"));
    }

    #[test]
    fn empty_or_corrupt_json_yields_no_timeline() {
        assert!(to_json(&[]).is_none());
        assert!(to_json(&[seg(0, 1_000, None, "  ")]).is_none());
        assert!(from_json(None).is_empty());
        assert!(from_json(Some("")).is_empty());
        assert!(from_json(Some("not json")).is_empty());
        assert!(!has_timeline(&from_json(Some("garbage"))));
    }

    #[test]
    fn has_timeline_detects_real_content() {
        assert!(!has_timeline(&[]));
        assert!(!has_timeline(&[seg(0, 1_000, None, "  ")]));
        assert!(has_timeline(&[seg(0, 1_000, None, "有字")]));
    }

    #[test]
    fn collapse_repetition_truncates_chinese_loop() {
        // 实测形态：正常开场 + 一长串「都」（末尾常带句号）
        let text = format!("个人的对对对,其实大家{}。", "都".repeat(18));
        let out = collapse_repetition(&text);
        assert_eq!(out, "个人的对对对,其实大家都…");
    }

    #[test]
    fn collapse_repetition_handles_ascii_loop_with_trailing_dot() {
        // 实测形态："arcsplusplusplusplus…."（末尾句号不能挡住判定）
        let text = format!("arcs{}", "plus".repeat(30)) + ".";
        let out = collapse_repetition(&text);
        assert!(out.ends_with('…'), "应截断: {out}");
        assert!(out.starts_with("arcspl"), "保留一个周期左右: {out}");
        assert!(out.chars().count() < 12, "截断后应很短: {out}");
    }

    #[test]
    fn collapse_repetition_truncates_english_loop() {
        // 实测形态：一段 7 秒的语音吐了 2082 字
        let text = format!("SEE {}", "your thoughts on ".repeat(40));
        let out = collapse_repetition(&text);
        assert!(out.ends_with('…'), "应截断: {out}");
        assert!(out.chars().count() < 40, "截断后应很短: {out}");
        assert!(out.starts_with("SEE your"), "保留开头: {out}");
        assert_eq!(
            out.matches("thoughts").count(),
            1,
            "复读单元应只剩一次: {out}"
        );
    }

    #[test]
    fn collapse_repetition_keeps_normal_text() {
        for ok in [
            "现在都是这样,你看这样加起来多少",
            "对对对，我同意",
            "哈哈哈哈哈",              // 短促重复不该被误伤
            "好的好的好的，那就这样吧", // 正常口头重复
            "",
        ] {
            assert_eq!(collapse_repetition(ok), ok, "不该改动正常文本: {ok:?}");
        }
    }
}
