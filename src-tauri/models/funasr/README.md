# 旧 FunASR 目录（已废弃）

FunASR-Nano 已不再是 ASR 引擎，本目录不再需要 850MB 的主模型文件。

- 语音识别：见 [`../sensevoice/README.md`](../sensevoice/README.md)
- 本目录目前只被当作**兼容路径**：旧安装的 `silero_vad.onnx` 与
  `models/diarization/{seg,emb}.onnx` 仍会被搜索到（这样老用户不必重装 35MB 辅助模型）。
  新布局是 `models/asr/`（VAD）与 `models/asr/diarization/`（说话人分离）。

可以安全删除本目录下除上述辅助模型之外的内容。
