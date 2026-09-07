//! Embedded dashboard page (HTML + CSS + vanilla JS, no CDN, no build step).

pub fn page(title: &str, refresh: u64, key: &str) -> String {
    let tpl = include_str!("../assets/index.html");
    tpl.replace("{{TITLE}}", title)
        .replace("{{REFRESH}}", &refresh.to_string())
        // key is injected as a JSON string literal so it can be used to build
        // the per-host agent command shown in the UI (no manual typing).
        .replace("{{KEY}}", &crate::json::escape_json(key))
}

