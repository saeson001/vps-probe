#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
vps-probe dashboard — 单文件探针面板（纯标准库，零依赖，Win/Linux 通用）

功能:
  * 接收各 VPS 上 agent.py 的心跳（CPU/内存/负载/网络/磁盘/端口探活）
  * 定时登录各台 3x-ui 面板拉取入站流量（已用 / 配额）→ 卡片显示流量剩余
  * 卡片式 Web UI，自动刷新；点击卡片直接跳转该 VPS 的管理页面

用法:
    python dashboard.py [--config config.json]
    默认读取同目录 config.json，模板见 config.example.json
"""

import argparse
import json
import os
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BASE_DIR = os.path.dirname(os.path.abspath(__file__))

# --------------------------------------------------------------- runtime state

LOCK = threading.Lock()
AGENTS = {}       # name -> {metrics..., "_last": ts}
TRAFFIC = {}      # vps name -> {"used": B, "quota": B, "ts": ts}
CFG = {}


# ------------------------------------------------------------------ 3x-ui poll

def _panel_login(url, user, pwd):
    """CSRF login to a 3x-ui panel; returns inbounds list payload or None."""
    import json as _json
    try:
        jar = {}

        def save_cookies(resp):
            for h in (resp.headers.get_all("Set-Cookie") or []):
                try:
                    k, v = h.split(";", 1)[0].split("=", 1)
                    jar[k.strip()] = v.strip()
                except Exception:
                    pass

        def cookie():
            return "; ".join("%s=%s" % kv for kv in jar.items())

        url = url.rstrip("/")
        resp = urllib.request.urlopen(url + "/csrf-token", timeout=10)
        save_cookies(resp)
        csrf = (_json.loads(resp.read()) or {}).get("obj") or ""
        if not csrf:
            return None
        req = urllib.request.Request(url + "/login",
            data=_json.dumps({"username": user, "password": pwd}).encode(), method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("X-CSRF-Token", csrf)
        if cookie():
            req.add_header("Cookie", cookie())
        resp = urllib.request.urlopen(req, timeout=10)
        save_cookies(resp)
        if not (_json.loads(resp.read()) or {}).get("success"):
            return None
        req = urllib.request.Request(url + "/panel/api/inbounds/list", method="GET")
        if cookie():
            req.add_header("Cookie", cookie())
        data = _json.loads(urllib.request.urlopen(req, timeout=10).read())
        return data if isinstance(data, dict) and data.get("success") else None
    except Exception as e:
        print("[traffic] %s poll failed: %s" % (url, e))
        return None


def _poll_traffic_once():
    for vps in CFG.get("vps", []):
        panel = vps.get("panel")
        if not panel or not panel.get("url"):
            continue
        data = _panel_login(panel["url"], panel.get("username", ""), panel.get("password", ""))
        if not data:
            continue
        used = quota = 0
        try:
            for ib in (data.get("obj") or []):
                if not isinstance(ib, dict):
                    continue
                used += ib.get("up", 0) or 0
                used += ib.get("down", 0) or 0
                # prefer per-client quotas when present
                cst = ib.get("clientStats") or []
                if cst:
                    quota += sum((c.get("total", 0) or 0) for c in cst
                                 if isinstance(c, dict))
                else:
                    quota += ib.get("total", 0) or 0
        except Exception:
            pass
        with LOCK:
            TRAFFIC[vps["name"]] = {"used": used, "quota": quota,
                                    "ts": int(time.time())}


def _traffic_loop():
    while True:
        try:
            _poll_traffic_once()
        except Exception as e:
            print("[traffic] loop error: %s" % e)
        time.sleep(CFG.get("traffic_interval", 300))


# -------------------------------------------------------------------- HTTP API

class Handler(BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):  # quiet
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path.split("?")[0] != "/api/report":
            self._json({"success": False}, 404)
            return
        qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        key = (qs.get("key") or [self.headers.get("X-Probe-Key", "")])[0]
        if key != CFG.get("key"):
            self._json({"success": False, "msg": "bad key"}, 403)
            return
        try:
            length = int(self.headers.get("Content-Length", 0))
            metrics = json.loads(self.rfile.read(length) or b"{}")
        except Exception:
            self._json({"success": False}, 400)
            return
        name = metrics.get("name")
        if not name:
            self._json({"success": False, "msg": "no name"}, 400)
            return
        metrics["_last"] = int(time.time())
        with LOCK:
            AGENTS[name] = metrics
        self._json({"success": True})

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/api/data":
            now = int(time.time())
            offline_after = CFG.get("offline_after", 90)
            with LOCK:
                vps_list = []
                for vps in CFG.get("vps", []):
                    name = vps["name"]
                    a = dict(AGENTS.get(name) or {})
                    online = bool(a) and (now - a.get("_last", 0)) <= offline_after
                    t = dict(TRAFFIC.get(name) or {})
                    vps_list.append({
                        "name": name,
                        "manage_url": vps.get("manage_url", ""),
                        "online": online,
                        "agent": {k: v for k, v in a.items() if k != "_last"},
                        "traffic": t,
                    })
            self._json({"success": True, "now": now, "vps": vps_list,
                        "title": CFG.get("title", "VPS Probe")})
            return
        if path == "/":
            self._html()
            return
        self._json({"success": False}, 404)

    def _html(self):
        body = PAGE.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


# ------------------------------------------------------------------------ UI

PAGE = r"""<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>VPS Probe</title>
<style>
  :root { --bg:#0f172a; --card:#1e293b; --txt:#e2e8f0; --dim:#94a3b8;
          --ok:#22c55e; --bad:#ef4444; --bar:#334155; --acc:#38bdf8; }
  * { box-sizing:border-box; margin:0; padding:0; }
  body { background:var(--bg); color:var(--txt); font:14px/1.5 system-ui,"Segoe UI",sans-serif;
         padding:24px; }
  h1 { font-size:20px; margin-bottom:16px; color:var(--acc); }
  .grid { display:grid; grid-template-columns:repeat(auto-fill,minmax(320px,1fr)); gap:16px; }
  .card { background:var(--card); border-radius:12px; padding:16px; cursor:pointer;
          transition:transform .12s, box-shadow .12s; border:1px solid transparent; }
  .card:hover { transform:translateY(-2px); box-shadow:0 6px 18px rgba(0,0,0,.35);
                border-color:var(--acc); }
  .head { display:flex; align-items:center; gap:8px; margin-bottom:10px; }
  .dot { width:10px; height:10px; border-radius:50%; background:var(--bad); flex:none; }
  .dot.on { background:var(--ok); box-shadow:0 0 6px var(--ok); }
  .name { font-size:16px; font-weight:600; }
  .os { margin-left:auto; color:var(--dim); font-size:12px; }
  .row { display:flex; justify-content:space-between; margin:4px 0; color:var(--dim); }
  .row b { color:var(--txt); font-weight:500; }
  .bar { height:8px; background:var(--bar); border-radius:4px; overflow:hidden; margin:4px 0 8px; }
  .bar>i { display:block; height:100%; background:var(--acc); border-radius:4px;
           transition:width .4s; }
  .bar.warn>i { background:#f59e0b; } .bar.crit>i { background:var(--bad); }
  .traffic { margin-top:8px; }
  .speed { color:var(--acc); font-size:12px; }
  .muted { color:var(--dim); }
  .warnnote { font-size:12px; color:var(--dim); margin-top:8px; }
</style>
</head>
<body>
<h1 id="title">VPS Probe</h1>
<div class="grid" id="grid"></div>
<div class="warnnote" id="foot"></div>
<script>
const fmtB = b => { if(b==null||isNaN(b)) return "-"; const u=["B","KB","MB","GB","TB"];
  let i=0,n=b; while(n>=1024&&i<4){n/=1024;i++} return n.toFixed(n>=100?0:1)+u[i]; };
const pct = (a,b) => (b>0 ? Math.min(100, a/b*100) : 0);
function bar(cls, label, used, total, unit) {
  const p = pct(used, total);
  const c = p>=90?"crit":(p>=70?"warn":"");
  return `<div class="row"><span>${label}</span><b>${fmtB(used)} / ${total>0?fmtB(total):"无限制"}` +
         `${total>0?` (${p.toFixed(1)}%)`:""}</b></div>` +
         `<div class="bar ${c}"><i style="width:${p}%"></i></div>`;
}
async function refresh() {
  try {
    const d = await (await fetch("/api/data")).json();
    document.getElementById("title").textContent = d.title || "VPS Probe";
    const g = document.getElementById("grid");
    g.innerHTML = "";
    for (const v of d.vps) {
      const a = v.agent || {}, t = v.traffic || {};
      const memU = a.mem_used, memT = a.mem_total;
      const card = document.createElement("div");
      card.className = "card";
      card.onclick = () => { if (v.manage_url) window.open(v.manage_url, "_blank"); };
      const speed = a.net_rx_rate != null
        ? `<span class="speed">↓ ${fmtB(a.net_rx_rate)}/s · ↑ ${fmtB(a.net_tx_rate)}/s</span>` : "";
      const load = a.load ? a.load.map(x=>x.toFixed(2)).join(" / ") : "-";
      const ports = a.ports ? Object.entries(a.ports).map(([k,on]) =>
        `<span style="color:${on?"var(--ok)":"var(--bad)"}">${k} ${on?"✓":"✗"}</span>`).join(" ")
        : "";
      card.innerHTML = `
        <div class="head">
          <span class="dot ${v.online?"on":""}"></span>
          <span class="name">${v.name}</span>
          <span class="os">${a.os||""}</span>
        </div>
        <div class="row"><span>CPU (${a.cores||"?"} 核)</span><b>${a.cpu!=null?a.cpu+"%":"-"}</b></div>
        <div class="bar ${pct(memU,memT)>=90?"crit":""}"><i style="width:${pct(memU,memT)}%"></i></div>
        <div class="row"><span>内存</span><b>${fmtB(memU)} / ${fmtB(memT)}</b></div>
        <div class="row"><span>负载</span><b>${load}</b></div>
        <div class="row"><span>运行时间</span><b>${a.uptime?Math.floor(a.uptime/86400)+"天 "+Math.floor(a.uptime%86400/3600)+"h":"-"}</b></div>
        <div class="row"><span>网络</span>${speed}</div>
        ${ports?`<div class="row"><span>端口探活</span><b>${ports}</b></div>`:""}
        <div class="traffic">
          <div class="row"><span>流量（面板入站）</span></div>
          ${bar("", "", t.used||0, t.quota||0)}
          <div class="row"><span class="muted">剩余</span>
            <b>${t.quota>0?fmtB(Math.max(0,t.quota-(t.used||0))):"-"}</b></div>
        </div>`;
      g.appendChild(card);
    }
    document.getElementById("foot").textContent =
      "更新于 " + new Date().toLocaleTimeString() + " · 点击卡片跳转对应 VPS 管理页面";
  } catch (e) { console.error(e); }
}
refresh(); setInterval(refresh, 5000);
</script>
</body>
</html>
"""


# ------------------------------------------------------------------------ main

def main():
    global CFG
    ap = argparse.ArgumentParser(description="vps-probe dashboard")
    ap.add_argument("--config", default=os.path.join(BASE_DIR, "config.json"))
    args = ap.parse_args()
    with open(args.config, "r", encoding="utf-8") as f:
        CFG = json.load(f)

    threading.Thread(target=_traffic_loop, daemon=True).start()

    host, _, port = CFG.get("listen", "0.0.0.0:8899").rpartition(":")
    srv = ThreadingHTTPServer((host or "0.0.0.0", int(port)), Handler)
    print("[dashboard] listening on %s:%s" % (host or "0.0.0.0", port))
    srv.serve_forever()


if __name__ == "__main__":
    main()
