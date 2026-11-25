#!/usr/bin/env python3
"""End-to-end test of rubrowser's HTTP layer against a local server.

Drives `rubrowser --headless --remote-debugging-port 0 --user-data-dir DIR`
through the DevTools Protocol (the client in scripts/cdp-test.py) and checks
what the server receives:

- every method (PUT, DELETE, PATCH) and the headers scripts set
  (forbidden ones dropped);
- one cookie jar: cookies set during a redirect are kept, HttpOnly cookies
  are sent but hidden from document.cookie and cannot be overwritten by it;
- 303 (and 302 after POST) redirects continue with GET;
- persistent cookies and localStorage survive a restart with the same
  --user-data-dir; session cookies do not.

Usage: scripts/http-test.py [path/to/rubrowser]
"""

import http.server
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import threading

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("cdp_test", os.path.join(ROOT, "scripts", "cdp-test.py"))
cdp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cdp)

seen = []  # (method, path, headers dict, body)
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status=200, body=b"", ctype="text/html; charset=utf-8", headers=()):
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def handle_any(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        seen.append((self.command, self.path, {k.lower(): v for k, v in self.headers.items()}, body))
        path = self.path.split("?")[0]
        if path == "/login":
            return self.reply(302, headers=[("Location", "/home"),
                                            ("Set-Cookie", "session=abc; Path=/; HttpOnly"),
                                            ("Set-Cookie", "pref=light; Path=/; Max-Age=3600")])
        if path in ("/home", "/done"):
            return self.reply(body=b"<!doctype html><title>home</title><p>home")
        if path == "/form":
            return self.reply(303, headers=[("Location", "/done")])
        if path.startswith("/api/"):
            echo = {"method": self.command, "auth": self.headers.get("Authorization"),
                    "x": self.headers.get("X-Custom"), "type": self.headers.get("Content-Type"),
                    "body": body.decode(), "cookie": self.headers.get("Cookie"),
                    "secret": self.headers.get("Sec-Secret")}
            return self.reply(body=json.dumps(echo).encode(), ctype="application/json")
        self.reply(404, b"no")

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = do_HEAD = handle_any


def start(exe, profile):
    proc = subprocess.Popen([exe, "--headless", "--remote-debugging-port", "0", "--user-data-dir", profile],
                            stderr=subprocess.PIPE, text=True)
    for line in proc.stderr:
        m = re.search(r"DevTools listening on ws://127\.0\.0\.1:(\d+)/", line)
        if m:
            threading.Thread(target=lambda: [None for _ in proc.stderr], daemon=True).start()
            port = int(m.group(1))
            targets = json.loads(cdp.get(port, "/json/list")[1])
            page = cdp.WebSocket(targets[0]["webSocketDebuggerUrl"])
            page.call("Page.enable")
            return proc, page
    raise RuntimeError("rubrowser did not start")


def go(page, url):
    page.call("Page.navigate", {"url": url})
    page.wait_event("Page.loadEventFired")


def ev(page, expr):
    r = page.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})
    return r["result"].get("result", {}).get("value")


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug", "rubrowser")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    site = f"http://127.0.0.1:{server.server_address[1]}"
    with tempfile.TemporaryDirectory() as profile:
        proc, page = start(exe, profile)
        try:
            go(page, site + "/login")
            check("redirect followed", ev(page, "location.pathname") == "/home")
            home = [s for s in seen if s[1] == "/home"][-1]
            check("cookies set during the redirect are sent",
                  home[2].get("cookie") in ("session=abc; pref=light", "pref=light; session=abc"), home[2].get("cookie"))
            check("HttpOnly hidden from document.cookie", ev(page, "document.cookie") == "pref=light", ev(page, "document.cookie"))
            ev(page, "document.cookie = 'session=hacked'; document.cookie = 'mine=1'")
            r = ev(page, "fetch('/api/c').then((r) => r.json())")
            check("scripts cannot replace HttpOnly cookies", r and "session=abc" in r["cookie"] and "mine=1" in r["cookie"], r)
            r = ev(page, "fetch('/api/x', { method: 'PUT', body: 'data', headers: { Authorization: 'Bearer t', "
                         "'X-Custom': 'yes', 'Content-Type': 'text/x-test', 'Sec-Secret': 'no' } }).then((r) => r.json())")
            check("PUT with headers", r and r["method"] == "PUT" and r["auth"] == "Bearer t" and r["x"] == "yes"
                  and r["type"] == "text/x-test" and r["body"] == "data" and r["secret"] is None, r)
            r = ev(page, "fetch('/api/x', { method: 'DELETE' }).then((r) => r.json())")
            check("DELETE", r and r["method"] == "DELETE", r)
            r = ev(page, "new Promise((ok) => { const x = new XMLHttpRequest(); x.open('PATCH', '/api/x'); "
                         "x.setRequestHeader('X-Custom', 'xhr'); x.onload = () => ok(JSON.parse(x.responseText)); x.send('p'); })")
            check("XHR PATCH", r and r["method"] == "PATCH" and r["x"] == "xhr" and r["body"] == "p", r)
            r = ev(page, "fetch('/api/x', { method: 'HEAD' }).then((r) => r.status)")
            check("HEAD", r == 200, r)
            ev(page, "localStorage.setItem('k', 'v'); sessionStorage.setItem('s', '1'); 1")
            ev(page, "const f = document.createElement('form'); f.method = 'post'; f.action = '/form'; "
                     "f.innerHTML = '<input name=a value=1>'; document.body.append(f); f.submit(); 1")
            page.wait_event("Page.loadEventFired")
            posts = [(s[0], s[1]) for s in seen if s[1] in ("/form", "/done")]
            check("303 after POST continues with GET", posts[-2:] == [("POST", "/form"), ("GET", "/done")], posts)
        finally:
            page.call("Browser.close")
            proc.wait(timeout=10)
        check("profile saved", os.path.exists(os.path.join(profile, "cookies.json"))
              and os.listdir(os.path.join(profile, "storage")), os.listdir(profile))
        # A restart with the same profile.
        proc, page = start(exe, profile)
        try:
            go(page, site + "/home")
            home = [s for s in seen if s[1] == "/home"][-1]
            check("persistent cookies survive a restart, session ones do not",
                  home[2].get("cookie") == "pref=light", home[2].get("cookie"))
            check("localStorage survives a restart", ev(page, "localStorage.getItem('k')") == "v")
            check("sessionStorage does not", ev(page, "sessionStorage.length") == 0)
        finally:
            page.call("Browser.close")
            proc.wait(timeout=10)
    server.shutdown()
    failed = [r for r in results if not r[1]]
    for name, ok, detail in results:
        print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({detail})"))
    if failed:
        sys.exit(1)
    print(f"all {len(results)} checks passed")


if __name__ == "__main__":
    main()
