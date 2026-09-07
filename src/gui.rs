//! Native GUI launcher (Windows), built on egui/eframe.
//!
//! Double-clicking `vps-probe.exe` opens this window instead of a console.
//! It is a configuration + launcher front-end: it writes config.json and
//! spawns the *same* binary as a child process in `serve` / `agent` mode,
//! and drives the running dashboard through its existing `/api/hosts` HTTP API
//! so you can manage N VPS hosts with N form clicks — no command line, no
//! batch file, no browser form.

use crate::config::{Config, VpsEntry};
use crate::panel::PanelCreds;
use crate::http;
use crate::json::J;
use std::io::{BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A single VPS host as edited in the GUI form.
#[derive(Clone, Default)]
struct HostForm {
    name: String,
    manage_url: String,
    panel_url: String,
    panel_user: String,
    panel_pass: String,
    port_check: String,
    quota: String,
}

impl HostForm {
    fn to_entry(&self) -> VpsEntry {
        let ports: Vec<String> = self
            .port_check
            .split([',', ' ', ';'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let panel = if self.panel_url.trim().is_empty() {
            None
        } else {
            Some(PanelCreds {
                url: self.panel_url.trim().to_string(),
                username: self.panel_user.trim().to_string(),
                password: self.panel_pass.clone(),
            })
        };
        VpsEntry {
            name: self.name.trim().to_string(),
            manage_url: self.manage_url.trim().to_string(),
            panel,
            port_check: ports,
            quota: self.quota.trim().parse().unwrap_or(0),
        }
    }
}

struct GuiApp {
    // ---- dashboard config ----
    title: String,
    listen: String,
    key: String,
    refresh: String,
    offline_after: String,
    hosts: Vec<HostForm>,
    serve_running: bool,
    serve_child: Option<std::process::Child>,

    // ---- agent config ----
    agent_server: String,
    agent_key: String,
    agent_name: String,
    agent_interval: String,
    agent_ports: String,
    agent_running: bool,
    agent_child: Option<std::process::Child>,

    // ---- ui state ----
    tab: Tab,
    log: Arc<Mutex<Vec<String>>>,
    editor_open: bool,
    editor: HostForm,
    editing_index: Option<usize>,
    status: String,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Dashboard,
    Agent,
}

fn jstr(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// Build the JSON body expected by `POST /api/hosts`.
fn host_to_json(h: &HostForm) -> String {
    let panel = if h.panel_url.trim().is_empty() {
        "null".to_string()
    } else {
        format!(
            "{{\"url\":{},\"username\":{},\"password\":{}}}",
            jstr(&h.panel_url),
            jstr(&h.panel_user),
            jstr(&h.panel_pass)
        )
    };
    let ports: Vec<String> = h
        .port_check
        .split([',', ' ', ';'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|p| jstr(&p))
        .collect();
    format!(
        "{{\"name\":{},\"manage_url\":{},\"panel\":{},\"port_check\":[{}],\"quota\":{}}}",
        jstr(&h.name),
        jstr(&h.manage_url),
        panel,
        ports.join(","),
        h.quota.trim().parse::<i64>().unwrap_or(0)
    )
}

fn parse_hosts(body: &str) -> Vec<HostForm> {
    let mut out = Vec::new();
    if let Ok(J::Arr(arr)) = crate::json::parse(body) {
        for el in arr {
            let name = el.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let manage_url = el
                .get("manage_url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let (panel_url, panel_user, panel_pass) = match el.get("panel") {
                Some(p) if !p.is_null() => (
                    p.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    p.get("username").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    p.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                ),
                _ => (String::new(), String::new(), String::new()),
            };
            let port_check = match el.get("port_check") {
                Some(J::Arr(a)) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
                    .join(","),
                _ => String::new(),
            };
            let quota = el.get("quota").map(|x| x.as_i64()).unwrap_or(0).to_string();
            out.push(HostForm {
                name,
                manage_url,
                panel_url,
                panel_user,
                panel_pass,
                port_check,
                quota,
            });
        }
    }
    out
}

fn exe_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("vps-probe.exe"))
}

fn config_path() -> std::path::PathBuf {
    let mut p = exe_path();
    p.set_file_name("panel-config.json");
    p
}

fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/c", "start", "", "/b", url]).spawn();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
}

fn spawn_reader<R: Read + Send + 'static>(r: Option<R>, log: Arc<Mutex<Vec<String>>>) {
    if let Some(r) = r {
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(r);
            for line in reader.lines().map(|l| l.unwrap_or_default()) {
                if let Ok(mut v) = log.lock() {
                    v.push(line);
                    if v.len() > 600 {
                        v.remove(0);
                    }
                }
            }
        });
    }
}

