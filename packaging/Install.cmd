@echo off
choice /C YN /M "Set Quire as the default .docx application"
if errorlevel 2 (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install.ps1"
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install.ps1" -SetDefault
)
pause
