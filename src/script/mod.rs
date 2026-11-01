//! WebAssembly scripts: rubrowser's replacement for JavaScript.
//!
//! A page includes `<script type="application/wasm" src="app.wasm">`. The
//! module runs in the wasmi interpreter and works on the page's DOM through
//! the `rb` imports defined here (see `doc/abi.md`). Nodes are passed as
//! integer handles (their [`NodeId`]). Every call into a script gets a fuel
//! budget and the script's memory is capped, so a misbehaving script can
//! neither hang nor exhaust the browser.

use wasmi::{
    Caller, Config, Engine, Error, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TrapCode, TypedFunc,
};

use crate::css;
use crate::document::Document;
use crate::dom::{Dom, Element, NodeId};

/// Instructions a compiled script may run per call (init, event, timer).
pub const FUEL_PER_CALL: u64 = 30_000_000;
/// Budget for language runtimes such as `js.wasm`, which interpret the
/// page's source and so need many more wasm instructions per call.
pub const RUNTIME_FUEL_PER_CALL: u64 = 10_000_000_000;
/// Maximum linear memory of a script.
pub const MAX_MEMORY: usize = 64 << 20;
/// Longest string a script may pass to the browser.
const MAX_STRING: usize = 1 << 20;

/// Resource limits for one wasm module.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Instructions per call.
    pub fuel: u64,
    /// Linear memory, in bytes.
    pub memory: usize,
}

impl Limits {
    /// Scripts compiled to wasm (Rust, …).
    pub const SCRIPT: Limits = Limits {
        fuel: FUEL_PER_CALL,
        memory: MAX_MEMORY,
    };
    /// Language runtimes (`js.wasm`), which interpret the page's source.
    pub const RUNTIME: Limits = Limits {
        fuel: RUNTIME_FUEL_PER_CALL,
        memory: 512 << 20,
    };
}

/// An event listener registered by a script.
#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub node: NodeId,
    pub event: String,
    pub id: i32,
}

/// Requests a script made during a call, for the browser to act on.
#[derive(Debug, Default)]
pub struct Output {
    pub navigate: Option<String>,
    pub logs: Vec<String>,
    pub prevent_default: bool,
    pub stop_propagation: bool,
    /// Timers to start: (delay in ms, timer id).
    pub timers: Vec<(f64, i32)>,
    /// Timers to cancel.
    pub cleared: Vec<i32>,
    /// History steps requested (`history.back()` is -1).
    pub history: i32,
    /// A form the script submitted with `form.submit()`.
    pub submit: Option<NodeId>,
}

/// The event being dispatched, readable through the `event_*` imports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventInfo {
    /// `click`, `keydown`, …
    pub kind: String,
    /// Node the event happened on.
    pub target: NodeId,
    /// Node whose listener is running (changes while bubbling).
    pub current: NodeId,
    /// `abi::key` code for keyboard events.
    pub key_code: i32,
    /// DOM-style key name: `"a"`, `"Enter"`, `"ArrowLeft"`, …
    pub key: String,
    /// `abi::modifier` bits.
    pub mods: i32,
    /// Pointer position in page coordinates.
    pub x: f64,
    pub y: f64,
    pub button: i32,
}

struct State {
    doc: Document,
    listeners: Vec<Listener>,
    now: f64,
    out: Output,
    limits: StoreLimits,
    event: Option<EventInfo>,
    /// Wall-clock milliseconds since the Unix epoch at `now == 0`.
    wall_offset: f64,
}

/// A loaded script module.
pub struct Script {
    store: Store<State>,
    event_fn: Option<TypedFunc<i32, ()>>,
    timer_fn: Option<TypedFunc<i32, ()>>,
    alloc_fn: Option<TypedFunc<i32, i32>>,
    eval_fn: Option<TypedFunc<(i32, i32, i32, i32), ()>>,
    memory: Option<Memory>,
    fuel: u64,
    /// Fuel left for a bounded phase (page load), shared by all calls.
    budget: Option<u64>,
}

impl Script {
    /// Compiles and instantiates `wasm` against `doc`, then calls its
    /// `rb_init` export.
    pub fn load(wasm: &[u8], doc: &mut Document, now: f64) -> Result<Script, String> {
        Self::load_with(wasm, doc, now, 0.0, Limits::SCRIPT)
    }

