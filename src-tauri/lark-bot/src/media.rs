//! 出站媒体：把本地文件上传到飞书，再以图片 / 语音 / 文件消息发出。
//!
//! 飞书没有"直接传路径"的接口：所有媒体都要**先上传拿 key，再发消息**。
//! 两条腿都走 `lark-cli`（typed 命令上传 + raw API 回复），与 `main.rs` 现有的
//! `reply_card` 保持同一套调用约定（`{ok, data}` 信封、参数数组不经 shell）。
//!
//! 本模块只做两件事：
//! - **决策**（纯函数）：这个文件该按图片、语音还是文件发；超限怎么办；
//! - **上传**（IO）：调 `lark-cli im images/files create`，把 key 解析出来。
//! 发消息那一步留在 `main.rs`（复用那里的 `lark_api`），不在这里重复一遍 HTTP 约定。
//!
//! 限制依据（2026-10-17 核对飞书开放平台文档）：
//! - 图片 ≤10MB；JPG/JPEG/PNG/WEBP/GIF/BMP/ICO/TIFF/HEIC（TIFF/HEIC 上传后转 JPG）；
//! - 文件/语音/视频 ≤30MB；语音必须是 Opus（其它格式需先转码）；
//! - 上传时的 `file_type` 必须和发送时的 `msg_type` 匹配（否则 230055）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// 飞书图片上传硬上限（`im/v1/images`）。
pub const IMAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// 飞书文件 / 语音 / 视频上传硬上限（`im/v1/files`）。
pub const FILE_MAX_BYTES: u64 = 30 * 1024 * 1024;

/// 图片扩展名（与飞书 `im/v1/images` 支持的格式一致）。
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "ico", "tiff", "heic",
];
/// 音频扩展名（其中 `opus` 可直接发语音气泡，其余需要转码）。
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "wma", "opus"];

/// 归一化扩展名：去空白、去前导 `.`、转小写。
fn norm_ext(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// 该扩展名是否按"图片消息"发送。
pub fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTS.contains(&norm_ext(ext).as_str())
}

/// 该扩展名是否是音频。
pub fn is_audio_ext(ext: &str) -> bool {
    AUDIO_EXTS.contains(&norm_ext(ext).as_str())
}

/// 音频的发送计划。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlan {
    /// 已经是 Opus → 直接按 `msg_type=audio` 发（可点击播放的语音气泡）。
    Direct,
    /// 其它音频且 ffmpeg 可用 → 先转 Opus 再按语音发。
    Convert,
    /// 不是音频，或没有 ffmpeg → 按普通文件发。
    AsFile,
}

/// 音频决策（纯函数）。
pub fn audio_plan(ext: &str, ffmpeg_available: bool) -> AudioPlan {
    let ext = norm_ext(ext);
    if ext == "opus" {
        return AudioPlan::Direct;
    }
    if is_audio_ext(&ext) && ffmpeg_available {
        return AudioPlan::Convert;
    }
    AudioPlan::AsFile
}

/// 非语音文件的发送形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlan {
    /// 图片且 ≤10MB → `msg_type=image`（聊天里直接预览）。
    Image,
    /// 其它（含 10–30MB 的图片）→ `msg_type=file`。
    File,
}

/// 决策：`size` 字节的文件按哪种形态发；`None` = 空文件或超过 30MB，飞书发不了。
pub fn classify(ext: &str, size: u64) -> Option<MediaPlan> {
    if size == 0 || size > FILE_MAX_BYTES {
        return None;
    }
    if is_image_ext(ext) && size <= IMAGE_MAX_BYTES {
        Some(MediaPlan::Image)
    } else {
        Some(MediaPlan::File)
    }
}

/// 上传文件时的 `file_type`（必须与后发消息的 `msg_type` 匹配，否则 230055）。
///
/// 只给四个文档类专有类型；`docx`/`xlsx`/`pptx` 在飞书文件类型里没有专属枚举，
/// 按 `stream` 上传即可（客户端按扩展名展示）。
pub fn file_type_for(ext: &str) -> &'static str {
    match norm_ext(ext).as_str() {
        "pdf" => "pdf",
        "doc" => "doc",
        "xls" => "xls",
        "ppt" => "ppt",
        _ => "stream",
    }
}

/// 统一的 `lark-cli` 调用：参数数组传值（不经 shell）、stderr 原样带进错误。
///
/// `cwd`：上传媒体时设为文件所在目录，并传**相对文件名** —— lark-cli 对本地媒体路径
/// 有一层"只能在 cwd 之内"的安全策略（`+messages-send --image` 的文档写得很明确），
/// 走相对路径可以同时满足新旧版本，不依赖"绝对路径恰好被放行"。
fn run_lark_cli(program: &Path, args: &[String], cwd: Option<&Path>) -> Result<Value, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("无法启动 lark-cli（{}）：{e}", program.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!(
            "lark-cli 退出码 {:?}：{} {}",
            out.status.code(),
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        format!(
            "lark-cli 输出不是 JSON：{e}（前 200 字符：{}）",
            &stdout[..stdout.len().min(200)]
        )
    })?;
    // ⚠️ 与 main.rs 相同的约定：lark-cli 的信封是 `{ ok, identity, data }`，
    // 不是飞书的 `{ code, msg }` —— 按 `code == 0` 判会把成功读成失败。
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return Err(format!("lark-cli 返回失败：{}", stdout.trim()));
    }
    Ok(v)
}

/// 上传时用的 `(cwd, 路径参数)`：优先"文件所在目录 + `./文件名`"，
/// 拿不到父目录/文件名时退回原路径（让 lark-cli 自己去解析）。
fn upload_cwd_and_path(file: &Path) -> (Option<PathBuf>, String) {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf());
    match (dir, name.is_empty()) {
        (Some(dir), false) => (Some(dir), format!("./{name}")),
        _ => (None, file.to_string_lossy().to_string()),
    }
}

