//! JavaScript runtime: the boa engine compiled to wasm (`js.wasm`).
//!
//! The browser only runs wasm. To run a page's JavaScript it loads this
//! module and hands it source code through the runtime protocol
//! (`rb_alloc` + `rb_eval`). Inside, boa executes the code; DOM access goes
//! through the native `__rb` object, which forwards to the browser's rb
//! imports, and `prelude.js` builds `document`, elements, events and timers
//! on top of it.
//!
//! Build with `scripts/build.sh` (feature `js-runtime`, example
//! `js_runtime`, target `wasm32-unknown-unknown`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::context::time::JsInstant;
use boa_engine::context::{Clock, HostHooks};
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsError, JsResult, JsString, JsValue, Module, NativeFunction, Source,
    js_error, js_string,
};

use crate::sdk::{ffi, read_string};

const PRELUDE: &str = include_str!("prelude.js");

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
    /// Buffers handed out by `rb_alloc`, freed after `rb_eval`.
    static BUFFERS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// Time comes from the browser; there is no OS clock in the sandbox.
struct HostClock;

impl Clock for HostClock {
    fn now(&self) -> JsInstant {
        let ms = unsafe { ffi::now() }.max(0.0);
        let secs = (ms / 1000.0) as u64;
        let nanos = ((ms - secs as f64 * 1000.0) * 1_000_000.0) as u32;
        JsInstant::new(secs, nanos)
    }

    fn system_time_millis(&self) -> i64 {
        unsafe { ffi::wall_time() as i64 }
    }
}

struct Hooks;

impl HostHooks for Hooks {
    fn local_timezone_offset_seconds(&self, _unix_time_seconds: i64) -> i32 {
        0
    }
}

/// boa needs random bytes (`Math.random`, hashing); this is the getrandom
/// "custom" backend, enabled by `.cargo/config.toml`. Seeded from the
/// browser clock: fine for scripts, not for cryptography.
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = (unsafe { ffi::wall_time() } as u64) ^ 0x9E37_79B9_7F4A_7C15 | 1;
        }
        for i in 0..len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            unsafe { *dest.add(i) = (x >> 24) as u8 };
        }
        s.set(x);
    });
    Ok(())
}

// ----- argument helpers -----------------------------------------------------

