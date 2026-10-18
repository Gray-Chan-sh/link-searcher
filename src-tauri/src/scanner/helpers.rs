//! Shared helper functions for directory scanning.
//!
//! Path conversion utilities for storing/retrieving files using relative paths
//! within a monitored directory root.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow};
use crate::db::tracker::{FileRecord, IndexedState};

/// One metadata request: the path plus the channel the worker answers on.
type MetaJob = (PathBuf, mpsc::Sender<std::io::Result<std::fs::Metadata>>);

/// Signature of the actual metadata syscall. Injectable so tests can simulate a
/// hung network mount without touching the filesystem.
type MetaFetch = Arc<dyn Fn(&Path) -> std::io::Result<std::fs::Metadata> + Send + Sync>;

/// Fixed-size worker pool behind [`metadata_timeout`].
///
/// 之前是「一个文件一条线程」：正常路径下每次 `metadata` 都 spawn+join（十万文件
/// 就是十万次线程创建），更糟的是**挂载卡死时每超时一次就永久泄漏一条线程** ——
/// 扫描 10 万个文件能把进程线程数打到 10 万级。
///
/// 现在固定 `size` 条工人 + 有界队列：
/// * 队列满 → 立刻报错，卡死时不再无界堆积请求与 `PathBuf`；
/// * 工人连续 `stall_window` 毫无进展（全卡在 metadata 上）→ 重建池：旧线程有界
///   泄漏（每窗口最多 `size` 条），新线程给「挂载已恢复」一次机会。
struct MetaPool {
    tx: mpsc::SyncSender<MetaJob>,
    fetch: MetaFetch,
    size: usize,
    /// 正在执行 metadata 的工人数。
    busy: Arc<AtomicUsize>,
    /// 每完成一次（无论成败）+1；用于判定「到底有没有进展」。
    epoch: Arc<AtomicU64>,
    /// `(epoch 值, 首次观察到该值且工人全忙的时刻)`。
    stall: Option<(u64, Instant)>,
    /// 已 spawn 的工人总数（有界泄漏自检 / 测试用）。
    spawned: Arc<AtomicUsize>,
}

/// 「零进展」容忍时长：与调用方给的单次超时挂钩（4×），下限 250ms
/// （测试用极小超时也能触发）。
fn stall_window(timeout: Duration) -> Duration {
    timeout.saturating_mul(4).max(Duration::from_millis(250))
}

impl MetaPool {
    /// 工人数：扫描是顺序调用，4 条足以并行化解 SMB/NFS 往返延迟，
    /// 同时把「挂载卡死」时的线程泄漏上限压到 4 条/窗口。
    const WORKERS: usize = 4;
    /// 请求队列上限：满了立刻失败，避免卡死时无界堆积。
    const QUEUE: usize = 64;

    fn new(size: usize, fetch: MetaFetch) -> Self {
        Self::with_counter(size, fetch, Arc::new(AtomicUsize::new(0)))
    }

    /// 与 [`MetaPool::new`] 相同，但复用调用方的计数器 —— 重建时必须**累计**
    /// 「一共 spawn 了多少工人」，否则泄漏检测会随重建被清零。
    fn with_counter(size: usize, fetch: MetaFetch, spawned: Arc<AtomicUsize>) -> Self {
        let size = size.max(1);
        let (tx, rx) = mpsc::sync_channel::<MetaJob>(Self::QUEUE);
        let rx = Arc::new(Mutex::new(rx));
        let busy = Arc::new(AtomicUsize::new(0));
        let epoch = Arc::new(AtomicU64::new(0));
        for _ in 0..size {
            let rx = Arc::clone(&rx);
            let busy = Arc::clone(&busy);
            let epoch = Arc::clone(&epoch);
            let fetch = Arc::clone(&fetch);
            // ⚠️ 计数必须**在 spawn 之前同步 +1**：放进线程体内会有竞态 ——
            // 刚建好的池在工人被调度前 `spawned == 0`，会被 `dispatch` 误判成
            // "工人创建失败"（实测：连测试都跑不过）。
            spawned.fetch_add(1, Ordering::Relaxed);
            let built = std::thread::Builder::new()
                .name("ls-metadata".to_string())
                .spawn(move || {
                    loop {
                        // 单接收端共享：同一时刻只有一名工人取任务（其余阻塞在锁上），
                        // 取到后立刻放锁 → 真正的 metadata 并行执行。
                        let job = {
                            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
                            guard.recv()
                        };
                        let Ok((path, reply)) = job else { break };
                        busy.fetch_add(1, Ordering::Relaxed);
                        let result = fetch(&path);
                        busy.fetch_sub(1, Ordering::Relaxed);
                        epoch.fetch_add(1, Ordering::Relaxed);
                        let _ = reply.send(result);
                    }
                });
            if let Err(e) = built {
                log::warn!("[SCAN] metadata 工人线程创建失败: {e}");
                spawned.fetch_sub(1, Ordering::Relaxed);
            }
        }
        Self { tx, fetch, size, busy, epoch, stall: None, spawned }
    }

