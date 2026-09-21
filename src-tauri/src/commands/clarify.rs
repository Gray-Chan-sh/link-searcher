//! 下游：提案 → 验证 → 裁决。
//!
//! 规则层只做 sound 的形式初筛；"指代绑谁 / 是否歧义"在这里**算出来**：
//! 按槽的 `stype` 去 State/Grounding 查，数候选个数——不查词表、不判词性。
//! 设计见 docs/diagrams/rag-query-sequence.png。

use std::collections::HashMap;

/// 在案实体达到该计数才参与裁决。
pub const THETA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub stype: String,
    pub surface: String,
    pub referent: Option<String>,
}

/// 检索约束（范围与条件）。由前端 `TurnScope` 填入——解析在前端，IR 只做聚合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Constraints {
    pub files: Vec<String>,
    pub dirs: Vec<String>,
    pub exts: Vec<String>,
    pub dates: Vec<String>,
}

impl Constraints {
    pub fn from_scope(
        mention_files: &[String],
        mention_dirs: &[String],
        conditions: &[(String, String)],
    ) -> Self {
        let pick = |kind: &str| -> Vec<String> {
            conditions.iter().filter(|(k, _)| k == kind).map(|(_, v)| v.clone()).collect()
        };
        Self {
            files: mention_files.to_vec(),
            dirs: mention_dirs.to_vec(),
            exts: pick("ext"),
            dates: pick("date"),
        }
    }

