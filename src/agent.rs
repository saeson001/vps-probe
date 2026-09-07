//! Agent mode: collect metrics and push them to the dashboard.

use crate::http;
use crate::json::J;
use crate::metrics;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn run(server: &str, key: &str, name: &str, interval: u64, ports: Vec<String>) -> ! {
    let server = server.trim_end_matches('/').to_string();
    let url = format!("{}/api/report?key={}", server, key);
    eprintln!(
        "[agent] name={} server={} interval={}s ports={:?}",
        name, server, interval, ports
    );

    // Resolve a default name from the hostname when not supplied.
    let name = if name.is_empty() {
        hostname().unwrap_or_else(|| "vps".to_string())
    } else {
        name.to_string()
    };

    loop {
        let m = metrics::collect(&ports);

        let mut root = BTreeMap::new();
        root.insert("name".to_string(), J::Str(name.clone()));
        root.insert("ts".to_string(), J::Num(now() as f64));
        root.insert("cpu".to_string(), J::Num(m.cpu_percent));
        root.insert("cores".to_string(), J::Num(m.cores as f64));
        root.insert("mem_used".to_string(), J::Num(m.mem_used as f64));
        root.insert("mem_total".to_string(), J::Num(m.mem_total as f64));
        root.insert(
            "mem_percent".to_string(),
            J::Num(m.mem_percent()),
        );
        root.insert("swap_used".to_string(), J::Num(m.swap_used as f64));
        root.insert("swap_total".to_string(), J::Num(m.swap_total as f64));
        root.insert("load".to_string(), J::Arr(vec![
            J::Num(m.load1),
            J::Num(m.load5),
            J::Num(m.load15),
        ]));
        root.insert("uptime".to_string(), J::Num(m.uptime_secs as f64));
        root.insert("net_rx_rate".to_string(), J::Num(m.net_rx_rate));
        root.insert("net_tx_rate".to_string(), J::Num(m.net_tx_rate));
        root.insert("disk_used".to_string(), J::Num(m.disk_used as f64));
        root.insert("disk_total".to_string(), J::Num(m.disk_total as f64));
        root.insert("disk_percent".to_string(), J::Num(m.disk_percent()));
        root.insert("os".to_string(), J::Str(m.os.clone()));

        let mut pm = BTreeMap::new();
        for (k, v) in &m.ports {
            pm.insert(k.clone(), J::Bool(*v));
        }
        root.insert("ports".to_string(), J::Obj(pm));

        let payload = J::Obj(root).to_string();
        match http::request(
            "POST",
            &url,
            &[("Content-Type", "application/json")],
            Some(&payload),
            Duration::from_secs(10),
        ) {
            Ok(r) if r.status == 200 => {
                eprintln!("[agent] ok cpu={:.1}% mem={:.1}%", m.cpu_percent, m.mem_percent());
            }
            Ok(r) => eprintln!("[agent] server returned HTTP {}", r.status),
            Err(e) => eprintln!("[agent] report failed: {}", e),
        }

        std::thread::sleep(Duration::from_secs(interval));
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