    /// 工人全忙且窗口内毫无进展 → 认定卡死，重建工作池。
    fn maybe_rebuild(&mut self, timeout: Duration) {
        if self.busy.load(Ordering::Relaxed) < self.size {
            self.stall = None;
            return;
        }
        let epoch = self.epoch.load(Ordering::Relaxed);
        match self.stall {
            Some((seen, at)) if seen == epoch => {
                if at.elapsed() >= stall_window(timeout) {
                    log::warn!(
                        "[SCAN] metadata 工人全部卡死 {:?} 无进展 → 重建工作池（旧线程有界泄漏）",
                        at.elapsed()
                    );
                    let fetch = Arc::clone(&self.fetch);
                    *self = MetaPool::with_counter(self.size, fetch, Arc::clone(&self.spawned));
                }
            }
            _ => self.stall = Some((epoch, Instant::now())),
        }
    }

    /// 投递请求，返回应答通道（**不**在持锁期间等待结果）。
    fn dispatch(
        &mut self,
        path: &Path,
        timeout: Duration,
    ) -> Result<mpsc::Receiver<std::io::Result<std::fs::Metadata>>> {
        if self.spawned.load(Ordering::Relaxed) == 0 {
            return Err(anyhow!("metadata 工作池不可用（工人线程创建失败）: {}", path.display()));
        }
        self.maybe_rebuild(timeout);
        let (tx, rx) = mpsc::channel();
        match self.tx.try_send((path.to_path_buf(), tx)) {
            Ok(()) => Ok(rx),
            // 队列满 = 工人全卡住且积压已满 → 立刻失败（不再排队等 15s）。
            Err(mpsc::TrySendError::Full(_)) => Err(anyhow!(
                "metadata 工作池队列已满（疑似挂载卡死，已排队 {} 条）: {}",
                Self::QUEUE,
                path.display()
            )),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                log::warn!("[SCAN] metadata 工人池已断开 → 重建");
                let fetch = Arc::clone(&self.fetch);
                *self = MetaPool::with_counter(self.size, fetch, Arc::clone(&self.spawned));
                let (tx, rx) = mpsc::channel();
                self.tx.try_send((path.to_path_buf(), tx)).map_err(|_| {
                    anyhow!("metadata 工作池不可用（重建后仍无法投递）: {}", path.display())
                })?;
                Ok(rx)
            }
        }
    }
}

static META_POOL: OnceLock<Mutex<MetaPool>> = OnceLock::new();

fn global_meta_pool() -> &'static Mutex<MetaPool> {
    META_POOL.get_or_init(|| {
        Mutex::new(MetaPool::new(
            MetaPool::WORKERS,
            Arc::new(|p: &Path| std::fs::metadata(p)),
        ))
    })
}

fn recv_metadata(
    rx: mpsc::Receiver<std::io::Result<std::fs::Metadata>>,
    path: &Path,
    timeout: Duration,
) -> Result<std::fs::Metadata> {
    match rx.recv_timeout(timeout) {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(anyhow!("metadata timed out after {:?}: {}", timeout, path.display())),
    }
}

/// `metadata()` with a timeout guard.
///
/// Network filesystems (NFS/SMB/FUSE) can hang indefinitely on `metadata`.
/// Run the call on a pool worker and give up after `timeout` — the caller
/// treats a timeout as "metadata unavailable" and skips the entry instead
/// of blocking the whole scan.
pub fn metadata_timeout(path: &Path, timeout: std::time::Duration) -> Result<std::fs::Metadata> {
    let rx = {
        let mut pool = global_meta_pool().lock().unwrap_or_else(|e| e.into_inner());
        pool.dispatch(path, timeout)?
    }; // 锁在等待结果之前释放，并发调用不会互相串行化
    recv_metadata(rx, path, timeout)
}

