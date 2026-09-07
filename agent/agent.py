#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
vps-probe agent — 轻量 VPS 探针客户端（纯标准库，零依赖）

在每台 VPS 上运行，定时采集 CPU / 内存 / 负载 / 网络 / 磁盘 / 在线状态，
POST 到 dashboard。Linux 全量支持；Windows 为基础支持（内存/CPU/网络）。

用法:
    python3 agent.py --server http://dash.example.com:8899 --key SECRET \
                     --name "HK-VPS" --interval 30 [--port-check host:port,...]

systemd 常驻见仓库内 vps-probe-agent.service
"""

import argparse
import json
import os
import platform
import socket
import sys
import threading
import time
import urllib.request

STATE = {
    "cpu_prev": None,      # (idle, total) jiffies
    "net_prev": None,      # (rx, tx, ts)
    "win_cpu_prev": None,  # windows: (idle, total) from GetSystemTimes
}


# ---------------------------------------------------------------- Linux proc

def _read_proc_stat():
    with open("/proc/stat", "r") as f:
        line = f.readline()
    parts = [int(x) for x in line.split()[1:]]
    idle = parts[3] + (parts[4] if len(parts) > 4 else 0)  # idle + iowait
    return idle, sum(parts)


def _read_proc_meminfo():
    mem = {}
    with open("/proc/meminfo", "r") as f:
        for line in f:
            k, v = line.split(":", 1)
            mem[k.strip()] = int(v.strip().split()[0]) * 1024  # kB -> bytes
    return mem


def _read_proc_loadavg():
    with open("/proc/loadavg", "r") as f:
        parts = f.read().split()
    return float(parts[0]), float(parts[1]), float(parts[2])


def _read_proc_net():
    rx = tx = 0
    with open("/proc/net/dev", "r") as f:
        for line in f.readlines()[2:]:
            cols = line.split()
            iface = cols[0].rstrip(":")
            if iface in ("lo",):
                continue
            rx += int(cols[1])
            tx += int(cols[9])
    return rx, tx


def _read_uptime():
    with open("/proc/uptime", "r") as f:
        return float(f.read().split()[0])


def _read_disk():
    st = os.statvfs("/")
    total = st.f_blocks * st.f_frsize
    free = st.f_bavail * st.f_frsize
    return total - free, total


# ------------------------------------------------------------- Windows fallback

def _win_mem():
    import ctypes

    class MEMORYSTATUSEX(ctypes.Structure):
        _fields_ = [("dwLength", ctypes.c_ulong), ("dwMemoryLoad", ctypes.c_ulong),
                    ("ullTotalPhys", ctypes.c_ulonglong), ("ullAvailPhys", ctypes.c_ulonglong),
                    ("ullTotalPageFile", ctypes.c_ulonglong), ("ullAvailPageFile", ctypes.c_ulonglong),
                    ("ullTotalVirtual", ctypes.c_ulonglong), ("ullAvailVirtual", ctypes.c_ulonglong),
                    ("ullAvailExtendedVirtual", ctypes.c_ulonglong)]

    st = MEMORYSTATUSEX()
    st.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
    ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(st))
    return st.ullTotalPhys - st.ullAvailPhys, st.ullTotalPhys


def _win_cpu_percent():
    """CPU % via GetSystemTimes delta."""
    import ctypes

    class FILETIME(ctypes.Structure):
        _fields_ = [("dwLowDateTime", ctypes.c_ulong), ("dwHighDateTime", ctypes.c_ulong)]

    idle, kernel, user = FILETIME(), FILETIME(), FILETIME()
    ctypes.windll.kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user))
    to64 = lambda ft: (ft.dwHighDateTime << 32) | ft.dwLowDateTime
    cur = (to64(idle), to64(kernel) + to64(user))
    prev = STATE.get("win_cpu_prev")
    STATE["win_cpu_prev"] = cur
    if not prev:
        return None
    didle, dtotal = cur[0] - prev[0], cur[1] - prev[1]
    if dtotal <= 0:
        return None
    return round((1.0 - didle / dtotal) * 100.0, 1)


def _win_net():
    """Best-effort: skip detailed net counters on Windows (needs psutil)."""
    prev = STATE.get("net_prev")
    STATE["net_prev"] = (0, 0, time.time())
    return None


# ---------------------------------------------------------------- collectors

def cpu_percent():
    if sys.platform.startswith("linux"):
        cur = _read_proc_stat()
        prev = STATE["cpu_prev"]
        STATE["cpu_prev"] = cur
        if not prev:
            return None
        didle, dtotal = cur[0] - prev[0], cur[1] - prev[1]
        if dtotal <= 0:
            return None
        return round((1.0 - didle / dtotal) * 100.0, 1)
    return _win_cpu_percent()


def net_rates():
    now = time.time()
    if sys.platform.startswith("linux"):
        rx, tx = _read_proc_net()
    else:
        return None
    prev = STATE["net_prev"]
    STATE["net_prev"] = (rx, tx, now)
    if not prev or now - prev[2] <= 0:
        return None
    dt = now - prev[2]
    return round((rx - prev[0]) / dt, 0), round((tx - prev[1]) / dt, 0), rx, tx


def tcp_check(targets, timeout=3.0):
    """Probe TCP connectability: host:port list -> {'host:port': bool}."""
    out = {}
    for t in targets or []:
        try:
            host, port = t.rsplit(":", 1)
            s = socket.create_connection((host, int(port)), timeout=timeout)
            s.close()
            out[t] = True
        except Exception:
            out[t] = False
    return out


def collect(args):
    m = {
        "name": args.name,
        "ts": int(time.time()),
        "os": platform.system() + " " + platform.release(),
        "cores": os.cpu_count() or 1,
    }
    cpu = cpu_percent()
    if cpu is not None:
        m["cpu"] = cpu

    if sys.platform.startswith("linux"):
        mem = _read_proc_meminfo()
        m["mem_used"] = mem.get("MemAvailable") and (mem["MemTotal"] - mem["MemAvailable"]) or 0
        m["mem_total"] = mem.get("MemTotal", 0)
        m["swap_used"] = mem.get("SwapTotal", 0) - mem.get("SwapFree", 0)
        m["swap_total"] = mem.get("SwapTotal", 0)
        m["load"] = _read_proc_loadavg()
        m["uptime"] = int(_read_uptime())
        used, total = _read_disk()
        m["disk_used"], m["disk_total"] = used, total
    else:
        used, total = _win_mem()
        m["mem_used"], m["mem_total"] = used, total
        m["uptime"] = int(time.time() - psutil_boot()) if False else None

    nr = net_rates()
    if nr:
        m["net_rx_rate"], m["net_tx_rate"], m["net_rx_total"], m["net_tx_total"] = nr

    if args.port_check:
        m["ports"] = tcp_check(args.port_check)
    return m


def psutil_boot():
    return 0  # placeholder, unused


def report_once(args):
    payload = json.dumps(collect(args)).encode()
    req = urllib.request.Request(
        args.server.rstrip("/") + "/api/report", data=payload, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("X-Probe-Key", args.key)
    with urllib.request.urlopen(req, timeout=10) as resp:
        resp.read()


def main():
    ap = argparse.ArgumentParser(description="vps-probe agent")
    ap.add_argument("--server", required=True, help="dashboard base URL")
    ap.add_argument("--key", required=True, help="shared secret")
    ap.add_argument("--name", required=True, help="VPS display name")
    ap.add_argument("--interval", type=int, default=30, help="seconds between reports")
    ap.add_argument("--port-check", default="", help="optional comma list of host:port to probe")
    args = ap.parse_args()
    args.port_check = [t for t in args.port_check.split(",") if t.strip()]

    print("[agent] %s -> %s every %ds" % (args.name, args.server, args.interval))
    while True:
        try:
            report_once(args)
            print("[agent] report ok %s" % time.strftime("%H:%M:%S"))
        except Exception as e:
            print("[agent] report failed: %s" % e)
        time.sleep(args.interval)


if __name__ == "__main__":
    main()
