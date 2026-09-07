# vps-probe — 轻量 VPS 探针

纯 Python 标准库、**零依赖**的 VPS 状态探针。Dashboard 卡片式展示每台 VPS 的
CPU / 内存 / 负载 / 网络 / 运行时间 / **流量已用与剩余**（从 3x-ui 面板实时拉取），
**点击卡片直接跳转该 VPS 的管理页面**。Windows / Linux 通用。

## 组成

| 组件 | 跑在哪 | 说明 |
|------|--------|------|
| `agent/agent.py` | 每台 VPS（被监控机） | 采集 CPU/内存/负载/网速/磁盘/端口探活，定时 POST 给 dashboard |
| `dashboard/dashboard.py` | 任意 Win/Linux 机器（也可放其中一台 VPS） | 接收心跳 + 定时拉 3x-ui 面板流量 + 卡片 Web UI |

## 快速开始

### 1. 启动 Dashboard（任意一台机器）

```bash
cd dashboard
cp config.example.json config.json   # 编辑：密钥、VPS 列表、3x-ui 面板信息
python dashboard.py                  # Windows: python dashboard.py
# 浏览器打开 http://<这台机器IP>:8899
```

`config.json` 关键字段：
- `key`：agent 与 dashboard 的共享密钥，务必修改
- `vps[].manage_url`：点击卡片跳转的管理页地址（一般是 3x-ui 面板地址）
- `vps[].panel`：3x-ui 面板登录信息（url 含 webBasePath），用于拉取流量已用/配额

### 2. 在每台 VPS 上安装 Agent

```bash
mkdir -p /opt/vps-probe && cd /opt/vps-probe
curl -LO https://raw.githubusercontent.com/saeson001/vps-probe/main/agent/agent.py

# 前台试跑
python3 agent.py --server http://面板机IP:8899 --key 你的密钥 \
                 --name HK-VPS --interval 30 \
                 --port-check 127.0.0.1:39999   # 可选：探活本机节点端口
```

### 3. systemd 常驻（可选）

```bash
cp vps-probe-agent.service /etc/systemd/system/
# 编辑 service 文件里的 --server/--key/--name
systemctl daemon-reload
systemctl enable --now vps-probe-agent
```

## 指标说明

- **CPU / 内存 / 负载 / 磁盘**：agent 从 `/proc` 采集（Linux）；Windows 基础支持内存与 CPU
- **网络速率**：agent 网卡收发差值（不含 lo）
- **流量已用/剩余**：dashboard 定时（默认 5 分钟）登录各台 3x-ui，累加入站 `up+down`；
  配额优先取 `clientStats[].total`（按客户端），否则取入站 `total`
- **端口探活**：agent 可对指定 `host:port` 做 TCP 连通测试（如本机 VLESS 端口）

## 常见问题

- **卡片显示离线**：检查 agent 是否在跑、`--key` 是否与 dashboard 一致、dashboard 端口防火墙
- **流量为空**：检查 `panel.url` 是否含 webBasePath、账号密码、以及 dashboard 机器能否访问面板端口
- **安全建议**：`key` 用长随机串；如公网暴露 dashboard，建议用 nginx 反代加 Basic Auth 或仅内网/防火墙白名单访问
