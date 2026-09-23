//! Retrieval scoring: scope filtering, BM25/semantic fusion, rerank ordering
//! and trace instrumentation.

use super::{AppState, ScoredHit, truncate_text};
/// Merge retrieval-scope entries into a non-overlapping (union) set of
/// path prefixes. dir 过滤（监控根, dir_ids）与子目录 prefix 过滤当前是两个
/// 独立 Must 条件——同设父目录+子目录会被 AND 错误收窄。此处按"父吞子"
/// 去冗余：已选根目录之下的子前缀全部删掉（根已覆盖），prefix 内部保留最短。
///
/// `dir_roots`: (dir_id, 根绝对路径)，用于判断 prefix 是否落在某已选根下。
pub fn merge_scope_prefixes(
    dir_roots: &[(String, String)],
    dir_ids: &[String],
    prefixes: &[String],
) -> (Vec<String>, Vec<String>) {
    // prefix 是相对监控根的路径（如 "Docs/B"）；根绝对路径如 "/Volumes/Docs"。
    // 判断 prefix 归属：根 basename 与 prefix 首段相同 → 属于该根；若该根在
    // dir_ids 中，这个 prefix 被根覆盖（根已含其全部内容）→ 去掉。
    let kept: Vec<String> = prefixes
        .iter()
        .filter(|p| {
            let first_seg = p.split('/').next().unwrap_or("");
            let covered_by_selected_root = dir_roots
                .iter()
                .any(|(id, root)| {
                    dir_ids.contains(id)
                        && std::path::Path::new(root)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n == first_seg)
                            .unwrap_or(false)
                });
            !covered_by_selected_root
        })
        .cloned()
        .collect();
    // prefix 内部父吞子：保留最短——若某 prefix 是另一 prefix 的严格父路径，
    // 删掉后者（前者的检索范围已覆盖它）。
    let mut result: Vec<String> = Vec::new();
    for p in &kept {
        if kept.iter().any(|q| {
            q != p
                && p.starts_with(q.as_str())
                && p.as_bytes().get(q.len()) == Some(&b'/')
        }) {
            continue; // p 有严格父 prefix，被覆盖
        }
        result.push(p.clone());
    }
    (dir_ids.to_vec(), result)
}

/// 统一范围判定（合并后硬过滤与注入过滤共用同一份语义）：
/// - 有目录前缀：命中 = 任意前缀内 ∪ scope 点名文件
///   （点名文件可能不在任何前缀下，必须白名单放行）
/// - 无目录前缀且点名了文件：范围只由文件构成 → 纯白名单硬拦，
///   堵住 BM25/向量/文件名三个全库通道把范围外文件卷进证据的泄漏
///   （strict 语义要求：范围=文件集合时，证据只能来自该集合）
/// - 两类条目皆无（空范围）：全库检索，不过滤
pub(super) fn hit_in_scope(
    file_id: &str,
    path: &str,
    prefixes: Option<&[String]>,
    scope_ids: Option<&[String]>,
) -> bool {
    if scope_ids.is_some_and(|ids| ids.iter().any(|i| i == file_id)) {
        return true;
    }
    match prefixes {
        Some(ps) => ps.iter().any(|p| path.starts_with(p.trim_end_matches('/'))),
        // 无目录前缀但范围点了文件（scope_ids 非空）：非白名单成员一律拦截；
        // 无任何范围条目：全库检索放行。
        None => scope_ids.is_none(),
    }
}

#[cfg(test)]
mod hit_in_scope_tests {
    use super::*;

    fn ps(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| s.to_string()).collect())
    }
    fn ids(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn prefixes_rule_with_mention_exception() {
        // 目录前缀内命中 → 放行
        assert!(hit_in_scope("f1", "案件/一审/a.pdf", ps(&["案件"]).as_deref(), ids(&[]).as_deref()));
        // 目录前缀外的命中 → 拦截
        assert!(!hit_in_scope("f2", "其它/b.txt", ps(&["案件"]).as_deref(), ids(&[]).as_deref()));
        // 点名文件即使不在前缀内（文件条目也进了 scope_ids）→ 放行
        assert!(hit_in_scope("f2", "其它/b.txt", ps(&["案件"]).as_deref(), ids(&["f2"]).as_deref()));
    }

    #[test]
    fn file_only_scope_is_pure_allowlist() {
        // 回归核心场景：范围=文件，全库通道命中范围外文件 → 必须拦截
        assert!(hit_in_scope("f1", "二审/x.pdf", None, ids(&["f1"]).as_deref()));
        assert!(!hit_in_scope("f2", "一审/y.pdf", None, ids(&["f1"]).as_deref()));
    }

    #[test]
    fn empty_scope_is_unfiltered_full_library() {
        // 两类条目皆无 → 全库检索（宽松语义），不过滤
        assert!(hit_in_scope("f1", "任意/a.txt", None, None));
        assert!(hit_in_scope("f2", "任意/b.txt", None, None));
    }
}


