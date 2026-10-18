use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};

use crate::config::{config_file_path, load_config, AppConfig, INDEX_DIR_NAME};
use crate::search::IndexManager;
use crate::state::AppState;

#[derive(Serialize)]
pub struct BackupInfo {
    pub last_backup: Option<i64>,
    pub backup_size: u64,
    pub backup_count: u64,
}

/// 快照中的单个文件：相对名（索引内用 `/` 分隔）+ 大小 + sha256。
#[derive(Serialize, Deserialize, Clone)]
pub struct SnapshotFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// 一次快照的清单，写入 `snapshot.json`；后续增量链 / 导出 zip / 恢复均以此为准。
#[derive(Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub files: Vec<SnapshotFile>,
    pub size: u64,
}

/// 链中的一次快照记录（增量链的持久化状态，跟随 `.chain.json`）。
#[derive(Serialize, Deserialize)]
pub struct ChainSnapshot {
    pub id: String,
    pub ts: i64,
    /// "baseline" 或 "incremental"
    pub kind: String,
    pub files: Vec<SnapshotFile>,
}

#[derive(Serialize)]
pub struct BackupSnapshot {
    pub id: String,
    pub ts: i64,
    pub kind: String,
    pub size: u64,
}

#[derive(Serialize)]
pub struct BackupExportResult {
    pub has_secrets: bool,
    pub dest_path: String,
}

#[derive(Serialize)]
pub struct DeadDirInfo {
    pub id: String,
    pub path: String,
    pub file_count: u64,
}

// ───────────────────────── 恢复前目录映射（R1）─────────────────────────
//
// 旧流程是「先整库覆盖，重启后按备份里记录的**原机绝对路径**去扫」。若某条路径在当前
// 机器上存在、但内容并非用户所需，就会被当成一次正常的目录变更扫描 —— 原记录被删除/
// 覆盖，而且没有任何确认。R1 把「目录落位」提前到覆盖之前：plan 只读列出备份里的目录，
// 用户逐个指定本机路径 → apply 时先把新路径写进 **staging 的 data.db 副本**，再灌回
// live，重启后就不会再用旧路径扫描。

/// 恢复前，为备份里每个监控目录指定的落位方式。
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DirMapping {
    pub dir_id: String,
    /// "remap"（改指本机目录）| "keep"（保留原路径）| "disable"（停用扫描但保留数据）
    /// | "remove"（不要该目录，连同清理其数据）
    pub action: String,
    pub new_path: Option<String>,
}

/// 某备份目录映射到候选本机路径时的命中情况（只读预检）。
#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RemapPreview {
    pub tracked: u64,
    pub found: u64,
    pub size_match: u64,
    pub sample_missing: Vec<String>,
}

/// 备份里记录的一个监控目录。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BackupDirInfo {
    pub id: String,
    pub original_path: String,
    pub alias: Option<String>,
    pub file_count: u64,
    /// 备份记录的**原路径**在当前机器上是否已存在（UI 提示用，不决定任何自动行为）。
    pub exists_locally: bool,
}

/// 备份清单（`snapshot.json`）的完整性校验结果。
#[derive(Serialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BackupVerify {
    pub checked: u64,
    /// 内容与清单不符（sha256 或大小不匹配）的文件名。
    pub mismatches: Vec<String>,
    /// 清单列出但文件缺失的名（含清单本身无法读取）。
    pub missing: Vec<String>,
}

impl BackupVerify {
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty() && self.missing.is_empty()
    }
}

/// `plan_restore` 的结果：恢复前的目录清单 + 完整性 + 暂存 token（供 `apply_restore` 复用）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    pub token: String,
    pub dirs: Vec<BackupDirInfo>,
    pub has_chat_history: bool,
    pub has_config: bool,
    /// 备份完整性校验（sha256）。**不阻塞恢复**，仅供 UI 提示。
    pub verify: BackupVerify,
}

/// plan→apply 之间暂存、已解压/已就位的备份源目录。
struct Staging {
    /// 含 `data.db` / `.ls-index` / `chat_history.json` 的源目录。
    src: std::path::PathBuf,
    /// 需要清理的临时目录（zip 解压产物）；快照恢复时为 None（不删用户的备份目录）。
    tmp: Option<std::path::PathBuf>,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(t) = &self.tmp {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

fn staging_registry()
-> &'static std::sync::Mutex<std::collections::HashMap<String, Staging>> {
    static S: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Staging>>,
    > = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 清掉上次未走完 plan/apply 的临时解压目录（进程重启后注册表为空，任何同名前缀目录
/// 都是残留）。best-effort，失败只记日志。
fn cleanup_stale_staging() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("ls_restore_plan_")
            && entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
        {
            let _ = std::fs::remove_dir_all(entry.path());
            log::info!("[RESTORE] 清理残留的 plan 暂存目录: {name}");
        }
    }
}

/// 流式计算文件的 sha256（分块读，避免把大文件整段读进内存）。
fn sha256_file(path: &std::path::Path) -> std::io::Result<(String, u64)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let hex = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((hex, total))
}

/// 校验备份目录的完整性：按 `snapshot.json` 逐文件比对 sha256 与大小。
/// **只读、不阻塞**：任何异常都转化为结果里的 `mismatches` / `missing`，不返回 Err
/// （恢复前不希望因为校验失败而挡住用户 —— 由 UI 决定是否继续）。
fn verify_backup_src(src: &std::path::Path) -> BackupVerify {
    let mut v = BackupVerify::default();
    let text = match std::fs::read_to_string(src.join("snapshot.json")) {
        Ok(t) => t,
        Err(_) => {
            v.missing.push("snapshot.json".to_string());
            return v;
        }
    };
    let manifest = match serde_json::from_str::<SnapshotManifest>(&text) {
        Ok(m) => m,
        Err(_) => {
            v.mismatches.push("snapshot.json（无法解析）".to_string());
            return v;
        }
    };
    for f in &manifest.files {
        let p = src.join(f.name.replace('\\', "/"));
        match sha256_file(&p) {
            Ok((hex, len)) => {
                v.checked += 1;
                if hex != f.sha256 || len != f.size {
                    v.mismatches.push(f.name.clone());
                }
            }
            Err(_) => v.missing.push(f.name.clone()),
        }
    }
    v
}

/// 打开备份目录里的 `data.db`（只读）并读出全部监控目录 + 每个目录的追踪文件数。
fn read_backup_dirs(db_path: &std::path::Path) -> Result<Vec<BackupDirInfo>, String> {
    if !db_path.is_file() {
        return Err(format!("备份缺少 data.db: {db_path:?}"));
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("读取备份数据库失败: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT id, path, alias FROM dir_config ORDER BY path")
        .map_err(|e| format!("读取备份目录失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| format!("读取备份目录失败: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, alias) = row.map_err(|e| format!("读取备份目录失败: {e}"))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_tracking WHERE dir_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        out.push(BackupDirInfo {
            id,
            exists_locally: std::path::Path::new(&path).is_dir(),
            original_path: path,
            alias,
            file_count: count.max(0) as u64,
        });
    }
    Ok(out)
}

/// 计算某个目录映射到 `new_path` 时的命中率（每个追踪记录一次 stat，**不读内容**）。
fn compute_remap_preview(
    conn: &Connection,
    dir_id: &str,
    new_path: &std::path::Path,
) -> Result<RemapPreview, String> {
    const SAMPLE: usize = 10;
    let mut stmt = conn
        .prepare("SELECT path, size FROM file_tracking WHERE dir_id=?1")
        .map_err(|e| format!("读取追踪记录失败: {e}"))?;
    let rows = stmt
        .query_map([dir_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| format!("读取追踪记录失败: {e}"))?;
    let mut p = RemapPreview::default();
    for row in rows {
        let (rel, size) = row.map_err(|e| format!("读取追踪记录失败: {e}"))?;
        p.tracked += 1;
        match std::fs::metadata(new_path.join(&rel)) {
            Ok(m) if m.is_file() => {
                p.found += 1;
                if m.len() == size.max(0) as u64 {
                    p.size_match += 1;
                }
            }
            _ => {
                if p.sample_missing.len() < SAMPLE {
                    p.sample_missing.push(rel);
                }
            }
        }
    }
    Ok(p)
}

/// 路径归一化 key：统一分隔符、去尾部分隔符、盘符/整串小写——
/// 仅用于「是否指向同一个目录」的判重，不写回数据库。
fn path_key(p: &str) -> String {
    let norm = crate::scanner::helpers::normalize_os_path(p);
    let mut s = norm.trim_end_matches('/').to_string();
    if s.is_empty() {
        s = "/".to_string();
    }
    s.to_lowercase()
}

/// 增量备份链：`{data_dir}/backups/.chain.json`，记录全部快照 + 已存储的
/// 不可变 segment 文件集合（segment_store，链内去重，硬链接共享一份物理副本）。
#[derive(Serialize, Deserialize, Default)]
pub struct ChainHead {
    /// 首个全量快照 id；被裁剪后可为空串（下次全量快照恢复）
    pub baseline_id: String,
    pub snapshots: Vec<ChainSnapshot>,
    pub segment_store: Vec<String>,
}

fn chain_path(backup_dir: &std::path::Path) -> std::path::PathBuf {
    backup_dir.join(".chain.json")
}

/// 解析 `backups/` 下的快照目录名，**拒绝一切路径成分**。
///
/// `backup_name` 直接来自 IPC 与 Web API 请求体（`POST /api/backup/export`），
/// 若原样 `join` 进路径，`../../..` 就能把任意目录（`.ssh`、浏览器 profile…）
/// 当作"备份"打包外泄。快照 id 由本模块生成，形态固定为 `backup_<ts>`，
/// 因此白名单（`[A-Za-z0-9_-]`）足够、且不依赖 `.chain.json` 是否完好。
fn resolve_backup_dir(backup_dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    let legal = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !legal {
        return Err(format!("非法的备份名: {name}"));
    }
    let dir = backup_dir.join(name);
    if !dir.is_dir() {
        return Err(format!("备份不存在: {name}"));
    }
    Ok(dir)
}

/// 读取链文件；缺失或损坏 → 返回空链。
fn load_chain(backup_dir: &std::path::Path) -> Result<ChainHead, String> {
    match std::fs::read_to_string(chain_path(backup_dir)) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(chain) => Ok(chain),
            Err(e) => {
                log::warn!("链文件损坏，已重置为全新备份链: {e}");
                Ok(ChainHead::default())
            }
        },
        Err(_) => Ok(ChainHead::default()),
    }
}

