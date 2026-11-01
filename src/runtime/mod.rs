//! Runs page programs (`.wasm`) with the wasmi interpreter.
//!
//! A page draws into its own [`Canvas`] the size of the browser's content
//! area, through the `rb` imports defined here (ABI v0, see `doc/abi.md`).
//! Every call into the page gets a fuel budget and the page's memory is
//! capped, so a misbehaving page cannot hang or exhaust the browser.

use wasmi::{
    Caller, Config, Engine, Error, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TrapCode, TypedFunc,
};

use crate::abi::event;
use crate::gfx::{Canvas, Color, Painter, Rect, text_width};

/// Instructions a page may run per call (frame or event).
pub const FUEL_PER_CALL: u64 = 30_000_000;
/// Maximum linear memory of a page.
pub const MAX_MEMORY: usize = 256 << 20;
/// Longest string a page may pass to the browser.
const MAX_STRING: usize = 64 << 10;

/// Requests a page made during a call, for the browser to act on.
#[derive(Debug, Default)]
pub struct Output {
    pub navigate: Option<String>,
    pub title: Option<String>,
    pub logs: Vec<String>,
    pub redraw: bool,
    /// Document height the page asked for, if it wants the browser to scroll.
    pub scroll_height: Option<i32>,
}

struct State {
    canvas: Canvas,
    scale: f32,
    /// Document scroll offset, in logical pixels.
    scroll_y: i32,
    now: f64,
    out: Output,
    limits: StoreLimits,
}

impl State {
    fn painter(&mut self) -> Painter<'_> {
        let mut p = Painter::new(&mut self.canvas, self.scale);
        p.translate(0, -self.scroll_y);
        p
    }
}

/// A loaded page program.
pub struct Page {
    store: Store<State>,
    frame_fn: Option<TypedFunc<(i32, i32, f64), ()>>,
    event_fn: Option<TypedFunc<(i32, i32, i32, i32), ()>>,
}

impl Page {
    /// Compiles and instantiates `wasm`, then calls its `rb_init` export.
    /// `width` and `height` are the physical size of the page's canvas.
    pub fn load(
        wasm: &[u8],
        width: usize,
        height: usize,
        scale: f32,
        now: f64,
    ) -> Result<Page, String> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| format!("invalid page: {e}"))?;
        let state = State {
            canvas: Canvas::new(width, height),
            scale,
            scroll_y: 0,
            now,
            out: Output::default(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY)
                .instances(1)
                .build(),
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| e.to_string())?;
        let linker = linker(&engine).map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(describe)?;
        let frame_fn = instance.get_typed_func(&store, "rb_frame").ok();
        let event_fn = instance.get_typed_func(&store, "rb_event").ok();
        if frame_fn.is_none() {
            return Err("not a rubrowser page: missing export `rb_frame`".into());
        }
        let init_fn = instance.get_typed_func::<(), ()>(&store, "rb_init").ok();
        let mut page = Page {
            store,
            frame_fn,
            event_fn,
        };
        if let Some(init) = init_fn {
            page.store
                .set_fuel(FUEL_PER_CALL)
                .map_err(|e| e.to_string())?;
            init.call(&mut page.store, ()).map_err(describe)?;
        }
        Ok(page)
    }

    /// Resizes the page canvas (physical pixels).
    pub fn resize(&mut self, width: usize, height: usize, scale: f32) {
        let s = self.store.data_mut();
        s.canvas.resize(width, height);
        s.scale = scale;
    }

    pub fn canvas(&self) -> &Canvas {
        &self.store.data().canvas
    }

    /// Takes the requests the page made since the last call.
    pub fn take_output(&mut self) -> Output {
        std::mem::take(&mut self.store.data_mut().out)
    }

    fn prepare(&mut self, now: f64) -> Result<(), String> {
        self.store.data_mut().now = now;
        self.store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| e.to_string())
    }

    /// Lets the page draw itself. `width` and `height` are the logical
    /// viewport size; `scroll_y` is the document scroll offset.
    pub fn frame(
        &mut self,
        width: i32,
        height: i32,
        scroll_y: i32,
        now: f64,
    ) -> Result<(), String> {
        self.prepare(now)?;
        let s = self.store.data_mut();
        s.scroll_y = scroll_y;
        s.canvas.reset_clip();
        let Some(f) = self.frame_fn else {
            return Ok(());
        };
        f.call(&mut self.store, (width, height, now))
            .map_err(describe)
    }

    /// Delivers an event (`abi::event` kind and arguments).
    pub fn event(&mut self, kind: i32, a: i32, b: i32, c: i32, now: f64) -> Result<(), String> {
        let Some(f) = self.event_fn else {
            return Ok(());
        };
        self.prepare(now)?;
        f.call(&mut self.store, (kind, a, b, c)).map_err(describe)
    }

    pub fn focus(&mut self, focused: bool, now: f64) -> Result<(), String> {
        let kind = if focused { event::FOCUS } else { event::BLUR };
        self.event(kind, 0, 0, 0, now)
    }
}

