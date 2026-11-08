//! 远端模型清单（`models-catalog.json`）—— 让模型可以**不发版**升级。
//!
//! 清单托管在 models 仓库的 **raw 分支文件**（默认 `main`）：
//! `https://raw.githubusercontent.com/<repo>/<branch>/models-catalog.json`
//! 同时按国内可用的前缀镜像（ghfast.top / gh-proxy.com）重试。
//!
//! 资产本身仍发布在 GitHub Releases（清单里用 `tag` 指定），这样可以复用
//! `download.rs` 既有的镜像链与断点续传。
//!
//! 环境变量：
//! - `LINK_SEARCHER_MODELS_CATALOG`：直接指定清单完整 URL（测试 / 私有部署）
//! - `LINK_SEARCHER_MODELS_GH`：`owner/repo`
//! - `LINK_SEARCHER_MODELS_BRANCH`：分支名（默认 `main`）
//!
//! 安全边界：
//! - `schema` 不认 → 整份忽略（不猜）；
//! - 每个文件必须带 `sha256`，下载层逐文件校验；
//! - `min_app_version` 高于当前 app → 跳过该条（老客户端不会装上跑不动的模型）。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::deps::catalog::{self, Source};

/// 清单格式版本。结构不兼容时 +1，老客户端会整份忽略。
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteFile {
    /// Releases 资产名（`<tag>/<remote>`）。
    pub remote: String,
    /// 落地文件名（可含子目录）。
    pub local: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteDep {
    /// 版本标识，同时用作 `versions/<version>/` 目录名。
    pub version: String,
    /// GitHub Releases tag（资产所在地）。
    pub tag: String,
    /// 低于该 app 版本时不提示（可选）。
    #[serde(default)]
    pub min_app_version: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub files: Vec<RemoteFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteCatalog {
    pub schema: u32,
    #[serde(default)]
    pub deps: BTreeMap<String, RemoteDep>,
}

/// 一次可执行的升级计划。
#[derive(Debug, Clone)]
pub struct Plan {
    pub dep: String,
    pub version: String,
    pub size_bytes: u64,
    pub notes: String,
    pub files: Vec<RemoteFile>,
    pub sources: Vec<Source>,
}

/// 依赖中心展示用的"有更新"条目。
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub dep: String,
    pub name: String,
    pub current: Option<String>,
    pub latest: String,
    pub size_bytes: u64,
    pub notes: String,
}

pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 该依赖是否走"版本化安装"（`models/<dep>/versions/<ver>`）。
pub fn is_versioned(dep: &str) -> bool {
    VERSIONED_DEPS.contains(&dep)
}

/// 清单 URL 候选（镜像优先，回落官方 raw）。
pub fn catalog_urls() -> Vec<String> {
    if let Ok(u) = std::env::var("LINK_SEARCHER_MODELS_CATALOG") {
        let u = u.trim().to_string();
        if !u.is_empty() {
            return vec![u];
        }
    }
    let repo = std::env::var("LINK_SEARCHER_MODELS_GH")
        .unwrap_or_else(|_| "Gray-Chan-sh/link-searcher-models".to_string());
    let branch = std::env::var("LINK_SEARCHER_MODELS_BRANCH").unwrap_or_else(|_| "main".to_string());
    let canonical =
        format!("https://raw.githubusercontent.com/{repo}/{branch}/models-catalog.json");
    vec![
        format!("https://ghfast.top/{canonical}"),
        format!("https://gh-proxy.com/{canonical}"),
        canonical,
    ]
}

/// 拉取并解析清单。失败返回错误（调用方应当**静默**沿用本地状态）。
pub fn fetch(timeout: Duration) -> Result<RemoteCatalog, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .user_agent(concat!("link-searcher/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut last_err = String::from("没有可用的清单地址");
    for url in catalog_urls() {
        match agent.get(&url).call() {
            Ok(resp) => {
                let mut body = String::new();
                // 最多读 1MB：清单只是元数据，防止被超大响应拖死
                use std::io::Read;
                match resp.into_reader().take(1024 * 1024).read_to_string(&mut body) {
                    Ok(_) => match serde_json::from_str::<RemoteCatalog>(&body) {
                        Ok(cat) if cat.schema == SCHEMA => return Ok(cat),
                        Ok(cat) => {
                            last_err = format!("清单 schema={} 不受支持（需要 {SCHEMA}）", cat.schema)
                        }
                        Err(e) => last_err = format!("清单解析失败: {e}"),
                    },
                    Err(e) => last_err = format!("读取清单失败: {e}"),
                }
            }
            Err(e) => last_err = format!("{url}: {e}"),
        }
        log::debug!("[DEPS] 清单源失败（继续下一个）: {last_err}");
    }
    Err(last_err)
}

/// `a > b`（点分数字逐段比较，缺失段按 0；非数字段退化为字符串比较）。
pub fn version_gt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<&str> {
        v.trim_start_matches('v').split('.').collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (pa.get(i).copied().unwrap_or("0"), pb.get(i).copied().unwrap_or("0"));
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => {
                if nx != ny {
                    return nx > ny;
                }
            }
            _ => {
                if x != y {
                    return x > y;
                }
            }
        }
    }
    false
}

