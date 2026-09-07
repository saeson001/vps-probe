//! Embedded dashboard page (HTML + CSS + vanilla JS, no CDN, no build step).

pub fn page(title: &str, refresh: u64) -> String {
    let tpl = include_str!("../assets/index.html");
    tpl.replace("{{TITLE}}", title)
        .replace("{{REFRESH}}", &refresh.to_string())
}

