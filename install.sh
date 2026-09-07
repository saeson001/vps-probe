#!/usr/bin/env bash
# vps-probe 一键安装脚本
#
# 目标机无需 Python / Rust / 任何运行库 —— 只下载一个静态编译的二进制。
#
#   curl -fsSL https://raw.githubusercontent.com/saeson001/vps-probe/main/install.sh | bash
#
# 非交互（推荐用于批量部署，用环境变量传参）：
#   MODE=agent SERVER=http://1.2.3.4:8899 KEY=mykey NAME=HK-VPS INTERVAL=30 \
#     PORT_CHECK=38.47.108.240:39999 \
#     curl -fsSL .../install.sh | bash
#
#   MODE=serve PORT=8899 KEY=mykey curl -fsSL .../install.sh | bash
#
# 卸载：  bash install.sh --uninstall
# 更新：  bash install.sh --update

set -eu

REPO="saeson001/vps-probe"
BIN_DIR="${BIN_DIR:-/usr/local/bin}"
BIN="$BIN_DIR/vps-probe"
CONF_DIR="/etc/vps-probe"
CONF="$CONF_DIR/config.json"
SVC_AGENT="vps-probe-agent"
SVC_SERVE="vps-probe"
VERSION="${VERSION:-latest}"

C_RESET='\033[0m'; C_G='\033[32m'; C_Y='\033[33m'; C_R='\033[31m'; C_B='\033[36m'
info() { printf "${C_G}[+]${C_RESET} %s\n" "$*"; }
warn() { printf "${C_Y}[!]${C_RESET} %s\n" "$*"; }
err()  { printf "${C_R}[x]${C_RESET} %s\n" "$*" >&2; }
ask()  { printf "${C_B}[?]${C_RESET} %s " "$*"; }

# ---------------------------------------------------------------- uninstall
if [ "${1:-}" = "--uninstall" ]; then
  systemctl stop "$SVC_AGENT" 2>/dev/null || true
  systemctl stop "$SVC_SERVE" 2>/dev/null || true
  systemctl disable "$SVC_AGENT" 2>/dev/null || true
  systemctl disable "$SVC_SERVE" 2>/dev/null || true
  rm -f "/etc/systemd/system/$SVC_AGENT.service" "/etc/systemd/system/$SVC_SERVE.service"
  rm -f "$BIN"; rm -rf "$CONF_DIR"
  systemctl daemon-reload 2>/dev/null || true
  info "已卸载 vps-probe"
  exit 0
fi

if [ "${1:-}" = "--update" ]; then
  UPDATE_ONLY=1
fi

# ---------------------------------------------------------------- arch
OS="$(uname -s)"
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64) ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  armv7l|armv6l) ARCH="armv7" ;;
  *) err "不支持的架构：$ARCH_RAW"; exit 1 ;;
esac

case "$OS" in
  Linux)  ASSET="vps-probe-linux-$ARCH" ; EXT="tar.gz" ;;
  Darwin) err "macOS 请直接 cargo build，或下载源码自行编译"; exit 1 ;;
  *)      err "不支持的系统：$OS（Windows 请从 Release 页面下载 exe）"; exit 1 ;;
esac
[ "$ARCH" = "armv7" ] && { err "armv7 暂未提供预编译包，请 cargo build --release"; exit 1; }

# ---------------------------------------------------------------- version
if [ "$VERSION" = "latest" ]; then
  DL="https://github.com/$REPO/releases/latest/download"
else
  DL="https://github.com/$REPO/releases/download/$VERSION"
fi
URL="$DL/$ASSET.$EXT"

# ---------------------------------------------------------------- download
need() { command -v "$1" >/dev/null 2>&1 || { err "缺少命令：$1"; exit 1; }; }
need curl
need tar

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

info "下载 $ASSET.$EXT  ($VERSION)"
if ! curl -fsSL --retry 3 --connect-timeout 15 -o "$TMP/pkg.$EXT" "$URL"; then
  err "下载失败：$URL"
  warn "若 Release 尚未生成，请到 https://github.com/$REPO/releases 查看，或本地编译：cargo build --release"
  exit 1
fi

tar xzf "$TMP/pkg.$EXT" -C "$TMP"
[ -f "$TMP/$ASSET" ] || { err "压缩包内容异常"; exit 1; }

install -m 0755 "$TMP/$ASSET" "$BIN"
info "已安装 $BIN"
"$BIN" version || true

if [ -n "${UPDATE_ONLY:-}" ]; then
  systemctl restart "$SVC_AGENT" 2>/dev/null || true
  systemctl restart "$SVC_SERVE" 2>/dev/null || true
  info "更新完成（如已注册服务已自动重启）"
  exit 0
fi

# ---------------------------------------------------------------- mode
if [ "$(id -u)" != "0" ]; then
  warn "非 root 运行：已装好二进制，但无法写入 /etc 或注册 systemd"
  warn "请手动运行： $BIN agent --server <面板地址> --key <密钥> --name <名称>"
  exit 0
fi

