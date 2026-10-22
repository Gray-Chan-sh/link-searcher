# FunASR 语音识别（sherpa-onnx · 纯 Rust，零 Python）

音频文件（mp3/wav/m4a/aac/flac/ogg/opus/wma）由 **sherpa-onnx** Rust 库直接推理
[Fun-ASR-Nano-2512](https://www.modelscope.cn/models/FunAudioLLM/Fun-ASR-Nano-2512) 的
ONNX 导出模型，**不再需要 Python / venv / torch / 外部进程**。

支持中文（含吴语、粤语、闽语、客家话、赣语、湘语、晋语等 7 大方言 + 26 地区口音）、英文、日文，以及歌词/说唱识别。

> ⚠️ 说话人分离由**独立的可选依赖**提供（「依赖中心 → 说话人分离模型」，约 35MB）：
> sherpa-onnx 的 `OfflineSpeakerDiarization` 流水线（pyannote 分段 + CAM++ 嵌入），
> 模型就绪时对整段音频跑一次，得到全局一致的说话人时间线。
> 标签**不写进正文**（正文保持纯文本，检索不受影响），只在预览/导出时渲染成
> `[00:12:34 · 说话人1] 正文…`；纯文本导出与正文逐字节相同。

## 模型下载（一次性，~850MB）

程序从设置页/启动提示处点击「下载 FunASR 模型」自动完成：

1. 下载 `sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2`（默认 GitHub，失败自动切
   ModelScope 镜像；`LINK_SEARCHER_FUNASR_MIRROR=modelscope` 强制镜像）
2. 解压到 **应用数据目录** `models/funasr/`（macOS：
   `~/Library/Application Support/link-searcher/models/funasr/`）
3. 校验 4 个必需文件后完成，无需重启即可索引

> **位置说明**：打包版（.app/安装包）只认数据目录，与数据库/索引同源；
> 开发版（`npm run tauri dev`）额外探测项目 `src-tauri/models/funasr/`。
> 模型只需下载一次，升级重装 app 后仍在。

手动下载（macOS）：

```bash
# GitHub
curl -L -o /tmp/model.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2
# 国内镜像（二选一）
curl -L -o /tmp/model.tar.bz2 \
  https://modelscope.cn/models/csukuangfj/asr-models/resolve/master/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2

mkdir -p ~/Library/Application\ Support/link-searcher/models/funasr
tar xjf /tmp/model.tar.bz2 -C ~/Library/Application\ Support/link-searcher/models/funasr/
# 归档内含顶层目录 sherpa-onnx-funasr-nano-int8-2025-12-30/，需将其内容平铺到 models/funasr/ 下
```

## 文件清单（模型就绪后）

```
models/funasr/
├── encoder_adaptor.int8.onnx   # 227M 音频编码器
├── llm.int8.onnx               # 573M 语音 LLM
├── embedding.int8.onnx         # 149M 文本嵌入
├── silero_vad.onnx             # VAD 长音频分段（独立下载，非归档内）
├── Qwen3-0.6B/                 # tokenizer（merges.txt / tokenizer.json / vocab.json）
└── models/diarization/         # 说话人分离模型（依赖中心可选，~35MB）
    ├── seg.onnx                # pyannote 分段
    └── emb.onnx                # CAM++ 说话人嵌入
```

## 工作原理

1. `extractor/audio.rs` 先用 ffmpeg 将音频解码为 16kHz 单声道 WAV（完整时长，不截断）
2. sherpa-onnx `OfflineRecognizer` 进程内推理（首次加载 ~1–2s，之后复用单例）
3. 切片：diarization 可用时按**说话人轮次**（整段只跑一次，编号全局一致）→ 否则 Silero VAD
   （保留段起始偏移）→ 否则 28s 硬切；单段 >28s 会再切，避免超窗口静默返回空
4. 输出**纯转写文本**（无 `[说话人N]`、无头部）+ 段级时间轴（`content_index.audio_timeline`）；
   标签只在预览/导出时渲染，正文进入全文索引与语义向量

识别器在 `OnceLock` 中全局复用，避免每个文件重新加载 ~950M int8 权重。
解码参数对齐官方配置：`greedy_search`、`temperature=1e-6`、`top_p=0.8`、
`user_prompt="语音转写："`、`max_new_tokens=1024`。

## 常见问题

- **`[ASR 模型未下载]`**：模型文件缺失/不完整，到设置页点击下载
- **推理失败**：模型可能被截断（文件大小应接近 573M/227M/149M），删除后重新下载
- **识别结果为空**：音频无人声或为静音/纯音乐
- **首次构建要联网**：sherpa-onnx-sys 需要下载静态链接库（18M，一次性，缓存于 target/）