fn int(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<i32> {
    args.get_or_undefined(i).to_i32(ctx)
}

fn num(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<f64> {
    args.get_or_undefined(i).to_number(ctx)
}

fn text(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    Ok(args
        .get_or_undefined(i)
        .to_string(ctx)?
        .to_std_string_escaped())
}

fn string(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

fn opt_string(s: Option<String>) -> JsValue {
    s.map_or(JsValue::null(), |s| string(&s))
}

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// The `__rb` object: one function per rb import, with JS-friendly types.
fn natives() -> Vec<(&'static str, usize, Native)> {
    vec![
        ("query", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::query(s.as_ptr(), s.len()) }.into())
        }),
        ("queryAll", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            let mut out = vec![0i32; 64];
            loop {
                let n = unsafe {
                    ffi::query_all(s.as_ptr(), s.len(), out.as_mut_ptr(), out.len() as i32)
                } as usize;
                if n <= out.len() {
                    out.truncate(n);
                    break;
                }
                out.resize(n, 0);
            }
            Ok(JsArray::from_iter(out.into_iter().map(JsValue::from), c).into())
        }),
        ("queryWithin", 2, |_, a, c| {
            let root = int(a, 0, c)?;
            let s = text(a, 1, c)?;
            let mut out = vec![0i32; 64];
            loop {
                let n = unsafe {
                    ffi::query_within(
                        root,
                        s.as_ptr(),
                        s.len(),
                        out.as_mut_ptr(),
                        out.len() as i32,
                    )
                } as usize;
                if n <= out.len() {
                    out.truncate(n);
                    break;
                }
                out.resize(n, 0);
            }
            Ok(JsArray::from_iter(out.into_iter().map(JsValue::from), c).into())
        }),
        ("rect", 2, |_, a, c| {
            Ok(unsafe { ffi::rect(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("nodeType", 1, |_, a, c| {
            Ok(unsafe { ffi::node_type(int(a, 0, c)?) }.into())
        }),
        ("relative", 2, |_, a, c| {
            Ok(unsafe { ffi::relative(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("childCount", 1, |_, a, c| {
            Ok(unsafe { ffi::child_count(int(a, 0, c)?) }.into())
        }),
        ("childAt", 2, |_, a, c| {
            Ok(unsafe { ffi::child_at(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("special", 1, |_, a, c| {
            Ok(unsafe { ffi::special(int(a, 0, c)?) }.into())
        }),
        ("tagName", 1, |_, a, c| {
            let h = int(a, 0, c)?;
            Ok(string(
                &read_string(|p, n| unsafe { ffi::tag_name(h, p, n) }).unwrap_or_default(),
            ))
        }),
        ("matches", 2, |_, a, c| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok((unsafe { ffi::matches(h, s.as_ptr(), s.len()) } != 0).into())
        }),
        ("getText", 1, |_, a, c| {
            let h = int(a, 0, c)?;
            Ok(opt_string(read_string(|p, n| unsafe {
                ffi::get_text(h, p, n)
            })))
        }),
        ("setText", 2, |_, a, c| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_text(h, s.as_ptr(), s.len()) };
            Ok(JsValue::undefined())
        }),
        ("getAttr", 2, |_, a, c| {
            let (h, name) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok(opt_string(read_string(|p, n| unsafe {
                ffi::get_attr(h, name.as_ptr(), name.len(), p, n)
            })))
        }),
        ("setAttr", 3, |_, a, c| {
            let (h, name, v) = (int(a, 0, c)?, text(a, 1, c)?, text(a, 2, c)?);
            unsafe { ffi::set_attr(h, name.as_ptr(), name.len(), v.as_ptr(), v.len()) };
            Ok(JsValue::undefined())
        }),
        ("removeAttr", 2, |_, a, c| {
            let (h, name) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::remove_attr(h, name.as_ptr(), name.len()) };
            Ok(JsValue::undefined())
        }),
        ("attrNames", 1, |_, a, c| {
            let h = int(a, 0, c)?;
            let n = unsafe { ffi::attr_count(h) }.max(0);
            let names: Vec<JsValue> = (0..n)
                .filter_map(|i| read_string(|p, cap| unsafe { ffi::attr_name_at(h, i, p, cap) }))
                .map(|s| string(&s))
                .collect();
            Ok(JsArray::from_iter(names, c).into())
        }),
        ("classOp", 3, |_, a, c| {
            let (h, op, s) = (int(a, 0, c)?, int(a, 1, c)?, text(a, 2, c)?);
            Ok((unsafe { ffi::class_op(h, op, s.as_ptr(), s.len()) } != 0).into())
        }),
        ("setStyle", 3, |_, a, c| {
            let (h, p, v) = (int(a, 0, c)?, text(a, 1, c)?, text(a, 2, c)?);
            unsafe { ffi::set_style(h, p.as_ptr(), p.len(), v.as_ptr(), v.len()) };
            Ok(JsValue::undefined())
        }),
        ("getValue", 1, |_, a, c| {
            let h = int(a, 0, c)?;
            Ok(string(
                &read_string(|p, n| unsafe { ffi::get_value(h, p, n) }).unwrap_or_default(),
            ))
        }),
        ("setValue", 2, |_, a, c| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_value(h, s.as_ptr(), s.len()) };
            Ok(JsValue::undefined())
        }),
        ("getHtml", 2, |_, a, c| {
            let (h, outer) = (int(a, 0, c)?, int(a, 1, c)?);
            Ok(string(
                &read_string(|p, n| unsafe { ffi::get_html(h, outer, p, n) }).unwrap_or_default(),
            ))
        }),
        ("setHtml", 2, |_, a, c| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_html(h, s.as_ptr(), s.len()) };
            Ok(JsValue::undefined())
        }),
        ("createElement", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::create_element(s.as_ptr(), s.len()) }.into())
        }),
        ("createText", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::create_text(s.as_ptr(), s.len()) }.into())
        }),
        ("insert", 3, |_, a, c| {
            let (p, ch, b) = (int(a, 0, c)?, int(a, 1, c)?, int(a, 2, c)?);
            Ok((unsafe { ffi::insert(p, ch, b) } != 0).into())
        }),
        ("remove", 1, |_, a, c| {
            unsafe { ffi::remove(int(a, 0, c)?) };
            Ok(JsValue::undefined())
        }),
        ("listen", 3, |_, a, c| {
            let (h, t, id) = (int(a, 0, c)?, text(a, 1, c)?, int(a, 2, c)?);
            unsafe { ffi::listen(h, t.as_ptr(), t.len(), id) };
            Ok(JsValue::undefined())
        }),
        ("unlisten", 3, |_, a, c| {
            let (h, t, id) = (int(a, 0, c)?, text(a, 1, c)?, int(a, 2, c)?);
            unsafe { ffi::unlisten(h, t.as_ptr(), t.len(), id) };
            Ok(JsValue::undefined())
        }),
        ("preventDefault", 0, |_, _, _| {
            unsafe { ffi::prevent_default() };
            Ok(JsValue::undefined())
        }),
        ("stopPropagation", 0, |_, _, _| {
            unsafe { ffi::stop_propagation() };
            Ok(JsValue::undefined())
        }),
        ("setTimeout", 2, |_, a, c| {
            let (ms, id) = (num(a, 0, c)?, int(a, 1, c)?);
            unsafe { ffi::set_timeout(ms, id) };
            Ok(JsValue::undefined())
        }),
        ("clearTimeout", 1, |_, a, c| {
            unsafe { ffi::clear_timeout(int(a, 0, c)?) };
            Ok(JsValue::undefined())
        }),
        ("navigate", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            unsafe { ffi::navigate(s.as_ptr(), s.len()) };
            Ok(JsValue::undefined())
        }),
        ("log", 1, |_, a, c| {
            let s = text(a, 0, c)?;
            unsafe { ffi::log(s.as_ptr(), s.len()) };
            Ok(JsValue::undefined())
        }),
        ("now", 0, |_, _, _| Ok(unsafe { ffi::now() }.into())),
        ("location", 0, |_, _, _| {
            Ok(string(
                &read_string(|p, n| unsafe { ffi::location(p, n) }).unwrap_or_default(),
            ))
        }),
        ("historyGo", 1, |_, a, c| {
            unsafe { ffi::history_go(int(a, 0, c)?) };
            Ok(JsValue::undefined())
        }),
        ("eventString", 1, |_, a, c| {
            let w = int(a, 0, c)?;
            Ok(string(
                &read_string(|p, n| unsafe { ffi::event_string(w, p, n) }).unwrap_or_default(),
            ))
        }),
        ("eventInt", 1, |_, a, c| {
            Ok(unsafe { ffi::event_int(int(a, 0, c)?) }.into())
        }),
        ("getChecked", 1, |_, a, c| {
            Ok((unsafe { ffi::get_checked(int(a, 0, c)?) } != 0).into())
        }),
        ("setChecked", 2, |_, a, c| {
            let on = a.get_or_undefined(1).to_boolean();
            unsafe { ffi::set_checked(int(a, 0, c)?, on as i32) };
            Ok(JsValue::undefined())
        }),
        ("getSelected", 1, |_, a, c| {
            Ok(unsafe { ffi::get_selected(int(a, 0, c)?) }.into())
        }),
        ("setSelected", 2, |_, a, c| {
            unsafe { ffi::set_selected(int(a, 0, c)?, int(a, 1, c)?) };
            Ok(JsValue::undefined())
        }),
        ("formAction", 2, |_, a, c| {
            unsafe { ffi::form_action(int(a, 0, c)?, int(a, 1, c)?) };
            Ok(JsValue::undefined())
        }),
        ("eventPos", 1, |_, a, c| {
            Ok(unsafe { ffi::event_pos(int(a, 0, c)?) }.into())
        }),
    ]
}

