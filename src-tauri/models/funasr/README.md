# FunASR 语音识别（sherpa-onnx · 纯 Rust，零 Python）

音频文件（mp3/wav/m4a/aac/flac/ogg/opus/wma）由 **sherpa-onnx** Rust 库直接推理
[Fun-ASR-Nano-2512](https://www.modelscope.cn/models/FunAudioLLM/Fun-ASR-Nano-2512) 的
ONNX 导出模型，**不再需要 Python / venv / torch / 外部进程**。

支持中文（含吴语、粤语、闽语、客家话、赣语、湘语、晋语等 7 大方言 + 26 地区口音）、英文、日文，以及歌词/说唱识别。

> ⚠️ 与旧版（Python venv）相比，**不再输出 `[Speaker X]` 说话人分离**——sherpa-onnx 的
> FunASR-Nano 接口只返回整段转写文本。索引搜索不受影响。

## 模型下载（一次性，~850MB）

程序从设置页/启动提示处点击「下载 FunASR 模型」自动完成：

1. 下载 `sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2`（GitHub release，可选
   `LINK_SEARCHER_FUNASR_MIRROR=modelscope` 走国内镜像）
2. 解压到 `<data_dir>/models/funasr/`
3. 校验 4 个必需文件后完成，无需重启即可索引

手动下载：

```bash
# GitHub
curl -L -o /tmp/model.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2
# 国内镜像（二选一）
curl -L -o /tmp/model.tar.bz2 \
  https://modelscope.cn/models/csukuangfj/asr-models/resolve/master/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2

mkdir -p ~/Library/Application\ Support/com.linksearcher.app/models/funasr
tar xjf /tmp/model.tar.bz2 -C <data_dir>/models/funasr/
```

## 文件清单（模型就绪后）

```
models/funasr/
├── encoder_adaptor.int8.onnx   # 227M 音频编码器
├── llm.int8.onnx               # 573M 语音 LLM
├── embedding.int8.onnx         # 149M 文本嵌入
└── Qwen3-0.6B/                 # tokenizer（merges.txt / tokenizer.json / vocab.json）
```

## 工作原理

1. `extractor/audio.rs` 先用 ffmpeg 将音频解码为 16kHz 单声道 WAV（完整时长，不截断）
2. sherpa-onnx `OfflineRecognizer` 进程内推理（首次加载 ~1–2s，之后复用单例）
3. 输出整段转写文本（支持 ITN 数字归一化），进入全文索引

识别器在 `OnceLock` 中全局复用，避免每个文件重新加载 ~950M int8 权重。
解码参数对齐官方配置：`greedy_search`、`temperature=1e-6`、`top_p=0.8`、
`user_prompt="语音转写："`、`max_new_tokens=512`。

## 常见问题

- **`[ASR 模型未下载]`**：模型文件缺失/不完整，到设置页点击下载
- **推理失败**：模型可能被截断（文件大小应接近 573M/227M/149M），删除后重新下载
- **识别结果为空**：音频无人声或为静音/纯音乐
- **首次构建要联网**：sherpa-onnx-sys 需要下载静态链接库（18M，一次性，缓存于 target/）