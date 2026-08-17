//! 检索范围合并去冗余：父目录吞噬子目录/子文件（并集聚合）。
//!
//! 背景：`dir_ids`（监控根过滤）与 `path_prefixes`（子目录前缀过滤）当前是两个
//! 独立 Must 条件——若用户同时设"A 目录"（→dir_id）与"A/B 子目录"（→prefix），
//! 检索被 AND 收窄成"A∩B"，而非用户意图的"A∪(A/B)=A"。本模块在源头将二者
//! 合并成一组路径前缀（并集、父吞子），消除 AND 交叉。

use link_searcher_lib::commands::ai::merge_scope_prefixes;

/// dir 列表：监控根 id → 绝对路径
fn roots() -> Vec<(String, String)> {
    vec![
        ("d1".to_string(), "/Volumes/Docs".to_string()),
        ("d2".to_string(), "/Volumes/Cases".to_string()),
    ]
}

#[test]
fn root_dir_swallows_subdir_and_file() {
    // 用户设 A 目录（dir_id=d1）+ @A/B（prefix="Docs/B"）+ @A/c.pdf（prefix="Docs/c.pdf"）
    // → 全被 d1 覆盖，无残留 prefix
    let dirs = roots();
    let (dirs_kept, prefixes) = merge_scope_prefixes(
        &dirs,
        &["d1".to_string()],
        &["Docs/B".to_string(), "Docs/c.pdf".to_string()],
    );
    assert_eq!(dirs_kept, vec!["d1".to_string()]);
    assert!(prefixes.is_empty(), "父目录 d1 应吞噬其下所有子路径: {prefixes:?}");
}

#[test]
fn unrelated_root_keeps_prefix() {
    // 设 d1 + 另一个根 d2 下的子目录 B → B 不属于 d1，保留
    let dirs = roots();
    let (dirs_kept, prefixes) = merge_scope_prefixes(
        &dirs,
        &["d1".to_string()],
        &["Cases/B".to_string()],
    );
    assert_eq!(dirs_kept, vec!["d1".to_string()]);
    assert_eq!(prefixes, vec!["Cases/B".to_string()]);
}

#[test]
fn nested_prefixes_keep_shortest() {
    // 同一根下的父子前缀：A/B 和 A/B/C → 保留 A/B（父吞子）
    let dirs: Vec<(String, String)> = vec![];
    let (_, prefixes) = merge_scope_prefixes(
        &dirs,
        &[],
        &["X/A/B".to_string(), "X/A/B/C".to_string()],
    );
    assert_eq!(prefixes, vec!["X/A/B".to_string()]);
}

#[test]
fn prefix_under_root_without_dir_id_is_kept() {
    // dir_ids 为空但 prefix 在已注册根下：保留 prefix（未显式选根时不吞噬）
    let dirs = roots();
    let (dirs_kept, prefixes) = merge_scope_prefixes(
        &dirs,
        &[],
        &["Docs/B".to_string()],
    );
    assert!(dirs_kept.is_empty());
    assert_eq!(prefixes, vec!["Docs/B".to_string()]);
}