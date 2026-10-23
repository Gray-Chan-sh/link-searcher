# SenseVoice 语音识别（sherpa-onnx · 纯 Rust，零 Python）

音频文件（mp3/wav/m4a/aac/flac/ogg/opus/wma）由 **sherpa-onnx** Rust 库直接推理
[SenseVoice](https://github.com/FunAudioLLM/SenseVoice) 的 ONNX 导出模型，
**不需要 Python / venv / torch / 外部进程**。

覆盖中文（含粤语）、英文、日文、韩文；非自回归、抗噪，自带标点与 ITN。

> 2026-10-23 起 **SenseVoice 是唯一的 ASR 引擎**。同一份 11 分钟车间噪声录音实测：
> SenseVoice 1966 中文字 / 0 个英文幻觉词 / 126s；原先的 FunASR-Nano
> 402 中文字 / 18 个幻觉词 / 332s —— 差别太大，不留回退。

> ⚠️ 说话人分离与 VAD 由**独立的可选依赖**提供（「依赖中心 → 说话人分离模型」，约 35MB），
> 装在 `<data>/models/asr/`，与 ASR 主体解耦（换引擎不影响标注）。
> 标签**不写进正文**（正文保持纯文本，检索不受影响），只在预览/导出时渲染成
> `[00:12:34 · 说话人1] 正文…`；纯文本导出与正文逐字节相同。

## 模型下载（一次性，~228MB）

程序从设置页/依赖中心点击安装即可（走 hf-mirror，直取单文件，不解压归档）：

| 文件 | 大小 | 说明 |
|------|-----:|------|
| `model.int8.onnx` | 228MB | SenseVoice int8 权重 |
| `tokens.txt` | 1.1MB | 词表 |

落在 **应用数据目录** `models/sensevoice/`（macOS：
`~/Library/Application Support/link-searcher/models/sensevoice/`）。

> **位置说明**：打包版只认数据目录（与数据库/索引同源）；开发版
> （`npm run tauri dev`）额外探测项目 `src-tauri/models/sensevoice/`。
> 也可用 `LINK_SEARCHER_SENSEVOICE_DIR` 指到别处。

手动下载：

```bash
BASE=https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main
DEST="$HOME/Library/Application Support/link-searcher/models/sensevoice"
mkdir -p "$DEST"
curl -L -o "$DEST/model.int8.onnx" "$BASE/model.int8.onnx"
curl -L -o "$DEST/tokens.txt" "$BASE/tokens.txt"
```

## 文件清单

```
models/sensevoice/          # ASR 主体（~228MB）
├── model.int8.onnx
└── tokens.txt

models/asr/                 # 辅助模型（可选，与 ASR 解耦）
├── silero_vad.onnx         # VAD 长音频分段（缺了退化为 28s 硬切）
└── diarization/
    ├── seg.onnx            # pyannote 分段
    └── emb.onnx            # CAM++ 说话人嵌入
```

> 旧版本把这些辅助模型放在 `models/funasr/models/diarization/`，
> 代码**仍兼容搜索**该旧布局，老用户不必重装。

## 工作原理

1. `extractor/audio.rs` 先用 ffmpeg 将音频解码为 16kHz 单声道 WAV（完整时长，不截断），
   降噪链 `highpass=f=80,afftdn=nr=12:nf=-40,dynaudnorm=f=200:g=15`
   （可用 `LINK_SEARCHER_ASR_FILTER` 覆盖；老 ffmpeg 缺滤镜时自动回退原样解码）
2. sherpa-onnx `OfflineRecognizer` 进程内推理（首次加载数秒，之后复用单例）
3. 切片：diarization 可用时按**说话人轮次**（整段只跑一次，编号全局一致）→ 否则 Silero VAD
   （保留段起始偏移）→ 否则 28s 硬切；单段 >28s 会再切，避免超窗口静默返回空
4. 输出**纯转写文本**（无 `[说话人N]`、无头部）+ 段级时间轴（`content_index.audio_timeline`）；
   标签只在预览/导出时渲染，正文进入全文索引与语义向量

识别器在 `OnceLock` 中全局复用，避免每个文件重新加载 228MB 权重。
解码参数：`greedy_search`、`use_itn=true`、语言默认 `zh`（`LINK_SEARCHER_ASR_LANGUAGE` 可改）。

## 常见问题

- **`[ASR 模型未就绪]`**：模型文件缺失/不完整，到「设置 → 依赖中心」安装
- **识别结果为空**：音频无人声或为静音/纯音乐
- **没有说话人标签**：缺 `models/asr/diarization/{seg,emb}.onnx`（可选依赖）
- **首次构建要联网**：sherpa-onnx-sys 需要下载静态链接库（一次性，缓存于 target/）
