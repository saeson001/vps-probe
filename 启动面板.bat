@echo off
chcp 65001 >nul
cd /d "%~dp0"

if not exist vps-probe.exe (
  echo ============================================
  echo  找不到 vps-probe.exe
  echo  请从 GitHub Release 下载 vps-probe-windows-amd64.exe.zip
  echo  解压后把 vps-probe.exe 放进本文件夹，再双击本文件。
  echo ============================================
  pause
  exit /b 1
)

if not exist config.json (
  echo 首次运行：生成默认 config.json（监听 8899，密钥默认 change-me）
  echo 如需自定义，可先编辑 config.json 再启动；或启动后在网页里「添加服务器」。
  vps-probe.exe init
)

echo 正在启动面板（会在新窗口显示日志，关闭那个窗口即停止服务）...
start "" vps-probe.exe serve
timeout /t 2 >nul
start "" http://127.0.0.1:8899
