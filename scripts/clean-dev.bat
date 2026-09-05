@echo off
rem Link-Searcher dev clean launcher (Windows)
rem
rem Thin wrapper that runs clean-dev.ps1 while bypassing the PowerShell
rem execution policy, mirroring setup-dev.bat.
rem
rem Usage:
rem   clean-dev.bat         interactive (asks for confirmation)
rem   clean-dev.bat -Yes    skip confirmation

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0clean-dev.ps1" %*
