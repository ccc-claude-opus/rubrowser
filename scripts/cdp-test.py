#!/usr/bin/env python3
"""End-to-end test of rubrowser's Chrome DevTools Protocol server.

Starts a small HTTP server with a test page, runs
`rubrowser --headless --remote-debugging-port 0`, and drives it the way
automation tools do: the /json endpoints, a page connection (Page,
Runtime, DOM, CSS, Network, Input, Log) and a browser connection with a
flattened session (Target). Uses only the Python standard library (a
minimal WebSocket client is included).

Usage: scripts/cdp-test.py [path/to/rubrowser]
(default: target/debug/rubrowser; build it with `cargo build`).
"""

import base64
import http.client
import http.server
import json
import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PAGE = b"""<!doctype html><meta charset=utf-8><title>CDP test</title>
<style>.card { color: rgb(10, 20, 30); padding: 4px }</style>
<p class=card id=card>card</p>
<button id=btn style="width: 120px; height: 40px">add</button> <span id=count>0</span>
<input id=field>
<script>
  console.log('hello from page');
  document.getElementById('btn').addEventListener('click', () => {
    const c = document.getElementById('count');
    c.textContent = String(Number(c.textContent) + 1);
  });
  fetch('/data.json').then((r) => r.json()).then((d) => { window.loaded = d.n; });
</script>"""
DATA = b'{"n": 42, "name": "\xe6\xb8\xac\xe8\xa9\xa6"}'

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body, ctype = (PAGE, "text/html; charset=utf-8") if self.path in ("/", "/index.html") else \
            (DATA, "application/json") if self.path == "/data.json" else (None, None)
        if body is None:
            return self.send_error(404)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class WebSocket:
    """A minimal RFC 6455 client: text frames, masking, no extensions."""

    def __init__(self, url):
        m = re.match(r"ws://([^:/]+):(\d+)(/.*)", url)
        host, port, path = m.group(1), int(m.group(2)), m.group(3)
        self.sock = socket.create_connection((host, port), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            head += self.sock.recv(1)
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise RuntimeError("handshake failed: " + head.decode(errors="replace"))
        self.next_id = 0
        self.events = []

    def send_text(self, text):
        data = text.encode()
        mask = os.urandom(4)
        n = len(data)
        head = bytes([0x81]) + (bytes([0x80 | n]) if n < 126 else
                                bytes([0x80 | 126]) + struct.pack(">H", n) if n < 65536 else
                                bytes([0x80 | 127]) + struct.pack(">Q", n))
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def recv_exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.sock.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("closed")
            buf += chunk
        return buf

    def recv_text(self):
        data = b""
        while True:
            b0, b1 = self.recv_exact(2)
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack(">H", self.recv_exact(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self.recv_exact(8))[0]
            payload = self.recv_exact(n)
            if b0 & 0x0F == 8:
                raise ConnectionError("closed by the server")
            data += payload
            if b0 & 0x80:
                return data.decode()

    def call(self, method, params=None, session=None):
        self.next_id += 1
        msg = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        self.send_text(json.dumps(msg))
        while True:
            reply = json.loads(self.recv_text())
            if reply.get("id") == self.next_id:
                return reply
            self.events.append(reply)

    def wait_event(self, method, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            for i, e in enumerate(self.events):
                if e.get("method") == method:
                    return self.events.pop(i)
            self.sock.settimeout(max(0.1, end - time.time()))
            try:
                self.events.append(json.loads(self.recv_text()))
            except socket.timeout:
                break
        return None


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug", "rubrowser")
    if not os.path.exists(exe):
        sys.exit(f"{exe} not found: build it with `cargo build`")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    site = f"http://127.0.0.1:{server.server_address[1]}/"

    proc = subprocess.Popen([exe, "--headless", "--remote-debugging-port", "0"],
                            stderr=subprocess.PIPE, text=True)
    port = None
    stderr_lines = []
    for line in proc.stderr:
        stderr_lines.append(line)
        m = re.search(r"DevTools listening on ws://127\.0\.0\.1:(\d+)/", line)
        if m:
            port = int(m.group(1))
            break
    threading.Thread(target=lambda: [stderr_lines.append(l) for l in proc.stderr], daemon=True).start()
    if not port:
        proc.kill()
        sys.exit("rubrowser did not print its DevTools address:\n" + "".join(stderr_lines))

    try:
        run(port, site)
    except Exception as e:  # noqa: BLE001 - reported as a failure
        check("no exception", False, repr(e))
    finally:
        try:
            proc.wait(timeout=10)
            check("Browser.close quits", True)
        except subprocess.TimeoutExpired:
            check("Browser.close quits", False)
            proc.kill()
        server.shutdown()

    failed = [r for r in results if not r[1]]
    for name, ok, detail in results:
        print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({detail})"))
    if failed:
        print("rubrowser output:\n" + "".join(stderr_lines[-40:]))
        sys.exit(1)
    print(f"all {len(results)} checks passed")


def get(port, path, host=None):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    c.request("GET", path, headers={"Host": host} if host else {})
    r = c.getresponse()
    return r.status, r.read().decode()


def run(port, site):
    status, body = get(port, "/json/version")
    version = json.loads(body)
    check("/json/version", status == 200 and version["Browser"].startswith("rubrowser/")
          and version["webSocketDebuggerUrl"].startswith(f"ws://127.0.0.1:{port}/devtools/browser/"), body)
    status, _ = get(port, "/json/version", host="evil.example")
    check("other Host headers are refused", status == 403, status)
    targets = json.loads(get(port, "/json/list")[1])
    check("/json/list", len(targets) == 1 and targets[0]["type"] == "page", targets)

    page = WebSocket(targets[0]["webSocketDebuggerUrl"])
    for domain in ["Page", "Runtime", "Network", "Log", "DOM"]:
        page.call(domain + ".enable")
    nav = page.call("Page.navigate", {"url": site})
    check("Page.navigate", "frameId" in nav.get("result", {}), nav)
    check("Page.loadEventFired", page.wait_event("Page.loadEventFired") is not None)
    console = page.wait_event("Runtime.consoleAPICalled")
    check("Runtime.consoleAPICalled", console and console["params"]["args"][0]["value"] == "hello from page", console)
    sent = page.wait_event("Network.requestWillBeSent")
    check("Network.requestWillBeSent", sent and sent["params"]["request"]["url"] == site, sent)

    r = page.call("Runtime.evaluate", {"expression": "document.title"})
    check("Runtime.evaluate", r["result"]["result"] == {"type": "string", "value": "CDP test"}, r)
    r = page.call("Runtime.evaluate", {"expression": "fetch('/data.json').then((r) => r.json())",
                                       "awaitPromise": True, "returnByValue": True})
    check("Runtime.evaluate awaitPromise", r["result"]["result"].get("value") == {"n": 42, "name": "測試"}, r)
    r = page.call("Runtime.evaluate", {"expression": "nope.x"})
    check("exceptions", "ReferenceError" in r["result"].get("exceptionDetails", {})
          .get("exception", {}).get("description", ""), r)
    r = page.call("Runtime.callFunctionOn", {"functionDeclaration": "(a, b) => a + b",
                                             "arguments": [{"value": 2}, {"value": 3}], "returnByValue": True})
    check("Runtime.callFunctionOn", r["result"]["result"].get("value") == 5, r)

    # The page's fetch of /data.json: response and body.
    fetched = None
    for _ in range(40):
        e = page.wait_event("Network.responseReceived", timeout=5)
        if e is None:
            break
        if e["params"]["response"]["url"].endswith("/data.json"):
            fetched = e
            break
    check("Network.responseReceived", fetched and fetched["params"]["response"]["status"] == 200
          and fetched["params"]["response"]["mimeType"] == "application/json", fetched)
    if fetched:
        body = page.call("Network.getResponseBody", {"requestId": fetched["params"]["requestId"]})
        check("Network.getResponseBody", json.loads(body["result"]["body"]) == {"n": 42, "name": "測試"}, body)

    doc = page.call("DOM.getDocument", {"depth": -1})["result"]["root"]
    check("DOM.getDocument", doc["nodeName"] == "#document" and doc["documentURL"] == site, doc.get("nodeName"))
    card = page.call("DOM.querySelector", {"nodeId": doc["nodeId"], "selector": "#card"})["result"]["nodeId"]
    html = page.call("DOM.getOuterHTML", {"nodeId": card})["result"]["outerHTML"]
    check("DOM.querySelector / getOuterHTML", html == '<p class="card" id="card">card</p>', html)
    page.call("DOM.setAttributeValue", {"nodeId": card, "name": "data-x", "value": "1"})
    r = page.call("Runtime.evaluate", {"expression": "document.getElementById('card').dataset.x"})
    check("DOM.setAttributeValue", r["result"]["result"].get("value") == "1", r)
    computed = page.call("CSS.getComputedStyleForNode", {"nodeId": card})["result"]["computedStyle"]
    color = next((p["value"] for p in computed if p["name"] == "color"), None)
    check("CSS.getComputedStyleForNode", color == "rgb(10, 20, 30)", color)
    matched = page.call("CSS.getMatchedStylesForNode", {"nodeId": card})["result"]["matchedCSSRules"]
    check("CSS.getMatchedStylesForNode", any(m["rule"]["selectorList"]["text"] == ".card" for m in matched), matched)

    # Clicking the button through Input: its listener runs.
    btn = page.call("DOM.querySelector", {"nodeId": doc["nodeId"], "selector": "#btn"})["result"]["nodeId"]
    quad = page.call("DOM.getBoxModel", {"nodeId": btn})["result"]["model"]["content"]
    x, y = (quad[0] + quad[2]) / 2, (quad[1] + quad[5]) / 2
    for kind in ["mouseMoved", "mousePressed", "mouseReleased"]:
        page.call("Input.dispatchMouseEvent", {"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1})
    r = page.call("Runtime.evaluate", {"expression": "document.getElementById('count').textContent"})
    check("Input.dispatchMouseEvent clicks", r["result"]["result"].get("value") == "1", r)
    field = page.call("DOM.querySelector", {"nodeId": doc["nodeId"], "selector": "#field"})["result"]["nodeId"]
    page.call("DOM.focus", {"nodeId": field})
    page.call("Input.insertText", {"text": "héllo"})
    for t in ["keyDown", "keyUp"]:
        page.call("Input.dispatchKeyEvent", {"type": t, "key": "!", "text": "!"})
    r = page.call("Runtime.evaluate", {"expression": "document.getElementById('field').value"})
    check("Input.insertText / dispatchKeyEvent", r["result"]["result"].get("value") == "héllo!", r)

    shot = page.call("Page.captureScreenshot")["result"]["data"]
    check("Page.captureScreenshot", base64.b64decode(shot)[:8] == b"\x89PNG\r\n\x1a\n", shot[:20])
    r = page.call("No.suchMethod")
    check("unknown methods", r.get("error", {}).get("code") == -32601, r)

    browser = WebSocket(json.loads(get(port, "/json/version")[1])["webSocketDebuggerUrl"])
    infos = browser.call("Target.getTargets")["result"]["targetInfos"]
    check("Target.getTargets", len(infos) == 1 and infos[0]["url"] == site, infos)
    sid = browser.call("Target.attachToTarget", {"targetId": infos[0]["targetId"], "flatten": True})["result"]["sessionId"]
    r = browser.call("Runtime.evaluate", {"expression": "loaded + 1"}, session=sid)
    check("flattened sessions", r.get("sessionId") == sid and r["result"]["result"].get("value") == 43, r)
    new = browser.call("Target.createTarget", {"url": site + "#second"})["result"]["targetId"]
    check("Target.createTarget", len(json.loads(get(port, "/json/list")[1])) == 2)
    browser.call("Target.closeTarget", {"targetId": new})
    check("Target.closeTarget", len(json.loads(get(port, "/json/list")[1])) == 1)
    browser.call("Browser.close")


if __name__ == "__main__":
    main()