/// 从 `{ok, data}` 信封里取 key；兼容 key 直接挂在顶层的情况。
fn extract_key(v: &Value, key: &str) -> Result<String, String> {
    v.get("data")
        .and_then(|d| d.get(key))
        .or_else(|| v.get(key))
        .and_then(|k| k.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("lark-cli 未返回 {key}：{v}"))
}

/// 上传图片（`image_type=message`）→ `image_key`。
pub fn upload_image(program: &Path, file: &Path) -> Result<String, String> {
    let data = json!({ "image_type": "message" }).to_string();
    let (cwd, path) = upload_cwd_and_path(file);
    let v = run_lark_cli(
        program,
        &[
            "im".into(),
            "images".into(),
            "create".into(),
            "--as".into(),
            "bot".into(),
            "--data".into(),
            data,
            "--file".into(),
            path,
        ],
        cwd.as_deref(),
    )?;
    extract_key(&v, "image_key")
}

/// 上传文件 / 语音 / 视频 → `file_key`。
pub fn upload_file(
    program: &Path,
    file: &Path,
    file_type: &str,
    file_name: &str,
) -> Result<String, String> {
    let data = json!({ "file_type": file_type, "file_name": file_name }).to_string();
    let (cwd, path) = upload_cwd_and_path(file);
    let v = run_lark_cli(
        program,
        &[
            "im".into(),
            "files".into(),
            "create".into(),
            "--as".into(),
            "bot".into(),
            "--data".into(),
            data,
            "--file".into(),
            path,
        ],
        cwd.as_deref(),
    )?;
    extract_key(&v, "file_key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_audio_extensions_are_case_insensitive_and_dot_tolerant() {
        assert!(is_image_ext(".PNG"));
        assert!(is_image_ext(" jpeg "));
        assert!(!is_image_ext("pdf"), "pdf 不是图片");
        assert!(is_audio_ext("MP3"));
        assert!(is_audio_ext(".opus"));
        assert!(!is_audio_ext("mp4"), "视频不是音频");
    }

    /// 决策边界：10MB 是图片上限，30MB 是文件上限，0 字节不能发。
    #[test]
    fn classify_respects_feishu_size_limits() {
        assert_eq!(classify("png", 1), Some(MediaPlan::Image));
        assert_eq!(classify("png", IMAGE_MAX_BYTES), Some(MediaPlan::Image));
        // 10–30MB 的图片：图片接口发不了，只能按文件发
        assert_eq!(classify("png", IMAGE_MAX_BYTES + 1), Some(MediaPlan::File));
        assert_eq!(classify("pdf", 1), Some(MediaPlan::File));
        assert_eq!(classify("pdf", FILE_MAX_BYTES), Some(MediaPlan::File));
        assert_eq!(classify("pdf", FILE_MAX_BYTES + 1), None, "超 30MB 发不了");
        assert_eq!(classify("png", 0), None, "空文件不能发");
    }

    /// 语音：opus 直发；其它音频有 ffmpeg 才转；没有就退回文件。
    #[test]
    fn audio_plan_prefers_voice_only_when_feasible() {
        assert_eq!(audio_plan("opus", false), AudioPlan::Direct);
        assert_eq!(audio_plan(".OPUS", false), AudioPlan::Direct);
        assert_eq!(audio_plan("mp3", true), AudioPlan::Convert);
        assert_eq!(audio_plan("wav", true), AudioPlan::Convert);
        assert_eq!(audio_plan("mp3", false), AudioPlan::AsFile, "没 ffmpeg → 按文件发");
        assert_eq!(audio_plan("pdf", true), AudioPlan::AsFile);
        assert_eq!(audio_plan("mp4", true), AudioPlan::AsFile, "视频不转语音");
    }

    #[test]
    fn file_type_follows_feishu_enum() {
        assert_eq!(file_type_for("pdf"), "pdf");
        assert_eq!(file_type_for(".PDF"), "pdf");
        assert_eq!(file_type_for("doc"), "doc");
        assert_eq!(file_type_for("docx"), "stream", "docx 没有专属类型");
        assert_eq!(file_type_for("xlsx"), "stream");
        assert_eq!(file_type_for("pptx"), "stream");
        assert_eq!(file_type_for("txt"), "stream");
        assert_eq!(file_type_for("mp4"), "stream", "视频不在首版支持范围，按文件发");
    }

    /// 上传失败必须是**可读错误**，不能 panic（lark-cli 不存在是最常见的失败）。
    #[test]
    fn upload_reports_missing_lark_cli_instead_of_panicking() {
        let bogus = Path::new("definitely-not-a-real-lark-cli-binary-xyz");
        let err = upload_image(bogus, Path::new("whatever.png")).unwrap_err();
        assert!(err.contains("无法启动 lark-cli"), "实际错误：{err}");
    }

    /// 上传路径统一走"cwd + `./文件名`"：lark-cli 对本地媒体路径有 cwd 限制。
    #[test]
    fn upload_path_prefers_cwd_relative_form() {
        let (cwd, path) = upload_cwd_and_path(Path::new("D:/docs/报告 2024.pdf"));
        assert!(cwd.is_some(), "有父目录时应把 cwd 设过去");
        assert_eq!(path, "./报告 2024.pdf", "相对路径避开 lark-cli 的路径策略");
        let (cwd, path) = upload_cwd_and_path(Path::new("bare.pdf"));
        assert!(cwd.is_none(), "没有父目录就不设 cwd");
        assert_eq!(path, "bare.pdf");
    }
}