impl GuiApp {
    fn new() -> Self {
        let mut app = GuiApp {
            title: "我的 VPS 监控面板".to_string(),
            listen: "0.0.0.0:8899".to_string(),
            key: "change-me".to_string(),
            refresh: "5".to_string(),
            offline_after: "90".to_string(),
            hosts: Vec::new(),
            serve_running: false,
            serve_child: None,
            agent_server: "http://127.0.0.1:8899".to_string(),
            agent_key: "change-me".to_string(),
            agent_name: String::new(),
            agent_interval: "30".to_string(),
            agent_ports: String::new(),
            agent_running: false,
            agent_child: None,
            tab: Tab::Dashboard,
            log: Arc::new(Mutex::new(Vec::new())),
            editor_open: false,
            editor: HostForm::default(),
            editing_index: None,
            status: "就绪".to_string(),
        };
        // Prefill from an existing config next to the exe.
        if let Ok(cfg) = crate::config::load(&config_path().to_string_lossy()) {
            app.title = cfg.title;
            app.listen = cfg.listen;
            app.key = cfg.key;
            app.refresh = cfg.refresh.to_string();
            app.offline_after = cfg.offline_after.to_string();
            app.hosts = cfg.vps.iter().map(|v| HostForm {
                name: v.name.clone(),
                manage_url: v.manage_url.clone(),
                panel_url: v.panel.as_ref().map(|p| p.url.clone()).unwrap_or_default(),
                panel_user: v.panel.as_ref().map(|p| p.username.clone()).unwrap_or_default(),
                panel_pass: v.panel.as_ref().map(|p| p.password.clone()).unwrap_or_default(),
                port_check: v.port_check.join(","),
                quota: if v.quota > 0 { v.quota.to_string() } else { String::new() },
            }).collect();
        }
        app
    }

    fn build_config(&self) -> Config {
        Config {
            listen: self.listen.trim().to_string(),
            key: self.key.trim().to_string(),
            title: self.title.trim().to_string(),
            refresh: self.refresh.trim().parse().unwrap_or(5),
            offline_after: self.offline_after.trim().parse().unwrap_or(90),
            panel_interval: 300,
            vps: self.hosts.iter().map(|h| h.to_entry()).collect(),
        }
    }

