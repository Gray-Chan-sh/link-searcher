# FunASR 语音识别 + CAM++ 说话人分离

音频文件（mp3/wav/m4a/aac/flac/ogg/opus/wma）通过 `infer.py` 调用
[FunASR-Nano](https://modelscope.cn/models/FunAudioLLM/Fun-ASR-Nano-2512) 转写为文字，
内置 VAD 自动分段 + CAM++ 说话人分离，输出 `[Speaker X] text` 格式。
模型通过 ModelScope 自动下载（首次约 2GB，之后离线可用），无需 token。
支持中文、英文、日语及吴语/粤语/闽语等 7 大汉语方言。

## 安装依赖（一次性）

程序从 `src-tauri/models/funasr/.venv/bin/python` 调用推理脚本。
创建 venv 并安装依赖：

```bash
cd src-tauri/models/funasr
python3 -m venv .venv
.venv/bin/pip install funasr torch torchaudio
```

> torch 下载约 200MB+，请耐心等待。venv 已加入 `.gitignore`。

## 工作原理

1. `extractor/audio.rs` 先用 ffmpeg 将音频解码为 16kHz 单声道 WAV（截取前 60s）
2. 调用 `.venv/bin/python models/funasr/infer.py <wav>`
3. FunASR-Nano 识别 + VAD 分段 + CAM++ 说话人分离，结果进入全文索引

## 模型文件清单

模型首次推理时自动下载到 ModelScope 缓存（`~/.cache/modelscope` 或自定义 `MODELSCOPE_CACHE`）：
- `FunAudioLLM/Fun-ASR-Nano-2512` — 语音识别（含 Qwen3-0.6B tokenizer）
- `iic/speech_fsmn_vad_zh-cn-16k-common-pytorch` — VAD 分段
- `iic/speech_campplus_sv_zh-cn_16k-common` — 说话人分离

## 常见问题

- **`ModuleNotFoundError: No module named 'funasr'`**：venv 未创建或未装依赖，按上面「安装依赖」步骤执行
- **`[ASR 环境未安装]`**：缺少 `.venv`，同上
- 识别结果为空：音频无人声或为静音/纯音乐