/// Turns a wasmi error into a message for the error page.
fn describe(e: Error) -> String {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => {
            "the page ran too long without returning (possible infinite loop)".into()
        }
        Some(TrapCode::UnreachableCodeReached) => "the page panicked".into(),
        Some(code) => format!("the page crashed: {code}"),
        None => e.to_string(),
    }
}

fn memory(caller: &Caller<'_, State>) -> Result<Memory, Error> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| Error::new("page does not export `memory`"))
}

/// Checks that `ptr..ptr+len` lies inside the page's memory.
fn range(data: &[u8], ptr: i32, len: usize) -> Result<std::ops::Range<usize>, Error> {
    let start = ptr as u32 as usize;
    let end = start
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| Error::new("pointer out of bounds"))?;
    Ok(start..end)
}

fn read_str(caller: &Caller<'_, State>, ptr: i32, len: i32) -> Result<String, Error> {
    let len = (len as u32 as usize).min(MAX_STRING);
    let data = memory(caller)?.data(caller);
    let r = range(data, ptr, len)?;
    Ok(String::from_utf8_lossy(&data[r]).into_owned())
}

fn color(c: i32) -> Color {
    c as u32 & 0xff_ffff
}

fn linker(engine: &Engine) -> Result<Linker<State>, wasmi::errors::LinkerError> {
    let mut l = Linker::<State>::new(engine);
    l.func_wrap("rb", "clear", |mut c: Caller<'_, State>, col: i32| {
        c.data_mut().canvas.clear(color(col));
    })?;
    l.func_wrap(
        "rb",
        "fill_rect",
        |mut c: Caller<'_, State>, x: i32, y: i32, w: i32, h: i32, col: i32| {
            c.data_mut()
                .painter()
                .fill_rect(Rect::new(x, y, w, h), color(col));
        },
    )?;
    l.func_wrap(
        "rb",
        "line",
        |mut c: Caller<'_, State>, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, col: i32| {
            // Keep a single call cheap enough that fuel bounds the total work.
            let t = t.clamp(1, 64);
            let bound = |v: i32| v.clamp(-1 << 15, 1 << 15);
            c.data_mut()
                .painter()
                .line(bound(x0), bound(y0), bound(x1), bound(y1), t, color(col));
        },
    )?;
    l.func_wrap(
        "rb",
        "draw_text",
        |mut c: Caller<'_, State>,
         ptr: i32,
         len: i32,
         x: i32,
         y: i32,
         size: i32,
         col: i32|
         -> Result<i32, Error> {
            let text = read_str(&c, ptr, len)?;
            let size = size.clamp(1, 256);
            Ok(c.data_mut().painter().text(x, y, size, &text, color(col)))
        },
    )?;
    l.func_wrap(
        "rb",
        "text_width",
        |c: Caller<'_, State>, ptr: i32, len: i32, size: i32| -> Result<i32, Error> {
            let text = read_str(&c, ptr, len)?;
            Ok(text_width(size.clamp(1, 256), c.data().scale, &text))
        },
    )?;
    l.func_wrap(
        "rb",
        "blit",
        |mut c: Caller<'_, State>, ptr: i32, w: i32, h: i32, x: i32, y: i32| -> Result<(), Error> {
            if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
                return Ok(());
            }
            let (w, h) = (w as usize, h as usize);
            let mem = memory(&c)?;
            let (data, state) = mem.data_and_store_mut(&mut c);
            let r = range(data, ptr, w * h * 4)?;
            let pixels: Vec<Color> = data[r]
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff)
                .collect();
            let mut p = state.painter();
            let dst = p.to_physical(Rect::new(x, y, w as i32, h as i32));
            p.canvas().blit_scaled(dst, w, h, &pixels);
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "navigate",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> Result<(), Error> {
            let url = read_str(&c, ptr, len)?;
            c.data_mut().out.navigate = Some(url);
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "set_title",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> Result<(), Error> {
            let title = read_str(&c, ptr, len)?;
            c.data_mut().out.title = Some(title);
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "log",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> Result<(), Error> {
            let msg = read_str(&c, ptr, len)?;
            let logs = &mut c.data_mut().out.logs;
            if logs.len() < 100 {
                logs.push(msg);
            }
            Ok(())
        },
    )?;
    l.func_wrap("rb", "request_redraw", |mut c: Caller<'_, State>| {
        c.data_mut().out.redraw = true;
    })?;
    l.func_wrap("rb", "now", |c: Caller<'_, State>| -> f64 { c.data().now })?;
    l.func_wrap(
        "rb",
        "set_scroll_height",
        |mut c: Caller<'_, State>, h: i32| {
            c.data_mut().out.scroll_height = Some(h.max(0));
        },
    )?;
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wasmi's `wat` feature (dev-dependency) accepts the text format.
    fn wasm(wat: &str) -> Vec<u8> {
        wat.as_bytes().to_vec()
    }

    const PAGE: &str = r#"
        (module
          (import "rb" "clear" (func $clear (param i32)))
          (import "rb" "fill_rect" (func $fill_rect (param i32 i32 i32 i32 i32)))
          (import "rb" "set_title" (func $set_title (param i32 i32)))
          (import "rb" "navigate" (func $navigate (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "Hi")
          (data (i32.const 16) "next.wasm")
          (global $clicks (mut i32) (i32.const 0))
          (func (export "rb_init") (call $set_title (i32.const 0) (i32.const 2)))
          (func (export "rb_frame") (param i32 i32 f64)
            (call $clear (i32.const 0x112233))
            (call $fill_rect (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 1) (i32.const 0xff0000)))
          (func (export "rb_event") (param $k i32) (param i32 i32 i32)
            (if (i32.eq (local.get $k) (i32.const 2))
              (then (call $navigate (i32.const 16) (i32.const 9))))))
    "#;

    #[test]
    fn load_draw_and_events() {
        let mut page = Page::load(&wasm(PAGE), 4, 4, 2.0, 0.0).unwrap();
        assert_eq!(page.take_output().title.as_deref(), Some("Hi"));
        page.frame(2, 2, 0, 0.0).unwrap();
        let px = page.canvas().pixels();
        assert_eq!(&px[..3], &[0xff0000, 0xff0000, 0x112233]);
        page.event(event::MOUSE_DOWN, 1, 1, 0, 0.0).unwrap();
        assert_eq!(page.take_output().navigate.as_deref(), Some("next.wasm"));
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let wat = r#"(module
            (func (export "rb_frame") (param i32 i32 f64) (loop $l (br $l))))"#;
        let mut page = Page::load(&wasm(wat), 1, 1, 1.0, 0.0).unwrap();
        let err = page.frame(1, 1, 0, 0.0).unwrap_err();
        assert!(err.contains("too long"), "{err}");
    }

    #[test]
    fn bad_pointer_traps_instead_of_crashing() {
        let wat = r#"(module
            (import "rb" "log" (func $log (param i32 i32)))
            (memory (export "memory") 1)
            (func (export "rb_frame") (param i32 i32 f64)
              (call $log (i32.const 65530) (i32.const 100))))"#;
        let mut page = Page::load(&wasm(wat), 1, 1, 1.0, 0.0).unwrap();
        assert!(page.frame(1, 1, 0, 0.0).is_err());
    }

    #[test]
    fn rejects_non_pages() {
        assert!(Page::load(b"not wasm", 1, 1, 1.0, 0.0).is_err());
        let err = Page::load(&wasm("(module)"), 1, 1, 1.0, 0.0).err().unwrap();
        assert!(err.contains("rb_frame"));
    }

    #[test]
    fn memory_is_capped() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "rb_frame") (param i32 i32 f64)
              (if (i32.ne (memory.grow (i32.const 8192)) (i32.const -1))
                (then unreachable))))"#;
        let mut page = Page::load(&wasm(wat), 1, 1, 1.0, 0.0).unwrap();
        page.frame(1, 1, 0, 0.0).unwrap();
    }
}