/// A discovered disk file entry carrying both absolute and relative paths.
#[derive(Debug, Clone)]
pub struct DiskEntry {
    /// Absolute path — used for filesystem access (reading content, MD5).
    pub abs_path: String,
    /// Relative path — used for DB matching and tracking.
    pub rel_path: String,
    pub size: u64,
    pub name: String,
}

const EXCLUDED_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", ".git", ".svn", "__pycache__"];

const EXCLUDED_PREFIXES: &[char] = &['#', '$', '.', '~'];

const EXCLUDED_SUFFIXES: &[&str] = &[".tmp", ".temp", ".bak", ".swp", ".swo", "~"];

pub fn is_excluded(path: &Path, exclude_patterns: &[glob::Pattern]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if EXCLUDED_NAMES.contains(&name) {
        return true;
    }
    if let Some(c) = name.chars().next()
        && EXCLUDED_PREFIXES.contains(&c) {
            return true;
        }
    if EXCLUDED_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    for pattern in exclude_patterns {
        if pattern.matches_path(path) {
            return true;
        }
    }
    false
}

/// Check whether a file's extension is in the allowed set, or whether the
/// extractor supports it when no explicit extension list is configured.
pub fn extension_allowed(path: &Path, include_exts: &Option<Vec<String>>) -> bool {
    match include_exts {
        Some(exts) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            !ext.is_empty() && exts.contains(&ext)
        }
        None => crate::extractor::is_supported(path),
    }
}

/// Extract modification time as microseconds since UNIX_EPOCH.
pub fn mtime_micros(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_micros() as i64)
}

/// Parse a comma-separated list of glob patterns from a config value.
pub fn parse_exclude_patterns(raw: &Option<String>) -> Vec<glob::Pattern> {
    raw.as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| glob::Pattern::new(s).ok())
        .collect()
}

/// Parse a comma-separated list of file extensions from a config value.
pub fn parse_include_exts(raw: &Option<String>) -> Option<Vec<String>> {
    let s = raw.as_deref()?;
    let exts: Vec<String> = s.split(',').map(|e| e.trim().to_lowercase()).collect();
    if exts.is_empty() {
        None
    } else {
        Some(exts)
    }
}

/// Retrieve the last-scan timestamp for a directory from `app_settings`.
pub fn get_last_scan_time(conn: &rusqlite::Connection, dir_id: &str) -> Result<i64> {
    let key = format!("last_scan_{dir_id}");
    let mut stmt = conn
        .prepare("SELECT value FROM app_settings WHERE key = ?1")
        .context("prepare last_scan")?;
    let mut rows = stmt
        .query_map(rusqlite::params![key], |row| row.get::<_, String>(0))
        .context("query last_scan")?;
    match rows.next().transpose()? {
        Some(v) => v.parse::<i64>().or(Ok(0)),
        None => Ok(0),
    }
}

/// Record the current timestamp as the last-scan time for a directory.
pub fn record_last_scan(conn: &rusqlite::Connection, dir_id: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp_micros().to_string();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![format!("last_scan_{dir_id}"), now],
    )?;
    Ok(())
}


