//! System metric collection.
//!
//! Linux is the primary target: everything is read straight from `/proc`,
//! which is cheap, dependency-free and works in every container/LXC/OpenVZ
//! image. Other platforms fall back to shelling out to builtin tools.

use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Metrics {
    pub cpu_percent: f64,
    pub cores: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub uptime_secs: u64,
    pub net_rx_rate: f64, // bytes/s
    pub net_tx_rate: f64, // bytes/s
    pub disk_used: u64,
    pub disk_total: u64,
    pub os: String,
    pub ports: HashMap<String, bool>,
}

impl Metrics {
    pub fn mem_percent(&self) -> f64 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f64 * 100.0 / self.mem_total as f64
        }
    }
    pub fn disk_percent(&self) -> f64 {
        if self.disk_total == 0 {
            0.0
        } else {
            self.disk_used as f64 * 100.0 / self.disk_total as f64
        }
    }
}

fn read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
fn cpu_snapshot() -> Option<(u64, u64)> {
    // user nice system idle iowait irq softirq steal
    let s = read_file("/proc/stat")?;
    let line = s.lines().find(|l| l.starts_with("cpu "))?;
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse::<u64>().ok())
        .collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    let total: u64 = nums.iter().sum();
    Some((total, idle))
}

#[cfg(target_os = "linux")]
fn net_snapshot() -> Option<(u64, u64)> {
    let s = read_file("/proc/net/dev")?;
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in s.lines().skip(2) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }
        let iface = parts[0].trim_end_matches(':');
        if iface == "lo" {
            continue;
        }
        rx += parts[1].parse::<u64>().unwrap_or(0);
        tx += parts[9].parse::<u64>().unwrap_or(0);
    }
    Some((rx, tx))
}

#[cfg(target_os = "linux")]
fn meminfo() -> (u64, u64, u64, u64) {
    let mut total = 0u64;
    let mut avail = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    if let Some(s) = read_file("/proc/meminfo") {
        for line in s.lines() {
            let mut it = line.split_whitespace();
            let key = it.next().unwrap_or("");
            let val = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            match key {
                "MemTotal:" => total = val * 1024,
                "MemAvailable:" => avail = val * 1024,
                "SwapTotal:" => swap_total = val * 1024,
                "SwapFree:" => swap_free = val * 1024,
                _ => {}
            }
        }
    }
    let used = total.saturating_sub(avail);
    (used, total, swap_total.saturating_sub(swap_free), swap_total)
}

#[cfg(target_os = "linux")]
fn disk_usage() -> (u64, u64) {
    // statvfs isn't in std; `df` is POSIX and present on every distro.
    let out = std::process::Command::new("df").arg("-B1").arg("/").output();
    if let Ok(o) = out {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        for line in s.lines().skip(1) {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.len() >= 3 {
                let total = p[1].parse::<u64>().unwrap_or(0);
                let used = p[2].parse::<u64>().unwrap_or(0);
                if total > 0 {
                    return (used, total);
                }
            }
        }
    }
    (0, 0)
}

#[cfg(target_os = "linux")]
fn os_name() -> String {
    let mut s = read_file("/etc/os-release").unwrap_or_default();
    if s.is_empty() {
        s = read_file("/etc/issue").unwrap_or_default();
    }
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim().trim_matches('"').to_string();
        }
    }
    let k = read_file("/proc/sys/kernel/ostype").unwrap_or_default();
    format!("Linux {}", k.trim()).trim().to_string()
}

