# vps-probe

轻量 VPS 探针：**Rust 单文件静态二进制**，零第三方依赖、零运行时要求。
一台机器上放一个 `vps-probe` 文件就能跑 —— **目标机不需要 Python、不需要 Rust、不需要 glibc 以外的任何东西**（musl 静态链接，连 glibc 都不需要）。

- **`agent`** —— 装在每台被监控的 VPS 上，采集 CPU / 内存 / 负载 / 网速 / 磁盘 / 运行时间 + TCP 端口探活，定时上报
- **`serve`** —— 面板端（只需装一台），卡片式网页展示所有 VPS 状态，**定时登录 3x-ui 拉取每个节点的流量明细**，点击卡片直接跳转管理页

---

## Windows 原生 GUI（双击即用，零命令行）

Windows 的 `vps-probe-windows-amd64.exe` **内建 GUI**：直接双击运行就会弹出一个原生窗口（不用命令行、不用 .bat、不用浏览器填表）。

- **面板模式**：填标题 / 监听地址 / 共享密钥，点「▶ 启动面板」即可；点「打开网页面板」看实时卡片
- **服务器列表**：点「+ 添加服务器」填表单（名字 / 管理页 URL / 3x-ui 面板账号密码 / 端口探活），N 台 VPS 就点 N 次；每台可编辑 / 删除
- **作为 Agent 上报**：切到「作为 Agent 上报」页，填面板地址 + 密钥 + 本机名，点「▶ 启动 Agent」即可把自己这台机器也监控起来
- 配置自动保存在 exe 同目录的 `panel-config.json`，下次打开自动回填
- 运行状态、Agent 上报日志实时显示在窗口底部

> GUI 只是配置 + 启动器：它把同样的 `vps-probe.exe` 以 `serve` / `agent` 子进程方式拉起，并通过面板自带
> 的 `/api/hosts` 接口管理主机——所以核心仍是单个零依赖二进制。
> Linux 服务器版（`linux-amd64` / `linux-arm64`）为保持 musl 静态零依赖，默认**不带 GUI**，请走下方命令行 / 一键脚本。

---

## 常见问题

**Q：`agent` 是在 VPS 上跑吗？VPS 没装 Python 怎么办？**

是，每台你想监控的 VPS 上都要跑一个 `agent`。
Rust 版就是为解决这个而写的：产物是 **静态链接的单个二进制**，不依赖 Python / Node / glibc / 任何运行库。丢上去 `chmod +x` 直接执行，或用下面的一键脚本自动安装。

**Q：面板怎么知道是哪个节点的流量？**

`serve` 会登录每台 3x-ui 面板，读取 `inbounds/list`，按 **入站（remark + port）+ 客户端（email）** 拆分成多行。卡片上既显示汇总的「已用 / 总量 / 剩余」，也逐条列出每个节点：

```
香港-saeson1:39999 · hk@a        120 GB 剩余 / 200 GB
日本-saeson:29214  · jp@a        150 GB 剩余 / 200 GB
泰国-liao:18706    · th@a        210 GB 剩余 / 300 GB
```

---

## 一键安装（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/saeson001/vps-probe/main/install.sh | bash
```

脚本会：探测架构 → 从 GitHub Release 下载对应静态二进制 → 装到 `/usr/local/bin/vps-probe` → 交互式问你是装 `agent` 还是 `serve` → 自动注册 systemd 常驻。

### 非交互部署（批量装机好用）

```bash
# 装 agent（每台被监控的 VPS）
MODE=agent \
SERVER=http://面板机IP:8899 \
KEY=你的共享密钥 \
NAME=HK-VPS \
INTERVAL=30 \
PORT_CHECK=38.47.108.240:39999,38.47.108.240:2096 \
curl -fsSL https://raw.githubusercontent.com/saeson001/vps-probe/main/install.sh | bash

# 装面板（只需一台）
MODE=serve PORT=8899 KEY=你的共享密钥 \
curl -fsSL https://raw.githubusercontent.com/saeson001/vps-probe/main/install.sh | bash
```

### 其他命令

```bash
bash install.sh --update      # 只更新二进制并重启服务
bash install.sh --uninstall   # 卸载（停服务 + 删文件）
```

---

## 手动使用

```
vps-probe serve  [--config config.json] [--listen 0.0.0.0:8899]
vps-probe agent  --server <URL> --key <密钥> --name <名称> [--interval 30] [--port-check a:1,b:2] [--once]
vps-probe init   [--output config.json]
vps-probe version
```

`--once` 只采集一次并打印 JSON 后退出（调试用）。

---

## 配置（`serve` 端）

`config.json`：

```json
{
  "listen": "0.0.0.0:8899",
  "key": "改成一个自己的共享密钥",
  "title": "VPS 探针",
  "refresh": 5,
  "offline_after": 90,
  "panel_interval": 300,
  "vps": [
    {
      "name": "HK-VPS",
      "manage_url": "http://<HK的IP>:<3x-ui端口>/",
      "panel": {
        "url": "http://<HK的IP>:<3x-ui端口>/<webBasePath>",
        "username": "admin",
        "password": "面板密码"
      },
      "port_check": ["38.47.108.240:39999"],
      "quota": 0
    }
  ]
}
```

| 字段 | 说明 |
|---|---|
| `name` | 卡片标题，必须与 agent 的 `--name` **完全一致**（这是关联上报数据的键） |
| `manage_url` | **点击卡片跳转的地址**（厂商后台 / 3x-ui 面板） |
| `panel` | 3x-ui 凭据，用于拉流量；不需要流量就设为 `null` |
| `port_check` | 提示 agent 额外探活的 TCP 端点 |
| `quota` | 可选，面板没设流量限制时的手动额度兜底（字节） |
| `key` | agent 上报用的共享密钥，必须一致，否则 403 |

> ⚠️ 面板地址必须是 **http**（本构建不含 TLS）。若面板是 https，请在前面套一层本机 http 反向代理。

---

## 防火墙

```bash
# 面板机
ufw allow 8899/tcp
# agent 只要能出网即可，无需放行入站端口
```

---

## 自己编译

```bash
cargo build --release
# Linux 静态版（零依赖）
rustup target add x86_64-unknown-linux-musl
sudo apt install musl-tools
cargo build --release --target x86_64-unknown-linux-musl

# Windows 原生 GUI 版（含 egui/eframe，双击即弹窗口）
cargo build --release --features gui --target x86_64-pc-windows-msvc
```

产物：`target/x86_64-unknown-linux-musl/release/vps-probe`（`ldd` 显示 `statically linked`）。
Windows GUI 版默认带 `gui` 特性由 CI 自动构建。

打 tag 会自动触发 GitHub Actions 编译 amd64 / arm64 / Windows 并发布 Release。

---

## 与 Python 版的关系

`agent/` 与 `dashboard/` 目录下的 Python 版保留作为参考，新部署请用根目录的 Rust 版。