MODE="${MODE:-}"
if [ -z "$MODE" ]; then
  echo
  echo "请选择要安装的组件："
  echo "  1) agent   —— 装在这台 VPS 上，上报 CPU/内存/负载/网速（每台被监控的 VPS 都要装）"
  echo "  2) serve   —— 面板端，接收上报 + 拉 3x-ui 流量并展示卡片（只需装一台）"
  echo "  3) 都不装  —— 只放二进制"
  ask "输入 1/2/3 [默认 1]:"
  read -r choice
  case "${choice:-1}" in
    1) MODE=agent ;;
    2) MODE=serve ;;
    *) info "完成。二进制位于 $BIN"; exit 0 ;;
  esac
fi

have_systemd=0
command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload >/dev/null 2>&1 && have_systemd=1

# ---------------------------------------------------------------- agent
if [ "$MODE" = "agent" ]; then
  SERVER="${SERVER:-}"; KEY="${KEY:-}"; NAME="${NAME:-}"; INTERVAL="${INTERVAL:-30}"; PORT_CHECK="${PORT_CHECK:-}"

  if [ -t 0 ]; then
    [ -z "$SERVER" ] && { ask "面板地址（如 http://1.2.3.4:8899）:"; read -r SERVER; }
    [ -z "$KEY" ]    && { ask "共享密钥（与面板 config.json 的 key 一致）:"; read -r KEY; }
    [ -z "$NAME" ]   && { ask "本机名称 [默认 $(hostname 2>/dev/null || echo vps)]:"; read -r NAME; }
  fi
  if [ -z "$SERVER" ]; then
    err "缺少面板地址（agent 必须知道往哪个面板上报）"
    echo "  用法一 · 非交互（推荐，用环境变量传参）："
    echo "    MODE=agent SERVER=http://面板IP:8899 KEY=共享密钥 NAME=本机名 \\"
    echo "      curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | bash"
    echo "  用法二 · 先下载再交互运行（避免 curl|bash 吃掉了终端输入）："
    echo "    curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh -o /tmp/install.sh"
    echo "    bash /tmp/install.sh"
    exit 1
  fi
  [ -z "$KEY" ] && {
    err "缺少共享密钥（KEY，须与面板 config.json 的 key 一致）"
    echo "  传入方式同上： MODE=agent SERVER=... KEY=你的密钥 ... curl ...|bash"
    exit 1
  }
  NAME="${NAME:-$(hostname 2>/dev/null || echo vps)}"

  ARGS="--server $SERVER --key $KEY --name $NAME --interval $INTERVAL"
  [ -n "$PORT_CHECK" ] && ARGS="$ARGS --port-check $PORT_CHECK"

  if [ "$have_systemd" = "1" ]; then
    cat > "/etc/systemd/system/$SVC_AGENT.service" <<EOF
[Unit]
Description=vps-probe agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN agent $ARGS
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now "$SVC_AGENT" >/dev/null 2>&1
    sleep 1
    info "agent 已注册为 systemd 服务：$SVC_AGENT"
    echo
    systemctl --no-pager --full status "$SVC_AGENT" 2>/dev/null | head -12 || true
    info "查看日志： journalctl -u $SVC_AGENT -f"
  else
    warn "未检测到 systemd，改用 nohup 后台运行"
    printf '%s\n' "#!/bin/sh" "exec $BIN agent $ARGS" > /usr/local/bin/vps-probe-agent-start
    chmod +x /usr/local/bin/vps-probe-agent-start
    nohup /usr/local/bin/vps-probe-agent-start >/var/log/vps-probe-agent.log 2>&1 &
    info "已后台启动，日志 /var/log/vps-probe-agent.log"
  fi
fi

# ---------------------------------------------------------------- serve
if [ "$MODE" = "serve" ]; then
  mkdir -p "$CONF_DIR"
  if [ ! -f "$CONF" ]; then
    "$BIN" init --output "$CONF" >/dev/null
    KEY="${KEY:-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
    PORT="${PORT:-8899}"
    sed -i "s|\"key\": \"change-me\"|\"key\": \"$KEY\"|" "$CONF" 2>/dev/null || true
    sed -i "s|\"listen\": \"0.0.0.0:8899\"|\"listen\": \"0.0.0.0:$PORT\"|" "$CONF" 2>/dev/null || true
    info "已生成配置 $CONF（请编辑 vps 列表：名称 / manage_url / panel 凭据）"
    info "共享密钥：$KEY"
    warn "记得放行端口 $PORT（如 ufw allow $PORT/tcp）"
  else
    info "配置文件已存在：$CONF（未覆盖）"
  fi

  if [ "$have_systemd" = "1" ]; then
    cat > "/etc/systemd/system/$SVC_SERVE.service" <<EOF
[Unit]
Description=vps-probe dashboard
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN serve --config $CONF
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now "$SVC_SERVE" >/dev/null 2>&1
    sleep 1
    info "面板已启动：$SVC_SERVE"
    systemctl --no-pager --full status "$SVC_SERVE" 2>/dev/null | head -10 || true
    info "查看日志： journalctl -u $SVC_SERVE -f"
  else
    nohup "$BIN" serve --config "$CONF" >/var/log/vps-probe.log 2>&1 &
    info "已后台启动，日志 /var/log/vps-probe.log"
  fi
fi

echo
info "完成。"
