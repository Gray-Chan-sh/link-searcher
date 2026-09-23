# PaddleOCR model

2025/05/31

模型为 PaddleOCR PP-OCRv5（det + rec）。Link-Searcher 不直接使用 PaddleOCR 官方 bcebos tar 包，而是将其重新发布为 GitHub Releases 资产后下载：

- 打包版：从 GitHub Releases `Gray-Chan-sh/link-searcher-models`（tag `models-v1`）下载 `paddleocr-det.onnx` / `paddleocr-rec.onnx` / `paddleocr-ppocrv5_dict.txt`，经 `ghfast.top` → `gh-proxy.com` → 官方地址的镜像链，落地为 `det.onnx` / `rec.onnx` / `ppocrv5_dict.txt`。
- 开发版：直接读取仓库内 `src-tauri/models/ppocrv5/`（已入库，无需下载）。

模型出处文档：<https://paddlepaddle.github.io/PaddleOCR/latest/version3.x/module_usage/text_detection.html> and <https://paddlepaddle.github.io/PaddleOCR/latest/version3.x/module_usage/text_recognition.html>

## det model

PP-OCRv5_mobile_det  
PP-OCRv5 的移动端文本检测模型，效率更高，适合在端侧设备部署

发布资产 `paddleocr-det.onnx` → 本地 `det.onnx`

## rec model

PP-OCRv5_mobile_rec  
PP-OCRv5_rec 是新一代文本识别模型。该模型致力于以单一模型高效、精准地支持简体中文、繁体中文、英文、日文四种主要语言，以及手写、竖版、拼音、生僻字等复杂文本场景的识别。在保持识别效果的同时，兼顾推理速度和模型鲁棒性，为各种场景下的文档理解提供高效、精准的技术支撑。

发布资产 `paddleocr-rec.onnx` → 本地 `rec.onnx`

## rec label

来自 `PP-OCRv5_mobile_rec` 的字符字典，运行时文件为 `ppocrv5_dict.txt`（发布资产 `paddleocr-ppocrv5_dict.txt`）。