    /// Like [`Script::load`], with the wall clock (`Date.now()` epoch
    /// milliseconds at `now == 0`) and resource limits.
    pub fn load_with(
        wasm: &[u8],
        doc: &mut Document,
        now: f64,
        wall_offset: f64,
        limits: Limits,
    ) -> Result<Script, String> {
        let fuel = limits.fuel;
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| format!("invalid script: {e}"))?;
        let state = State {
            doc: placeholder(),
            listeners: Vec::new(),
            now,
            out: Output::default(),
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory)
                .instances(1)
                .build(),
            event: None,
            wall_offset,
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(fuel).map_err(|e| e.to_string())?;
        let linker = linker(&engine).map_err(|e| e.to_string())?;
        let mut script = Script {
            store,
            event_fn: None,
            timer_fn: None,
            alloc_fn: None,
            eval_fn: None,
            memory: None,
            fuel,
            budget: None,
        };
        script.with_doc(doc, now, |s| {
            let instance = linker
                .instantiate_and_start(&mut s.store, &module)
                .map_err(describe)?;
            s.event_fn = instance.get_typed_func(&s.store, "rb_event").ok();
            s.timer_fn = instance.get_typed_func(&s.store, "rb_timer").ok();
            s.alloc_fn = instance.get_typed_func(&s.store, "rb_alloc").ok();
            s.eval_fn = instance.get_typed_func(&s.store, "rb_eval").ok();
            s.memory = instance.get_memory(&s.store, "memory");
            if let Ok(init) = instance.get_typed_func::<(), ()>(&s.store, "rb_init") {
                init.call(&mut s.store, ()).map_err(describe)?;
            }
            Ok(())
        })?;
        Ok(script)
    }

    /// Runs `f` with `doc` moved into the store, so host functions can
    /// reach it, and moves it back afterwards.
    fn with_doc<R>(
        &mut self,
        doc: &mut Document,
        now: f64,
        f: impl FnOnce(&mut Self) -> Result<R, String>,
    ) -> Result<R, String> {
        let s = self.store.data_mut();
        s.now = now;
        std::mem::swap(&mut s.doc, doc);
        let fuel = self.budget.map_or(self.fuel, |b| b.min(self.fuel));
        let r = match self.store.set_fuel(fuel) {
            Ok(()) => f(self),
            Err(e) => Err(e.to_string()),
        };
        if let Some(b) = &mut self.budget {
            let left = self.store.get_fuel().unwrap_or(0);
            *b = b.saturating_sub(fuel - left.min(fuel));
        }
        std::mem::swap(&mut self.store.data_mut().doc, doc);
        r
    }

    /// Limits the total fuel of all following calls to `budget` (`None`:
    /// only the per-call limit applies).
    pub fn set_budget(&mut self, budget: Option<u64>) {
        self.budget = budget;
    }

    /// Fuel left in the budget, if one is set.
    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

    /// Sets the wall clock (`Date.now()`): epoch milliseconds at `now == 0`.
    pub fn set_wall_offset(&mut self, offset: f64) {
        self.store.data_mut().wall_offset = offset;
    }

    pub fn listeners(&self) -> &[Listener] {
        &self.store.data().listeners
    }

    /// Calls listener `id` for `event`.
    pub fn dispatch(
        &mut self,
        doc: &mut Document,
        id: i32,
        event: &EventInfo,
        now: f64,
    ) -> Result<(), String> {
        let Some(f) = self.event_fn else {
            return Ok(());
        };
        self.store.data_mut().event = Some(event.clone());
        let r = self.with_doc(doc, now, |s| f.call(&mut s.store, id).map_err(describe));
        self.store.data_mut().event = None;
        r
    }

    /// Whether this module is a language runtime (it can [`Script::eval`]).
    pub fn is_runtime(&self) -> bool {
        self.alloc_fn.is_some() && self.eval_fn.is_some()
    }

    /// Runs source code in a language runtime module (see `doc/plan2.md`,
    /// runtime protocol): copies it into the module and calls `rb_eval`.
    pub fn eval(
        &mut self,
        doc: &mut Document,
        source: &str,
        name: &str,
        now: f64,
    ) -> Result<(), String> {
        let (Some(alloc), Some(eval)) = (self.alloc_fn, self.eval_fn) else {
            return Err("not a language runtime".into());
        };
        self.with_doc(doc, now, |s| {
            let put = |s: &mut Script, text: &str| -> Result<i32, String> {
                let ptr = alloc
                    .call(&mut s.store, text.len() as i32)
                    .map_err(describe)?;
                let memory = s
                    .memory
                    .ok_or_else(|| "runtime does not export memory".to_string())?;
                let data = memory.data_mut(&mut s.store);
                let r = range(data, ptr, text.len()).map_err(|e| e.to_string())?;
                data[r].copy_from_slice(text.as_bytes());
                Ok(ptr)
            };
            let src = put(s, source)?;
            let file = put(s, name)?;
            eval.call(
                &mut s.store,
                (src, source.len() as i32, file, name.len() as i32),
            )
            .map_err(describe)
        })
    }

    /// Fires timer `id`.
    pub fn timer(&mut self, doc: &mut Document, id: i32, now: f64) -> Result<(), String> {
        let Some(f) = self.timer_fn else {
            return Ok(());
        };
        self.with_doc(doc, now, |s| f.call(&mut s.store, id).map_err(describe))
    }

    /// Takes the requests the script made since the last call.
    pub fn take_output(&mut self) -> Output {
        std::mem::take(&mut self.store.data_mut().out)
    }
}

