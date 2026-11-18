#!/usr/bin/env python3
"""End-to-end test of uploads and downloads in rubrowser.

Starts a small HTTP server on 127.0.0.1, then runs
`rubrowser --screenshot` on its pages (no window, so file inputs stay
empty). The pages:

1. `/`            fetch() POSTs a FormData (text, a File with a non-ASCII
                  name and CR/LF/NUL bytes, a Blob of all 256 byte values)
                  as multipart/form-data, then goes to /form.
2. `/form`        a <form enctype=multipart/form-data> with a text field
                  and an empty file input, sent with form.submit().
3. `/form-upload` saves a blob: URL (<a download>), a same-origin
                  <a download> link, then follows a link the server marks
                  `Content-Disposition: attachment`.

The server checks every part it receives; afterwards the script checks the
files in the download folder (RUBROWSER_DOWNLOAD_DIR, a temporary folder).

Usage: scripts/file-transfer-test.py [path/to/rubrowser]
(default: target/debug/rubrowser; build it with `cargo build`).
"""

import http.server
import os
import subprocess
import sys
import tempfile
import threading
import urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FILE_NAME = '報告 "a".txt'
FILE_BYTES = b"hello\r\n--not-a-boundary\x00binary"
ALL_BYTES = bytes(range(256))
PLAIN = "plain download\n".encode()
REPORT = "id,名稱\n1,測試\n".encode()

results = {}  # check name -> (ok, detail)


def check(name, ok, detail=""):
    results[name] = (bool(ok), detail)


def parse_multipart(ctype, body):
    """(headers dict, data) parts of a multipart/form-data body."""
    boundary = ctype.split("boundary=", 1)[1].strip().encode()
    delim = b"--" + boundary
    assert body.startswith(delim), "body starts with the boundary"
    assert body.endswith(delim + b"--\r\n"), "body ends with the closing boundary"
    parts = []
    for chunk in body[len(delim):].split(b"\r\n" + delim):
        if chunk.startswith(b"--"):
            break
        head, _, data = chunk[2:].partition(b"\r\n\r\n")
        headers = {}
        for line in head.decode("utf-8").split("\r\n"):
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()
        parts.append((headers, data))
    return parts


def disposition(headers):
    """name and filename of a part (quoted values as sent)."""
    params = {}
    for item in headers.get("content-disposition", "").split(";")[1:]:
        k, _, v = item.strip().partition("=")
        params[k] = v[1:-1] if v.startswith('"') else v
    return params.get("name"), params.get("filename")


INDEX = """<!doctype html><meta charset=utf-8><title>upload</title>
<p id=out>uploading…</p>
<script>
  const fd = new FormData();
  fd.append('title', '測試\\n標題');
  const bytes = new Uint8Array([%s]);
  fd.append('doc', new File([bytes], %s, { type: 'text/plain' }));
  const all = new Uint8Array(256);
  for (let i = 0; i < 256; i++) all[i] = i;
  fd.append('bin', new Blob([all]));
  fetch('/upload', { method: 'POST', body: fd })
    .then((r) => r.json())
    .then((j) => { document.getElementById('out').textContent = 'server: ' + j.ok; location.href = '/form'; })
    .catch((e) => { document.getElementById('out').textContent = 'failed ' + e; });
</script>""" % (",".join(str(b) for b in FILE_BYTES), repr(FILE_NAME))

FORM = """<!doctype html><meta charset=utf-8><title>form</title>
<form id=f method=post action=/form-upload enctype="multipart/form-data">
  <textarea name=note>line1
line2</textarea>
  <input type=file name=attachment>
</form>
<script>document.getElementById('f').submit();</script>"""