/// Reciprocal Rank Fusion: score = Σ 1/(k + rank) summed over two
/// pre-sorted lists (best-first). Lists must be ordered by score descending;
/// fusion uses position as rank (0‑based → 1/(k+0), 1/(k+1), …).
/// 生产路径已被 weighted mixing 替代；保留供测试验证 RRF 行为。
#[cfg(test)]
pub(super) fn rrf_fuse(
    bm25_ranked: &[(String, f64)],
    semantic_ranked: &[(String, f32)],
    k: f64,
) -> Vec<(String, f64)> {
    let mut fusion = std::collections::HashMap::new();
    for (i, (fid, _)) in bm25_ranked.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (k + i as f64);
    }
    for (i, (fid, _)) in semantic_ranked.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (k + i as f64);
    }
    let mut ordered: Vec<(String, f64)> = fusion.into_iter().collect();
    ordered.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered
}

/// 加权混合排序：score = w×cosine + (1-w)×(bm25/max_bm25)。
/// BM25 归一化到 0~1 与 cosine 同尺度；返回按混合分降序的 (文件, 归一bm25, cosine, mix)。
pub fn weighted_mix(
    hits: Vec<(String, f64, f64)>,
    w: f64,
) -> Vec<(String, f64, f64, f64)> {
    let max_bm25 = hits.iter().map(|(_, b, _)| *b).fold(0.0_f64, f64::max);
    let norm = |raw: f64| if max_bm25 > 0.0 { raw / max_bm25 } else { 0.0 };
    let mut fused: Vec<(String, f64, f64, f64)> = hits
        .into_iter()
        .map(|(fid, b, c)| (fid.clone(), norm(b), c, w * c + (1.0 - w) * norm(b)))
        .collect();
    fused.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

pub(crate) const RRF_K: f64 = 60.0;

/// Reciprocal Rank Fusion 累加：score(d) += 1/(k + rank0 + 1)。
/// 按"排名"而非"分数"融合，避免某通道因分数量纲不同被结构性压制。
pub(crate) fn rrf_add(
    acc: &mut std::collections::HashMap<String, f64>,
    file_id: &str,
    rank0: usize,
    weight: f64,
) {
    *acc.entry(file_id.to_string()).or_insert(0.0) += weight / (RRF_K + rank0 as f64 + 1.0);
}

/// 诊断：设 `LINK_SEARCHER_TRACE_TARGET=<路径子串>` 时，记录目标文档在管线各步的
/// 名次（BM25 通道 → 入池 → 三通道合并 → rerank → 注入候选），用于排查
/// "检索到了却被挤出注入"。未设环境变量时零开销。
pub(super) fn trace_rank<'a>(
    label: &str,
    ids_iter: impl Iterator<Item = &'a str>,
    targets: &[String],
) {
    if targets.is_empty() { return; }
    let mut total = 0usize;
    let mut pos = None;
    for (i, fid) in ids_iter.enumerate() {
        total += 1;
        if pos.is_none() && targets.iter().any(|t| t == fid) { pos = Some(i + 1); }
    }
    log::info!(
        "[TRACE] {label:<28} {} / {total}",
        pos.map(|p| p.to_string()).unwrap_or_else(|| "缺席".to_string())
    );
}