fn placeholder() -> Document {
    Document::new(Dom::new(), "")
}

/// Turns a wasmi error into a message for the console.
fn describe(e: Error) -> String {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => {
            "the script ran too long without returning (possible infinite loop)".into()
        }
        Some(TrapCode::UnreachableCodeReached) => "the script panicked".into(),
        Some(code) => format!("the script crashed: {code}"),
        None => e.to_string(),
    }
}

fn memory(caller: &Caller<'_, State>) -> Result<Memory, Error> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| Error::new("script does not export `memory`"))
}

/// Checks that `ptr..ptr+len` lies inside the script's memory.
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

/// Copies as much of `s` as fits in `cap` bytes at `ptr`; returns the full
/// length so the script can retry with a bigger buffer.
fn write_str(caller: &mut Caller<'_, State>, ptr: i32, cap: i32, s: &str) -> Result<i32, Error> {
    let n = s.len().min(cap.max(0) as usize);
    let mem = memory(caller)?;
    let data = mem.data_mut(caller);
    let r = range(data, ptr, n)?;
    data[r].copy_from_slice(&s.as_bytes()[..n]);
    Ok(s.len() as i32)
}

/// A handle that refers to an existing node.
fn node(doc: &Document, h: i32) -> Option<NodeId> {
    let id = usize::try_from(h).ok()?;
    doc.dom.get(id).map(|_| id)
}

fn element(doc: &Document, h: i32) -> Option<NodeId> {
    node(doc, h).filter(|&n| doc.dom.element(n).is_some())
}

/// Elements matching `selector`, in document order.
fn select(doc: &Document, selector: &str) -> Vec<NodeId> {
    let state = doc.state();
    doc.dom
        .descendants(Dom::ROOT)
        .into_iter()
        .filter(|&n| css::matches(&doc.dom, n, selector, &state))
        .collect()
}

/// Sets (or with an empty value removes) one property in a `style` attribute.
fn set_style_property(style: &str, prop: &str, value: &str) -> String {
    let mut out: Vec<String> = css::syntax::parse_declarations(style, None)
        .into_iter()
        .filter(|d| !d.name.eq_ignore_ascii_case(prop))
        .map(|d| {
            let imp = if d.important { " !important" } else { "" };
            format!("{}: {}{imp}", d.name, d.value)
        })
        .collect();
    if !value.trim().is_empty() {
        out.push(format!("{prop}: {value}"));
    }
    out.join("; ")
}