    fn listen_port(&self) -> u16 {
        self.listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8899)
    }

    fn start_serve(&mut self) {
        let cfg = self.build_config();
        let path = config_path();
        if let Err(e) = crate::config::save(path.to_string_lossy().as_ref(), &cfg) {
            self.status = format!("写配置失败：{}", e);
            return;
        }
        let exe = exe_path();
        match Command::new(&exe)
            .args(["serve", "--config", path.to_string_lossy().as_ref()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let out = child.stdout.take();
                let err = child.stderr.take();
                spawn_reader(out, self.log.clone());
                spawn_reader(err, self.log.clone());
                self.serve_child = Some(child);
                self.serve_running = true;
                self.status = format!("面板已启动 → http://{}", self.listen.trim());
            }
            Err(e) => self.status = format!("启动失败：{}", e),
        }
    }

    fn stop_serve(&mut self) {
        if let Some(mut c) = self.serve_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.serve_running = false;
        self.status = "面板已停止".to_string();
    }

    fn start_agent(&mut self) {
        let exe = exe_path();
        let mut args: Vec<String> = vec![
            "agent".into(),
            "--server".into(),
            self.agent_server.trim().to_string(),
            "--key".into(),
            self.agent_key.trim().to_string(),
        ];
        if !self.agent_name.trim().is_empty() {
            args.push("--name".into());
            args.push(self.agent_name.trim().to_string());
        }
        if self.agent_interval.trim().parse::<u64>().unwrap_or(30) != 30 {
            args.push("--interval".into());
            args.push(self.agent_interval.trim().to_string());
        }
        if !self.agent_ports.trim().is_empty() {
            args.push("--port-check".into());
            args.push(self.agent_ports.trim().to_string());
        }
        match Command::new(&exe)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let out = child.stdout.take();
                let err = child.stderr.take();
                spawn_reader(out, self.log.clone());
                spawn_reader(err, self.log.clone());
                self.agent_child = Some(child);
                self.agent_running = true;
                self.status = format!("Agent 已启动 → {}", self.agent_server.trim());
            }
            Err(e) => self.status = format!("启动失败：{}", e),
        }
    }

    fn stop_agent(&mut self) {
        if let Some(mut c) = self.agent_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.agent_running = false;
        self.status = "Agent 已停止".to_string();
    }

    /// Push a host to the running dashboard via its HTTP API (best-effort).
    fn push_host_live(&mut self, h: &HostForm) -> bool {
        if !self.serve_running {
            return true; // not running yet; will be included in config on start
        }
        let port = self.listen_port();
        let url = format!("http://127.0.0.1:{}/api/hosts?key={}", port, self.key.trim());
        match http::request(
            "POST",
            &url,
            &[("Content-Type", "application/json")],
            Some(&host_to_json(h)),
            Duration::from_secs(10),
        ) {
            Ok(r) if r.status == 200 => true,
            Ok(r) => {
                self.status = format!("添加主机失败：HTTP {}", r.status);
                false
            }
            Err(e) => {
                self.status = format!("添加主机失败：{}", e);
                false
            }
        }
    }

    fn delete_host_live(&self, name: &str) {
        if !self.serve_running {
            return;
        }
        let port = self.listen_port();
        let enc = url_encode(name);
        let url = format!("http://127.0.0.1:{}/api/hosts/{}?key={}", port, enc, self.key.trim());
        let _ = http::request("DELETE", &url, &[], None, Duration::from_secs(10));
    }

    fn refresh_hosts(&mut self) {
        if !self.serve_running {
            self.status = "面板未运行，无法刷新".to_string();
            return;
        }
        let port = self.listen_port();
        let url = format!("http://127.0.0.1:{}/api/hosts", port);
        match http::request("GET", &url, &[], None, Duration::from_secs(10)) {
            Ok(r) if r.status == 200 => {
                self.hosts = parse_hosts(&r.body);
                self.status = format!("已刷新，共 {} 台", self.hosts.len());
            }
            Ok(r) => self.status = format!("刷新失败：HTTP {}", r.status),
            Err(e) => self.status = format!("刷新失败：{}", e),
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(400));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("VPS Probe {}", VERSION));
                ui.label("  ·  原生 Windows 面板 / Agent 启动器");
            });
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.tab == Tab::Dashboard, "面板模式 (Dashboard)")
                    .clicked()
                {
                    self.tab = Tab::Dashboard;
                }
                if ui
                    .selectable_label(self.tab == Tab::Agent, "作为 Agent 上报")
                    .clicked()
                {
                    self.tab = Tab::Agent;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&self.status).color(egui::Color32::YELLOW));
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Dashboard => self.dashboard_ui(ui),
            Tab::Agent => self.agent_ui(ui),
        });

        // ---- host editor modal ----
        if self.editor_open {
            let mut open = self.editor_open;
            egui::Window::new(if self.editing_index.is_some() {
                "编辑服务器"
            } else {
                "添加服务器"
            })
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("名称（面板上的显示名，需唯一）:");
                ui.text_edit_singleline(&mut self.editor.name);
                ui.label("管理页面 URL（点击卡片跳转）:");
                ui.text_edit_singleline(&mut self.editor.manage_url);
                ui.separator();
                ui.label("3x-ui 面板（可选，用于拉流量）:");
                ui.horizontal(|ui| {
                    ui.label("地址:");
                    ui.text_edit_singleline(&mut self.editor.panel_url);
                });
                ui.horizontal(|ui| {
                    ui.label("账号:");
                    ui.text_edit_singleline(&mut self.editor.panel_user);
                    ui.label("密码:");
                    ui.text_edit_singleline(&mut self.editor.panel_pass);
                });
                ui.label("端口探活（逗号分隔，如 38.47.108.240:39999）:");
                ui.text_edit_singleline(&mut self.editor.port_check);
                ui.label("流量配额（字节，0=用面板值）:");
                ui.text_edit_singleline(&mut self.editor.quota);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        if self.editor.name.trim().is_empty() {
                            self.status = "名称不能为空".to_string();
                        } else {
                            let h = self.editor.clone();
                            let ok = self.push_host_live(&h);
                            if ok {
                                match self.editing_index {
                                    Some(i) => {
                                        if i < self.hosts.len() {
                                            self.hosts[i] = h;
                                        }
                                    }
                                    None => self.hosts.push(h),
                                }
                                self.editor_open = false;
                                self.editing_index = None;
                                self.status = "已保存服务器".to_string();
                            }
                        }
                    }
                    if ui.button("取消").clicked() {
                        self.editor_open = false;
                        self.editing_index = None;
                    }
                });
            });
            if !open {
                self.editor_open = false;
                self.editing_index = None;
            }
        }
    }
}