fn log(msg: &str) {
    unsafe { ffi::log(msg.as_ptr(), msg.len()) };
}

/// Reports an uncaught exception through the prelude (for stack traces),
/// or directly if that fails.
fn report(err: JsError, ctx: &mut Context) {
    let value = err.into_opaque(ctx).unwrap_or_default();
    let reporter = ctx.global_object().get(js_string!("__rb_report"), ctx);
    let reported = reporter
        .ok()
        .and_then(|r| r.as_callable())
        .is_some_and(|f| {
            f.call(&JsValue::undefined(), std::slice::from_ref(&value), ctx)
                .is_ok()
        });
    if !reported {
        log(&format!("Uncaught {}", value.display()));
    }
}

fn with_context(f: impl FnOnce(&mut Context)) {
    // Take the context out so nested calls cannot double-borrow it.
    let ctx = CONTEXT.with(|c| c.borrow_mut().take());
    if let Some(mut ctx) = ctx {
        f(&mut ctx);
        if let Err(e) = ctx.run_jobs() {
            report(e, &mut ctx);
        }
        CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
    }
}

/// Calls the global function `name` with `id` (event and timer dispatch).
fn call_global(name: &str, id: i32) {
    with_context(|ctx| {
        let f = ctx.global_object().get(JsString::from(name), ctx);
        if let Some(f) = f.ok().and_then(|f| f.as_callable())
            && let Err(e) = f.call(&JsValue::undefined(), &[id.into()], ctx)
        {
            report(e, ctx);
        }
    });
}

