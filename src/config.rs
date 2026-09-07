//! Dashboard configuration (`config.json`).

use crate::json::{self, J};
use crate::panel::PanelCreds;

#[derive(Debug, Clone)]
pub struct VpsEntry {
    pub name: String,
    /// URL opened when the card is clicked (the VPS provider / panel page).
    pub manage_url: String,
    pub panel: Option<PanelCreds>,
    /// Extra TCP endpoints the agent should probe (optional).
    pub port_check: Vec<String>,
    /// Optional manual quota override in bytes (used when the panel reports 0).
    pub quota: i64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub key: String,
    pub title: String,
    /// dashboard auto-refresh interval in seconds
    pub refresh: u64,
    /// seconds without a heartbeat before a node is marked offline
    pub offline_after: u64,
    /// panel traffic poll interval in seconds
    pub panel_interval: u64,
    pub vps: Vec<VpsEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0:8899".into(),
            key: "change-me".into(),
            title: "VPS 探针".into(),
            refresh: 5,
            offline_after: 90,
            panel_interval: 300,
            vps: Vec::new(),
        }
    }
}

pub fn load(path: &str) -> Result<Config, String> {
    let txt = std::fs::read_to_string(path)
        .map_err(|e| format!("read {} failed: {} (copy config.example.json first)", path, e))?;
    let v = json::parse(&txt).map_err(|e| format!("config JSON invalid: {}", e))?;

    let mut c = Config::default();
    if let Some(s) = v.get("listen").and_then(|x| x.as_str()) {
        c.listen = s.to_string();
    }
    if let Some(s) = v.get("key").and_then(|x| x.as_str()) {
        c.key = s.to_string();
    }
    if let Some(s) = v.get("title").and_then(|x| x.as_str()) {
        c.title = s.to_string();
    }
    if let Some(n) = v.get("refresh").and_then(|x| x.as_f64()) {
        c.refresh = n as u64;
    }
    if let Some(n) = v.get("offline_after").and_then(|x| x.as_f64()) {
        c.offline_after = n as u64;
    }
    if let Some(n) = v.get("panel_interval").and_then(|x| x.as_f64()) {
        c.panel_interval = n as u64;
    }

    if let Some(arr) = v.get("vps").map(|x| x.as_arr()) {
        for item in arr {
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("unnamed")
                .to_string();
            let manage_url = item
                .get("manage_url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let panel = item.get("panel").and_then(|p| {
                if p.is_null() {
                    return None;
                }
                let url = p.get("url").and_then(|x| x.as_str()).unwrap_or("");
                if url.is_empty() {
                    return None;
                }
                Some(PanelCreds {
                    url: url.to_string(),
                    username: p.get("username").and_then(|x| x.as_str()).unwrap_or("admin").to_string(),
                    password: p.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                })
            });
            let port_check = item
                .get("port_check")
                .map(|x| {
                    x.as_arr()
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let quota = item.get("quota").map(|x| x.as_i64()).unwrap_or(0);
            c.vps.push(VpsEntry {
                name,
                manage_url,
                panel,
                port_check,
                quota,
            });
        }
    }

    Ok(c)
}

/// Serialize a config object (used by the installer to write config.json).
pub fn dump(c: &Config) -> String {
    let vps: Vec<J> = c
        .vps
        .iter()
        .map(|v| {
            let mut m = std::collections::BTreeMap::new();
            m.insert("name".to_string(), J::Str(v.name.clone()));
            m.insert("manage_url".to_string(), J::Str(v.manage_url.clone()));
            m.insert("port_check".to_string(), J::Arr(
                v.port_check.iter().map(|p| J::Str(p.clone())).collect(),
            ));
            if v.quota > 0 {
                m.insert("quota".to_string(), J::Num(v.quota as f64));
            }
            m.insert(
                "panel".to_string(),
                match &v.panel {
                    Some(p) => {
                        let mut pm = std::collections::BTreeMap::new();
                        pm.insert("url".to_string(), J::Str(p.url.clone()));
                        pm.insert("username".to_string(), J::Str(p.username.clone()));
                        pm.insert("password".to_string(), J::Str(p.password.clone()));
                        J::Obj(pm)
                    }
                    None => J::Null,
                },
            );
            J::Obj(m)
        })
        .collect();

    let mut m = std::collections::BTreeMap::new();
    m.insert("listen".to_string(), J::Str(c.listen.clone()));
    m.insert("key".to_string(), J::Str(c.key.clone()));
    m.insert("title".to_string(), J::Str(c.title.clone()));
    m.insert("refresh".to_string(), J::Num(c.refresh as f64));
    m.insert("offline_after".to_string(), J::Num(c.offline_after as f64));
    m.insert("panel_interval".to_string(), J::Num(c.panel_interval as f64));
    m.insert("vps".to_string(), J::Arr(vps));
    format!("{}\n", pretty(&J::Obj(m), 0))
}

fn pretty(v: &J, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    match v {
        J::Obj(m) => {
            if m.is_empty() {
                return "{}".to_string();
            }
            let inner: Vec<String> = m
                .iter()
                .map(|(k, vv)| format!("{}  {}: {}", pad, json::escape_json(k), pretty(vv, indent + 1)))
                .collect();
            format!("{{\n{}\n{}}}", inner.join(",\n"), pad)
        }
        J::Arr(a) => {
            if a.is_empty() {
                return "[]".to_string();
            }
            let inner: Vec<String> = a.iter().map(|vv| format!("{}  {}", pad, pretty(vv, indent + 1))).collect();
            format!("[\n{}\n{}]", inner.join(",\n"), pad)
        }
        other => other.to_string(),
    }
}