    /// 显式范围条目（文件 + 目录）。非空 = 用户已主动收窄，State 以它为准。
    pub fn scope_items(&self) -> Vec<String> {
        self.files.iter().chain(self.dirs.iter()).cloned().collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ir {
    pub intent: String,
    pub slots: Vec<Slot>,
    pub constraints: Constraints,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEntity {
    pub value: String,
    pub stype: String,
    pub salience: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub entities: Vec<StateEntity>,
}

impl SessionState {
    pub fn add(&mut self, value: &str, stype: &str, salience: u32) {
        if let Some(e) = self.entities.iter_mut().find(|e| e.value == value && e.stype == stype) {
            e.salience = e.salience.max(salience);
        } else {
            self.entities.push(StateEntity { value: value.into(), stype: stype.into(), salience });
        }
    }

    pub fn candidates(&self, stype: &str) -> Vec<&StateEntity> {
        let mut v: Vec<&StateEntity> =
            self.entities.iter().filter(|e| e.stype == stype && e.salience >= THETA).collect();
        v.sort_by(|a, b| b.salience.cmp(&a.salience).then_with(|| a.value.cmp(&b.value)));
        v
    }
}

/// 语料可知性（v1 由证据路径构建；后续由全库索引构建）。
#[derive(Debug, Clone, Default)]
pub struct Grounding {
    entries: Vec<(String, String)>,
}

impl Grounding {
    pub fn values(&self, stype: &str) -> Vec<&str> {
        let mut v: Vec<&str> =
            self.entries.iter().filter(|(_, t)| t == stype).map(|(v, _)| v.as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn count(&self, stype: &str) -> usize {
        self.values(stype).len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Answerable,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotOutcome {
    Bound { surface: String, value: String },
    Ask { surface: String, stype: String, options: Vec<String> },
    NoSuchType { surface: String, stype: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub verdict: Verdict,
    pub outcomes: Vec<SlotOutcome>,
}

/// 逐槽裁决：在案唯一 → 绑定；在案多个 → 问；在案没有 → 看语料；语料也没有 → 明说没有。
pub fn resolve(ir: &Ir, state: &SessionState, grounding: &Grounding) -> Resolution {
    let mut outcomes = Vec::new();
    let mut ambiguous = false;
    for slot in &ir.slots {
        if let Some(v) = &slot.referent {
            outcomes.push(SlotOutcome::Bound { surface: slot.surface.clone(), value: v.clone() });
            continue;
        }
        let in_session: Vec<String> =
            state.candidates(&slot.stype).iter().map(|e| e.value.clone()).collect();
        match in_session.len() {
            1 => outcomes.push(SlotOutcome::Bound {
                surface: slot.surface.clone(),
                value: in_session[0].clone(),
            }),
            0 => {
                let g: Vec<String> =
                    grounding.values(&slot.stype).iter().map(|s| (*s).to_string()).collect();
                match g.len() {
                    1 => outcomes.push(SlotOutcome::Bound {
                        surface: slot.surface.clone(),
                        value: g[0].clone(),
                    }),
                    0 => outcomes.push(SlotOutcome::NoSuchType {
                        surface: slot.surface.clone(),
                        stype: slot.stype.clone(),
                    }),
                    _ => {
                        ambiguous = true;
                        outcomes.push(SlotOutcome::Ask {
                            surface: slot.surface.clone(),
                            stype: slot.stype.clone(),
                            options: g,
                        });
                    }
                }
            }
            _ => {
                ambiguous = true;
                outcomes.push(SlotOutcome::Ask {
                    surface: slot.surface.clone(),
                    stype: slot.stype.clone(),
                    options: in_session,
                });
            }
        }
    }
    Resolution {
        verdict: if ambiguous { Verdict::Ambiguous } else { Verdict::Answerable },
        outcomes,
    }
}

/// 把裁决结果转成 UI 契约：披露文案 + 一键收窄候选。无 Ask 则返回 None（不打扰用户）。
pub fn clarify_from_resolution(res: &Resolution) -> Option<(String, Vec<String>)> {
    let asks: Vec<(&str, &Vec<String>)> = res
        .outcomes
        .iter()
        .filter_map(|o| match o {
            SlotOutcome::Ask { surface, options, .. } => Some((surface.as_str(), options)),
            _ => None,
        })
        .collect();
    if asks.is_empty() {
        return None;
    }
    let mut cands: Vec<String> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for (surface, options) in asks {
        parts.push(format!("{surface} → {}", options.join(" / ")));
        for o in options {
            if !cands.contains(o) {
                cands.push(o.clone());
            }
        }
    }
    Some((format!("（提示：{}。如需精确，请用 @ 指定文件或目录。）", parts.join("；")), cands))
}

const PERSON_PRONOUN: &[&str] = &["他", "她"];
const DEMONSTRATIVE: &[&str] = &["这个", "那个", "这些", "那些"];
const PRECOMPOSED: &[(&str, &str)] =
    &[("本案", "case"), ("该案", "case"), ("该文件", "doc"), ("该材料", "doc")];

/// 关系型角色名词：本身不指名，必须靠上文才能确定指代（"被告"是哪个案子的被告？）。
/// 仅当问句里**没有任何具名实体**时才产槽——否则"汪均益案中，被告歙县…"这类
/// 已由问句自身锚定的提问会被误判为需要澄清。
const ROLE_NOUN: &[(&str, &str)] = &[
    ("被告", "person"), ("原告", "person"), ("第三人", "person"),
    ("上诉人", "person"), ("被上诉人", "person"), ("申请人", "person"),
    ("我方", "person"), ("对方", "person"), ("本方", "person"),
];

/// 类型名词 → 槽类型。**这是提案层的脏词典**（允许不全）：Resolver 会用
/// State/Grounding 的计数验证，认不出就落到 NoSuchType 披露，不会误绑。
const NOUN_TYPE: &[(&str, &str)] = &[
    ("案子", "case"), ("案件", "case"), ("案", "case"),
    ("法院", "org"), ("法庭", "org"), ("公司", "org"), ("单位", "org"), ("机关", "org"),
    ("部门", "org"), ("事务所", "org"), ("委员会", "org"),
    ("人员", "person"), ("当事人", "person"), ("原告", "person"), ("被告", "person"),
    ("律师", "person"), ("继承人", "person"), ("人", "person"),
    ("材料", "doc"), ("文件", "doc"), ("证据", "doc"), ("判决书", "doc"), ("裁定书", "doc"),
    ("文书", "doc"), ("合同", "doc"), ("协议", "doc"),
    ("房屋", "place"), ("不动产", "place"), ("地点", "place"), ("地方", "place"),
];

fn noun_type(noun: &str) -> &'static str {
    NOUN_TYPE.iter().find(|(k, _)| noun.contains(k)).map(|(_, v)| *v).unwrap_or("other")
}

fn jieba_tags(text: &str) -> Vec<(String, String)> {
    let jieba = crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner());
    jieba.tag(text, true).iter().map(|t| (t.word.to_string(), t.tag.to_string())).collect()
}

/// 脏 IR 提议（规则版；日后可换 LLM，契约不变）。
pub fn propose_ir(q: &str) -> Ir {
    let intent = if q.contains("时间线") || q.contains("经过") || q.contains("沿革") {
        "timeline"
    } else if q.contains("多少") || q.contains("几个") || q.contains("数量") || q.contains("几件") {
        "count"
    } else {
        "lookup"
    };
    let toks = jieba_tags(q);
    let has_named = !content_entities(q).is_empty();
    let mut slots: Vec<Slot> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let w = &toks[i].0;
        if PERSON_PRONOUN.contains(&w.as_str()) {
            slots.push(Slot { stype: "person".into(), surface: w.clone(), referent: None });
        } else if DEMONSTRATIVE.contains(&w.as_str()) {
            let mut stype = "other";
            let mut surface = w.clone();
            for (nw, nt) in toks.iter().skip(i + 1) {
                if nt.starts_with('n') {
                    stype = noun_type(nw);
                    surface = format!("{w}{nw}");
                    break;
                }
                if nt != "uj" {
                    break;
                }
            }
            slots.push(Slot { stype: stype.into(), surface, referent: None });
        } else if let Some((_, st)) = PRECOMPOSED.iter().find(|(k, _)| k == w) {
            slots.push(Slot { stype: (*st).into(), surface: w.clone(), referent: None });
        } else if !has_named
            && let Some((_, st)) = ROLE_NOUN.iter().find(|(k, _)| k == w)
        {
            slots.push(Slot { stype: (*st).into(), surface: w.clone(), referent: None });
        }
        i += 1;
    }
    Ir { intent: intent.into(), slots, constraints: Constraints::default() }
}

/// 从文本抽取候选实体：词性 + 「地名+机构」合并（歙县 + 人民法院 → 歙县人民法院）。
/// 提案层，允许脏——能否绑定由 Resolver 按计数裁决。
fn content_entities(text: &str) -> Vec<(String, String)> {
    let toks = jieba_tags(text);
    let mut out = Vec::new();
    for (i, (w, tag)) in toks.iter().enumerate() {
        let tag = tag.as_str();
        if tag == "nt" {
            let value = if i > 0 && toks[i - 1].1 == "ns" {
                format!("{}{}", toks[i - 1].0, w)
            } else {
                w.clone()
            };
            if value.chars().count() >= 2 && !is_type_noun(&value) {
                out.push((value, "org".to_string()));
            }
            continue;
        }
        if tag == "ns" && i + 1 < toks.len() && toks[i + 1].1 == "nt" {
            continue;
        }
        let stype = match tag {
            "nr" => "person",
            "ns" => "place",
            "nz" => "other",
            _ => continue,
        };
        if w.chars().count() >= 2 && !is_type_noun(w) {
            out.push((w.clone(), stype.to_string()));
        }
    }
    out
}

fn is_type_noun(w: &str) -> bool {
    crate::commands::ai::DOC_TYPE_NOUNS.contains(&w)
}

/// 问句里点名的实体（高显著入册）。
pub fn question_entities(q: &str) -> Vec<(String, String)> {
    content_entities(q)
}

fn path_entities(path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 {
        out.push((format!("{}/{}", segs[0], segs[1]), "case".to_string()));
    }
    out.extend(content_entities(&path.replace('/', " ")));
    out
}

/// 主导案卷：证据里出现最多的两级目录前缀。用它把模板/书籍路径挡在 State 之外。
fn dominant_case(paths: &[String]) -> Option<String> {
    let mut c: HashMap<&str, u32> = HashMap::new();
    for p in paths {
        let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() >= 2 {
            *c.entry(segs[1]).or_insert(0) += 1;
        }
    }
    let best = c.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)))?;
    paths
        .iter()
        .find(|p| p.split('/').nth(1) == Some(best.0))
        .map(|p| p.split('/').take(2).collect::<Vec<_>>().join("/"))
}

/// 生效的范围前缀：显式 @（文件/目录）优先；否则回退到"主导案卷"启发式。
fn scope_prefixes(all_paths: &[String], explicit_scope: &[String]) -> Vec<String> {
    if !explicit_scope.is_empty() {
        return explicit_scope.to_vec();
    }
    dominant_case(all_paths).into_iter().collect()
}

fn matches_scope(path: &str, prefixes: &[String]) -> bool {
    if prefixes.is_empty() {
        return true;
    }
    prefixes.iter().any(|s| path == s || path.starts_with(&format!("{s}/")))
}

fn scoped_entities(
    session_paths: &[String],
    current: &[(String, String)],
    explicit_scope: &[String],
) -> Vec<(String, String)> {
    let all_paths: Vec<String> = session_paths
        .iter()
        .cloned()
        .chain(current.iter().map(|(p, _)| p.clone()))
        .collect();
    let prefixes = scope_prefixes(&all_paths, explicit_scope);
    let mut out = Vec::new();
    for p in session_paths {
        if matches_scope(p, &prefixes) {
            out.extend(path_entities(p));
        }
    }
    for (p, snippet) in current {
        if !matches_scope(p, &prefixes) {
            continue;
        }
        out.extend(path_entities(p));
        if !snippet.is_empty() {
            out.extend(content_entities(snippet));
        }
    }
    out
}

/// `current` 为本轮命中的 (`路径`, `正文片段`)：机构/人名常只在正文里，路径抽不到。
/// `explicit_scope` 非空时以它为准（用户已收窄 → 候选自然变少、不必再问）。
pub fn build_state(
    session_paths: &[String],
    current: &[(String, String)],
    explicit_scope: &[String],
) -> SessionState {
    let mut counts: HashMap<(String, String), u32> = HashMap::new();
    for (v, t) in scoped_entities(session_paths, current, explicit_scope) {
        *counts.entry((v, t)).or_insert(0) += 1;
    }
    let mut st = SessionState::default();
    for ((v, t), n) in counts {
        st.add(&v, &t, n);
    }
    st
}

pub fn build_grounding(
    session_paths: &[String],
    current: &[(String, String)],
    explicit_scope: &[String],
) -> Grounding {
    let mut entries: Vec<(String, String)> = Vec::new();
    for (v, t) in scoped_entities(session_paths, current, explicit_scope) {
        if !entries.iter().any(|(ev, et)| ev == &v && et == &t) {
            entries.push((v, t));
        }
    }
    Grounding { entries }
}

#[cfg(test)]
mod tests {
    /// 角色名词只在"问句没有任何具名实体"时产槽——已具名的提问不该被误判为需澄清。
    #[test]
    fn role_noun_slot_only_without_named_entity() {
        let bare = propose_ir("被告在答辩状里是怎么抗辩的？");
        assert!(
            bare.slots.iter().any(|s| s.surface == "被告"),
            "无具名实体时应为「被告」产槽: {:?}",
            bare.slots
        );
        let named = propose_ir("汪均益案中，被告歙县自然资源和规划局在答辩状里是怎么抗辩的？");
        assert!(
            !named.slots.iter().any(|s| s.surface == "被告"),
            "已具名时不应为「被告」产槽: {:?}",
            named.slots
        );
    }

    /// 第 2 轮那道题：人称代词「他」应产 person 槽（无具名实体）。
    #[test]
    fn pronoun_slot_for_unanchored_question() {
        let ir = propose_ir("判决书里为什么认定他是利害关系人？");
        assert!(
            ir.slots.iter().any(|s| s.stype == "person"),
            "应为人称代词产 person 槽: {:?}",
            ir.slots
        );
    }

    use super::*;

    /// 一个聚焦会话：汪均益案卷 + 跨案污染源（模板/书籍）。
    fn session_paths() -> Vec<String> {
        vec![
            "案件/WJY 汪均益/行政诉讼/二审/第一次/行政上诉状.docx".into(),
            "案件/WJY 汪均益/行政诉讼/一审/第二次/庭审笔录.pdf".into(),
            "案件/WJY 汪均益/更正登记/情况说明/情况说明.docx".into(),
            "案件/WJY 汪均益/委托手续/03 聘请律师合同（2份）.docx".into(),
            "案件/WJY 汪均益/行政诉讼/一审/第二次/追加第三人申请书.docx".into(),
            "案件/WJY 汪均益/身份证明/汪均丰身份证.pdf".into(),
            "律师业务文书模板/民事文书样式/流程格式文书供参考/文书13 申请书(申请书证提出命令用).docx".into(),
            "律师业务文书模板/民事文书样式/流程格式文书供参考/文书88 申请书(申请撤销确认调解协议裁定用).docx".into(),
            "学习/理解与适用/最高人民法院新民事诉讼法司法解释理解与适用（上）.pdf".into(),
        ]
    }

    fn run(q: &str) -> (Ir, Resolution) {
        let paths = session_paths();
        let mut st = build_state(&paths, &[], &[]);
        for (v, t) in question_entities(q) {
            st.add(&v, &t, 3);
        }
        let g = build_grounding(&paths, &[], &[]);
        let ir = propose_ir(q);
        let res = resolve(&ir, &st, &g);
        (ir, res)
    }

    #[test]
    fn round4_timeline_binds_case_and_does_not_ask() {
        let (ir, res) = run("梳理这个案子的时间线");
        assert_eq!(ir.slots.len(), 1, "{:?}", ir.slots);
        assert_eq!(ir.slots[0].stype, "case");
        assert_eq!(
            res.outcomes[0],
            SlotOutcome::Bound {
                surface: "这个案子".into(),
                value: "案件/WJY 汪均益".into()
            },
            "{:?}",
            res.outcomes
        );
        assert!(clarify_from_resolution(&res).is_none(), "must not prompt: {:?}", res.outcomes);
    }

    #[test]
    fn grounding_scope_excludes_templates_and_books() {
        let paths = session_paths();
        let g = build_grounding(&paths, &[], &[]);
        assert!(!g.values("person").contains(&"申请书"), "type-noun leaked: {:?}", g.values("person"));
        assert!(!g.values("org").contains(&"最高人民法院"), "book org leaked");
        assert!(g.values("person").contains(&"汪均益"));
    }

    #[test]
    fn court_slot_is_never_wrongly_asked() {
        let (ir, res) = run("这个法院做的判决");
        assert_eq!(ir.slots[0].stype, "org");
        assert_eq!(res.outcomes[0], SlotOutcome::NoSuchType { surface: "这个法院".into(), stype: "org".into() });
        assert!(clarify_from_resolution(&res).is_none());
    }

    #[test]
    fn court_slot_binds_when_the_court_is_in_play() {
        let paths = session_paths();
        let mut st = build_state(&paths, &[], &[]);
        st.add("歙县人民法院", "org", 5);
        let g = build_grounding(&paths, &[], &[]);
        let res = resolve(&propose_ir("这个法院做的判决"), &st, &g);
        assert_eq!(
            res.outcomes[0],
            SlotOutcome::Bound { surface: "这个法院".into(), value: "歙县人民法院".into() }
        );
    }

    #[test]
    fn multi_slot_resolves_independently() {
        let (ir, res) = run("他在这个法院有多少案件");
        assert_eq!(ir.intent, "count");
        assert_eq!(ir.slots.len(), 2, "{:?}", ir.slots);
        let ask = res.outcomes.iter().find(|o| matches!(o, SlotOutcome::Ask { .. })).expect("ask");
        match ask {
            SlotOutcome::Ask { surface, options, .. } => {
                assert_eq!(surface, "他");
                assert!(options.contains(&"汪均益".to_string()), "{options:?}");
                assert!(options.contains(&"汪均丰".to_string()), "{options:?}");
            }
            _ => unreachable!(),
        }
        assert!(res.outcomes.iter().any(|o| matches!(o, SlotOutcome::NoSuchType { stype, .. } if stype == "org")));
        let (note, cands) = clarify_from_resolution(&res).expect("should ask about the person slot");
        assert!(note.contains("他"));
        assert!(cands.contains(&"汪均益".to_string()) && !cands.iter().any(|c| c.contains("法院")));
    }

    #[test]
    fn same_surface_form_yields_different_classes_by_count() {
        let (ir, res) = run("这个人在那个法院判决书结果是什么？");
        assert_eq!(ir.slots.len(), 2, "{:?}", ir.slots);
        assert_eq!(ir.slots[0].stype, "person");
        assert_eq!(ir.slots[1].stype, "org");
        assert!(matches!(res.outcomes[0], SlotOutcome::Ask { .. }), "{:?}", res.outcomes);
        assert!(matches!(res.outcomes[1], SlotOutcome::NoSuchType { .. }), "{:?}", res.outcomes);
    }

    #[test]
    fn court_slot_binds_from_snippet_content() {
        let paths = session_paths();
        let current = vec![(
            "案件/WJY 汪均益/行政诉讼/一审/行政判决书.pdf".to_string(),
            "本院认为，歙县人民法院于1990年5月15日办理的房屋登记，权属来源不明。".to_string(),
        )];
        let st = build_state(&paths, &current, &[]);
        let g = build_grounding(&paths, &current, &[]);
        let res = resolve(&propose_ir("这个法院做的判决"), &st, &g);
        assert_eq!(
            res.outcomes[0],
            SlotOutcome::Bound { surface: "这个法院".into(), value: "歙县人民法院".into() },
            "{:?}",
            res.outcomes
        );
    }

    #[test]
    fn explicit_scope_narrows_candidates_and_stops_asking() {
        let paths = session_paths();
        let scope = vec!["案件/WJY 汪均益/行政诉讼/二审/第一次/行政上诉状.docx".to_string()];
        let st = build_state(&paths, &[], &scope);
        let g = build_grounding(&paths, &[], &scope);
        let res = resolve(&propose_ir("他在这个法院有多少案件"), &st, &g);
        match &res.outcomes[0] {
            SlotOutcome::Bound { value, .. } => assert_eq!(value, "汪均益", "{:?}", res.outcomes),
            other => panic!("expected bind under explicit scope, got {other:?}"),
        }
        assert!(clarify_from_resolution(&res).is_none(), "explicit scope should stop asking");
    }

    #[test]
    fn constraints_from_scope_maps_conditions() {
        let c = Constraints::from_scope(
            &["a/b.pdf".to_string()],
            &["a".to_string()],
            &[
                ("ext".to_string(), "pdf".to_string()),
                ("date".to_string(), "2025-01-01~2025-12-31".to_string()),
            ],
        );
        assert_eq!(c.exts, vec!["pdf".to_string()]);
        assert_eq!(c.dates, vec!["2025-01-01~2025-12-31".to_string()]);
        assert_eq!(c.scope_items(), vec!["a/b.pdf".to_string(), "a".to_string()]);
    }

    #[test]
    fn other_pronoun_is_not_a_reference() {
        let ir = propose_ir("其他案件的时间线");
        assert_eq!(ir.intent, "timeline");
        assert!(ir.slots.is_empty(), "{:?}", ir.slots);
    }
}
