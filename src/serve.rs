//! Dashboard mode: HTTP server + background 3x-ui traffic poller.
//!
//! The VPS host list lives in shared mutable state so the web UI can add /
//! edit / remove hosts at runtime (no more hand-editing config.json). Every
//! change is persisted back to config.json immediately.

use crate::config::{Config, VpsEntry};
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
    /// live host list (mutable via the dashboard UI)
    vps: Vec<VpsEntry>,
    /// config.json path, so host edits can be persisted
    cfg_path: String,
    /// full base config (listen/key/title/...), kept so we can re-save
    base: Config,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn run(cfg: Config, cfg_path: &str) -> Result<(), String> {
    let state: Shared = Arc::new(Mutex::new(State {
        nodes: HashMap::new(),
        traffic: HashMap::new(),
        vps: cfg.vps.clone(),
        cfg_path: cfg_path.to_string(),
        base: cfg.clone(),
    }));

    // ---- background panel poller ----
    let poll_state = state.clone();
    std::thread::spawn(move || loop {
        let (vps, panel_interval) = {
            let s = match poll_state.lock() {
                Ok(s) => s,
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(30));
                    continue;
                }
            };
            (s.vps.clone(), s.base.panel_interval)
        };
        for v in &vps {
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
        std::thread::sleep(Duration::from_secs(panel_interval.max(30)));
    });

    // ---- http server ----
    let html = web::page(
        &state_base_title(&state),
        state_base_refresh(&state),
        &state_base_key(&state),
    );

    let handler_state = state.clone();
    http::serve(&state_base_listen(&state), move |req: &Request| -> (u16, &'static str, String) {
        let st: Shared = handler_state.clone();
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => (200, "text/html; charset=utf-8", html.clone()),

            ("POST", "/api/report") => {
                let got = req.query.get("key").cloned().unwrap_or_default();
                let key = st_key(&st);
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
                        if let Ok(mut s) = st.lock() {
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

            // ---- host management (dashboard UI) ----
            ("GET", "/api/hosts") => {
                let list = match st.lock() {
                    Ok(s) => hosts_to_json(&s.vps),
                    Err(_) => J::Arr(Vec::new()),
                };
                (200, "application/json; charset=utf-8", list.to_string())
            }

            ("POST", "/api/hosts") => {
                let key = st_key(&st);
                let got = req.query.get("key").cloned().unwrap_or_default();
                if got != key {
                    return (403, "application/json", r#"{"ok":false,"error":"bad key"}"#.to_string());
                }
                match json::parse(&req.body) {
                    Ok(v) => match entry_from_json(&v) {
                        Ok(entry) => match upsert_host(&st, entry) {
                            Ok(_) => (200, "application/json", r#"{"ok":true}"#.to_string()),
                            Err(e) => (
                                500,
                                "application/json",
                                format!(r#"{{"ok":false,"error":"{}"}}"#, e),
                            ),
                        },
                        Err(e) => (
                            400,
                            "application/json",
                            format!(r#"{{"ok":false,"error":"{}"}}"#, e),
                        ),
                    },
                    Err(e) => (
                        400,
                        "application/json",
                        format!(r#"{{"ok":false,"error":"invalid json: {}"}}"#, e),
                    ),
                }
            }

            ("DELETE", p) if p.starts_with("/api/hosts/") => {
                let key = st_key(&st);
                let got = req.query.get("key").cloned().unwrap_or_default();
                if got != key {
                    return (403, "application/json", r#"{"ok":false,"error":"bad key"}"#.to_string());
                }
                let name = url_decode_name(&p["/api/hosts/".len()..]);
                match remove_host(&st, &name) {
                    Ok(removed) => (
                        200,
                        "application/json",
                        format!(r#"{{"ok":true,"removed":{}}}"#, removed),
                    ),
                    Err(e) => (
                        500,
                        "application/json",
                        format!(r#"{{"ok":false,"error":"{}"}}"#, e),
                    ),
                }
            }

            ("GET", "/api/data") => {
                let snap = {
                    let s = match st.lock() {
                        Ok(s) => s,
                        Err(_) => return (500, "application/json", r#"{"error":"lock"}"#.to_string()),
                    };
                    let offline_after = s.base.offline_after;
                    let mut list = Vec::new();
                    for v in &s.vps {
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
                        m.insert("metrics".to_string(), metrics.unwrap_or(J::Null));

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
                                        nm.insert(
                                            "total".to_string(),
                                            J::Num(if n.total > 0 { n.total } else { 0 } as f64),
                                        );
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

// ----------------------------------------------------------------- helpers

fn st_key(st: &Shared) -> String {
    st.lock().map(|s| s.base.key.clone()).unwrap_or_default()
}

fn state_base_title(st: &Shared) -> String {
    st.lock().map(|s| s.base.title.clone()).unwrap_or_default()
}

fn state_base_refresh(st: &Shared) -> u64 {
    st.lock().map(|s| s.base.refresh).unwrap_or(5)
}

fn state_base_key(st: &Shared) -> String {
    st.lock().map(|s| s.base.key.clone()).unwrap_or_default()
}

fn state_base_listen(st: &Shared) -> String {
    st.lock().map(|s| s.base.listen.clone()).unwrap_or_else(|_| "0.0.0.0:8899".to_string())
}

fn hosts_to_json(vps: &[VpsEntry]) -> J {
    let arr: Vec<J> = vps
        .iter()
        .map(|v| {
            let mut m = BTreeMap::new();
            m.insert("name".to_string(), J::Str(v.name.clone()));
            m.insert("manage_url".to_string(), J::Str(v.manage_url.clone()));
            m.insert(
                "panel".to_string(),
                match &v.panel {
                    Some(p) => {
                        let mut pm = BTreeMap::new();
                        pm.insert("url".to_string(), J::Str(p.url.clone()));
                        pm.insert("username".to_string(), J::Str(p.username.clone()));
                        pm.insert("password".to_string(), J::Str(p.password.clone()));
                        J::Obj(pm)
                    }
                    None => J::Null,
                },
            );
            m.insert(
                "port_check".to_string(),
                J::Arr(v.port_check.iter().map(|p| J::Str(p.clone())).collect()),
            );
            if v.quota > 0 {
                m.insert("quota".to_string(), J::Num(v.quota as f64));
            }
            J::Obj(m)
        })
        .collect();
    J::Arr(arr)
}

/// Build a VpsEntry from a JSON object (dashboard form submission).
fn entry_from_json(v: &J) -> Result<VpsEntry, String> {
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Err("name 不能为空".to_string());
    }
    let manage_url = v
        .get("manage_url")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let panel = v.get("panel").and_then(|p| {
        if p.is_null() {
            return None;
        }
        let url = p.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            return None;
        }
        Some(panel::PanelCreds {
            url,
            username: p
                .get("username")
                .and_then(|x| x.as_str())
                .unwrap_or("admin")
                .to_string(),
            password: p.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
    });
    let port_check = v
        .get("port_check")
        .map(|x| x.as_arr().iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let quota = v.get("quota").map(|x| x.as_i64()).unwrap_or(0);
    Ok(VpsEntry {
        name,
        manage_url,
        panel,
        port_check,
        quota,
    })
}

/// Insert or replace a host by name, then persist config.json.
fn upsert_host(st: &Shared, entry: VpsEntry) -> Result<(), String> {
    let (cfg_path, base, mut vps) = {
        let s = st.lock().map_err(|_| "state lock poisoned".to_string())?;
        (s.cfg_path.clone(), s.base.clone(), s.vps.clone())
    };
    if let Some(pos) = vps.iter().position(|x| x.name == entry.name) {
        vps[pos] = entry;
    } else {
        vps.push(entry);
    }
    if let Ok(mut s) = st.lock() {
        s.vps = vps.clone();
    }
    let mut c = base;
    c.vps = vps;
    crate::config::save(&cfg_path, &c)
}

/// Remove a host by name, then persist config.json.
fn remove_host(st: &Shared, name: &str) -> Result<bool, String> {
    let (cfg_path, base, mut vps) = {
        let s = st.lock().map_err(|_| "state lock poisoned".to_string())?;
        (s.cfg_path.clone(), s.base.clone(), s.vps.clone())
    };
    let before = vps.len();
    vps.retain(|x| x.name != name);
    let removed = vps.len() != before;
    if let Ok(mut s) = st.lock() {
        s.vps = vps.clone();
        s.traffic.remove(name);
        s.nodes.remove(name);
    }
    let mut c = base;
    c.vps = vps;
    crate::config::save(&cfg_path, &c)?;
    Ok(removed)
}

fn url_decode_name(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = (b[i + 1] as char).to_digit(16);
            let l = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h * 16 + l) as u8 as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
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
            if t > 0.0 {
                u * 100.0 / t
            } else {
                0.0
            }
        }
    };
    m.insert("mem_percent".to_string(), J::Num(mem_pct));

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
            if t > 0.0 {
                u * 100.0 / t
            } else {
                0.0
            }
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