/// 原子写链文件：先写 `.chain.json.tmp` 再 rename 覆盖，避免写一半损坏。
fn save_chain(backup_dir: &std::path::Path, chain: &ChainHead) -> Result<(), String> {
    let path = chain_path(backup_dir);
    let tmp = backup_dir.join(".chain.json.tmp");
    let json = serde_json::to_string_pretty(chain).map_err(|e| format!("failed to serialize chain: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("failed to write chain tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("failed to write chain file: {e}"))
}

/// 把 `.ls-index` / `data.db` / `config.json` / `chat_history.json` 写入 `dest`，
/// 返回清单。data.db 用 SQLite 在线备份 API（WAL 安全），Busy/Locked 重试 3 次。
fn snapshot_core(state: &AppState, dest: &std::path::Path) -> Result<SnapshotManifest, String> {
    let mut files: Vec<SnapshotFile> = Vec::new();
    let mut total_size: u64 = 0;

    // Backup Tantivy index
    let index_dest = dest.join(INDEX_DIR_NAME);
    copy_dir(&state.index_dir, &index_dest)?;
    for f in collect_files(&index_dest)? {
        let name = f
            .strip_prefix(&index_dest)
            .map_err(|e| format!("path strip error: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        if name.ends_with("-wal") || name.ends_with("-shm") {
            continue;
        }
        let meta = std::fs::metadata(&f).map_err(|e| format!("failed to stat {f:?}: {e}"))?;
        total_size += meta.len();
        files.push(SnapshotFile {
            name,
            size: meta.len(),
            sha256: file_sha256(&f)?,
        });
    }

    // Backup SQLite database（在线备份 API，保证 WAL 一致性，不直接 fs::copy 活跃 DB）
    let db_dest = dest.join("data.db");
    let src_conn = Connection::open(&state.db_path)
        .map_err(|e| format!("failed to open source db: {e}"))?;
    let mut dst_conn = Connection::open(&db_dest)
        .map_err(|e| format!("failed to open backup db: {e}"))?;
    let backup = Backup::new(&src_conn, &mut dst_conn)
        .map_err(|e| format!("failed to init backup: {e}"))?;
    let mut r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    let mut busy = 0;
    while r == StepResult::Busy || r == StepResult::Locked {
        busy += 1;
        if busy >= 3 {
            return Err("数据库繁忙，备份未完成，请重试".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
        r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    }
    if r != StepResult::Done {
        return Err("数据库繁忙，备份未完成，请重试".to_string());
    }
    drop(backup);
    drop(src_conn);
    drop(dst_conn);
    let db_meta = std::fs::metadata(&db_dest).map_err(|e| format!("failed to stat {db_dest:?}: {e}"))?;
    total_size += db_meta.len();
    files.push(SnapshotFile {
        name: "data.db".to_string(),
        size: db_meta.len(),
        sha256: file_sha256(&db_dest)?,
    });

    // config.json 位于 config_dir（LS_CONFIG_DIR / ~/.config/.link-searcher），非 data_dir
    copy_file_if_exists(
        &config_file_path(),
        &dest.join("config.json"),
        "config.json",
        &mut files,
        &mut total_size,
    )?;

    // chat_history.json 位于 data_dir
    copy_file_if_exists(
        &state.data_dir.join("chat_history.json"),
        &dest.join("chat_history.json"),
        "chat_history.json",
        &mut files,
        &mut total_size,
    )?;

    Ok(SnapshotManifest { files, size: total_size })
}

/// 增量快照：直接读 live 索引目录，不可变 segment 硬链接到链中已存副本
/// （无则复制并登记 segment_store）；meta.json/.managed.json 原子替换、每次复制；
/// 再走与 snapshot_core 相同的在线备份 API 复制 data.db + config.json + chat_history.json。
fn snapshot_incremental(
    state: &AppState,
    dest: &std::path::Path,
    chain: &mut ChainHead,
) -> Result<SnapshotManifest, String> {
    let mut files: Vec<SnapshotFile> = Vec::new();
    let mut total_size: u64 = 0;

    // 遍历 LIVE 索引目录（不复制整棵树——硬链接必须指向 live 的不可变 segment）
    for f in collect_files(&state.index_dir)? {
        let rel = f
            .strip_prefix(&state.index_dir)
            .map_err(|e| format!("path strip error: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        if rel.ends_with("-wal")
            || rel.ends_with("-shm")
            || name == ".tantivy-writer.lock"
            || name == ".tantivy-meta.lock"
        {
            continue;
        }
        let dest_path = dest.join(INDEX_DIR_NAME).join(&rel);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {parent:?}: {e}"))?;
        }
        if name == "meta.json" || name == ".managed.json" {
            // 原子 tmp+rename 写入的元数据：不能硬链接，每次复制新副本
            std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
        } else if chain.segment_store.iter().any(|s| s == &rel) {
            // 链中已有该 segment：硬链接（跨设备等失败时回退复制）
            if std::fs::hard_link(&f, &dest_path).is_err() {
                std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
            }
        } else {
            std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
            chain.segment_store.push(rel.clone());
        }
        let meta = std::fs::metadata(&dest_path).map_err(|e| format!("failed to stat {dest_path:?}: {e}"))?;
        total_size += meta.len();
        files.push(SnapshotFile {
            name: rel,
            size: meta.len(),
            sha256: file_sha256(&dest_path)?,
        });
    }

    // SQLite 在线备份 API（WAL 安全），Busy/Locked 重试 3 次——与 snapshot_core 相同
    let db_dest = dest.join("data.db");
    let src_conn = Connection::open(&state.db_path)
        .map_err(|e| format!("failed to open source db: {e}"))?;
    let mut dst_conn = Connection::open(&db_dest)
        .map_err(|e| format!("failed to open backup db: {e}"))?;
    let backup = Backup::new(&src_conn, &mut dst_conn)
        .map_err(|e| format!("failed to init backup: {e}"))?;
    let mut r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    let mut busy = 0;
    while r == StepResult::Busy || r == StepResult::Locked {
        busy += 1;
        if busy >= 3 {
            return Err("数据库繁忙，备份未完成，请重试".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
        r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    }
    if r != StepResult::Done {
        return Err("数据库繁忙，备份未完成，请重试".to_string());
    }
    drop(backup);
    drop(src_conn);
    drop(dst_conn);
    let db_meta = std::fs::metadata(&db_dest).map_err(|e| format!("failed to stat {db_dest:?}: {e}"))?;
    total_size += db_meta.len();
    files.push(SnapshotFile {
        name: "data.db".to_string(),
        size: db_meta.len(),
        sha256: file_sha256(&db_dest)?,
    });

    // config.json 位于 config_dir（LS_CONFIG_DIR / ~/.config/.link-searcher），非 data_dir
    copy_file_if_exists(
        &config_file_path(),
        &dest.join("config.json"),
        "config.json",
        &mut files,
        &mut total_size,
    )?;

    // chat_history.json 位于 data_dir
    copy_file_if_exists(
        &state.data_dir.join("chat_history.json"),
        &dest.join("chat_history.json"),
        "chat_history.json",
        &mut files,
        &mut total_size,
    )?;

    Ok(SnapshotManifest { files, size: total_size })
}

const MERGE_THRESHOLD: usize = 10;

fn merge_chain(backup_dir: &std::path::Path, chain: &mut ChainHead) -> Result<(), String> {
    if chain.snapshots.len() < MERGE_THRESHOLD {
        return Ok(());
    }
    let to_merge = chain.snapshots.len() - 1;

    let merged_id = format!(
        "merged_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("time error: {e}"))?
            .as_secs()
    );
    let merged_dir = backup_dir.join(&merged_id);
    std::fs::create_dir_all(&merged_dir).map_err(|e| format!("failed to create merged dir: {e}"))?;

    let mut latest_non_segs: std::collections::HashMap<String, SnapshotFile> = std::collections::HashMap::new();
    let mut seen_segs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged_files: Vec<SnapshotFile> = Vec::new();
    let mut merged_size: u64 = 0;

    for snap in &chain.snapshots[..to_merge] {
        let snap_dir = backup_dir.join(&snap.id);
        for file in &snap.files {
            let is_seg = chain.segment_store.iter().any(|s| s == &file.name);
            if is_seg && !seen_segs.contains(&file.name) {
                let src = snap_dir.join(INDEX_DIR_NAME).join(&file.name);
                let dst = merged_dir.join(INDEX_DIR_NAME).join(&file.name);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("failed to create {parent:?}: {e}"))?;
                }
                if std::fs::hard_link(&src, &dst).is_err() {
                    std::fs::copy(&src, &dst).map_err(|e| format!("failed to copy {src:?}: {e}"))?;
                }
                seen_segs.insert(file.name.clone());
                merged_files.push(file.clone());
                merged_size += file.size;
            } else if !is_seg {
                latest_non_segs.insert(file.name.clone(), file.clone());
            }
        }
    }

    for (name, file) in &latest_non_segs {
        for snap in chain.snapshots[..to_merge].iter().rev() {
            if snap.files.iter().any(|f| &f.name == name) {
                let snap_dir = backup_dir.join(&snap.id);
                let src = snap_dir.join(name);
                if src.is_file() {
                    let dst = merged_dir.join(name);
                    std::fs::copy(&src, &dst).map_err(|e| format!("failed to copy {src:?}: {e}"))?;
                    merged_files.push(file.clone());
                    merged_size += file.size;
                }
                break;
            }
        }
    }

    let manifest = SnapshotManifest { files: merged_files.clone(), size: merged_size };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;
    std::fs::write(merged_dir.join("snapshot.json"), manifest_json)
        .map_err(|e| format!("failed to write snapshot.json: {e}"))?;

    let removed_ids: Vec<String> = chain.snapshots.drain(..to_merge).map(|s| s.id).collect();
    for id in &removed_ids {
        let dir = backup_dir.join(id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    chain.snapshots.insert(0, ChainSnapshot {
        id: merged_id.clone(),
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("time error: {e}"))?
            .as_secs() as i64,
        kind: "merged".to_string(),
        files: merged_files,
    });
    chain.baseline_id = merged_id;

    Ok(())
}

fn prune_orphan_dirs(backup_dir: &std::path::Path, chain: &ChainHead) {
    let valid_ids: std::collections::HashSet<&str> =
        chain.snapshots.iter().map(|s| s.id.as_str()).collect();
    if let Ok(entries) = std::fs::read_dir(backup_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if (name.starts_with("backup_") || name.starts_with("merged_"))
                    && !valid_ids.contains(name.as_str())
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

#[tauri::command]
pub async fn trigger_backup(state: State<'_, AppState>) -> Result<(), String> {
    let backup_dir = state.data_dir.join("backups");
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("failed to create backup dir: {e}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("time error: {e}"))?
        .as_secs();
    let backup_name = format!("backup_{timestamp}");
    let dest = backup_dir.join(&backup_name);

    std::fs::create_dir_all(&dest).map_err(|e| format!("failed to create backup dir: {e}"))?;

    let mut chain = load_chain(&backup_dir)?;
    let is_baseline = chain.snapshots.is_empty();
    let manifest = snapshot_incremental(&state, &dest, &mut chain)?;
    if is_baseline {
        chain.baseline_id = backup_name.clone();
    }
    chain.snapshots.push(ChainSnapshot {
        id: backup_name.clone(),
        ts: timestamp as i64,
        kind: if is_baseline { "baseline" } else { "incremental" }.to_string(),
        files: manifest.files.clone(),
    });

    merge_chain(&backup_dir, &mut chain)?;
    save_chain(&backup_dir, &chain)?;
    prune_orphan_dirs(&backup_dir, &chain);

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;
    std::fs::write(dest.join("snapshot.json"), manifest_json)
        .map_err(|e| format!("failed to write snapshot.json: {e}"))?;

    log::info!("backup completed: {backup_name}");
    Ok(())
}

#[tauri::command]
pub async fn get_backup_status(state: State<'_, AppState>) -> Result<BackupInfo, String> {
    let backup_dir = state.data_dir.join("backups");
    let chain = load_chain(&backup_dir)?;
    if chain.snapshots.is_empty() {
        return Ok(BackupInfo {
            last_backup: None,
            backup_size: 0,
            backup_count: 0,
        });
    }

    let last_backup = chain.snapshots.last().map(|s| s.ts);
    let count = chain.snapshots.len() as u64;

    // 物理占用：每个快照中“复制的新文件”（非 segment 的 db/config/chat/meta/.managed 等小文件）
    // 各占一份；segment_store 里每个 segment 整链只占一份（其余快照硬链接共享）
    let fresh_bytes: u64 = chain
        .snapshots
        .iter()
        .flat_map(|s| s.files.iter())
        .filter(|f| !chain.segment_store.iter().any(|n| n == &f.name))
        .map(|f| f.size)
        .sum();
    let segment_bytes: u64 = chain
        .segment_store
        .iter()
        .map(|seg| {
            chain
                .snapshots
                .iter()
                .flat_map(|s| s.files.iter())
                .find(|f| &f.name == seg)
                .map(|f| f.size)
                .unwrap_or(0)
        })
        .sum();

    Ok(BackupInfo {
        last_backup,
        backup_size: fresh_bytes + segment_bytes,
        backup_count: count,
    })
}

#[tauri::command]
pub async fn list_backups(state: State<'_, AppState>) -> Result<Vec<BackupSnapshot>, String> {
    let backup_dir = state.data_dir.join("backups");
    let chain = load_chain(&backup_dir)?;
    let snapshots = chain
        .snapshots
        .iter()
        .map(|s| {
            let size: u64 = s.files.iter().map(|f| f.size).sum();
            BackupSnapshot {
                id: s.id.clone(),
                ts: s.ts,
                kind: s.kind.clone(),
                size,
            }
        })
        .collect();
    Ok(snapshots)
}

#[tauri::command]
pub async fn export_backup(
    state: State<'_, AppState>,
    dest_path: String,
    password: Option<String>,
    backup_name: Option<String>,
) -> Result<BackupExportResult, String> {
    if dest_path.trim().is_empty() {
        return Err("导出路径不能为空".to_string());
    }

    let config = load_config();
    let has_secrets = has_config_secrets(&config);

    let (source_dir, cleanup_tmp) = match &backup_name {
        Some(name) if !name.is_empty() => {
            (resolve_backup_dir(&state.data_dir.join("backups"), name)?, None::<std::path::PathBuf>)
        }
        _ => {
            let tmp = std::env::temp_dir().join(format!("ls_export_{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&tmp).map_err(|e| format!("failed to create temp dir: {e}"))?;
            let _snapshot = snapshot_core(&state, &tmp)?;
            let chain_path = state.data_dir.join("backups").join(".chain.json");
            if chain_path.is_file() {
                std::fs::copy(&chain_path, tmp.join(".chain.json"))
                    .map_err(|e| format!("failed to copy chain: {e}"))?;
            }
            (tmp.clone(), Some(tmp))
        }
    };

    zip_dir(&source_dir, &dest_path, password.as_deref())?;
    if let Some(t) = cleanup_tmp {
        let _ = std::fs::remove_dir_all(&t);
    }

    let label = backup_name.as_deref().unwrap_or("live");
    log::info!("backup exported ({label}) to {dest_path}, has_secrets={has_secrets}");
    Ok(BackupExportResult { has_secrets, dest_path: dest_path.clone() })
}

fn zip_dir(source: &std::path::Path, dest: &str, password: Option<&str>) -> Result<(), String> {
    let file = std::fs::File::create(dest)
        .map_err(|e| format!("failed to create zip: {e}"))?;
    let mut zip_writer = zip::ZipWriter::new(file);

    for entry in collect_files(source)? {
        let rel = entry
            .strip_prefix(source)
            .map_err(|e| format!("path strip error: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let data = std::fs::read(&entry)
            .map_err(|e| format!("failed to read {entry:?}: {e}"))?;
        let mut options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Deflated);
        if let Some(pw) = password
            && !pw.is_empty() {
                options = options.with_aes_encryption(zip::AesMode::Aes256, pw);
            }
        zip_writer.start_file(&rel, options)
            .map_err(|e| format!("zip start_file failed: {e}"))?;
        use std::io::Write;
        zip_writer.write_all(&data)
            .map_err(|e| format!("zip write failed: {e}"))?;
    }

    zip_writer.finish().map_err(|e| format!("zip finish failed: {e}"))?;
    Ok(())
}

fn has_config_secrets(config: &AppConfig) -> bool {
    if !config.ai_api_key.is_empty()
        || !config.embedding_api_key.is_empty()
        || !config.llm_api_key.is_empty()
    {
        return true;
    }
    for p in &config.providers {
        if !p.api_key.is_empty() {
            return true;
        }
    }
    false
}

/// 交换失败时把内存中的索引实例重新装回（从磁盘当前目录重开）。
fn reinstall_index(
    index_dir: &std::path::Path,
    index_manager: &std::sync::Arc<std::sync::RwLock<IndexManager>>,
) {
    match IndexManager::open_or_create(index_dir) {
        Ok(mgr) => {
            let mut g = index_manager.write().unwrap_or_else(|e| e.into_inner());
            *g = mgr;
        }
        Err(e) => log::error!("[RESTORE] 回滚索引实例失败（重启后恢复）: {e}"),
    }
}

/// 备份 zip 的解压前预检（纯函数，便于单测）。
///
/// ① 条目名只能是**纯路径组件**（拒绝 `..` / 绝对路径 / 盘符前缀 / 反斜杠分隔，
///    即 Zip Slip）；② 必须含 `snapshot.json`（合法备份包）；③ 条目数上限。
///
/// 必须**解压之前**跑：否则畸形包也能先把文件写到临时目录（再被拒绝），
/// 按路径穿越条目就可能落在临时目录之外。
fn validate_backup_zip(names: &[String]) -> Result<(), String> {
    const MAX_ENTRIES: usize = 200_000;
    if names.len() > MAX_ENTRIES {
        return Err(format!("备份包条目过多（{}），已拒绝解压", names.len()));
    }
    for name in names {
        // 反斜杠单独拒绝：本应用导出的 zip 一律用 `/`（见 zip_dir），而
        // `..\x` 在 Windows 上是穿越、在 POSIX 上只是普通字符 —— 统一拒绝才跨平台一致。
        let legal = !name.is_empty()
            && !name.contains('\\')
            && std::path::Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !legal {
            return Err(format!("备份包包含非法条目路径，已拒绝解压: {name}"));
        }
    }
    if !names.iter().any(|n| n == "snapshot.json") {
        return Err("无效的备份文件：缺少 snapshot.json".to_string());
    }
    Ok(())
}

fn restore_from_dir(
    src: &std::path::Path,
    index_dir: &std::path::Path,
    index_manager: &std::sync::Arc<std::sync::RwLock<IndexManager>>,
    indexer: &crate::indexer::IndexerService,
    db_pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    mappings: &[DirMapping],
) -> Result<(), String> {
    let index_src = src.join(INDEX_DIR_NAME);
    if index_src.is_dir() {
        let tmp_name = format!("index.restore-{}", uuid::Uuid::new_v4().simple());
        let tmp_dir = index_dir.with_file_name(&tmp_name);
        copy_dir(&index_src, &tmp_dir)?;

        // 先验证拷出来的索引能打开；失败则磁盘/内存都还没动过。
        if let Err(e) = IndexManager::open_or_create(&tmp_dir) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("无法打开恢复的索引: {e}"));
        }

        // 交换前必须先释放所有指向旧目录/tmp 的句柄（Windows 上 rename 会被拒，
        // 见 rebuild_index 同款处理）：丢 writer，用内存占位 IndexManager 顶掉
        // 旧实例（其 reader 持有旧目录内的句柄），交换完成后重开。
        indexer.reset_writer();
        {
            let mut mgr = index_manager.write().unwrap_or_else(|e| e.into_inner());
            *mgr = IndexManager::create_in_ram();
        }

        let old = index_dir.with_file_name("index.old");
        if index_dir.exists() {
            let _ = std::fs::remove_dir_all(&old);
            if let Err(e) = std::fs::rename(index_dir, &old) {
                // 磁盘保持原样；把内存里的旧索引装回去（此前这里静默继续，
                // 会让"磁盘换了、内存没换"或反之，界面与搜索结果互相打架）。
                reinstall_index(index_dir, &index_manager);
                return Err(format!("暂存旧索引目录失败: {e}"));
            }
        }
        if let Err(e) = std::fs::rename(&tmp_dir, index_dir) {
            if old.exists() {
                let _ = std::fs::rename(&old, index_dir);
            }
            let _ = std::fs::remove_dir_all(&tmp_dir);
            reinstall_index(index_dir, &index_manager);
            return Err(format!("切换索引目录失败: {e}"));
        }
        // 在**最终路径**上重新打开（不能沿用 tmp_dir 上打开的实例：其内部路径
        // 在 rename 后失效 —— 旧实现靠随后的 app.restart() 兜底）。
        match IndexManager::open_or_create(index_dir) {
            Ok(new_mgr) => {
                let mut mgr = index_manager.write().unwrap_or_else(|e| e.into_inner());
                *mgr = new_mgr;
            }
            Err(e) => log::error!("[RESTORE] 切换后无法打开索引目录（重启后恢复）: {e}"),
        }
        indexer.reset_writer();
        let _ = std::fs::remove_dir_all(&old);
    }

    let db_src = src.join("data.db");
    if db_src.is_file() {
        let staging = std::env::temp_dir().join(format!(
            "ls-restore-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::copy(&db_src, &staging).map_err(|e| format!("复制备份数据库失败: {e}"))?;

        // R1：把用户指定的「重映射」先写进 staging 副本，再灌回 live —— 这样重启后
        // `startup_scan` 用的就是新路径，不会再拿备份里的旧路径去扫。
        // 只处理 remap（keep 不动、remove 在 apply_restore 里于恢复后清理）。
        if !mappings.is_empty() {
            let conn = Connection::open(&staging)
                .map_err(|e| format!("打开备份数据库失败: {e}"))?;
            let now = chrono::Utc::now().timestamp();
            for m in mappings {
                if m.action == "remap"
                    && let Some(np) = m.new_path.as_deref()
                    && !np.trim().is_empty()
                {
                    let normalized = crate::scanner::helpers::normalize_os_path(np);
                    conn.execute(
                        "UPDATE dir_config SET path=?1, updated_at=?2 WHERE id=?3",
                        rusqlite::params![normalized, now, m.dir_id],
                    )
                    .map_err(|e| format!("改写备份目录 {np:?} 失败: {e}"))?;
                } else if m.action == "disable" {
                    // 停用：保留 path 与数据，只关掉扫描/监控开关。
                    conn.execute(
                        "UPDATE dir_config SET monitor_enabled=0, updated_at=?1 WHERE id=?2",
                        rusqlite::params![now, m.dir_id],
                    )
                    .map_err(|e| format!("停用备份目录失败: {e}"))?;
                }
            }
            drop(conn);
        }

        let restore_result = (|| -> Result<(), String> {
            let src_conn = Connection::open(&staging)
                .map_err(|e| format!("打开备份数据库失败: {e}"))?;
            let mut dst = db_pool
                .get()
                .map_err(|e| format!("获取数据库连接失败: {e}"))?;
            let backup = Backup::new(&src_conn, &mut dst)
                .map_err(|e| format!("初始化恢复失败: {e}"))?;
            let mut r = backup.step(-1).map_err(|e| format!("恢复数据库失败: {e}"))?;
            let mut busy = 0;
            while r == StepResult::Busy || r == StepResult::Locked {
                busy += 1;
                if busy >= 3 {
                    return Err("数据库繁忙，恢复未完成，请重试".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
                r = backup.step(-1).map_err(|e| format!("恢复数据库失败: {e}"))?;
            }
            if r != StepResult::Done {
                return Err("数据库繁忙，恢复未完成，请重试".to_string());
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&staging);
        restore_result?;
    }

    Ok(())
}

/// 解压备份 zip 到 `tmp`：先做条目预检（Zip Slip / 必须有 snapshot.json / 条目数上限），
/// 再逐条目解压并施加解压量双上限（zip bomb 防护）。**不碰 live 数据**——失败由调用方
/// 清理 `tmp`。`plan_restore` 与 `restore_from_zip` 共用此函数，避免两处安全逻辑分叉。
fn extract_backup_zip(
    zip_path: &str,
    password: Option<&str>,
    tmp: &std::path::Path,
) -> Result<(), String> {
    std::fs::create_dir_all(tmp).map_err(|e| format!("failed to create temp dir: {e}"))?;

    let file = std::fs::File::open(zip_path).map_err(|e| format!("failed to open zip: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("failed to read zip: {e}"))?;

    // ── 预检：先只读清单（不解压），任何一项不过都在**落盘之前**失败 ──
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();
    validate_backup_zip(&names)?;

    // 解压量双上限（zip bomb 防护）：条目数已有上限（见 `validate_backup_zip`），
    // 但单个条目仍可声称几 GB —— 而下面的 `read_to_end` 是**整段读进内存**再落盘，
    // 一个几 KB 的恶意包足以把进程撑爆或写满磁盘。压缩比再离谱也越不过这两道。
    const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB / 条目
    const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024 * 1024; // 200 GiB / 整包
    let mut total_uncompressed: u64 = 0;

    for i in 0..archive.len() {
        let pw = password.unwrap_or("");
        let mut entry = if pw.is_empty() {
            archive.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?
        } else {
            archive
                .by_index_decrypt(i, pw.as_bytes())
                .map_err(|e| format!("zip entry {i} decrypt: {e}"))?
        };
        // `enclosed_name`：库官方的安全路径 API，会拒绝 RootDir/Prefix，
        // 并保证 `..` 不逃出基准目录（预检已挡一次，这里是第二道防线）。
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("备份包包含非法条目路径，已拒绝解压: {}", entry.name()));
        };
        let out = tmp.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| format!("failed to create dir {out:?}: {e}"))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create parent {parent:?}: {e}"))?;
            }
            // 第一道：按条目**声明**的解压后大小拦（还没读任何字节）。
            let declared = entry.size();
            if declared > MAX_ENTRY_BYTES
                || total_uncompressed.saturating_add(declared) > MAX_TOTAL_BYTES
            {
                return Err(format!(
                    "备份包解压量异常（条目 {declared} 字节，累计 {}），已拒绝解压（疑似 zip bomb）",
                    total_uncompressed.saturating_add(declared)
                ));
            }
            total_uncompressed = total_uncompressed.saturating_add(declared);
            // 第二道：声明量不可信 —— 实际读入时再用 `take` 兜一次上限。
            use std::io::Read;
            let mut data = Vec::new();
            (&mut entry)
                .take(MAX_ENTRY_BYTES.saturating_add(1))
                .read_to_end(&mut data)
                .map_err(|e| format!("failed to read {:?}: {e}", rel))?;
            if data.len() as u64 > MAX_ENTRY_BYTES {
                return Err("备份包条目过大，已拒绝解压（疑似 zip bomb）".to_string());
            }
            std::fs::write(&out, &data).map_err(|e| format!("failed to write {out:?}: {e}"))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn restore_from_zip(
    state: State<'_, AppState>,
    app: AppHandle,
    zip_path: String,
    password: Option<String>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    if state
        .is_restoring
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("正在恢复中，请稍候".to_string());
    }

    let tmp = std::env::temp_dir().join(format!("ls_restore_zip_{}", uuid::Uuid::new_v4().simple()));
    let index_dir = state.index_dir.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    let is_restoring = state.is_restoring.clone();
    let zip_path_in = zip_path.clone();

    let result = match tokio::task::spawn_blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(&tmp).map_err(|e| format!("failed to create temp dir: {e}"))?;
        if let Err(e) = extract_backup_zip(&zip_path_in, password.as_deref(), &tmp) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        // 快捷恢复：不做目录映射（用户指定的映射走 plan/apply）。
        let r = restore_from_dir(&tmp, &index_dir, &index_manager, &indexer, &db_pool, &[]);
        let _ = std::fs::remove_dir_all(&tmp);
        r
    })
    .await
    {
        Ok(r) => r,
        Err(e) => Err(format!("恢复任务异常: {e}")),
    };
    // 无论成败都复位：此前 `result?` 在复位之前返回，一次失败（例如
    // "无效的备份文件"）就让 `is_restoring` 永远为 true —— 恢复功能从此被
    // "正在恢复中，请稍候"永久拒绝，直到重启应用。
    is_restoring.store(false, Ordering::SeqCst);
    result?;

    log::info!("restored from zip: {zip_path}");

    let _ = app.emit("restore-completed", serde_json::json!({ "name": zip_path }));
    app.restart()
}

#[tauri::command]
pub async fn get_dead_dirs(state: State<'_, AppState>) -> Result<Vec<DeadDirInfo>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| format!("db error: {e}"))?;
    let mut result = Vec::new();
    for d in dirs {
        if !std::path::Path::new(&d.path).is_dir() {
            let count = crate::db::tracker::count_files_by_dir(&conn, &d.id)
                .map_err(|e| format!("db error: {e}"))?;
            result.push(DeadDirInfo {
                id: d.id,
                path: d.path,
                file_count: count,
            });
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn remap_dir(state: State<'_, AppState>, dir_id: String, new_path: String) -> Result<(), String> {
    if dir_id.trim().is_empty() || new_path.trim().is_empty() {
        return Err("dir_id 和 new_path 不能为空".to_string());
    }
    let new_path = std::path::PathBuf::from(&new_path);
    if !new_path.is_dir() {
        return Err(format!("目标路径不存在或不是目录: {new_path:?}"));
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    conn.execute(
        "UPDATE dir_config SET path=?1, updated_at=?2 WHERE id=?3",
        rusqlite::params![new_path.to_str().unwrap_or(""), chrono::Utc::now().timestamp(), dir_id],
    )
    .map_err(|e| format!("db error: {e}"))?;
    log::info!("remapped dir {dir_id} to {new_path:?}");
    Ok(())
}

#[tauri::command]
pub async fn remove_dir_with_files(state: State<'_, AppState>, dir_id: String) -> Result<(), String> {
    if dir_id.trim().is_empty() {
        return Err("dir_id 不能为空".to_string());
    }
    // 与「删除资料库」共用同一套彻底清理：Tantivy 文档 + file_tracking + 逐文件
    // 向量/摘要/错误 + md5 键的孤儿内容/块向量。以前这里只删 file_tracking，
    // 会留下全文索引里的"幽灵"命中。
    let indexer = state.indexer.clone();
    let dir_for_purge = dir_id.clone();
    tokio::task::spawn_blocking(move || indexer.purge_dir_data(&dir_for_purge, true))
        .await
        .map_err(|e| format!("purge task failed: {e}"))?
        .map_err(|e| format!("db error: {e}"))?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    crate::db::dir_config::remove_dir(&conn, &dir_id)
        .map_err(|e| format!("db error: {e}"))?;
    drop(conn);

    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
        dir_id: dir_id.clone(),
    });
    log::info!("removed dir {dir_id} and all associated data");
    Ok(())
}

#[tauri::command]
pub async fn restore_backup(
    state: State<'_, AppState>,
    app: AppHandle,
    backup_name: String,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    // 名字净化 + 存在性检查放在**占用恢复标志之前**：这条路径失败不需要复位标志。
    let backup_dir = resolve_backup_dir(&state.data_dir.join("backups"), &backup_name)?;

    if state
        .is_restoring
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("正在恢复中，请稍候".to_string());
    }

    let index_dir = state.index_dir.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    let is_restoring = state.is_restoring.clone();

    let result = match tokio::task::spawn_blocking(move || -> Result<(), String> {
        restore_from_dir(&backup_dir, &index_dir, &index_manager, &indexer, &db_pool, &[])
    })
    .await
    {
        Ok(r) => r,
        Err(e) => Err(format!("恢复任务异常: {e}")),
    };
    // 失败也必须复位（同 restore_from_zip）：否则一次失败永久拒绝后续恢复。
    is_restoring.store(false, Ordering::SeqCst);
    result?;

    log::info!("restored backup: {backup_name}");

    let _ = app.emit("restore-completed", serde_json::json!({ "name": backup_name }));
    app.restart()
}

/// 校验恢复映射：必须覆盖备份里的**每一个**目录；remap 目标必须存在且互不重复；
/// 嵌套根视为风险，需要 `acknowledge_risks`。
///
/// 注：「内容不符」（命中率过低）由前端预检提示、用户勾选后置 `acknowledge_risks=true`，
/// 后端不重复做全量 stat（成本按记录数增长，且前端已展示）。
fn validate_mappings(
    backup_dirs: &[BackupDirInfo],
    mappings: &[DirMapping],
    acknowledge_risks: bool,
) -> Result<(), String> {
    let known: std::collections::HashSet<&str> =
        backup_dirs.iter().map(|d| d.id.as_str()).collect();

    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for m in mappings {
        if !known.contains(m.dir_id.as_str()) {
            return Err(format!("映射包含备份中不存在的目录: {}", m.dir_id));
        }
        if !seen.insert(m.dir_id.as_str()) {
            return Err(format!("目录出现重复映射: {}", m.dir_id));
        }
    }
    for d in backup_dirs {
        if !seen.contains(d.id.as_str()) {
            return Err(format!(
                "目录未指定处理方式（保留/重映射/移除）: {}",
                d.original_path
            ));
        }
    }

    // remap：目标存在 + 归一化判重 + 嵌套风险。
    let mut keys: Vec<(String, String)> = Vec::new();
    for m in mappings {
        if m.action != "remap" {
            continue;
        }
        let np = m.new_path.as_deref().unwrap_or("").trim().to_string();
        if np.is_empty() {
            return Err("重映射缺少目标路径".to_string());
        }
        if !std::path::Path::new(&np).is_dir() {
            return Err(format!("目标路径不存在或不是目录: {np}"));
        }
        keys.push((m.dir_id.clone(), path_key(&np)));
    }
    for i in 0..keys.len() {
        for j in (i + 1)..keys.len() {
            if keys[i].1 == keys[j].1 {
                return Err(format!(
                    "两个目录被映射到同一个本机目录（{} / {}），请改为不同目录",
                    keys[i].0, keys[j].0
                ));
            }
        }
    }
    if !acknowledge_risks {
        for i in 0..keys.len() {
            for j in 0..keys.len() {
                if i != j && keys[j].1.starts_with(&format!("{}/", keys[i].1)) {
                    return Err(format!(
                        "目录存在嵌套（{} 位于 {} 之下），会重复索引；确认无误请勾选风险提示后重试",
                        keys[j].0, keys[i].0
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 恢复前只读阶段：列出备份里的监控目录 + 暂存备份源，供用户在 `apply_restore`
/// 里逐个指定落位。**不触碰 live 数据**（zip 只解压到临时目录）。
#[tauri::command]
pub async fn plan_restore(
    state: State<'_, AppState>,
    source: String,
    path: String,
    password: Option<String>,
) -> Result<RestorePlan, String> {
    cleanup_stale_staging();

    let (src, tmp): (std::path::PathBuf, Option<std::path::PathBuf>) = match source.as_str() {
        "zip" => {
            if path.trim().is_empty() {
                return Err("zip 路径不能为空".to_string());
            }
            let tmp = std::env::temp_dir()
                .join(format!("ls_restore_plan_{}", uuid::Uuid::new_v4().simple()));
            if let Err(e) = extract_backup_zip(&path, password.as_deref(), &tmp) {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
            (tmp.clone(), Some(tmp))
        }
        "snapshot" => {
            let dir = resolve_backup_dir(&state.data_dir.join("backups"), &path)?;
            (dir, None)
        }
        other => return Err(format!("未知的备份来源类型: {other}")),
    };

    let dirs = match read_backup_dirs(&src.join("data.db")) {
        Ok(d) => d,
        Err(e) => {
            if let Some(t) = &tmp {
                let _ = std::fs::remove_dir_all(t);
            }
            return Err(e);
        }
    };

    // 完整性校验（sha256）：只读、不阻塞；结果随 plan 返回供 UI 提示。
    let verify = verify_backup_src(&src);
    if !verify.ok() {
        log::warn!(
            "[RESTORE] 备份完整性校验有异常：checked={} mismatches={} missing={}",
            verify.checked,
            verify.mismatches.len(),
            verify.missing.len()
        );
    }

    let plan = RestorePlan {
        token: uuid::Uuid::new_v4().simple().to_string(),
        dirs,
        has_chat_history: src.join("chat_history.json").is_file(),
        has_config: src.join("config.json").is_file(),
        verify,
    };

    {
        let mut reg = staging_registry().lock().unwrap_or_else(|e| e.into_inner());
        reg.insert(plan.token.clone(), Staging { src, tmp });
    }

    Ok(plan)
}

/// 校验某个已有快照的完整性（不恢复、不改动）：按 `snapshot.json` 逐文件比对 sha256。
#[tauri::command]
pub async fn verify_backup(
    state: State<'_, AppState>,
    backup_name: String,
) -> Result<BackupVerify, String> {
    let dir = resolve_backup_dir(&state.data_dir.join("backups"), &backup_name)?;
    Ok(verify_backup_src(&dir))
}

/// 只读预检：某目录改指 `new_path` 时，有多少已索引文件能在该目录下对上（同名+同大小）。
/// 成本 = 每个追踪记录一次 `stat`，**不读内容**；规模只与备份里该目录的记录数相关。
///
/// 追踪记录来自 **staging（备份）的 `data.db`**——备份里的 dir id 在 live 库里还不存在；
/// 传 `token` 为空时退化为读 live 库（供恢复后「死目录补救」使用）。
#[tauri::command]
pub async fn preview_remap(
    state: State<'_, AppState>,
    token: Option<String>,
    dir_id: String,
    new_path: String,
) -> Result<RemapPreview, String> {
    if dir_id.trim().is_empty() || new_path.trim().is_empty() {
        return Err("dir_id 和 new_path 不能为空".to_string());
    }
    let p = std::path::PathBuf::from(&new_path);
    if !p.is_dir() {
        return Err(format!("目标路径不存在或不是目录: {new_path}"));
    }

    // 取追踪记录来源：有 token → staging 的备份库（缺失即视为计划过期）；否则 live 库。
    if let Some(t) = &token {
        let src = {
            let reg = staging_registry().lock().unwrap_or_else(|e| e.into_inner());
            reg.get(t).map(|s| s.src.clone())
        };
        let Some(src) = src else {
            return Err("恢复计划不存在或已过期，请重新选择备份".to_string());
        };
        let conn = Connection::open_with_flags(
            src.join("data.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("读取备份数据库失败: {e}"))?;
        return compute_remap_preview(&conn, &dir_id, &p);
    }

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    compute_remap_preview(&conn, &dir_id, &p)
}

/// 恢复执行阶段：校验映射（覆盖全部目录、路径互不重复、remap 目标存在）→ 把 remap
/// 写进 staging 的 `data.db` → 灌回 live → 清理被 `remove` 的目录 → 重启。
///
/// 重启后 `startup_scan` 用的是用户指定的路径，不会再拿备份里的原机旧路径扫描。
#[tauri::command]
pub async fn apply_restore(
    state: State<'_, AppState>,
    app: AppHandle,
    token: String,
    mappings: Vec<DirMapping>,
    acknowledge_risks: bool,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    // 取出暂存（不在则说明 plan 未跑 / 已过期）。
    let staging = {
        let mut reg = staging_registry().lock().unwrap_or_else(|e| e.into_inner());
        reg.remove(&token)
    };
    let Some(staging) = staging else {
        return Err("恢复计划不存在或已过期，请重新选择备份".to_string());
    };

    // 从 staging 的 data.db 读出目录集合，据此校验映射。失败时 `staging` 在此 drop
    // → 自动清理临时解压目录。
    let backup_dirs = read_backup_dirs(&staging.src.join("data.db"))?;
    validate_mappings(&backup_dirs, &mappings, acknowledge_risks)?;

    if state
        .is_restoring
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("正在恢复中，请稍候".to_string());
    }

    let index_dir = state.index_dir.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    let is_restoring = state.is_restoring.clone();
    let src = staging.src.clone();
    let mappings_in = mappings.clone();
    let remove_ids: Vec<String> = mappings
        .iter()
        .filter(|m| m.action == "remove")
        .map(|m| m.dir_id.clone())
        .collect();
    let remove_count = remove_ids.len();

    let result = match tokio::task::spawn_blocking(move || -> Result<(), String> {
        // 让临时目录的清理跟着这次恢复的结束走。
        let _staging = staging;
        restore_from_dir(
            &src,
            &index_dir,
            &index_manager,
            &indexer,
            &db_pool,
            &mappings_in,
        )?;
        // 恢复完成后，清理用户选择「移除」的目录：Tantivy 文档 + 追踪行 + 内容 + dir_config 行。
        for id in &remove_ids {
            indexer
                .purge_dir_data(id, true)
                .map_err(|e| format!("清理目录 {id} 失败: {e}"))?;
            if let Ok(conn) = db_pool.get() {
                let _ = crate::db::dir_config::remove_dir(&conn, id);
            }
        }
        Ok(())
    })
    .await
    {
        Ok(r) => r,
        Err(e) => Err(format!("恢复任务异常: {e}")),
    };
    // 失败也必须复位（同 restore_backup）。
    is_restoring.store(false, Ordering::SeqCst);
    result?;

    log::info!(
        "applied restore via plan mapping ({} dirs, {} removed)",
        mappings.len(),
        remove_count
    );
    let _ = app.emit("restore-completed", serde_json::json!({ "name": "mapped" }));
    app.restart()
}

#[tauri::command]
pub async fn delete_backup(state: State<'_, AppState>, backup_name: String) -> Result<(), String> {
    if backup_name.trim().is_empty() {
        return Err("backup_name 不能为空".to_string());
    }
    let backup_dir = state.data_dir.join("backups");
    let snap_dir = backup_dir.join(&backup_name);
    let mut chain = load_chain(&backup_dir)?;
    let idx = chain.snapshots.iter().position(|s| s.id == backup_name)
        .ok_or_else(|| format!("备份不存在: {backup_name}"))?;
    chain.snapshots.remove(idx);
    if chain.baseline_id == backup_name {
        chain.baseline_id = chain.snapshots.first().map(|s| s.id.clone()).unwrap_or_default();
    }
    if snap_dir.is_dir() {
        std::fs::remove_dir_all(&snap_dir)
            .map_err(|e| format!("删除备份目录失败: {e}"))?;
    }
    save_chain(&backup_dir, &chain)?;
    prune_orphan_dirs(&backup_dir, &chain);
    log::info!("deleted backup: {backup_name}");
    Ok(())
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    copy_dir_depth(src, dst, 0)
}

/// 递归复制目录，`depth` 防止极端深目录导致栈溢出（上限 64 层）。
fn copy_dir_depth(src: &std::path::Path, dst: &std::path::Path, depth: u32) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("目录嵌套过深（>{depth} 层），已停止复制: {src:?}"));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("failed to create {dst:?}: {e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("failed to read {src:?}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry error: {e}"))?;
        let ty = entry.file_type().map_err(|e| format!("file type error: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_depth(&src_path, &dst_path, depth + 1)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("failed to copy {src_path:?}: {e}"))?;
        }
    }
    Ok(())
}

/// 递归收集目录下所有文件（不包含目录本身）。
fn collect_files(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("failed to read {d:?}: {e}"))? {
            let entry = entry.map_err(|e| format!("read entry error: {e}"))?;
            let path = entry.path();
            if entry.file_type().map_err(|e| format!("file type error: {e}"))?.is_dir() {
                dirs.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(path).map_err(|e| format!("failed to open {path:?}: {e}"))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("failed to read {path:?}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 源存在则复制并计入清单，否则跳过。
fn copy_file_if_exists(
    src: &std::path::Path,
    dst: &std::path::Path,
    name: &str,
    files: &mut Vec<SnapshotFile>,
    total_size: &mut u64,
) -> Result<(), String> {
    if !src.is_file() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| format!("failed to copy {src:?}: {e}"))?;
    let meta = std::fs::metadata(dst).map_err(|e| format!("failed to stat {dst:?}: {e}"))?;
    *total_size += meta.len();
    files.push(SnapshotFile {
        name: name.to_string(),
        size: meta.len(),
        sha256: file_sha256(dst)?,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_backup_zip_rejects_zip_slip_and_requires_manifest() {
        let mut good = vec!["snapshot.json".to_string(), ".ls-index/meta.json".to_string()];
        good.push("backup_20261018/data.db".to_string());
        assert!(validate_backup_zip(&good).is_ok());

        // Zip Slip 的各类形态（含反斜杠穿越与盘符/绝对路径）一律拒绝。
        for bad in [
            "../evil.exe",
            "..\\evil.exe",
            "a/../../evil",
            "/abs/evil",
            "C:\\evil",
            "\\\\server\\share\\evil",
            "",
        ] {
            let names = vec![bad.to_string(), "snapshot.json".to_string()];
            assert!(
                validate_backup_zip(&names).is_err(),
                "必须拒绝非法条目: {bad:?}"
            );
        }

        // 缺 snapshot.json = 不是合法备份包（此检查必须在解压之前）。
        let no_manifest = vec!["backup_1/data.db".to_string()];
        assert!(validate_backup_zip(&no_manifest).is_err());

        // 条目数上限：挡住"几十万小文件"式畸形包。
        let too_many: Vec<String> = (0..200_001).map(|i| format!("f{i}")).collect();
        assert!(validate_backup_zip(&too_many).is_err());
    }

    #[test]
    fn resolve_backup_dir_rejects_traversal_and_missing() {
        let base = std::env::temp_dir().join(format!("ls_bk_name_{}", std::process::id()));
        let snap = base.join("backup_123");
        std::fs::create_dir_all(&snap).unwrap();

        assert!(resolve_backup_dir(&base, "backup_123").is_ok());
        for bad in ["../../etc", "..", "a/b", "a\\b", "C:\\x", "", ".ssh", "backup_123/../.."] {
            assert!(
                resolve_backup_dir(&base, bad).is_err(),
                "必须拒绝非法备份名: {bad:?}"
            );
        }
        assert!(resolve_backup_dir(&base, "nope_123").is_err(), "不存在的快照必须拒绝");

        let _ = std::fs::remove_dir_all(&base);
    }

    // 镜像 restore_backup 的核心机制：从 WAL 格式的备份副本在线恢复到活跃连接，
    // 验证数据完整、无需关闭连接池（直接 fs::copy 覆盖活跃 data.db 会损坏库）。
    #[test]
    fn test_backup_api_restore_from_wal_copy() {
        let src_path = std::env::temp_dir().join(format!("ls_bk_src_{}.db", std::process::id()));
        let dst_path = std::env::temp_dir().join(format!("ls_bk_dst_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);

        // 备份副本：WAL 模式的 data.db（模拟 trigger_backup 的 fs::copy 产物）
        {
            let src = Connection::open(&src_path).unwrap();
            src.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // 活跃连接：已打开并持有（模拟连接池连接）
        let mut dst = Connection::open(&dst_path).unwrap();
        dst.execute_batch("CREATE TABLE t(x INTEGER);").unwrap();

        let src = Connection::open(&src_path).unwrap();
        let backup = Backup::new(&src, &mut dst).unwrap();
        assert_eq!(backup.step(-1).unwrap(), StepResult::Done);
        drop(backup);
        drop(src);
        drop(dst);

        let check = Connection::open(&dst_path).unwrap();
        let x: i64 = check.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);
    }

    #[test]
    fn test_snapshot_core_copies_all_artifacts() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};

        use crate::db;
        use crate::indexer::IndexerService;
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let base = std::env::temp_dir().join(format!("ls_snap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // config.json 放进 LS_CONFIG_DIR 指向的目录（config.rs 的测试后门）
        let config_dir = base.join("config_dir");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        unsafe { std::env::set_var("LS_CONFIG_DIR", &config_dir) };

        // 小 SQLite 库（WAL 模式）
        let db_path = base.join("data.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // data_dir 放 chat_history.json；index_dir 放一个文本文件
        let data_dir = base.join("data");
        let index_dir = data_dir.join(INDEX_DIR_NAME);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("a.txt"), "hello").unwrap();
        std::fs::write(data_dir.join("chat_history.json"), "[]").unwrap();

        // AppState（snapshot_core 只用 db_path/data_dir/index_dir，其余字段占位）
        let db_str = db_path.to_str().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        drop(conn);
        let pool = db::get_pool(db_str).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool,
            im,
            indexer,
            scanner,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            data_dir,
            index_dir,
            db_path,
            dummy_tx,
            None,
        );

        let dest = base.join("backup_1");
        std::fs::create_dir_all(&dest).unwrap();
        let manifest = snapshot_core(&state, &dest).unwrap();

        // 4 类产物全部存在
        assert!(dest.join(".ls-index/a.txt").is_file());
        assert!(dest.join("data.db").is_file());
        assert!(dest.join("config.json").is_file());
        assert!(dest.join("chat_history.json").is_file());

        // 清单：1 索引文件 + data.db + config.json + chat_history.json
        assert_eq!(manifest.files.len(), 4);
        let names: Vec<&str> = manifest.files.iter().map(|f| f.name.as_str()).collect();
        for n in ["a.txt", "data.db", "config.json", "chat_history.json"] {
            assert!(names.contains(&n), "manifest 缺少 {n}");
        }
        let total: u64 = manifest.files.iter().map(|f| f.size).sum();
        assert_eq!(manifest.size, total);

        // sha256 正确性（"hello" 的 sha256 与 size=5）
        let expected_hello: String = Sha256::digest(b"hello")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let a = manifest.files.iter().find(|f| f.name == "a.txt").unwrap();
        assert_eq!(a.sha256, expected_hello);
        assert_eq!(a.size, 5);

        // 备份的 data.db 可读：查询到插入的行
        let check = Connection::open(dest.join("data.db")).unwrap();
        let x: i64 = check.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);

        // 清理
        drop(state);
        let _ = std::fs::remove_dir_all(&base);
        unsafe { std::env::remove_var("LS_CONFIG_DIR") };
    }

    #[test]
    fn test_snapshot_incremental_hardlinks_segments() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};

        use crate::db;
        use crate::indexer::IndexerService;
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let base = std::env::temp_dir().join(format!("ls_incr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // config.json 放进 LS_CONFIG_DIR 指向的目录（config.rs 的测试后门）
        let config_dir = base.join("config_dir");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        unsafe { std::env::set_var("LS_CONFIG_DIR", &config_dir) };

        // 小 SQLite 库（WAL 模式）
        let db_path = base.join("data.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // data_dir 放 chat_history.json；index_dir 模拟 Tantivy：2 个 segment + meta.json
        let data_dir = base.join("data");
        let index_dir = data_dir.join(INDEX_DIR_NAME);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("s1.idx"), "segment-one").unwrap();
        std::fs::write(index_dir.join("s2.pos"), "segment-two").unwrap();
        std::fs::write(index_dir.join("meta.json"), r#"{"version":1}"#).unwrap();
        std::fs::write(data_dir.join("chat_history.json"), "[]").unwrap();

        let db_str = db_path.to_str().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        drop(conn);
        let pool = db::get_pool(db_str).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool,
            im,
            indexer,
            scanner,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            data_dir,
            index_dir.clone(),
            db_path,
            dummy_tx,
            None,
        );

        let mut chain = ChainHead::default();
        let b1 = base.join("backup_1");
        let b2 = base.join("backup_2");
        std::fs::create_dir_all(&b1).unwrap();
        std::fs::create_dir_all(&b2).unwrap();

        // 第一次：2 segments 复制 + meta 复制 + data.db + config.json + chat_history.json
        let m1 = snapshot_incremental(&state, &b1, &mut chain).unwrap();
        assert_eq!(m1.files.len(), 6);

        // 第二次前：新增 s3.idx、修改 meta.json
        std::fs::write(index_dir.join("s3.idx"), "segment-three").unwrap();
        std::fs::write(index_dir.join("meta.json"), r#"{"version":2}"#).unwrap();

        let m2 = snapshot_incremental(&state, &b2, &mut chain).unwrap();

        // backup_2 清单：s1/s2（硬链接）+ s3（复制）+ meta（新副本）+ 3 个小文件
        assert_eq!(m2.files.len(), 7);
        let names1: Vec<&str> = m1.files.iter().map(|f| f.name.as_str()).collect();
        let names2: Vec<&str> = m2.files.iter().map(|f| f.name.as_str()).collect();
        for n in [
            "s1.idx",
            "s2.pos",
            "s3.idx",
            "meta.json",
            "data.db",
            "config.json",
            "chat_history.json",
        ] {
            assert!(names2.contains(&n), "backup_2 清单缺少 {n}");
        }

        // meta.json 每次都是全新副本：backup_1 是 v1、backup_2 是 v2
        assert_eq!(
            std::fs::read_to_string(b1.join(INDEX_DIR_NAME).join("meta.json")).unwrap(),
            r#"{"version":1}"#
        );
        assert_eq!(
            std::fs::read_to_string(b2.join(INDEX_DIR_NAME).join("meta.json")).unwrap(),
            r#"{"version":2}"#
        );

        // s1/s2 与 live 索引是同一 inode（硬链接）→ nlink==2；s3 与 backup_1 的 s1 是独立副本 → nlink==1
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s1.idx")).unwrap().nlink(), 2);
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s2.pos")).unwrap().nlink(), 2);
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s3.idx")).unwrap().nlink(), 1);
            assert_eq!(std::fs::metadata(b1.join(INDEX_DIR_NAME).join("s1.idx")).unwrap().nlink(), 1);
        }
        assert_eq!(
            std::fs::read_to_string(b2.join(INDEX_DIR_NAME).join("s1.idx")).unwrap(),
            "segment-one"
        );

        // segment_store 每个 segment 只登记一次
        assert_eq!(chain.segment_store.len(), 3);
        for n in ["s1.idx", "s2.pos", "s3.idx"] {
            assert!(chain.segment_store.iter().any(|s| s == n), "segment_store 缺少 {n}");
        }

        // data.db / config.json / chat_history.json 两次备份都出现
        for n in ["data.db", "config.json", "chat_history.json"] {
            assert!(names1.contains(&n), "backup_1 清单缺少 {n}");
            assert!(names2.contains(&n), "backup_2 清单缺少 {n}");
        }

        // 清理
        drop(state);
        let _ = std::fs::remove_dir_all(&base);
        unsafe { std::env::remove_var("LS_CONFIG_DIR") };
    }

    #[test]
    fn test_merge_chain_materializes_baseline() {
        let base = std::env::temp_dir().join(format!("ls_merge_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut chain = ChainHead::default();
        let n = MERGE_THRESHOLD;

        for i in 0..n {
            let id = format!("backup_{i}");
            let dir = base.join(&id);
            std::fs::create_dir_all(dir.join(INDEX_DIR_NAME)).unwrap();
            std::fs::write(dir.join(INDEX_DIR_NAME).join(format!("seg_{i}.idx")), format!("seg-content-{i}")).unwrap();
            std::fs::write(dir.join("data.db"), format!("db-content-{i}")).unwrap();
            let seg_file = SnapshotFile {
                name: format!("seg_{i}.idx"),
                size: (format!("seg-content-{i}").len()) as u64,
                sha256: "dummy".to_string(),
            };
            let db_file = SnapshotFile {
                name: "data.db".to_string(),
                size: (format!("db-content-{i}").len()) as u64,
                sha256: "dummy".to_string(),
            };
            chain.segment_store.push(format!("seg_{i}.idx"));
            chain.snapshots.push(ChainSnapshot {
                id: id.clone(),
                ts: i as i64,
                kind: if i == 0 { "baseline" } else { "incremental" }.to_string(),
                files: vec![seg_file, db_file],
            });
        }
        chain.baseline_id = "backup_0".to_string();

        assert_eq!(chain.snapshots.len(), n);
        for i in 0..n {
            assert!(base.join(format!("backup_{i}")).is_dir());
        }

        merge_chain(&base, &mut chain).unwrap();

        assert_eq!(chain.snapshots.len(), 2);
        assert_eq!(chain.snapshots[0].kind, "merged");
        assert_eq!(chain.snapshots[1].id, format!("backup_{}", n - 1));
        assert!(!chain.baseline_id.is_empty());

        for i in 0..(n - 1) {
            assert!(!base.join(format!("backup_{i}")).is_dir(), "backup_{i} should be removed");
        }
        assert!(base.join(&chain.snapshots[0].id).is_dir());

        let merged_files: Vec<&str> = chain.snapshots[0].files.iter().map(|f| f.name.as_str()).collect();
        for i in 0..(n - 1) {
            assert!(merged_files.contains(&format!("seg_{i}.idx").as_str()), "merged missing seg_{i}.idx");
        }
        assert!(merged_files.contains(&"data.db"), "merged missing data.db");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_has_config_secrets() {
        use crate::config::{AppConfig, ProviderConfig};

        let mut cfg = AppConfig::default();
        assert!(!has_config_secrets(&cfg));

        cfg.ai_api_key = "sk-test".to_string();
        assert!(has_config_secrets(&cfg));

        cfg.ai_api_key = String::new();
        cfg.embedding_api_key = "emb-key".to_string();
        assert!(has_config_secrets(&cfg));

        cfg.embedding_api_key = String::new();
        assert!(!has_config_secrets(&cfg));

        cfg.providers.push(ProviderConfig {
            id: "p1".into(),
            name: "test".into(),
            base_url: "http://localhost".into(),
            api_key: "provider-key".into(),
            models: vec![],
        });
        assert!(has_config_secrets(&cfg));
    }

    #[test]
    fn test_snapshot_core_creates_zip_ready_dir() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};

        use crate::db;
        use crate::indexer::IndexerService;
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let base = std::env::temp_dir().join(format!("ls_ziprt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let config_dir = base.join("config_dir");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        unsafe { std::env::set_var("LS_CONFIG_DIR", &config_dir) };

        let db_path = base.join("data.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(99);",
            )
            .unwrap();
        }

        let data_dir = base.join("data");
        let index_dir = data_dir.join(INDEX_DIR_NAME);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("doc.txt"), "hello zip").unwrap();
        std::fs::write(data_dir.join("chat_history.json"), "[]").unwrap();

        let db_str = db_path.to_str().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        drop(conn);
        let pool = db::get_pool(db_str).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool, im, indexer, scanner,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            data_dir, index_dir, db_path, dummy_tx, None,
        );

        let dest = base.join("snapshot_out");
        std::fs::create_dir_all(&dest).unwrap();
        let manifest = snapshot_core(&state, &dest).unwrap();

        assert!(dest.join(INDEX_DIR_NAME).join("doc.txt").is_file());
        assert!(dest.join("data.db").is_file());
        assert!(dest.join("config.json").is_file());
        assert!(dest.join("chat_history.json").is_file());

        let total: u64 = manifest.files.iter().map(|f| f.size).sum();
        assert_eq!(manifest.size, total);
        assert!(manifest.size > 0);

        drop(state);
        let _ = std::fs::remove_dir_all(&base);
        unsafe { std::env::remove_var("LS_CONFIG_DIR") };
    }

    // ───────────────────────── R1：恢复前目录映射 ─────────────────────────

    fn dir_info(id: &str, path: &str) -> BackupDirInfo {
        BackupDirInfo {
            id: id.to_string(),
            original_path: path.to_string(),
            alias: None,
            file_count: 3,
            exists_locally: false,
        }
    }

    #[test]
    fn verify_backup_src_detects_mismatch_and_missing() {
        let base = std::env::temp_dir().join(format!("ls_verify_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".ls-index")).unwrap();
        std::fs::write(base.join("data.db"), b"dbdata").unwrap();
        std::fs::write(base.join(".ls-index").join("seg.0"), b"seg").unwrap();

        let (db_hex, db_len) = sha256_file(&base.join("data.db")).unwrap();
        let (seg_hex, seg_len) = sha256_file(&base.join(".ls-index").join("seg.0")).unwrap();
        let manifest = serde_json::json!({
            "files": [
                { "name": "data.db", "size": db_len, "sha256": db_hex },
                { "name": ".ls-index/seg.0", "size": seg_len, "sha256": seg_hex },
            ],
            "size": db_len + seg_len,
        });
        std::fs::write(base.join("snapshot.json"), manifest.to_string()).unwrap();

        let v = verify_backup_src(&base);
        assert!(v.ok(), "干净备份应通过: {v:?}");
        assert_eq!(v.checked, 2);

        // 篡改内容 → mismatches
        std::fs::write(base.join(".ls-index").join("seg.0"), b"TAMPERED").unwrap();
        let v = verify_backup_src(&base);
        assert!(!v.ok());
        assert_eq!(v.mismatches, vec![".ls-index/seg.0".to_string()]);

        // 删除文件 → missing
        std::fs::remove_file(base.join("data.db")).unwrap();
        let v = verify_backup_src(&base);
        assert!(!v.ok());
        assert!(v.missing.contains(&"data.db".to_string()));

        // 缺 snapshot.json → missing 清单本身
        std::fs::remove_file(base.join("snapshot.json")).unwrap();
        let v = verify_backup_src(&base);
        assert!(!v.ok());
        assert_eq!(v.missing, vec!["snapshot.json".to_string()]);

        let _ = std::fs::remove_dir_all(&base);
    }

    fn mapping(id: &str, action: &str, new_path: Option<&str>) -> DirMapping {
        DirMapping {
            dir_id: id.to_string(),
            action: action.to_string(),
            new_path: new_path.map(str::to_string),
        }
    }

    #[test]
    fn validate_mappings_requires_full_coverage() {
        let dirs = vec![dir_info("d1", "/old/a"), dir_info("d2", "/old/b")];
        // 少一条 → 拒绝
        let partial = vec![mapping("d1", "keep", None)];
        assert!(validate_mappings(&dirs, &partial, false).is_err());

        // 全给出且均为 keep → 通过
        let full = vec![mapping("d1", "keep", None), mapping("d2", "remove", None)];
        assert!(validate_mappings(&dirs, &full, false).is_ok());

        // 多出未知目录 → 拒绝
        let unknown = vec![
            mapping("d1", "keep", None),
            mapping("d2", "keep", None),
            mapping("dX", "keep", None),
        ];
        assert!(validate_mappings(&dirs, &unknown, false).is_err());

        // 同一目录两条映射 → 拒绝
        let dup = vec![
            mapping("d1", "keep", None),
            mapping("d1", "remove", None),
            mapping("d2", "keep", None),
        ];
        assert!(validate_mappings(&dirs, &dup, false).is_err());
    }

    #[test]
    fn validate_mappings_rejects_duplicate_target_and_needs_ack_for_nesting() {
        let base = std::env::temp_dir().join(format!("ls_mapv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("a");
        let a_sub = a.join("sub");
        std::fs::create_dir_all(&a_sub).unwrap();

        let dirs = vec![dir_info("d1", "/old/1"), dir_info("d2", "/old/2")];
        let a_str = a.to_str().unwrap().to_string();

        // 两个映射指向同一个目录 → 拒绝（归一化判重）
        let same = vec![
            mapping("d1", "remap", Some(&a_str)),
            mapping("d2", "remap", Some(&a_str)),
        ];
        assert!(validate_mappings(&dirs, &same, true).is_err());

        // 嵌套根：未确认 → 拒绝；确认后 → 通过
        let nested = vec![
            mapping("d1", "remap", Some(&a_str)),
            mapping("d2", "remap", Some(a_sub.to_str().unwrap())),
        ];
        assert!(validate_mappings(&dirs, &nested, false).is_err());
        assert!(validate_mappings(&dirs, &nested, true).is_ok());

        // remap 目标不是目录 → 拒绝
        let missing = vec![
            mapping("d1", "remap", Some(base.join("nope").to_str().unwrap())),
            mapping("d2", "keep", None),
        ];
        assert!(validate_mappings(&dirs, &missing, true).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_backup_dirs_and_preview_use_snapshot_db() {
        let base = std::env::temp_dir().join(format!("ls_plandb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // 伪造一个备份 db：dir_config + file_tracking（read_backup_dirs 只读这些列）
        let db = base.join("data.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE dir_config (id TEXT, path TEXT, alias TEXT);
                 CREATE TABLE file_tracking (dir_id TEXT, path TEXT, size INTEGER);
                 INSERT INTO dir_config VALUES ('d1','/old/docs','Docs');
                 INSERT INTO file_tracking VALUES ('d1','keep.txt',5);
                 INSERT INTO file_tracking VALUES ('d1','gone.txt',3);",
            )
            .unwrap();
        }

        let dirs = read_backup_dirs(&db).unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].id, "d1");
        assert_eq!(dirs[0].original_path, "/old/docs");
        assert_eq!(dirs[0].alias.as_deref(), Some("Docs"));
        assert_eq!(dirs[0].file_count, 2);

        // 候选目录里只放 keep.txt（同名+同大小）→ found=1, size_match=1, 缺 gone.txt
        let cand = base.join("cand");
        std::fs::create_dir_all(&cand).unwrap();
        std::fs::write(cand.join("keep.txt"), b"hello").unwrap();

        let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let p = compute_remap_preview(&conn, "d1", &cand).unwrap();
        assert_eq!(p.tracked, 2);
        assert_eq!(p.found, 1);
        assert_eq!(p.size_match, 1);
        assert_eq!(p.sample_missing, vec!["gone.txt".to_string()]);

        // 缺失 data.db → 明确报错（不 panic）
        assert!(read_backup_dirs(&base.join("nope.db")).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}