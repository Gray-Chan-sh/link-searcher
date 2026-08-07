# FunASR-Nano ONNX 模型 + CAM++ 说话人分离

下载以下 ONNX 模型到此目录即可启用语音识别：

## FunASR-Nano（语音识别）

```bash
# 从 ModelScope 下载
pip install modelscope
python -c "from modelscope.hub.snapshot_download import snapshot_download; snapshot_download('iic/speech_funasr_asr_nano-zh-cn-16k-common-vocab', cache_dir='.')"
mv iic/speech_funasr_asr_nano-zh-cn-16k-common-vocab/funasr-nano.onnx models/funasr/
```

或从 HuggingFace:
```bash
wget https://huggingface.co/FunAudioLLM/FunASR-Nano/resolve/main/funasr-nano.onnx
```

## CAM++（说话人分离）

```bash
wget https://huggingface.co/FunAudioLLM/CAM++/resolve/main/campp.onnx
mv campp.onnx models/funasr/
```

## 模型文件清单
- `funasr-nano.onnx` (~200MB) — 语音识别
- `campp.onnx` (~30MB) — 说话人分离
- `tokens.txt` — 词典文件（随 funasr-nano 下载）