impl GuiApp {
    fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("面板设置");
            ui.horizontal(|ui| {
                ui.label("标题:");
                ui.text_edit_singleline(&mut self.title);
                ui.label("监听:");
                ui.text_edit_singleline(&mut self.listen);
            });
            ui.horizontal(|ui| {
                ui.label("共享密钥:");
                ui.text_edit_singleline(&mut self.key);
                ui.label("刷新(s):");
                ui.text_edit_singleline(&mut self.refresh);
                ui.label("离线判定(s):");
                ui.text_edit_singleline(&mut self.offline_after);
            });
            ui.horizontal(|ui| {
                if !self.serve_running {
                    if ui.button("▶ 启动面板").clicked() {
                        self.start_serve();
                    }
                } else {
                    if ui.button("■ 停止面板").clicked() {
                        self.stop_serve();
                    }
                    if ui.button("打开网页面板").clicked() {
                        open_url(&format!("http://127.0.0.1:{}", self.listen_port()));
                    }
                }
            });
        });

        ui.group(|ui| {
            ui.heading("服务器列表（被监控的 VPS）");
            ui.horizontal(|ui| {
                if ui.button("+ 添加服务器").clicked() {
                    self.editor = HostForm::default();
                    self.editing_index = None;
                    self.editor_open = true;
                }
                if ui.button("刷新").clicked() {
                    self.refresh_hosts();
                }
                ui.label(format!("共 {} 台", self.hosts.len()));
            });
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                let hosts = self.hosts.clone();
                for (i, h) in hosts.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{}  {}  {}",
                            h.name,
                            if h.panel_url.is_empty() {
                                "（无面板）"
                            } else {
                                "（含流量面板）"
                            },
                            if h.port_check.is_empty() {
                                "".to_string()
                            } else {
                                format!("端口:{}", h.port_check)
                            }
                        ));
                        if ui.button("编辑").clicked() {
                            self.editor = h.clone();
                            self.editing_index = Some(i);
                            self.editor_open = true;
                        }
                        if ui.button("删除").clicked() {
                            self.delete_host_live(&h.name);
                            self.hosts.remove(i);
                        }
                    });
                }
            });
        });

        self.log_ui(ui);
    }

    fn agent_ui(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Agent 设置（本机作为被监控端上报）");
            ui.horizontal(|ui| {
                ui.label("面板地址:");
                ui.text_edit_singleline(&mut self.agent_server);
            });
            ui.horizontal(|ui| {
                ui.label("密钥:");
                ui.text_edit_singleline(&mut self.agent_key);
                ui.label("本机名:");
                ui.text_edit_singleline(&mut self.agent_name);
            });
            ui.horizontal(|ui| {
                ui.label("间隔(s):");
                ui.text_edit_singleline(&mut self.agent_interval);
                ui.label("端口探活:");
                ui.text_edit_singleline(&mut self.agent_ports);
            });
            ui.horizontal(|ui| {
                if !self.agent_running {
                    if ui.button("▶ 启动 Agent").clicked() {
                        if self.agent_server.trim().is_empty() {
                            self.status = "请填写面板地址".to_string();
                        } else {
                            self.start_agent();
                        }
                    }
                } else if ui.button("■ 停止 Agent").clicked() {
                    self.stop_agent();
                }
            });
        });
        self.log_ui(ui);
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("运行日志");
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
                for line in log.iter() {
                    ui.label(line);
                }
            });
        });
    }
}

/// Entry point for the GUI build. Never returns (runs until the window closes).
pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([920.0, 660.0]),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "VPS Probe",
        options,
        Box::new(|_cc| Ok(Box::new(GuiApp::new()) as Box<dyn eframe::App>)),
    );
}
