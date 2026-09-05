@echo off
rem Link-Searcher dev setup launcher (Windows)
rem
rem Thin wrapper that runs setup-dev.ps1 while bypassing the PowerShell
rem execution policy, so double-clicking this file or running it from cmd
rem works without any Set-ExecutionPolicy step. All real logic lives in
rem setup-dev.ps1 (single source of truth).
rem
rem Usage:
rem   setup-dev.bat                   run with defaults
rem   setup-dev.bat -IncludeTesseract also install tesseract OCR CLI
rem   setup-dev.bat -SkipSystemDeps   skip poppler/ffmpeg/tesseract install
rem   setup-dev.bat -ForceRedownload  force re-download sherpa-onnx archive

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup-dev.ps1" %*