/// ES modules: sources the browser fetched (by URL), parsed on demand.
/// The browser sends every module of a graph before evaluating its root.
#[derive(Default)]
struct UrlModules {
    sources: RefCell<HashMap<String, String>>,
    modules: RefCell<HashMap<String, Module>>,
    /// Import map entries (`specifier` or `prefix/` → URL).
    import_map: RefCell<Vec<(String, String)>>,
}

thread_local! {
    static MODULES: Rc<UrlModules> = Rc::new(UrlModules::default());
}

impl UrlModules {
    /// The parsed module at `url`, if its source was provided.
    fn get(&self, url: &str, ctx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.modules.borrow().get(url) {
            return Ok(m.clone());
        }
        let src = self
            .sources
            .borrow()
            .get(url)
            .cloned()
            .ok_or_else(|| js_error!(TypeError: "module not loaded: {}", url))?;
        let module = Module::parse(
            Source::from_bytes(&src).with_path(Path::new(url)),
            None,
            ctx,
        )?;
        self.modules
            .borrow_mut()
            .insert(url.to_string(), module.clone());
        Ok(module)
    }
}

impl ModuleLoader for UrlModules {
    async fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> JsResult<Module> {
        let base = referrer
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let url = crate::abi::resolve_module(
            &request.specifier().to_std_string_escaped(),
            &base,
            &self.import_map.borrow(),
        );
        self.get(&url, &mut context.borrow_mut())
    }

    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &boa_engine::JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        // `import.meta.url` (bundlers derive their public path from it).
        if let Some(path) = module.path() {
            let url = JsString::from(path.to_string_lossy().as_ref());
            let _ = import_meta.set(js_string!("url"), url, false, context);
        }
    }
}

/// Evaluates the module at `url` (its graph was provided already).
fn run_module(url: &str, ctx: &mut Context) {
    let loader = MODULES.with(Rc::clone);
    let result = loader.get(url, ctx).and_then(|m| {
        let promise = m.load_link_evaluate(ctx);
        ctx.run_jobs()?;
        Ok(promise)
    });
    match result {
        Ok(p) => {
            if let PromiseState::Rejected(e) = p.state() {
                log(&format!("Uncaught {} ({url})", e.display()));
            }
        }
        Err(e) => {
            let value = e.into_opaque(ctx).unwrap_or_default();
            log(&format!("Uncaught {} ({url})", value.display()));
        }
    }
}

