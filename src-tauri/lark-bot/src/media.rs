//! 出站媒体：把本地文件作为图片 / 语音 / 文件消息发到飞书。
//!
//! 走 `lark-cli im +messages-reply --image/--file/--audio`：shortcut 在**同一个进程内**
//! 完成"上传 → 回复"（2026-10-18 通讯审计后改的）。此前的两步式（先
//! `im images/files create` 拿 key、再 `api reply`）要起**两个** `lark-cli` 进程、取两次
//! token —— 实测单次调用 ~0.6s，一步式直接省掉其中一次。
//!
//! 本模块只做两件事：
//! - **决策**（纯函数）：这个文件该按图片、语音还是文件发；超限怎么办；
//! - **发送**（IO）：调 `+messages-reply`，参数数组不经 shell、本地路径相对 cwd。
//!
//! 限制依据（2026-10-17 核对飞书开放平台文档）：
//! - 图片 ≤10MB；JPG/JPEG/PNG/WEBP/GIF/BMP/ICO/TIFF/HEIC（TIFF/HEIC 上传后转 JPG）；
//! - 文件/语音/视频 ≤30MB；语音必须是 Opus（其它格式需先转码）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

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
    /// 已经是 Opus → 直接按语音发（可点击播放的语音气泡）。
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
    /// 图片且 ≤10MB → 图片消息（聊天里直接预览）。
    Image,
    /// 其它（含 10–30MB 的图片）→ 文件消息。
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

/// 媒体类型（决定 `+messages-reply` 用哪个 flag，也决定 `msg_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    File,
    Audio,
}

impl MediaKind {
    /// 对应的 lark-cli flag。
    fn flag(self) -> &'static str {
        match self {
            MediaKind::Image => "--image",
            MediaKind::File => "--file",
            MediaKind::Audio => "--audio",
        }
    }
}

/// 统一的 `lark-cli` 调用：参数数组传值（不经 shell）、stderr 原样带进错误。
///
/// `cwd`：发送媒体时设为文件所在目录，并传**相对文件名** —— lark-cli 对本地媒体路径
/// 有一层"只能在 cwd 之内"的安全策略，走相对路径可以同时满足新旧版本。
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

/// 发送时用的 `(cwd, 路径参数)`：优先"文件所在目录 + `./文件名`"，
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

/// **一步式**发送媒体：`im +messages-reply --image/--file/--audio <本地路径>`。
///
/// shortcut 内部先上传再回复（同一个进程、同一次取 token）；本地路径必须相对 cwd，
/// 故统一把 cwd 设成文件所在目录并传 `./文件名`（见 [`upload_cwd_and_path`]）。
///
/// 失败信息里带 lark-cli 的 stderr（例如缺 `im:resource` 权限、超限、类型不符），
/// 由调用方决定怎么展示给用户。
pub fn reply_media_file(
    program: &Path,
    message_id: &str,
    kind: MediaKind,
    file: &Path,
) -> Result<(), String> {
    if message_id.trim().is_empty() {
        return Err("没有 message_id，无法回复".to_string());
    }
    let (cwd, path) = upload_cwd_and_path(file);
    run_lark_cli(
        program,
        &[
            "im".into(),
            "+messages-reply".into(),
            "--message-id".into(),
            message_id.trim().into(),
            kind.flag().into(),
            path,
        ],
        cwd.as_deref(),
    )
    .map(|_| ())
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

    /// 三种媒体形态各自对应的 shortcut flag（flag 决定 `msg_type`，不能错）。
    #[test]
    fn media_kind_maps_to_the_right_shortcut_flag() {
        assert_eq!(MediaKind::Image.flag(), "--image");
        assert_eq!(MediaKind::File.flag(), "--file");
        assert_eq!(MediaKind::Audio.flag(), "--audio");
    }

    /// 发送失败必须是**可读错误**，不能 panic（lark-cli 不存在是最常见的失败）。
    #[test]
    fn send_reports_missing_lark_cli_instead_of_panicking() {
        let bogus = Path::new("definitely-not-a-real-lark-cli-binary-xyz");
        let err = reply_media_file(bogus, "om_x", MediaKind::File, Path::new("whatever.pdf"))
            .unwrap_err();
        assert!(err.contains("无法启动 lark-cli"), "实际错误：{err}");
    }

    /// 空 message_id 不调 lark-cli，直接报错（防止"回复到空气里"）。
    #[test]
    fn send_rejects_empty_message_id_without_spawning() {
        let err = reply_media_file(
            Path::new("lark-cli"),
            "   ",
            MediaKind::Image,
            Path::new("a.png"),
        )
        .unwrap_err();
        assert!(err.contains("没有 message_id"), "实际错误：{err}");
    }

    /// 发送路径统一走"cwd + `./文件名`"：lark-cli 对本地媒体路径有 cwd 限制。
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
