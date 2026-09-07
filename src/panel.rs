//! 3x-ui panel traffic reader.
//!
//! 3x-ui (v3.4.x) protects `POST /login` with a CSRF middleware, so the
//! login flow must be:
//!   1. GET  {base}/csrf-token          -> {"obj":"<token>"} + Set-Cookie session
//!   2. POST {base}/login               -> with X-CSRF-Token + Cookie
//!   3. GET  {base}/panel/api/inbounds/list -> with session Cookie
//!
//! All traffic is reported **per inbound / per client** so the dashboard can
//! show which node each number belongs to.

use crate::http;
use crate::json::{self, J};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct NodeTraffic {
    /// inbound remark (the node name shown in 3x-ui)
    pub node: String,
    pub port: i64,
    /// client email / subId (empty when the inbound has no client stats)
    pub client: String,
    pub up: i64,
    pub down: i64,
    pub total: i64,
}

impl NodeTraffic {
    pub fn used(&self) -> i64 {
        self.up + self.down
    }
    pub fn remain(&self) -> i64 {
        if self.total > 0 {
            (self.total - self.used()).max(0)
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PanelTraffic {
    pub nodes: Vec<NodeTraffic>,
    pub error: Option<String>,
}

impl PanelTraffic {
    pub fn total_up(&self) -> i64 {
        self.nodes.iter().map(|n| n.up).sum()
    }
    pub fn total_down(&self) -> i64 {
        self.nodes.iter().map(|n| n.down).sum()
    }
    pub fn total_quota(&self) -> i64 {
        self.nodes.iter().map(|n| n.total).sum()
    }
    pub fn total_used(&self) -> i64 {
        self.nodes.iter().map(|n| n.used()).sum()
    }
}

#[derive(Debug, Clone)]
pub struct PanelCreds {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl PanelCreds {
    pub fn base(&self) -> String {
        self.url.trim_end_matches('/').to_string()
    }
}

/// Full login + fetch cycle. Never panics; returns `error` on failure.
pub fn fetch_traffic(creds: &PanelCreds, timeout: Duration) -> PanelTraffic {
    match try_fetch(creds, timeout) {
        Ok(t) => t,
        Err(e) => PanelTraffic {
            nodes: Vec::new(),
            error: Some(e),
        },
    }
}

fn try_fetch(creds: &PanelCreds, timeout: Duration) -> Result<PanelTraffic, String> {
    let base = creds.base();

    // 1) CSRF token + session cookie
    let r = http::request("GET", &format!("{}/csrf-token", base), &[], None, timeout)?;
    if r.status != 200 {
        return Err(format!("csrf-token HTTP {}", r.status));
    }
    let body = json::parse(&r.body).map_err(|e| format!("csrf json: {}", e))?;
    let token = body
        .get("obj")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if token.is_empty() {
        return Err("empty csrf token".to_string());
    }
    let mut cookie = join_cookies(&r.headers_all("set-cookie"));

    // 2) login
    let payload = J::Obj({
        let mut m = std::collections::BTreeMap::new();
        m.insert("username".into(), J::Str(creds.username.clone()));
        m.insert("password".into(), J::Str(creds.password.clone()));
        m
    })
    .to_string();
    let mut hdrs: Vec<(&str, &str)> = vec![
        ("Content-Type", "application/json"),
        ("X-CSRF-Token", token.as_str()),
    ];
    if !cookie.is_empty() {
        hdrs.push(("Cookie", cookie.as_str()));
    }
    let r = http::request(
        "POST",
        &format!("{}/login", base),
        &hdrs,
        Some(&payload),
        timeout,
    )?;
    if r.status != 200 {
        return Err(format!("login HTTP {}", r.status));
    }
    let lb = json::parse(&r.body).unwrap_or(J::Null);
    if !lb.get("success").map(|v| v.as_bool()).unwrap_or(false) {
        let msg = lb.get("msg").and_then(|v| v.as_str()).unwrap_or("?");
        return Err(format!("login failed: {}", msg));
    }
    let c2 = join_cookies(&r.headers_all("set-cookie"));
    if !c2.is_empty() {
        cookie = c2;
    }

    // 3) inbounds list
    let mut hdrs2: Vec<(&str, &str)> = Vec::new();
    if !cookie.is_empty() {
        hdrs2.push(("Cookie", cookie.as_str()));
    }
    let r = http::request(
        "GET",
        &format!("{}/panel/api/inbounds/list", base),
        &hdrs2,
        None,
        timeout,
    )?;
    if r.status != 200 {
        return Err(format!("inbounds HTTP {}", r.status));
    }
    let data = json::parse(&r.body).map_err(|e| format!("inbounds json: {}", e))?;
    if !data.get("success").map(|v| v.as_bool()).unwrap_or(false) {
        return Err("inbounds API returned success=false".to_string());
    }

    let mut nodes = Vec::new();
    for ib in data.get("obj").map(|v| v.as_arr()).unwrap_or_default() {
        let remark = ib
            .get("remark")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let port = ib.get("port").map(|v| v.as_i64()).unwrap_or(0);

        let stats = ib.get("clientStats").map(|v| v.as_arr()).unwrap_or_default();
        if stats.is_empty() {
            // Inbound without per-client stats: report the inbound itself.
            let up = ib.get("up").map(|v| v.as_i64()).unwrap_or(0);
            let down = ib.get("down").map(|v| v.as_i64()).unwrap_or(0);
            let total = ib.get("total").map(|v| v.as_i64()).unwrap_or(0);
            nodes.push(NodeTraffic {
                node: remark,
                port,
                client: String::new(),
                up,
                down,
                total,
            });
            continue;
        }

        for c in stats {
            let email = c
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let up = c.get("up").map(|v| v.as_i64()).unwrap_or(0);
            let down = c.get("down").map(|v| v.as_i64()).unwrap_or(0);
            let total = c.get("total").map(|v| v.as_i64()).unwrap_or(0);
            nodes.push(NodeTraffic {
                node: remark.clone(),
                port,
                client: email,
                up,
                down,
                total,
            });
        }
    }

    Ok(PanelTraffic { nodes, error: None })
}

/// Merge repeated Set-Cookie headers into one Cookie request header value.
fn join_cookies(cookies: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in cookies {
        if let Some(kv) = c.split(';').next() {
            let kv = kv.trim();
            if !kv.is_empty() {
                // Later cookies win for the same key.
                if let Some(pos) = parts.iter().position(|p| p.starts_with(&format!("{}=", kv.split('=').next().unwrap_or("")))) {
                    parts[pos] = kv.to_string();
                } else {
                    parts.push(kv.to_string());
                }
            }
        }
    }
    parts.join("; ")
}
