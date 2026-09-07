//! Tiny HTTP/1.1 client and server built on `std::net` only.
//!
//! No TLS: probes talk to panels over plain HTTP (same host / private
//! network / reverse proxy). Keeps the binary dependency-free and static.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

// ------------------------------------------------------------------ client

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }

    /// All values for a header (used for Set-Cookie, which repeats).
    pub fn headers_all(&self, name: &str) -> Vec<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Split `http://host[:port]/path?query` into (host, port, path_and_query).
pub fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// URLs are supported (no TLS in this build)".to_string())?
        .to_string();
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rfind(':') {
        Some(i) => (
            authority[..i].to_string(),
            authority[i + 1..].parse::<u16>().unwrap_or(80),
        ),
        None => (authority, 80u16),
    };
    Ok((host, port, path))
}

/// Perform one HTTP request. `headers` are extra request headers.
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    timeout: Duration,
) -> Result<Resp, String> {
    let (host, port, path) = split_url(url)?;

    let mut stream = connect(&host, port, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;

    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nUser-Agent: vps-probe/2.0\r\n",
        method, path, host, port
    );
    let mut has_len = false;
    for (k, v) in headers {
        if k.to_ascii_lowercase() == "content-length" {
            has_len = true;
        }
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    match body {
        Some(b) if !b.is_empty() => {
            if !has_len {
                req.push_str(&format!("Content-Length: {}\r\n", b.len()));
            }
            req.push_str("\r\n");
            req.push_str(b);
        }
        _ => {
            req.push_str("\r\n");
        }
    }

    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    // ---- read status line ----
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).map_err(|e| e.to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {}", status_line.trim()))?;

    // ---- read headers ----
    let mut hdrs: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let t = line.trim_end_matches(['\r', '\n']).to_string();
        if t.is_empty() {
            break;
        }
        if let Some(i) = t.find(':') {
            let k = t[..i].trim().to_string();
            let v = t[i + 1..].trim().to_string();
            hdrs.push((k, v));
        }
    }

    // ---- read body ----
    let resp = Resp {
        status,
        headers: hdrs.clone(),
        body: String::new(),
    };
    let len_opt = resp
        .header("content-length")
        .and_then(|s| s.parse::<usize>().ok());
    let chunked = resp
        .header("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    let mut body_out = Vec::new();
    if let Some(len) = len_opt {
        let mut buf = vec![0u8; len];
        let mut read = 0usize;
        while read < len {
            let n = reader.read(&mut buf[read..]).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            read += n;
        }
        body_out = buf[..read].to_vec();
    } else if chunked {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).map_err(|e| e.to_string())? == 0 {
                break;
            }
            let size_line = size_line.trim().to_string();
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            let mut got = 0usize;
            while got < size {
                let n = reader.read(&mut chunk[got..]).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            body_out.extend_from_slice(&chunk[..got]);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else {
        let _ = reader.read_to_end(&mut body_out);
    }

    Ok(Resp {
        status,
        headers: hdrs,
        body: String::from_utf8_lossy(&body_out).to_string(),
    })
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addr = format!("{}:{}", host, port);
    let mut last_err = String::from("no addresses");
    for a in addr
        .to_socket_addrs()
        .map_err(|e| format!("resolve {} failed: {}", addr, e))?
    {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("connect {} failed: {}", addr, last_err))
}

/// Quick TCP reachability probe (used by the agent for port checks).
pub fn tcp_probe(addr: &str, timeout: Duration) -> bool {
    match addr.to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(a) => TcpStream::connect_timeout(&a, timeout).is_ok(),
            None => false,
        },
        Err(_) => false,
    }
}

// ------------------------------------------------------------------ server

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Very small blocking HTTP server: `handler` returns (status, content_type, body).
type Handler = std::sync::Arc<
    dyn Fn(&Request) -> (u16, &'static str, String) + Send + Sync + 'static,
>;

pub fn serve<F>(bind: &str, handler: F) -> Result<(), String>
where
    F: Fn(&Request) -> (u16, &'static str, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind(bind).map_err(|e| format!("bind {} failed: {}", bind, e))?;
    eprintln!("[vps-probe] listening on http://{}", bind);
    let handler: Handler = std::sync::Arc::new(handler);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let h = handler.clone();
                std::thread::spawn(move || handle_conn(s, h));
            }
            Err(e) => eprintln!("[vps-probe] accept error: {}", e),
        }
    }
    Ok(())
}

fn handle_conn(mut stream: TcpStream, handler: Handler) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut req_line = String::new();
    if reader.read_line(&mut req_line).is_err() {
        return;
    }
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return,
        }
        let t = line.trim_end_matches(['\r', '\n']).to_string();
        if t.is_empty() {
            break;
        }
        if let Some(i) = t.find(':') {
            headers.insert(
                t[..i].trim().to_ascii_lowercase(),
                t[i + 1..].trim().to_string(),
            );
        }
    }

    let mut body = String::new();
    if let Some(len) = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
    {
        let mut buf = vec![0u8; len];
        let mut got = 0usize;
        while got < len {
            match reader.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break,
            }
        }
        body = String::from_utf8_lossy(&buf[..got]).to_string();
    }

    let (path, query) = match target.find('?') {
        Some(i) => (
            target[..i].to_string(),
            parse_query(&target[i + 1..]),
        ),
        None => (target, HashMap::new()),
    };

    let req = Request {
        method,
        path,
        query,
        headers,
        body,
    };
    let (status, ctype, resp_body) = handler(&req);
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        reason,
        ctype,
        resp_body.len()
    );
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(resp_body.as_bytes());
    let _ = stream.flush();
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in q.split('&') {
        if let Some(i) = pair.find('=') {
            m.insert(
                url_decode(&pair[..i]),
                url_decode(&pair[i + 1..]),
            );
        } else if !pair.is_empty() {
            m.insert(url_decode(pair), String::new());
        }
    }
    m
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