/// Convert an absolute path to a relative path within the given directory root.
/// Returns the relative path string, or an error if the file is not under dir_root.
pub fn to_relative(dir_root: &str, file_path: &Path) -> Result<String> {
    let root = PathBuf::from(dir_root);
    let rel = file_path
        .strip_prefix(&root)
        .map_err(|_| anyhow!("file {} is not under dir root {}", file_path.display(), dir_root))?;
    // Store with forward slashes so DB/IPC matches on Windows too.
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Convert a stored relative path back to an absolute path by joining with dir_root.
pub fn to_absolute(dir_root: &str, rel_path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(dir_root).join(rel_path)
}

/// Normalize an OS path string to the internal canonical form used by the
/// database and frontend: forward slashes only, no Windows verbatim prefix.
///
/// Windows sources (folder dialog, `read_dir`, `canonicalize`) all produce
/// backslash paths, and `canonicalize` additionally emits `\\?\` verbatim
/// prefixes; unnormalized storage made every separator-sensitive comparison
/// downstream (strip_prefix, starts_with, LIKE prefixes, HashMap keys) fail
/// on Windows while macOS/Linux were unaffected. All persisted paths
/// (dir_config.path, file_tracking.path, Tantivy path) go through this.
pub fn normalize_os_path(input: &str) -> String {
    let stripped = if let Some(rest) = input.strip_prefix(r"\\?\UNC\") {
        // \\?\UNC\server\share -> //server/share (network path)
        format!("//{rest}")
    } else {
        input.strip_prefix(r"\\?\").map(str::to_string).unwrap_or_else(|| input.to_string())
    };
    let mut out = stripped.replace('\\', "/");
    while let Some(trimmed) = out.strip_suffix('/') {
        if trimmed.is_empty() || trimmed.ends_with(':') {
            break;
        }
        out = trimmed.to_string();
    }
    out
}

pub fn needs_reindex(existing: &Option<FileRecord>, mtime: i64) -> bool {
    match existing {
        // Extracted (Phase-1 done, Tantivy write pending) is treated as
        // incomplete: a crash after extraction but before the Tantivy write
        // would otherwise orphan the record forever (searchable-never).
        // Also re-index files marked indexed=1 but missing md5 (stale state
        // from old-version bugs or interrupted scans — content never stored).
        Some(r) => {
            r.mtime != mtime
                || r.indexed == IndexedState::Pending as i64
                || r.indexed == IndexedState::Extracted as i64
                || (r.indexed == IndexedState::Indexed as i64 && r.md5.is_none())
        }
        None => true,
    }
}

/// RAII temporary directory. Unique per instance (pid + uuid), removed
/// including contents when dropped. Solves concurrent runs sharing the same
/// temp dir and leaking leftovers on early returns.
pub struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_removed_on_drop() {
        let path = {
            let td = TempDir::new("test_tmp").unwrap();
            assert!(td.path().exists());
            td.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn temp_dir_names_are_unique() {
        let a = TempDir::new("test_tmp").unwrap().path().to_path_buf();
        let b = TempDir::new("test_tmp").unwrap().path().to_path_buf();
        assert_ne!(a, b);
    }

    #[test]
    fn needs_reindex_treats_extracted_as_incomplete() {
        let rec = |indexed: i64, md5: Option<String>| Some(crate::db::tracker::FileRecord {
            id: "id".into(), path: "p".into(), dir_id: "d".into(),
            mtime: 100, size: 1, md5, status: "active".into(),
            indexed, error_msg: None, created_at: 0, updated_at: 0, dead_content: 0,
        });
        // Same mtime: indexed=1 WITH md5 is up-to-date; extracted(3) and
        // indexed=1-but-missing-md5 are not (content never stored).
        assert!(!needs_reindex(&rec(1, Some("abc".into())), 100));
        assert!(needs_reindex(&rec(1, None), 100));
        assert!(needs_reindex(&rec(3, None), 100));
        // mtime change forces re-index regardless of state.
        assert!(needs_reindex(&rec(1, Some("abc".into())), 1001));
        // No record → re-index.
        assert!(needs_reindex(&None, 0));
    }

    #[test]
    fn to_relative_respects_component_boundary() {
        let root = "/tmp/foo";
        assert_eq!(to_relative(root, Path::new("/tmp/foo/bar.txt")).unwrap(), "bar.txt");
        assert_eq!(to_relative(root, Path::new("/tmp/foo/sub/deep.txt")).unwrap(), "sub/deep.txt");
        // sibling with shared prefix must NOT match
        assert!(to_relative(root, Path::new("/tmp/foobar/x.txt")).is_err());
    }

    /// 「相对路径 → 绝对路径」（DB 归一化正斜杠路径 → 磁盘绝对路径）的**平台无关**契约。
    ///
    /// `to_absolute` / `to_relative` 是这一转换的配对入口，Windows 与 Unix 都走它，
    /// 所以断言**只用 `Path` 语义**（组件比较、与 `to_relative` 往返），**不比对字面串**
    /// —— `join` 插入的分隔符随平台而变（Windows `\`、Unix `/`），写死字面串会让
    /// 测试退化成平台专属（在本机绿、换平台红，或者反过来）。
    #[test]
    fn to_absolute_is_platform_neutral_and_round_trips() {
        for (root, rel) in [
            ("/tmp/root", "a.pdf"),
            ("/tmp/root", "sub/deep/a.pdf"),
            // Windows 上 DB 里存的也是这种归一化正斜杠路径（normalize_os_path）
            ("D:/Syncthing/工作日志", "67类起诉状/1.刑事（附带民事）自诉状/我的 文件.docx"),
            ("C:/Users/x/Docs", "带 空格/子 目录.txt"),
        ] {
            let abs = to_absolute(root, rel);
            assert!(abs.starts_with(Path::new(root)), "{abs:?} 应位于根 {root} 之下");
            assert!(abs.ends_with(Path::new(rel)), "{abs:?} 应以相对路径 {rel} 结尾");
            // 与扫描侧 to_relative 往返：锁住「存什么、还原什么」的一致性
            //（若 strip_prefix 在 join 产生的分隔符上留下前导分隔符，这条会红）。
            assert_eq!(to_relative(root, &abs).unwrap(), rel, "往返还原失败: {abs:?}");
        }
    }

    #[test]
    fn normalize_os_path_converts_and_strips_verbatim_prefix() {
        assert_eq!(normalize_os_path(r"D:\我的目录\子目录"), "D:/我的目录/子目录");
        assert_eq!(normalize_os_path(r"\\?\D:\foo\bar"), "D:/foo/bar");
        assert_eq!(normalize_os_path(r"\\?\UNC\server\share\x.txt"), "//server/share/x.txt");
        assert_eq!(normalize_os_path("/tmp/foo/"), "/tmp/foo");
        assert_eq!(normalize_os_path("D:/"), "D:/");
        assert_eq!(normalize_os_path("/"), "/");
        assert_eq!(normalize_os_path("already/normal.txt"), "already/normal.txt");
    }

    /// 正常路径：全局池（真实 `fs::metadata`）必须返回真实 metadata。
    #[test]
    fn metadata_timeout_returns_real_metadata() {
        let dir = TempDir::new("meta_pool_ok").unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hello").unwrap();
        let m = metadata_timeout(&f, Duration::from_secs(5)).expect("metadata ok");
        assert_eq!(m.len(), 5);

        // 不存在的文件 → Err（io 错误，而非超时文案）
        let missing = dir.path().join("nope.txt");
        let err = metadata_timeout(&missing, Duration::from_secs(5)).expect_err("must fail");
        assert!(!err.to_string().contains("timed out"), "应为 io 错误: {err}");
    }

    /// 挂载卡死 + 队列塞满 → **快速失败**，不再每条请求都干等一个超时、
    /// 也不再每文件泄漏一条线程。
    #[test]
    fn metadata_pool_fails_fast_when_queue_is_full() {
        let hang: MetaFetch = Arc::new(|_p: &Path| {
            std::thread::sleep(Duration::from_secs(5));
            Err(std::io::Error::other("hung"))
        });
        let mut pool = MetaPool::new(2, hang);
        let timeout = Duration::from_millis(20);
        let started = Instant::now();
        let mut full_errors = 0;
        for i in 0..(MetaPool::QUEUE + 10) {
            let path = PathBuf::from(format!("/hung/{i}"));
            if let Err(e) = pool.dispatch(&path, timeout) {
                if e.to_string().contains("队列已满") {
                    full_errors += 1;
                }
            }
        }
        assert!(full_errors > 0, "队列应塞满并快速失败");
        // 2 个工人 + 64 条队列，绝不该出现「每请求一线程」式的爆炸
        assert!(
            pool.spawned.load(Ordering::Relaxed) <= 4,
            "窗口内不应重建池，实际 spawn {}",
            pool.spawned.load(Ordering::Relaxed)
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "应快速失败，实际耗时 {:?}",
            started.elapsed()
        );
    }

    /// 工人全部卡死超过「零进展」窗口 → 重建池，且重建次数有界
    /// （线程泄漏被限制在 `size`/窗口，而不是每文件一条）。
    #[test]
    fn metadata_pool_rebuilds_after_total_stall() {
        let hang: MetaFetch = Arc::new(|_p: &Path| {
            std::thread::sleep(Duration::from_secs(5));
            Err(std::io::Error::other("hung"))
        });
        let mut pool = MetaPool::new(2, hang);
        let timeout = Duration::from_millis(20); // stall 窗口 = 250ms
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline {
            let _ = pool.dispatch(Path::new("/hung/x"), timeout);
            std::thread::sleep(Duration::from_millis(10));
        }
        let spawned = pool.spawned.load(Ordering::Relaxed);
        assert!(spawned > 2, "卡死超过窗口后应重建（spawn={spawned}）");
        // 1200ms / 250ms ≈ 4 次重建 × 2 条 = 8，留一倍余量
        assert!(spawned <= 16, "线程泄漏必须有界，实际 spawn={spawned}");
    }
}