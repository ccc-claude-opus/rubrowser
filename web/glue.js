// Glue between a single <canvas> and rubrowser.wasm.
// It only forwards input, copies pixels and performs requests the browser
// queues (fetch, cursor, title, clipboard); everything on screen — including
// the address bar and tabs — is drawn by Rust.

const canvas = document.getElementById('screen');
const ctx = canvas.getContext('2d');

const { instance } = await WebAssembly.instantiateStreaming(fetch('rubrowser.wasm'), {});
const rb = instance.exports;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

let width = 0;
let height = 0;
let scale = 1;

function bytesIn(ptr, len) {
  return new Uint8Array(rb.memory.buffer, ptr, len);
}

/** Writes bytes into the shared buffer and returns their length. */
function put(bytes) {
  const ptr = rb.rb_web_buffer(bytes.length);
  bytesIn(ptr, bytes.length).set(bytes);
  return bytes.length;
}

function getString(len) {
  return decoder.decode(bytesIn(rb.rb_web_buffer_ptr(), len).slice());
}

function fitCanvas() {
  scale = window.devicePixelRatio || 1;
  width = Math.max(1, Math.floor(window.innerWidth * scale));
  height = Math.max(1, Math.floor(window.innerHeight * scale));
  canvas.width = width;
  canvas.height = height;
  canvas.style.width = window.innerWidth + 'px';
  canvas.style.height = window.innerHeight + 'px';
}

fitCanvas();
rb.rb_web_init(width, height, scale);

window.addEventListener('resize', () => {
  fitCanvas();
  rb.rb_web_resize(width, height, scale);
  schedule();
});

// ----- requests from the browser -------------------------------------------

const CURSORS = ['default', 'pointer', 'text'];
let lastTitle = '';

function service() {
  for (;;) {
    const id = rb.rb_web_next_fetch();
    if (id === 0) break;
    const url = getString(rb.rb_web_buffer_len());
    load(id, url);
  }
  for (;;) {
    const len = rb.rb_web_next_log();
    if (len < 0) break;
    console.log('[rubrowser]', getString(len));
  }
  canvas.style.cursor = CURSORS[rb.rb_web_cursor()] || 'default';
  const title = getString(rb.rb_web_title());
  if (title !== lastTitle) {
    document.title = title;
    lastTitle = title;
  }
}

async function load(id, url) {
  let ok = 1;
  let body;
  try {
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HTTP ${resp.status} ${resp.statusText}`);
    body = new Uint8Array(await resp.arrayBuffer());
  } catch (e) {
    ok = 0;
    body = encoder.encode(String(e.message || e));
  }
  rb.rb_web_fetch_done(id, put(body), ok);
  after();
}

// ----- input -----------------------------------------------------------------

function mods(e) {
  return (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0);
}

const NAMED = {
  Backspace: 8, Tab: 9, Enter: 13, Escape: 27, ' ': 32, PageUp: 33, PageDown: 34,
  End: 35, Home: 36, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40,
  Delete: 46, F5: 116, '[': 219, ']': 221,
};

function keyCode(e) {
  if (e.key in NAMED) return NAMED[e.key];
  if (/^Key[A-Z]$/.test(e.code)) return e.code.charCodeAt(3);
  if (/^Digit[0-9]$/.test(e.code)) return e.code.charCodeAt(5);
  return 0;
}

canvas.addEventListener('mousemove', (e) => { rb.rb_web_mouse(0, e.offsetX, e.offsetY, 0); after(); });
canvas.addEventListener('mousedown', (e) => { rb.rb_web_mouse(1, e.offsetX, e.offsetY, e.button); after(); });
window.addEventListener('mouseup', (e) => {
  const r = canvas.getBoundingClientRect();
  rb.rb_web_mouse(2, Math.round(e.clientX - r.left), Math.round(e.clientY - r.top), e.button);
  after();
});
canvas.addEventListener('mouseleave', () => { rb.rb_web_mouse(3, 0, 0, 0); after(); });
canvas.addEventListener('contextmenu', (e) => e.preventDefault());
canvas.addEventListener('wheel', (e) => {
  e.preventDefault();
  const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? height / scale : 1;
  rb.rb_web_wheel(Math.round(e.deltaX * unit), Math.round(e.deltaY * unit));
  after();
}, { passive: false });

window.addEventListener('keydown', (e) => {
  const code = keyCode(e);
  const command = e.ctrlKey || e.metaKey;
  if (code) rb.rb_web_key(1, code, mods(e));
  if (e.key.length === 1 && !command) {
    rb.rb_web_text(put(encoder.encode(e.key)));
  }
  // Let the real copy/paste events happen; keep everything else for us.
  if (!(command && ['c', 'v', 'x'].includes(e.key.toLowerCase()))) e.preventDefault();
  after();
});
window.addEventListener('keyup', (e) => {
  const code = keyCode(e);
  if (code) rb.rb_web_key(0, code, mods(e));
  after();
});
window.addEventListener('paste', (e) => {
  const text = e.clipboardData.getData('text/plain').split('\n')[0];
  if (text) rb.rb_web_text(put(encoder.encode(text)));
  e.preventDefault();
  after();
});
for (const kind of ['copy', 'cut']) {
  window.addEventListener(kind, (e) => {
    // The keydown handler already ran the browser's copy/cut.
    e.clipboardData.setData('text/plain', getString(rb.rb_web_clipboard()));
    e.preventDefault();
    after();
  });
}

// ----- frames ----------------------------------------------------------------

let frameRequested = false;
let timer = 0;

function frame(now) {
  frameRequested = false;
  const ptr = rb.rb_web_frame(now);
  if (ptr !== 0) {
    // Wasm memory may have grown, so build a fresh view every frame.
    const pixels = new Uint8ClampedArray(rb.memory.buffer, ptr, width * height * 4);
    ctx.putImageData(new ImageData(pixels, width, height), 0, 0);
  }
  service();
  const wait = rb.rb_web_next_wakeup(performance.now());
  clearTimeout(timer);
  if (wait >= 0) timer = setTimeout(schedule, wait);
}

function schedule() {
  if (!frameRequested) {
    frameRequested = true;
    requestAnimationFrame(frame);
  }
}

function after() {
  service();
  schedule();
}

if (location.hash.length > 1) {
  rb.rb_web_open(put(encoder.encode(decodeURIComponent(location.hash.slice(1)))));
}
after();
