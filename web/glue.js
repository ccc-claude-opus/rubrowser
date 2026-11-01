// Minimal glue between the canvas and rubrowser.wasm.
// It only resizes the canvas, forwards events, and copies pixels;
// everything on screen is drawn by Rust.

const canvas = document.getElementById('screen');
const ctx = canvas.getContext('2d');

const { instance } = await WebAssembly.instantiateStreaming(fetch('rubrowser.wasm'), {});
const rb = instance.exports;

let width = 0;
let height = 0;

function fitCanvas() {
  const dpr = window.devicePixelRatio || 1;
  width = Math.max(1, Math.floor(window.innerWidth * dpr));
  height = Math.max(1, Math.floor(window.innerHeight * dpr));
  canvas.width = width;
  canvas.height = height;
  canvas.style.width = window.innerWidth + 'px';
  canvas.style.height = window.innerHeight + 'px';
}

fitCanvas();
rb.rb_web_init(width, height);

window.addEventListener('resize', () => {
  fitCanvas();
  rb.rb_web_resize(width, height);
});

function frame() {
  const ptr = rb.rb_web_frame();
  // Wasm memory may have grown, so build a fresh view every frame.
  const pixels = new Uint8ClampedArray(rb.memory.buffer, ptr, width * height * 4);
  ctx.putImageData(new ImageData(pixels, width, height), 0, 0);
  requestAnimationFrame(frame);
}

requestAnimationFrame(frame);