pub(super) fn trace_target_ids(state: &AppState, sub: &str) -> Vec<String> {
    if sub.is_empty() { return Vec::new(); }
    let Ok(conn) = state.db.get() else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare("SELECT id FROM file_tracking WHERE path LIKE ?1") else { return Vec::new() };
    let pat = format!("%{sub}%");
    stmt.query_map([&pat], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
}

/// Semantic rerank of BM25 hits: embed the query, score the stored
/// embeddings of the BM25 candidates by cosine, fuse both orders via
/// weighted mixing (score = w×cosine + (1-w)×bm25_norm), and return the
/// fused hits with their scores. `None` when any step fails (embedding
/// gateway down, no stored vectors) — caller falls back to BM25.
fn semantic_fuse(
    conn: &rusqlite::Connection,
    query: &str,
    bm25_hits: &[ScoredHit],
    semantic_weight: f64,
) -> Option<Vec<ScoredHit>> {
    let q_vec = {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(crate::ai::cached_embed(&q));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .and_then(|r| r)
    }?;
    let rows = crate::db::tracker::get_all_embeddings(conn).ok()?;
    let bm25_ids: std::collections::HashSet<&str> =
        bm25_hits.iter().map(|h| h.file_id.as_str()).collect();
    let emb_map: std::collections::HashMap<&str, &Vec<f32>> = rows
        .iter()
        .filter(|(fid, _)| bm25_ids.contains(fid.as_str()))
        .map(|(fid, v)| (fid.as_str(), v))
        .collect();
    if emb_map.is_empty() {
        return None;
    }
    // 每份候选：原始 BM25 分 + cosine 相似度。
    let scored: Vec<(&ScoredHit, f64, f64)> = bm25_hits
        .iter()
        .filter_map(|h| {
            emb_map
                .get(h.file_id.as_str())
                .map(|v| (h, h.bm25_score.unwrap_or(0.0), crate::ai::cosine(&q_vec, v) as f64))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    let max_cos = scored.iter().map(|(_, _, c)| *c).fold(0.0_f64, f64::max);
    if max_cos == 0.0 {
        log::warn!("[AI] semantic_fuse: all cosine=0.0 — q_vec dim={} nonzero={} doc dim={}",
            q_vec.len(), q_vec.iter().any(|x| *x != 0.0),
            emb_map.values().next().map(|v| v.len()).unwrap_or(0));
    }
    // BM25 分数归一化到 0~1（max 归一到 1）——与 cosine 同尺度才能加权。
    let pairs: Vec<(String, f64, f64)> = scored
        .iter()
        .map(|(h, b, c)| (h.file_id.clone(), *b, *c))
        .collect();
    let fused = weighted_mix(pairs, semantic_weight.clamp(0.0, 1.0));
    Some(
        fused
            .iter()
            .filter_map(|(fid, _, c, mix)| {
                scored
                    .iter()
                    .find(|(h, _, _)| h.file_id == *fid)
                    .map(|(h, b, _)| ScoredHit {
                        file_id: h.file_id.clone(),
                        path: h.path.clone(),
                        bm25_score: Some(*b),
                        semantic_score: Some(*c),
                        rrf_score: Some(*mix),
                        from_history: false,
                        from_chunk: false,
                        hit_chunks: Vec::new(),
                    })
            })
            .collect(),
    )
}

/// Reorder permutation from reranker scores. Given the full hit list, the
/// indices of candidates that were actually reranked, and their scores,
/// returns a permutation of `0..original.len()` where reranked candidates
/// come first (sorted by score descending) and all others follow in their
/// original relative order.
/// 重排结果与原排序的 RRF 融合。`w=1.0` 完全采用重排名次（原行为），
/// `w=0.0` 完全保留原序。中间值把两个**排名**做 RRF 融合，避免重排把原本
/// 排序正确的文档（尤其长文档）误压出前 10。
const RERANK_FUSION_K: f64 = 60.0;

pub(super) fn apply_rerank_order(
    original: &[ScoredHit],
    rerank_idx: &[usize],
    scores: &[f32],
    w: f64,
) -> Vec<usize> {
    if rerank_idx.is_empty() || scores.is_empty() {
        return (0..original.len()).collect();
    }
    let mut paired: Vec<(usize, f32)> = rerank_idx
        .iter()
        .zip(scores.iter())
        .map(|(&idx, &s)| (idx, s))
        .collect();
    paired.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let rerank_rank: std::collections::HashMap<usize, usize> = paired
        .iter()
        .enumerate()
        .map(|(r, (idx, _))| (*idx, r))
        .collect();

    let w = w.clamp(0.0, 1.0);
    let mut fused: Vec<(usize, f64)> = (0..original.len())
        .map(|i| {
            let s_orig = 1.0 / (RERANK_FUSION_K + i as f64);
            let s_rr = rerank_rank
                .get(&i)
                .map(|r| 1.0 / (RERANK_FUSION_K + *r as f64))
                .unwrap_or(0.0);
            (i, w * s_rr + (1.0 - w) * s_orig)
        })
        .collect();
    // stable sort：同分（如 w=1.0 时全部未重排候选）保持原相对顺序
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.into_iter().map(|(i, _)| i).collect()
}

/// BM25 retrieval returning top relevant hits — tokenised as explicit OR
/// (a raw question would parse as an exact phrase and miss). When
/// `semantic` is true and the embedding gateway is configured, reranks the
/// BM25 candidates via RRF fusion with embedding cosine scores (gracefully
/// falls back to BM25-only on any failure).
pub(crate) fn bm25_relevant_hits(
    state: &AppState,
    query: &str,
    limit: usize,
    semantic: bool,
    dir_ids: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    date_from: Option<i64>,
    date_to: Option<i64>,
    path_prefixes: Option<Vec<String>>,
    file_ids: Option<Vec<String>>,
) -> Result<Vec<ScoredHit>, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
    let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
    let reader = mgr.reader().map_err(|e| format!("{e}"))?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    // When semantic fusion is active we fetch more candidates for the RRF pool.
    // When path_prefixes are set, fetch more to compensate for post-filtering
    // (RegexQuery on STRING fields can silently fail on Unicode paths).
    let fetch = if semantic && crate::ai::embedding_enabled() {
        limit.max(100)
    } else if path_prefixes.as_ref().is_some_and(|p| !p.is_empty()) {
        limit.max(50)
    } else {
        limit
    };
    let params = SearchParams {
        query: crate::search::schema::split_query_terms(&query.to_lowercase()),
        dir_ids: dir_ids.clone(), file_ids: file_ids.clone(), ext_filter: ext_filter.clone(),
        date_from, date_to, path_prefixes: path_prefixes.clone(),
        sort: SortField::Score, sort_order: "desc".to_string(),
        page: 1, page_size: fetch, fuzzy: false, semantic: false,
        dedupe: false,
    };
    log::info!("[AI]   bm25: q=\"{}\" dirs={} files={} limit={}", truncate_text(query, 30), dir_ids.as_ref().map_or(0, |v| v.len()), file_ids.as_ref().map_or(0, |v| v.len()), limit);
    let mut result = searcher.search(&params).map_err(|e| format!("{e}"))?;

    // Fallback: if BM25 returns zero hits but file_ids scope the search to
    // specific files, retry with an empty query (match-all within scope) so
    // the user's scoped files are still returned even when query terms
    // don't match the indexed content (tokenization/extraction differences).
    if result.hits.is_empty() && file_ids.as_ref().is_some_and(|ids| !ids.is_empty()) {
        let fallback_params = SearchParams {
            query: String::new(),
            dir_ids: dir_ids.clone(), file_ids: file_ids.clone(), ext_filter: ext_filter.clone(),
            date_from, date_to, path_prefixes: path_prefixes.clone(),
            sort: SortField::Score, sort_order: "desc".to_string(),
            page: 1, page_size: fetch, fuzzy: false, semantic: false,
            dedupe: false,
        };
        log::info!("[AI] bm25_relevant_hits: zero hits with file_ids, retrying with empty query");
        result = searcher.search(&fallback_params).map_err(|e| format!("{e}"))?;
    }

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut bm25_hits: Vec<ScoredHit> = Vec::new();
    let mut seen_md5: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        // 批量取文件记录，避免每 hit 一次 DB round trip（N+1）
        let ids: Vec<String> = result.hits.iter().map(|h| h.file_id.clone()).collect();
        let recs = crate::db::tracker::get_files_by_ids(&conn, &ids).unwrap_or_default();
        for (hit, rec) in result.hits.iter().zip(recs.iter()) {
            let Some(rec) = rec else { continue };
            if rec.status != "active" { continue; }
            let Some(md5) = &rec.md5 else { continue };
            if !seen_md5.insert(md5.clone()) { continue; }
            bm25_hits.push(ScoredHit {
                file_id: hit.file_id.clone(),
                path: rec.path.clone(),
                bm25_score: Some(hit.score),
                semantic_score: None,
                rrf_score: None,
                from_history: false,
                from_chunk: false,
                hit_chunks: Vec::new(),
            });
        }
    }

    // Safety net: RegexQuery on STRING fields can silently fail on Unicode
    // paths, falling back to AllQuery which disables scope filtering.
    if let Some(prefixes) = &path_prefixes
        && !prefixes.is_empty() {
            bm25_hits.retain(|h| {
                prefixes.iter().any(|p| h.path.starts_with(p.trim_end_matches('/')))
            });
        }

    if semantic && crate::ai::embedding_enabled() && !bm25_hits.is_empty() {
        let weight = crate::config::load_config().semantic_weight.clamp(0.0, 1.0);
        if let Some(fused) = semantic_fuse(&conn, query, &bm25_hits, weight) {
            drop(conn);
            return Ok(fused.into_iter().take(limit).collect());
        }
    }
    drop(conn);
    Ok(bm25_hits.into_iter().take(limit).collect())
}