#[cfg(target_os = "linux")]
pub fn collect(ports: &[String]) -> Metrics {
    let (u1, i1) = cpu_snapshot().unwrap_or((0, 0));
    let (n1rx, n1tx) = net_snapshot().unwrap_or((0, 0));
    std::thread::sleep(Duration::from_millis(400));
    let (u2, i2) = cpu_snapshot().unwrap_or((0, 0));
    let (n2rx, n2tx) = net_snapshot().unwrap_or((0, 0));

    let dt = 0.4_f64;
    let cpu = if u2 > u1 {
        let total = (u2 - u1) as f64;
        let idle = (i2.saturating_sub(i1)) as f64;
        ((total - idle) / total * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let rx_rate = ((n2rx.saturating_sub(n1rx)) as f64 / dt).max(0.0);
    let tx_rate = ((n2tx.saturating_sub(n1tx)) as f64 / dt).max(0.0);

    let (mem_used, mem_total, swap_used, swap_total) = meminfo();
    let (disk_used, disk_total) = disk_usage();

    let (l1, l5, l15) = match read_file("/proc/loadavg") {
        Some(s) => {
            let p: Vec<f64> = s
                .split_whitespace()
                .take(3)
                .filter_map(|v| v.parse::<f64>().ok())
                .collect();
            (
                p.get(0).copied().unwrap_or(0.0),
                p.get(1).copied().unwrap_or(0.0),
                p.get(2).copied().unwrap_or(0.0),
            )
        }
        None => (0.0, 0.0, 0.0),
    };

    let uptime = read_file("/proc/uptime")
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or(0.0) as u64;

    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    let mut port_map = HashMap::new();
    for p in ports {
        port_map.insert(p.clone(), crate::http::tcp_probe(p, Duration::from_millis(800)));
    }

    Metrics {
        cpu_percent: cpu,
        cores,
        mem_used,
        mem_total,
        swap_used,
        swap_total,
        load1: l1,
        load5: l5,
        load15: l15,
        uptime_secs: uptime,
        net_rx_rate: rx_rate,
        net_tx_rate: tx_rate,
        disk_used,
        disk_total,
        os: os_name(),
        ports: port_map,
    }
}

// ------------------------------------------------------- non-Linux fallback

#[cfg(not(target_os = "linux"))]
pub fn collect(ports: &[String]) -> Metrics {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    // Memory: try /proc first (WSL), then platform commands.
    let (mut mem_used, mut mem_total) = (0u64, 0u64);
    if let Some(s) = read_file("/proc/meminfo") {
        let (mut t, mut a) = (0u64, 0u64);
        for line in s.lines() {
            let mut it = line.split_whitespace();
            let k = it.next().unwrap_or("");
            let v = it.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            match k {
                "MemTotal:" => t = v * 1024,
                "MemAvailable:" => a = v * 1024,
                _ => {}
            }
        }
        if t > 0 {
            mem_total = t;
            mem_used = t.saturating_sub(a);
        }
    }
    if mem_total == 0 {
        #[cfg(target_os = "windows")]
        {
            // wmic is deprecated but still present on Win10/11 images.
            if let Ok(o) = std::process::Command::new("wmic")
                .args(["OS", "get", "TotalVisibleMemorySize,FreePhysicalMemory", "/value"])
                .output()
            {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                let (mut total_kb, mut free_kb) = (0u64, 0u64);
                for line in s.lines() {
                    if let Some(v) = line.strip_prefix("TotalVisibleMemorySize=") {
                        total_kb = v.trim().parse().unwrap_or(0);
                    }
                    if let Some(v) = line.strip_prefix("FreePhysicalMemory=") {
                        free_kb = v.trim().parse().unwrap_or(0);
                    }
                }
                mem_total = total_kb * 1024;
                mem_used = total_kb.saturating_sub(free_kb) * 1024;
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(o) = std::process::Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
            {
                mem_total = String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
    }

    let (disk_used, disk_total) = {
        match std::process::Command::new("df").arg("-B1").arg(".").output() {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                let mut r = (0u64, 0u64);
                for line in s.lines().skip(1) {
                    let p: Vec<&str> = line.split_whitespace().collect();
                    if p.len() >= 3 {
                        r = (
                            p[2].parse().unwrap_or(0),
                            p[1].parse().unwrap_or(0),
                        );
                        break;
                    }
                }
                r
            }
            Err(_) => (0, 0),
        }
    };

    let cpu = cpu_fallback();

    let mut port_map = HashMap::new();
    for p in ports {
        port_map.insert(p.clone(), crate::http::tcp_probe(p, Duration::from_millis(800)));
    }

    Metrics {
        cpu_percent: cpu,
        cores,
        mem_used,
        mem_total,
        swap_used: 0,
        swap_total: 0,
        load1: 0.0,
        load5: 0.0,
        load15: 0.0,
        uptime_secs: 0,
        net_rx_rate: 0.0,
        net_tx_rate: 0.0,
        disk_used,
        disk_total,
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ports: port_map,
    }
}

#[cfg(not(target_os = "linux"))]
fn cpu_fallback() -> f64 {
    #[cfg(target_os = "windows")]
    {
        if let Ok(o) = std::process::Command::new("wmic")
            .args(["cpu", "get", "loadpercentage", "/value"])
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            for line in s.lines() {
                if let Some(v) = line.strip_prefix("LoadPercentage=") {
                    return v.trim().parse::<f64>().unwrap_or(0.0);
                }
            }
        }
    }
    0.0
}
