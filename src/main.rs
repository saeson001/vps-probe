//! vps-probe — single-binary VPS probe (agent + dashboard).
//!
//! Zero third-party crates: JSON, HTTP client and HTTP server are all
//! hand-rolled on top of `std`, so the release build cross-compiles to a
//! fully static musl binary that runs on any Linux box with no runtime deps.

mod agent;
mod config;
mod http;
mod json;
mod metrics;
mod panel;
mod serve;
mod web;

use std::env;

const VERSION: &str = "2.1.0";

fn usage() -> String {
    format!(
        r#"vps-probe {VERSION}  — 轻量 VPS 探针（Rust 单文件静态二进制，零依赖）

用法:
  vps-probe serve  [--config <config.json>]      启动面板（接收 agent 上报 + 拉 3x-ui 流量）
  vps-probe agent  --server <URL> --key <密钥> --name <名称> [选项]
  vps-probe init   [--output config.json]        生成一份配置模板
  vps-probe version

serve 选项:
  --config <file>     配置文件路径（默认 ./config.json）
  --listen <addr>     覆盖配置里的监听地址，如 0.0.0.0:8899

  面板网页里可直接「添加 / 编辑 / 删除」服务器（无需手改 config.json），
  N 台 VPS 就在网页里点 N 次表单；每台 VPS 仍只需跑一次 agent。

agent 选项:
  --server <URL>      面板地址，如 http://1.2.3.4:8899
  --key <密钥>        与面板 config.json 里的 key 一致
  --name <名称>       本机在面板上显示的名字（默认取 /etc/hostname）
  --interval <秒>     上报间隔（默认 30）
  --port-check <列表> 额外探活的 TCP 端点，逗号分隔，如 1.2.3.4:39999,1.2.3.4:2096
  --once              只采集并上报一次然后退出

示例:
  # 面板机（任意一台常开的 VPS）
  vps-probe serve --config /etc/vps-probe/config.json

  # 被监控的 VPS（不需要 Python / Rust / 任何运行库）
  vps-probe agent --server http://1.2.3.4:8899 --key mykey --name HK-VPS \
                  --interval 30 --port-check 38.47.108.240:39999
"#
    )
}

fn arg(flag: &str) -> Option<String> {
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        if a == flag {
            return it.next();
        }
        if a.starts_with(flag) && a.as_bytes().get(flag.len()) == Some(&b'=') {
            return Some(a[flag.len() + 1..].to_string());
        }
    }
    None
}

fn has(flag: &str) -> bool {
    env::args().skip(1).any(|a| a == flag)
}

fn main() {
    let cmd = env::args().nth(1).unwrap_or_default();

    match cmd.as_str() {
        "serve" => {
            let path = arg("--config").unwrap_or_else(|| "config.json".to_string());
            let mut cfg = match config::load(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[vps-probe] {}", e);
                    std::process::exit(1);
                }
            };
            if let Some(l) = arg("--listen") {
                cfg.listen = l;
            }
            if cfg.vps.is_empty() {
                eprintln!(
                    "[vps-probe] 警告：配置里没有 vps 条目，面板将没有任何卡片。\n\
                     请先运行 `vps-probe init` 生成模板并填写。"
                );
            }
            eprintln!(
                "[vps-probe] {} 启动，{} 台 VPS，面板 http://{}",
                cfg.title,
                cfg.vps.len(),
                cfg.listen
            );
            if let Err(e) = serve::run(cfg, &path) {
                eprintln!("[vps-probe] 启动失败：{}", e);
                std::process::exit(1);
            }
        }

        "agent" => {
            let server = match arg("--server") {
                Some(s) => s,
                None => {
                    eprintln!("缺少 --server\n\n{}", usage());
                    std::process::exit(2);
                }
            };
            let key = arg("--key").unwrap_or_default();
            let name = arg("--name").unwrap_or_default();
            let interval: u64 = arg("--interval")
                .and_then(|s| s.parse().ok())
                .unwrap_or(30)
                .max(5);
            let ports: Vec<String> = arg("--port-check")
                .map(|s| {
                    s.split([',', ' ', ';'])
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            if has("--once") {
                let m = metrics::collect(&ports);
                println!("{}", metrics_to_json(&name, &m));
                return;
            }
            agent::run(&server, &key, &name, interval, ports);
        }

        "init" => {
            let out = arg("--output").unwrap_or_else(|| "config.json".to_string());
            let cfg = config::Config {
                vps: vec![
                    config::VpsEntry {
                        name: "HK-VPS".into(),
                        manage_url: "http://38.47.108.240:2054/".into(),
                        panel: Some(panel::PanelCreds {
                            url: "http://38.47.108.240:2054/你的webBasePath".into(),
                            username: "admin".into(),
                            password: "面板密码".into(),
                        }),
                        port_check: vec!["38.47.108.240:39999".into()],
                        quota: 0,
                    },
                    config::VpsEntry {
                        name: "JP-VPS".into(),
                        manage_url: "http://140.235.37.110:8284/mGo8HWHf".into(),
                        panel: Some(panel::PanelCreds {
                            url: "http://140.235.37.110:8284/mGo8HWHf".into(),
                            username: "admin".into(),
                            password: "面板密码".into(),
                        }),
                        port_check: vec!["140.235.37.110:29214".into()],
                        quota: 0,
                    },
                ],
                ..config::Config::default()
            };
            if let Err(e) = std::fs::write(&out, config::dump(&cfg)) {
                eprintln!("写入 {} 失败：{}", out, e);
                std::process::exit(1);
            }
            println!("已生成配置模板：{}", out);
            println!("请编辑后运行： vps-probe serve --config {}", out);
        }

        "version" | "--version" | "-V" => println!("vps-probe {}", VERSION),

        _ => println!("{}", usage()),
    }
}

fn metrics_to_json(name: &str, m: &metrics::Metrics) -> String {
    let mut root = std::collections::BTreeMap::new();
    root.insert("name".to_string(), json::J::Str(name.to_string()));
    root.insert("cpu_percent".to_string(), json::J::Num(m.cpu_percent));
    root.insert("cores".to_string(), json::J::Num(m.cores as f64));
    root.insert("mem_used".to_string(), json::J::Num(m.mem_used as f64));
    root.insert("mem_total".to_string(), json::J::Num(m.mem_total as f64));
    root.insert("mem_percent".to_string(), json::J::Num(m.mem_percent()));
    root.insert("load1".to_string(), json::J::Num(m.load1));
    root.insert("load5".to_string(), json::J::Num(m.load5));
    root.insert("load15".to_string(), json::J::Num(m.load15));
    root.insert("uptime_secs".to_string(), json::J::Num(m.uptime_secs as f64));
    root.insert("net_rx_rate".to_string(), json::J::Num(m.net_rx_rate));
    root.insert("net_tx_rate".to_string(), json::J::Num(m.net_tx_rate));
    root.insert("disk_used".to_string(), json::J::Num(m.disk_used as f64));
    root.insert("disk_total".to_string(), json::J::Num(m.disk_total as f64));
    root.insert("os".to_string(), json::J::Str(m.os.clone()));
    json::J::Obj(root).to_string()
}