DOWNLOADS = """<!doctype html><meta charset=utf-8><title>downloads</title>
<a id=plain href="/files/plain.txt" download="ignored-name.txt">plain</a>
<a id=report href="/report">report</a>
<script>
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob(['blob 內容'], { type: 'text/plain' }));
  a.download = 'from-blob.txt';
  a.click();
  document.getElementById('plain').click();
  document.getElementById('report').click();
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, body, ctype="text/html; charset=utf-8", extra=()):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        if path == "/":
            self.send(INDEX.encode())
        elif path == "/form":
            self.send(FORM.encode())
        elif path == "/files/plain.txt":
            # No Content-Disposition: the link's download name is used.
            self.send(PLAIN, "text/plain")
        elif path == "/report":
            name = urllib.parse.quote("報表.csv")
            self.send(REPORT, "text/csv", [
                ("Content-Disposition", f"attachment; filename=\"report.csv\"; filename*=UTF-8''{name}"),
            ])
        else:
            self.send_error(404)

    def read_body(self):
        return self.rfile.read(int(self.headers.get("Content-Length", 0)))

    def do_POST(self):
        ctype = self.headers.get("Content-Type", "")
        body = self.read_body()
        if self.path == "/upload":
            try:
                parts = parse_multipart(ctype, body)
                names = [disposition(h) for h, _ in parts]
                check("fetch: three parts", len(parts) == 3, names)
                check("fetch: text field", names[0] == ("title", None) and parts[0][1] == "測試\r\n標題".encode(), parts[0])
                check("fetch: file name", names[1] == ("doc", FILE_NAME.replace('"', "%22")), names[1])
                check("fetch: file type", parts[1][0].get("content-type") == "text/plain", parts[1][0])
                check("fetch: file bytes", parts[1][1] == FILE_BYTES, parts[1][1])
                check("fetch: blob name", names[2] == ("bin", "blob"), names[2])
                check("fetch: blob type", parts[2][0].get("content-type") == "application/octet-stream", parts[2][0])
                check("fetch: blob bytes", parts[2][1] == ALL_BYTES, len(parts[2][1]))
            except Exception as e:  # noqa: BLE001 - reported as a failure
                check("fetch: multipart body", False, repr(e))
            self.send(b'{"ok": true}', "application/json")
        elif self.path == "/form-upload":
            try:
                check("form: multipart type", ctype.startswith("multipart/form-data; boundary="), ctype)
                parts = parse_multipart(ctype, body)
                names = [disposition(h) for h, _ in parts]
                check("form: two parts", len(parts) == 2, names)
                check("form: textarea (CRLF)", names[0] == ("note", None) and parts[0][1] == b"line1\r\nline2", parts[0])
                check("form: empty file input", names[1] == ("attachment", "") and parts[1][1] == b""
                      and parts[1][0].get("content-type") == "application/octet-stream", parts[1])
            except Exception as e:  # noqa: BLE001
                check("form: multipart body", False, repr(e))
            self.send(DOWNLOADS.encode())
        else:
            self.send_error(404)


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug", "rubrowser")
    if not os.path.exists(exe):
        sys.exit(f"{exe} not found: build it with `cargo build`")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/"
    with tempfile.TemporaryDirectory() as downloads:
        env = dict(os.environ, RUBROWSER_DOWNLOAD_DIR=downloads)
        shot = os.path.join(downloads, "..", os.path.basename(downloads) + ".png")
        run = subprocess.run([exe, "--screenshot", shot, "--timeout", "20", url],
                             env=env, capture_output=True, text=True, timeout=120)
        if os.path.exists(shot):
            os.remove(shot)
        saved = {n: open(os.path.join(downloads, n), "rb").read() for n in os.listdir(downloads)}
    server.shutdown()

    check("download: blob: URL", saved.get("from-blob.txt") == "blob 內容".encode(), saved.get("from-blob.txt"))
    check("download: <a download> name", saved.get("ignored-name.txt") == PLAIN, sorted(saved))
    check("download: Content-Disposition name", saved.get("報表.csv") == REPORT, sorted(saved))
    check("download: nothing else", len(saved) == 3, sorted(saved))

    failed = 0
    for name, (ok, detail) in results.items():
        print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({detail})"))
        failed += not ok
    expected = 16
    if len(results) < expected:
        print(f"FAIL only {len(results)} of {expected} checks ran (a request never arrived)")
        failed += 1
    if failed:
        print("rubrowser output:\n" + run.stderr)
        sys.exit(1)
    print(f"all {len(results)} checks passed")


if __name__ == "__main__":
    main()