/// `rb_init`: creates the engine and the browser environment.
pub fn init() {
    let mut ctx = Context::builder()
        .clock(Rc::new(HostClock))
        .host_hooks(Rc::new(Hooks))
        .module_loader(MODULES.with(Rc::clone))
        .build()
        .expect("create JavaScript context");
    let mut rb = ObjectInitializer::new(&mut ctx);
    for (name, argc, f) in natives() {
        rb.function(NativeFunction::from_fn_ptr(f), JsString::from(name), argc);
    }
    let rb = rb.build();
    ctx.register_global_property(js_string!("__rb"), rb, Attribute::all())
        .expect("register __rb");
    if let Err(e) = ctx.eval(Source::from_bytes(PRELUDE)) {
        log(&format!("prelude failed: {e}"));
    }
    CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
}

/// `rb_alloc`: a buffer for the browser to write source code into.
pub fn alloc(len: usize) -> *mut u8 {
    BUFFERS.with(|b| {
        let mut b = b.borrow_mut();
        b.push(vec![0; len]);
        b.last_mut()
            .map_or(std::ptr::null_mut(), |v| v.as_mut_ptr())
    })
}

/// `rb_eval`: runs a script.
///
/// # Safety
///
/// `src..src+len` and `name..name+name_len` must be readable (buffers from
/// [`alloc`]).
pub unsafe fn eval(src: *const u8, len: usize, name: *const u8, name_len: usize) {
    let (source, name) = unsafe {
        (
            std::slice::from_raw_parts(src, len).to_vec(),
            String::from_utf8_lossy(std::slice::from_raw_parts(name, name_len)).into_owned(),
        )
    };
    BUFFERS.with(|b| b.borrow_mut().clear());
    // Module protocol (see doc/javascript.md): the name says what to do.
    if let Some(url) = name.strip_prefix("rb-define-module:") {
        let src = String::from_utf8_lossy(&source).into_owned();
        MODULES.with(|m| m.sources.borrow_mut().insert(url.to_string(), src));
        return;
    }
    if name == "rb-import-map" {
        let text = String::from_utf8_lossy(&source).into_owned();
        let map: Vec<(String, String)> = text
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        MODULES.with(|m| *m.import_map.borrow_mut() = map);
        return;
    }
    if let Some(url) = name.strip_prefix("rb-module:") {
        let url = url.to_string();
        with_context(|ctx| run_module(&url, ctx));
        return;
    }
    with_context(|ctx| {
        if let Err(e) = ctx.eval(Source::from_bytes(&source)) {
            let value = e.into_opaque(ctx).unwrap_or_default();
            log(&format!("Uncaught {} ({name})", value.display()));
        }
    });
}

/// `rb_event`: runs listener `id`.
pub fn event(id: i32) {
    call_global("__rb_dispatch", id);
}

/// `rb_timer`: runs timer `id`.
pub fn timer(id: i32) {
    call_global("__rb_timer", id);
}

/// Defines the runtime's exports; used by `examples/js_runtime.rs`.
#[macro_export]
macro_rules! js_runtime {
    () => {
        #[unsafe(no_mangle)]
        pub extern "C" fn rb_init() {
            $crate::lang::js::init()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_alloc(len: usize) -> *mut u8 {
            $crate::lang::js::alloc(len)
        }

        #[unsafe(no_mangle)]
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn rb_eval(src: *const u8, len: usize, name: *const u8, name_len: usize) {
            // The browser passes buffers it got from rb_alloc.
            unsafe { $crate::lang::js::eval(src, len, name, name_len) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_event(id: i32) {
            $crate::lang::js::event(id)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_timer(id: i32) {
            $crate::lang::js::timer(id)
        }
    };
}