/// 支持"版本化安装 + 在线升级"的依赖（其余仍是平铺安装，升级无意义）。
pub const VERSIONED_DEPS: &[&str] = &["paddleocr"];

/// 为某个依赖挑出可执行的升级（版本不同 + app 版本满足 `min_app_version`）。
pub fn plan_for(cat: &RemoteCatalog, dep: &str, current: Option<&str>) -> Option<Plan> {
    if !crate::deps::remote::is_versioned(dep) {
        return None;
    }
    let d = cat.deps.get(dep)?;
    if let Some(min) = d.min_app_version.as_deref() {
        if version_gt(min, app_version()) {
            log::info!(
                "[DEPS] {dep}: 远端版本 {} 需要 app ≥ {min}（当前 {}），跳过",
                d.version,
                app_version()
            );
            return None;
        }
    }
    if current == Some(d.version.as_str()) {
        return None;
    }
    if d.files.is_empty() {
        return None;
    }
    Some(Plan {
        dep: dep.to_string(),
        version: d.version.clone(),
        size_bytes: d.size_bytes,
        notes: d.notes.clone(),
        files: d.files.clone(),
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: catalog::gh_base_for_tag(&d.tag),
        }],
    })
}

/// 汇总"哪些依赖有更新"（用于依赖中心的提示）。
pub fn pending_updates(cat: &RemoteCatalog, data_dir: &std::path::Path) -> Vec<UpdateInfo> {
    let st = crate::deps::state::load(data_dir);
    let mut out = Vec::new();
    for def in catalog::all() {
        if def.files.is_empty() || !is_versioned(def.id) {
            continue;
        }
        let current = st.deps.get(def.id).map(|e| e.current.clone());
        if let Some(plan) = plan_for(cat, def.id, current.as_deref()) {
            out.push(UpdateInfo {
                dep: def.id.to_string(),
                name: def.name.to_string(),
                current,
                latest: plan.version,
                size_bytes: plan.size_bytes,
                notes: plan.notes,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "schema": 1,
      "deps": {
        "paddleocr": {
          "version": "v6-tiny-1",
          "tag": "models-v2",
          "min_app_version": "1.5.0",
          "size_bytes": 6400000,
          "notes": "v6 tiny",
          "files": [
            {"remote":"paddleocr-v6-det.onnx","local":"det.onnx","sha256":"aa","size":1783000}
          ]
        },
        "bge-large": {
          "version": "future-1",
          "tag": "models-v9",
          "min_app_version": "99.0.0",
          "files": [{"remote":"x","local":"y","sha256":"bb"}]
        }
      }
    }"#;

    #[test]
    fn parses_catalog_and_filters_by_app_version() {
        let cat: RemoteCatalog = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(cat.schema, SCHEMA);
        // min_app_version 满足 → 有计划
        let plan = plan_for(&cat, "paddleocr", None).expect("should plan");
        assert_eq!(plan.version, "v6-tiny-1");
        assert_eq!(plan.files.len(), 1);
        assert!(plan.sources[0].base_url.ends_with("models-v2"));
        // min_app_version 不满足 → 跳过
        assert!(plan_for(&cat, "bge-large", None).is_none());
        // 已是该版本 → 不需要升级
        assert!(plan_for(&cat, "paddleocr", Some("v6-tiny-1")).is_none());
        // 未知依赖
        assert!(plan_for(&cat, "nope", None).is_none());
    }

    #[test]
    fn rejects_unknown_schema() {
        let cat: RemoteCatalog = serde_json::from_str(r#"{"schema":2,"deps":{}}"#).unwrap();
        assert_ne!(cat.schema, SCHEMA);
    }

    #[test]
    fn version_comparison_is_numeric() {
        assert!(version_gt("1.10.0", "1.9.9"));
        assert!(!version_gt("1.9.9", "1.10.0"));
        assert!(!version_gt("1.5.0", "1.5.0"));
        assert!(version_gt("v2.0", "1.99"));
    }

    #[test]
    fn missing_optional_fields_still_parse() {
        let cat: RemoteCatalog = serde_json::from_str(
            r#"{"schema":1,"deps":{"x":{"version":"v1","tag":"t","files":[]}}}"#,
        )
        .unwrap();
        assert_eq!(cat.deps["x"].size_bytes, 0);
        assert!(cat.deps["x"].notes.is_empty());
    }

    /// 依赖中心"有更新"列表：以本地状态为基线，只报版本化安装的依赖。
    #[test]
    fn pending_updates_uses_local_state_as_baseline() {
        let root = std::env::temp_dir().join(format!(
            "ls_remote_pending_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        crate::deps::state::register_install(&root, "paddleocr", "v5-mobile-1").unwrap();

        let cat: RemoteCatalog = serde_json::from_str(SAMPLE).unwrap();
        let ups = pending_updates(&cat, &root);
        assert_eq!(ups.len(), 1, "只有 paddleocr 有更新: {ups:?}");
        assert_eq!(ups[0].dep, "paddleocr");
        assert_eq!(ups[0].current.as_deref(), Some("v5-mobile-1"));
        assert_eq!(ups[0].latest, "v6-tiny-1");

        // 升到目标版本之后就不再提示
        crate::deps::state::register_install(&root, "paddleocr", "v6-tiny-1").unwrap();
        assert!(pending_updates(&cat, &root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
