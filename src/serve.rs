//! Dashboard mode: HTTP server + background 3x-ui traffic poller.

use crate::config::Config;
use crate::http::{self, Request};
use crate::json::{self, J};
use crate::panel;
use crate::web;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Shared = Arc<Mutex<State>>;

struct State {
    /// agent name -> last reported payload
    nodes: HashMap<String, (u64, J)>,
    /// vps name -> last panel traffic snapshot
    traffic: HashMap<String, panel::PanelTraffic>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn run(cfg: Config) -> Result<(), String> {
    let state: Shared = Arc::new(Mutex::new(State {
        nodes: HashMap::new(),
        traffic: HashMap::new(),
    }));

    // ---- background panel poller ----
    let poll_state = state.clone();
    let poll_cfg = Config { ..cfg.clone() };
    std::thread::spawn(move || loop {
        for v in &poll_cfg.vps {
            if let Some(p) = &v.panel {
                eprintln!("[panel] fetching {} ({})", v.name, p.url);
                let t = panel::fetch_traffic(p, Duration::from_secs(12));
                if let Some(e) = &t.error {
                    eprintln!("[panel] {} error: {}", v.name, e);
                } else {
                    eprintln!(
                        "[panel] {} ok: {} node(s), used={} quota={}",
                        v.name,
                        t.nodes.len(),
                        t.total_used(),
                        t.total_quota()
                    );
                }
                if let Ok(mut s) = poll_state.lock() {
                    s.traffic.insert(v.name.clone(), t);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(poll_cfg.panel_interval.max(30)));
    });

    // ---- http server ----
    let key = cfg.key.clone();
    let offline_after = cfg.offline_after;
    let cfg_arc = Arc::new(cfg);
    let html = web::page(&cfg_arc.title, cfg_arc.refresh);

    let cfg_handler = cfg_arc.clone();
    http::serve(&cfg_arc.listen.clone(), move |req: &Request| -> (u16, &'static str, String) {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => (200, "text/html; charset=utf-8", html.clone()),

            ("POST", "/api/report") => {
                let got = req.query.get("key").cloned().unwrap_or_default();
                if got != key {
                    return (403, "application/json", r#"{"ok":false,"error":"bad key"}"#.to_string());
                }
                match json::parse(&req.body) {
                    Ok(v) => {
                        let name = v
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        if let Ok(mut s) = state.lock() {
                            s.nodes.insert(name.clone(), (now(), v));
                        }
                        (200, "application/json", r#"{"ok":true}"#.to_string())
                    }
                    Err(e) => (
                        400,
                        "application/json",
                        format!(r#"{{"ok":false,"error":"invalid json: {}"}}"#, e),
                    ),
                }
            }

            ("GET", "/api/data") => {
                let snap = {
                    let s = match state.lock() {
                        Ok(s) => s,
                        Err(_) => return (500, "application/json", r#"{"error":"lock"}"#.to_string()),
                    };
                    let mut list = Vec::new();
                    for v in &cfg_handler.vps {
                        let (online, metrics) = match s.nodes.get(&v.name) {
                            Some((ts, payload)) => {
                                let fresh = now().saturating_sub(*ts) <= offline_after;
                                (fresh, Some(normalize_metrics(payload)))
                            }
                            None => (false, None),
                        };
                        let mut m = BTreeMap::new();
                        m.insert("name".to_string(), J::Str(v.name.clone()));
                        m.insert("manage_url".to_string(), J::Str(v.manage_url.clone()));
                        m.insert("online".to_string(), J::Bool(online));
                        m.insert(
                            "panel_configured".to_string(),
                            J::Bool(v.panel.is_some()),
                        );
                        m.insert(
                            "metrics".to_string(),
                            metrics.unwrap_or(J::Null),
                        );

                        let quota_override = v.quota;
                        match s.traffic.get(&v.name) {
                            Some(t) if t.error.is_none() => {
                                let nodes: Vec<J> = t
                                    .nodes
                                    .iter()
                                    .map(|n| {
                                        let mut nm = BTreeMap::new();
                                        nm.insert("node".to_string(), J::Str(n.node.clone()));
                                        nm.insert("port".to_string(), J::Num(n.port as f64));
                                        nm.insert("client".to_string(), J::Str(n.client.clone()));
                                        nm.insert("up".to_string(), J::Num(n.up as f64));
                                        nm.insert("down".to_string(), J::Num(n.down as f64));
                                        nm.insert("total".to_string(), J::Num(if n.total > 0 { n.total } else { 0 } as f64));
                                        J::Obj(nm)
                                    })
                                    .collect();
                                let mut q = t.total_quota();
                                if q <= 0 && quota_override > 0 {
                                    q = quota_override;
                                }
                                let mut tm = BTreeMap::new();
                                tm.insert("used".to_string(), J::Num(t.total_used() as f64));
                                tm.insert("quota".to_string(), J::Num(q as f64));
                                tm.insert("nodes".to_string(), J::Arr(nodes));
                                m.insert("traffic".to_string(), J::Obj(tm));
                            }
                            Some(t) => {
                                m.insert(
                                    "traffic_error".to_string(),
                                    J::Str(t.error.clone().unwrap_or_default()),
                                );
                            }
                            None => {
                                if v.panel.is_some() {
                                    m.insert(
                                        "traffic_error".to_string(),
                                        J::Str("尚未拉取（首次轮询进行中）".to_string()),
                                    );
                                }
                            }
                        }
                        list.push(J::Obj(m));
                    }
                    J::Arr(list)
                };

                let mut root = BTreeMap::new();
                root.insert("ts".to_string(), J::Num(now() as f64));
                root.insert("vps".to_string(), snap);
                (200, "application/json; charset=utf-8", J::Obj(root).to_string())
            }

            ("GET", "/api/health") => (200, "application/json", r#"{"ok":true}"#.to_string()),

            _ => (404, "application/json", r#"{"error":"not found"}"#.to_string()),
        }
    })
}

/// Map the agent payload onto the field names the dashboard JS expects.
/// Accepts both the compact agent form and an already-normalized one.
fn normalize_metrics(v: &J) -> J {
    let mut m = BTreeMap::new();
    let g = |k: &str| v.get(k);

    m.insert(
        "cpu_percent".to_string(),
        J::Num(g("cpu_percent").or_else(|| g("cpu")).map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "cores".to_string(),
        J::Num(g("cores").map(|x| x.as_f64().unwrap_or(1.0)).unwrap_or(1.0)),
    );
    m.insert(
        "mem_used".to_string(),
        J::Num(g("mem_used").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "mem_total".to_string(),
        J::Num(g("mem_total").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    let mem_pct = match g("mem_percent") {
        Some(x) => x.as_f64().unwrap_or(0.0),
        None => {
            let u = g("mem_used").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0);
            let t = g("mem_total").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0);
            if t > 0.0 { u * 100.0 / t } else { 0.0 }
        }
    };
    m.insert("mem_percent".to_string(), J::Num(mem_pct));

    // load: array [1,5,15] or individual fields
    let (l1, l5, l15) = match g("load").and_then(|x| match x {
        J::Arr(_) => Some(x.as_arr()),
        _ => None,
    }) {
        Some(a) => (
            a.get(0).map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
            a.get(1).map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
            a.get(2).map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
        ),
        None => (
            g("load1").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
            g("load5").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
            g("load15").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0),
        ),
    };
    m.insert("load1".to_string(), J::Num(l1));
    m.insert("load5".to_string(), J::Num(l5));
    m.insert("load15".to_string(), J::Num(l15));

    m.insert(
        "uptime_secs".to_string(),
        J::Num(g("uptime_secs").or_else(|| g("uptime")).map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "net_rx_rate".to_string(),
        J::Num(g("net_rx_rate").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "net_tx_rate".to_string(),
        J::Num(g("net_tx_rate").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "disk_used".to_string(),
        J::Num(g("disk_used").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    m.insert(
        "disk_total".to_string(),
        J::Num(g("disk_total").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0)),
    );
    let dp = match g("disk_percent") {
        Some(x) => x.as_f64().unwrap_or(0.0),
        None => {
            let u = g("disk_used").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0);
            let t = g("disk_total").map(|x| x.as_f64().unwrap_or(0.0)).unwrap_or(0.0);
            if t > 0.0 { u * 100.0 / t } else { 0.0 }
        }
    };
    m.insert("disk_percent".to_string(), J::Num(dp));
    m.insert(
        "os".to_string(),
        J::Str(g("os").and_then(|x| x.as_str()).unwrap_or("").to_string()),
    );
    m.insert("ports".to_string(), g("ports").cloned().unwrap_or(J::Null));
    J::Obj(m)
}