fn linker(engine: &Engine) -> Result<Linker<State>, wasmi::errors::LinkerError> {
    type C<'a> = Caller<'a, State>;
    let mut l = Linker::<State>::new(engine);

    l.func_wrap(
        "rb",
        "query",
        |c: C, p: i32, n: i32| -> Result<i32, Error> {
            let sel = read_str(&c, p, n)?;
            Ok(select(&c.data().doc, &sel)
                .first()
                .map_or(-1, |&n| n as i32))
        },
    )?;
    l.func_wrap(
        "rb",
        "query_all",
        |mut c: C, p: i32, n: i32, out: i32, max: i32| -> Result<i32, Error> {
            let sel = read_str(&c, p, n)?;
            let found = select(&c.data().doc, &sel);
            let count = found.len().min(max.max(0) as usize);
            let bytes: Vec<u8> = found[..count]
                .iter()
                .flat_map(|&n| (n as i32).to_le_bytes())
                .collect();
            let mem = memory(&c)?;
            let data = mem.data_mut(&mut c);
            let r = range(data, out, bytes.len())?;
            data[r].copy_from_slice(&bytes);
            Ok(found.len() as i32)
        },
    )?;
    l.func_wrap(
        "rb",
        "query_within",
        |mut c: C, root: i32, p: i32, n: i32, out: i32, max: i32| -> Result<i32, Error> {
            let sel = read_str(&c, p, n)?;
            let doc = &c.data().doc;
            let state = doc.state();
            let found: Vec<NodeId> = match node(doc, root) {
                Some(r) => doc
                    .dom
                    .descendants(r)
                    .into_iter()
                    .filter(|&d| d != r && css::matches(&doc.dom, d, &sel, &state))
                    .collect(),
                None => Vec::new(),
            };
            let count = found.len().min(max.max(0) as usize);
            let bytes: Vec<u8> = found[..count]
                .iter()
                .flat_map(|&n| (n as i32).to_le_bytes())
                .collect();
            let mem = memory(&c)?;
            let data = mem.data_mut(&mut c);
            let r = range(data, out, bytes.len())?;
            data[r].copy_from_slice(&bytes);
            Ok(found.len() as i32)
        },
    )?;
    l.func_wrap("rb", "rect", |c: C, h: i32, which: i32| -> f64 {
        // Layout geometry from the last layout (logical px, viewport
        // coordinates): 0 x, 1 y, 2 width, 3 height of the border box;
        // 4 width, 5 height of the padding box. Node 0: the viewport.
        let doc = &c.data().doc;
        let Some(n) = node(doc, h) else { return 0.0 };
        if n == Dom::ROOT {
            let (w, h) = doc.viewport_size();
            return match which {
                2 | 4 => w as f64,
                3 | 5 => h as f64,
                _ => 0.0,
            };
        }
        let Some((b, content)) = doc.box_rects(n) else {
            return 0.0;
        };
        let pad = doc.style(n).map_or([0.0; 4], |s| {
            [0, 1, 2, 3].map(|i| s.padding[i].px_or_zero(b.w))
        });
        (match which {
            0 => b.x,
            1 => b.y,
            2 => b.w,
            3 => b.h,
            4 => content.w + pad[1] + pad[3],
            5 => content.h + pad[0] + pad[2],
            _ => 0.0,
        }) as f64
    })?;
    l.func_wrap("rb", "parent", |c: C, h: i32| -> i32 {
        let doc = &c.data().doc;
        node(doc, h)
            .and_then(|n| doc.dom.parent(n))
            .filter(|&p| doc.dom.element(p).is_some())
            .map_or(-1, |p| p as i32)
    })?;
    l.func_wrap(
        "rb",
        "get_text",
        |mut c: C, h: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let text = node(&c.data().doc, h).map(|n| c.data().doc.dom.text_content(n));
            match text {
                Some(t) => write_str(&mut c, p, cap, &t),
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap(
        "rb",
        "set_text",
        |mut c: C, h: i32, p: i32, n: i32| -> Result<(), Error> {
            let text = read_str(&c, p, n)?;
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h) {
                doc.dom_mut().set_text_content(e, &text);
            }
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "get_attr",
        |mut c: C, h: i32, np: i32, nl: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let name = read_str(&c, np, nl)?;
            let doc = &c.data().doc;
            let value = element(doc, h).and_then(|e| doc.dom.attr(e, &name).map(str::to_string));
            match value {
                Some(v) => write_str(&mut c, p, cap, &v),
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap(
        "rb",
        "set_attr",
        |mut c: C, h: i32, np: i32, nl: i32, vp: i32, vl: i32| -> Result<(), Error> {
            let name = read_str(&c, np, nl)?.to_ascii_lowercase();
            let value = read_str(&c, vp, vl)?;
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h)
                && let Some(el) = doc.dom_mut().element_mut(e)
            {
                el.set_attr(&name, &value);
            }
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "remove_attr",
        |mut c: C, h: i32, np: i32, nl: i32| -> Result<(), Error> {
            let name = read_str(&c, np, nl)?.to_ascii_lowercase();
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h)
                && let Some(el) = doc.dom_mut().element_mut(e)
            {
                el.remove_attr(&name);
            }
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "class_op",
        |mut c: C, h: i32, op: i32, p: i32, n: i32| -> Result<i32, Error> {
            // op: 0 has, 1 add, 2 remove, 3 toggle. Returns whether the
            // element has the class afterwards.
            let class = read_str(&c, p, n)?;
            let doc = &mut c.data_mut().doc;
            let Some(e) = element(doc, h) else {
                return Ok(0);
            };
            let had = doc.dom.element(e).is_some_and(|el| el.has_class(&class));
            let want = match op {
                1 => true,
                2 => false,
                3 => !had,
                _ => return Ok(had as i32),
            };
            if want != had
                && let Some(el) = doc.dom_mut().element_mut(e)
            {
                let mut classes: Vec<String> = el.classes().map(str::to_string).collect();
                if want {
                    classes.push(class);
                } else {
                    classes.retain(|x| *x != class);
                }
                el.set_attr("class", &classes.join(" "));
            }
            Ok(want as i32)
        },
    )?;
    l.func_wrap(
        "rb",
        "set_style",
        |mut c: C, h: i32, pp: i32, pl: i32, vp: i32, vl: i32| -> Result<(), Error> {
            let prop = read_str(&c, pp, pl)?.to_ascii_lowercase();
            let value = read_str(&c, vp, vl)?;
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h) {
                let old = doc.dom.attr(e, "style").unwrap_or("").to_string();
                let new = set_style_property(&old, &prop, &value);
                if let Some(el) = doc.dom_mut().element_mut(e) {
                    el.set_attr("style", &new);
                }
            }
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "get_value",
        |mut c: C, h: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let doc = &c.data().doc;
            match element(doc, h) {
                Some(e) => {
                    let v = doc.input_value(e);
                    write_str(&mut c, p, cap, &v)
                }
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap(
        "rb",
        "set_value",
        |mut c: C, h: i32, p: i32, n: i32| -> Result<(), Error> {
            let value = read_str(&c, p, n)?;
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h) {
                doc.set_input_value(e, &value);
                doc.dom_changed();
            }
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "create_element",
        |mut c: C, p: i32, n: i32| -> Result<i32, Error> {
            let tag = read_str(&c, p, n)?;
            if tag.is_empty()
                || !tag
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            {
                return Ok(-1);
            }
            Ok(c.data_mut()
                .doc
                .dom_mut()
                .create_element(Element::new(&tag)) as i32)
        },
    )?;
    l.func_wrap(
        "rb",
        "create_text",
        |mut c: C, p: i32, n: i32| -> Result<i32, Error> {
            let text = read_str(&c, p, n)?;
            Ok(c.data_mut().doc.dom_mut().create_text(&text) as i32)
        },
    )?;
    l.func_wrap(
        "rb",
        "insert",
        |mut c: C, parent: i32, child: i32, before: i32| -> i32 {
            let doc = &mut c.data_mut().doc;
            let (Some(p), Some(ch)) = (element(doc, parent), node(doc, child)) else {
                return 0;
            };
            let before = node(doc, before).filter(|&b| doc.dom.parent(b) == Some(p));
            if ch == Dom::ROOT {
                return 0;
            }
            doc.dom_mut().insert_before(p, ch, before) as i32
        },
    )?;
    l.func_wrap("rb", "remove", |mut c: C, h: i32| {
        let doc = &mut c.data_mut().doc;
        if let Some(n) = node(doc, h).filter(|&n| n != Dom::ROOT) {
            doc.dom_mut().detach(n);
        }
    })?;
    l.func_wrap(
        "rb",
        "listen",
        |mut c: C, h: i32, p: i32, n: i32, id: i32| -> Result<(), Error> {
            let event = read_str(&c, p, n)?.to_ascii_lowercase();
            let s = c.data_mut();
            if let Some(n) = node(&s.doc, h)
                && s.listeners.len() < 10_000
            {
                s.listeners.push(Listener { node: n, event, id });
            }
            Ok(())
        },
    )?;
    l.func_wrap("rb", "prevent_default", |mut c: C| {
        c.data_mut().out.prevent_default = true;
    })?;
    l.func_wrap("rb", "set_timeout", |mut c: C, ms: f64, id: i32| {
        let timers = &mut c.data_mut().out.timers;
        if timers.len() < 1000 {
            timers.push((if ms.is_finite() { ms.max(0.0) } else { 0.0 }, id));
        }
    })?;
    l.func_wrap(
        "rb",
        "navigate",
        |mut c: C, p: i32, n: i32| -> Result<(), Error> {
            let url = read_str(&c, p, n)?;
            c.data_mut().out.navigate = Some(url);
            Ok(())
        },
    )?;
    l.func_wrap(
        "rb",
        "log",
        |mut c: C, p: i32, n: i32| -> Result<(), Error> {
            let msg = read_str(&c, p, n)?;
            let logs = &mut c.data_mut().out.logs;
            if logs.len() < 100 {
                logs.push(msg);
            }
            Ok(())
        },
    )?;
    l.func_wrap("rb", "now", |c: C| -> f64 { c.data().now })?;
    l.func_wrap("rb", "wall_time", |c: C| -> f64 {
        c.data().now + c.data().wall_offset
    })?;

    // ----- ABI v1: tree navigation ---------------------------------------
    l.func_wrap("rb", "node_type", |c: C, h: i32| -> i32 {
        use crate::dom::NodeData;
        let doc = &c.data().doc;
        match node(doc, h).map(|n| &doc.dom.node(n).data) {
            Some(NodeData::Element(_)) => 1,
            Some(NodeData::Text(_)) => 3,
            Some(NodeData::Comment(_)) => 8,
            Some(NodeData::Document) => 9,
            None => -1,
        }
    })?;
    l.func_wrap(
        "rb",
        "tag_name",
        |mut c: C, h: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let doc = &c.data().doc;
            match element(doc, h)
                .and_then(|e| doc.dom.tag(e))
                .map(str::to_string)
            {
                Some(t) => write_str(&mut c, p, cap, &t),
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap("rb", "relative", |c: C, h: i32, which: i32| -> i32 {
        // which: 0 parent node, 1 first child, 2 last child, 3 next
        // sibling, 4 previous sibling.
        let dom = &c.data().doc.dom;
        let Some(n) = node(&c.data().doc, h) else {
            return -1;
        };
        let sibling = |delta: isize| {
            let p = dom.parent(n)?;
            let kids = dom.children(p);
            let i = kids.iter().position(|&k| k == n)? as isize + delta;
            kids.get(usize::try_from(i).ok()?).copied()
        };
        let r = match which {
            0 => dom.parent(n),
            1 => dom.children(n).first().copied(),
            2 => dom.children(n).last().copied(),
            3 => sibling(1),
            4 => sibling(-1),
            _ => None,
        };
        r.map_or(-1, |r| r as i32)
    })?;
    l.func_wrap("rb", "child_count", |c: C, h: i32| -> i32 {
        let doc = &c.data().doc;
        node(doc, h).map_or(-1, |n| doc.dom.children(n).len() as i32)
    })?;
    l.func_wrap("rb", "child_at", |c: C, h: i32, i: i32| -> i32 {
        let doc = &c.data().doc;
        node(doc, h)
            .and_then(|n| doc.dom.children(n).get(usize::try_from(i).ok()?).copied())
            .map_or(-1, |n| n as i32)
    })?;
    l.func_wrap("rb", "special", |c: C, which: i32| -> i32 {
        // which: 0 document, 1 <html>, 2 <head>, 3 <body>.
        let dom = &c.data().doc.dom;
        let r = match which {
            0 => Some(Dom::ROOT),
            1 => dom.html(),
            2 => dom.head(),
            3 => dom.body(),
            _ => None,
        };
        r.map_or(-1, |n| n as i32)
    })?;
    l.func_wrap(
        "rb",
        "matches",
        |c: C, h: i32, p: i32, n: i32| -> Result<i32, Error> {
            let sel = read_str(&c, p, n)?;
            let doc = &c.data().doc;
            let state = doc.state();
            Ok(element(doc, h).is_some_and(|e| css::matches(&doc.dom, e, &sel, &state)) as i32)
        },
    )?;

    // ----- ABI v1: markup and attributes ---------------------------------
    l.func_wrap(
        "rb",
        "get_html",
        |mut c: C, h: i32, outer: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let doc = &c.data().doc;
            match node(doc, h).map(|n| doc.dom.to_html(n, outer != 0)) {
                Some(html) => write_str(&mut c, p, cap, &html),
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap(
        "rb",
        "set_html",
        |mut c: C, h: i32, p: i32, n: i32| -> Result<(), Error> {
            let html = read_str(&c, p, n)?;
            let doc = &mut c.data_mut().doc;
            if let Some(e) = element(doc, h) {
                doc.dom_mut().set_inner_html(e, &html);
            }
            Ok(())
        },
    )?;
    l.func_wrap("rb", "attr_count", |c: C, h: i32| -> i32 {
        let doc = &c.data().doc;
        element(doc, h)
            .and_then(|e| doc.dom.element(e))
            .map_or(-1, |e| e.attrs.len() as i32)
    })?;
    l.func_wrap(
        "rb",
        "attr_name_at",
        |mut c: C, h: i32, i: i32, p: i32, cap: i32| -> Result<i32, Error> {
            let doc = &c.data().doc;
            let name = element(doc, h)
                .and_then(|e| doc.dom.element(e))
                .and_then(|e| e.attrs.get(usize::try_from(i).ok()?))
                .map(|(n, _)| n.clone());
            match name {
                Some(n) => write_str(&mut c, p, cap, &n),
                None => Ok(-1),
            }
        },
    )?;

    // ----- ABI v1: events, timers, location -------------------------------
    l.func_wrap(
        "rb",
        "unlisten",
        |mut c: C, h: i32, p: i32, n: i32, id: i32| -> Result<(), Error> {
            let event = read_str(&c, p, n)?.to_ascii_lowercase();
            let s = c.data_mut();
            if let Some(n) = node(&s.doc, h) {
                s.listeners
                    .retain(|l| !(l.node == n && l.event == event && l.id == id));
            }
            Ok(())
        },
    )?;
    l.func_wrap("rb", "stop_propagation", |mut c: C| {
        c.data_mut().out.stop_propagation = true;
    })?;
    l.func_wrap("rb", "clear_timeout", |mut c: C, id: i32| {
        let out = &mut c.data_mut().out;
        out.timers.retain(|&(_, t)| t != id);
        out.cleared.push(id);
    })?;
    l.func_wrap(
        "rb",
        "event_string",
        |mut c: C, which: i32, p: i32, cap: i32| -> Result<i32, Error> {
            // which: 0 type, 1 key.
            let v = c.data().event.as_ref().map(|e| match which {
                0 => e.kind.clone(),
                _ => e.key.clone(),
            });
            match v {
                Some(v) => write_str(&mut c, p, cap, &v),
                None => Ok(-1),
            }
        },
    )?;
    l.func_wrap("rb", "event_int", |c: C, which: i32| -> i32 {
        // which: 0 target, 1 current target, 2 key code, 3 modifiers, 4 button.
        c.data().event.as_ref().map_or(-1, |e| match which {
            0 => e.target as i32,
            1 => e.current as i32,
            2 => e.key_code,
            3 => e.mods,
            4 => e.button,
            _ => -1,
        })
    })?;
    l.func_wrap("rb", "event_pos", |c: C, which: i32| -> f64 {
        // which: 0 x, 1 y (page coordinates).
        c.data()
            .event
            .as_ref()
            .map_or(0.0, |e| if which == 0 { e.x } else { e.y })
    })?;
    l.func_wrap(
        "rb",
        "location",
        |mut c: C, p: i32, cap: i32| -> Result<i32, Error> {
            let url = c.data().doc.url.clone();
            write_str(&mut c, p, cap, &url)
        },
    )?;
    l.func_wrap("rb", "history_go", |mut c: C, delta: i32| {
        c.data_mut().out.history = delta.clamp(-100, 100);
    })?;

    // ----- ABI v1: form controls -------------------------------------------
    l.func_wrap("rb", "get_checked", |c: C, h: i32| -> i32 {
        let doc = &c.data().doc;
        element(doc, h).is_some_and(|e| doc.forms.checked(&doc.dom, e)) as i32
    })?;
    l.func_wrap("rb", "set_checked", |mut c: C, h: i32, v: i32| {
        let doc = &mut c.data_mut().doc;
        if let Some(e) = element(doc, h) {
            doc.forms.set_checked(&doc.dom, e, v != 0);
            doc.dom_changed();
        }
    })?;
    l.func_wrap("rb", "get_selected", |c: C, h: i32| -> i32 {
        let doc = &c.data().doc;
        element(doc, h)
            .and_then(|e| doc.forms.selected_index(&doc.dom, e))
            .map_or(-1, |i| i as i32)
    })?;
    l.func_wrap("rb", "set_selected", |mut c: C, h: i32, i: i32| {
        let doc = &mut c.data_mut().doc;
        if let (Some(e), Ok(i)) = (element(doc, h), usize::try_from(i)) {
            if i < crate::document::options(&doc.dom, e).len() {
                doc.forms.set_selected(e, i);
                doc.dom_changed();
            }
        }
    })?;
    l.func_wrap("rb", "form_action", |mut c: C, h: i32, op: i32| {
        // op: 0 submit, 1 reset.
        let s = c.data_mut();
        let Some(form) = element(&s.doc, h).filter(|&e| s.doc.dom.tag(e) == Some("form")) else {
            return;
        };
        if op == 0 {
            s.out.submit = Some(form);
        } else {
            s.doc.reset_form(form);
        }
    })?;
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wasmi's `wat` feature (dev-dependency) accepts the text format.
    fn wasm(wat: &str) -> Vec<u8> {
        wat.as_bytes().to_vec()
    }

    fn page() -> Document {
        Document::from_html(
            "<p id=out class=a>old</p><button id=b>go</button><input id=i value=hi>",
            "test.html",
        )
    }

    const SCRIPT: &str = r##"
        (module
          (import "rb" "query" (func $query (param i32 i32) (result i32)))
          (import "rb" "set_text" (func $set_text (param i32 i32 i32)))
          (import "rb" "class_op" (func $class_op (param i32 i32 i32 i32) (result i32)))
          (import "rb" "listen" (func $listen (param i32 i32 i32 i32)))
          (import "rb" "set_timeout" (func $set_timeout (param f64 i32)))
          (import "rb" "get_value" (func $get_value (param i32 i32 i32) (result i32)))
          (import "rb" "navigate" (func $navigate (param i32 i32)))
          (import "rb" "event_int" (func $event_int (param i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "#out")
          (data (i32.const 16) "#b")
          (data (i32.const 32) "click")
          (data (i32.const 48) "clicked")
          (data (i32.const 64) "big")
          (data (i32.const 80) "#i")
          (data (i32.const 96) "next.html")
          (func (export "rb_init")
            (call $listen (call $query (i32.const 16) (i32.const 2))
                          (i32.const 32) (i32.const 5) (i32.const 7))
            (call $set_timeout (f64.const 100) (i32.const 9)))
          (func (export "rb_event") (param $id i32)
            (if (i32.and (i32.eq (local.get $id) (i32.const 7))
                         (i32.ge_s (call $event_int (i32.const 0)) (i32.const 0)))
              (then
                (call $set_text (call $query (i32.const 0) (i32.const 4))
                                (i32.const 48) (i32.const 7))
                (drop (call $class_op (call $query (i32.const 0) (i32.const 4))
                                      (i32.const 1) (i32.const 64) (i32.const 3))))))
          (func (export "rb_timer") (param $id i32)
            ;; Copy the input's value into memory at 200 and navigate if it is "hi".
            (if (i32.eq (call $get_value (call $query (i32.const 80) (i32.const 2))
                                         (i32.const 200) (i32.const 16))
                        (i32.const 2))
              (then (call $navigate (i32.const 96) (i32.const 9))))))
    "##;

    #[test]
    fn listeners_events_dom_and_timers() {
        let mut doc = page();
        let mut s = Script::load(&wasm(SCRIPT), &mut doc, 0.0).unwrap();
        let b = doc.dom.element_by_id("b").unwrap();
        assert_eq!(
            s.listeners(),
            &[Listener {
                node: b,
                event: "click".into(),
                id: 7
            }]
        );
        assert_eq!(s.take_output().timers, vec![(100.0, 9)]);
        let ev = EventInfo {
            kind: "click".into(),
            target: b,
            current: b,
            ..Default::default()
        };
        s.dispatch(&mut doc, 7, &ev, 1.0).unwrap();
        let out = doc.dom.element_by_id("out").unwrap();
        assert_eq!(doc.dom.text_content(out), "clicked");
        assert_eq!(doc.dom.attr(out, "class"), Some("a big"));
        assert!(doc.needs_update(), "DOM edits trigger a restyle");
        s.timer(&mut doc, 9, 100.0).unwrap();
        assert_eq!(s.take_output().navigate.as_deref(), Some("next.html"));
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let wat = r#"(module (func (export "rb_timer") (param i32) (loop $l (br $l))))"#;
        let mut doc = page();
        let mut s = Script::load(&wasm(wat), &mut doc, 0.0).unwrap();
        let err = s.timer(&mut doc, 0, 0.0).unwrap_err();
        assert!(err.contains("too long"), "{err}");
        assert!(doc.dom.element_by_id("out").is_some(), "document survives");
    }

    #[test]
    fn bad_pointers_and_handles_are_safe() {
        let wat = r#"(module
            (import "rb" "set_text" (func $set_text (param i32 i32 i32)))
            (import "rb" "remove" (func $remove (param i32)))
            (memory (export "memory") 1)
            (func (export "rb_init")
              (call $remove (i32.const 0))
              (call $remove (i32.const 99999))
              (call $set_text (i32.const 99999) (i32.const 0) (i32.const 1))
              (call $set_text (i32.const 1) (i32.const 65530) (i32.const 100))))"#;
        let mut doc = page();
        let err = Script::load(&wasm(wat), &mut doc, 0.0).err().unwrap();
        assert!(err.contains("out of bounds"), "{err}");
        assert!(doc.dom.html().is_some(), "the root cannot be removed");
    }

    #[test]
    fn abi_doc_example_works() {
        let doc_md = include_str!("../../doc/abi.md");
        let start = doc_md.find("```wat").unwrap() + 6;
        let end = start + doc_md[start..].find("```").unwrap();
        let mut doc = Document::from_html("<button id=btn>Go</button>", "t.html");
        let mut s = Script::load(&wasm(&doc_md[start..end]), &mut doc, 0.0).unwrap();
        let btn = doc.dom.element_by_id("btn").unwrap();
        let l = s.listeners()[0].clone();
        let ev = EventInfo {
            kind: "click".into(),
            target: btn,
            current: btn,
            ..Default::default()
        };
        s.dispatch(&mut doc, l.id, &ev, 0.0).unwrap();
        assert_eq!(doc.dom.text_content(btn), "Clicked!");
    }

    #[test]
    fn rejects_invalid_modules() {
        let mut doc = page();
        assert!(Script::load(b"nope", &mut doc, 0.0).is_err());
    }

    #[test]
    fn style_attribute_editing() {
        assert_eq!(
            set_style_property("color: red; width: 1px", "color", "blue"),
            "width: 1px; color: blue"
        );
        assert_eq!(set_style_property("color: red", "color", ""), "");
    }
}
