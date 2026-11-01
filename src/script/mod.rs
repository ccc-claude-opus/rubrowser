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

/// Instructions a script may run per call (init, event, timer).
pub const FUEL_PER_CALL: u64 = 30_000_000;
/// Maximum linear memory of a script.
pub const MAX_MEMORY: usize = 64 << 20;
/// Longest string a script may pass to the browser.
const MAX_STRING: usize = 1 << 20;

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
    /// Timers to start: (delay in ms, timer id).
    pub timers: Vec<(f64, i32)>,
}

struct State {
    doc: Document,
    listeners: Vec<Listener>,
    now: f64,
    out: Output,
    limits: StoreLimits,
}

/// A loaded script module.
pub struct Script {
    store: Store<State>,
    event_fn: Option<TypedFunc<(i32, i32, i32, i32), ()>>,
    timer_fn: Option<TypedFunc<i32, ()>>,
}

impl Script {
    /// Compiles and instantiates `wasm` against `doc`, then calls its
    /// `rb_init` export.
    pub fn load(wasm: &[u8], doc: &mut Document, now: f64) -> Result<Script, String> {
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
                .memory_size(MAX_MEMORY)
                .instances(1)
                .build(),
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| e.to_string())?;
        let linker = linker(&engine).map_err(|e| e.to_string())?;
        let mut script = Script {
            store,
            event_fn: None,
            timer_fn: None,
        };
        script.with_doc(doc, now, |s| {
            let instance = linker
                .instantiate_and_start(&mut s.store, &module)
                .map_err(describe)?;
            s.event_fn = instance.get_typed_func(&s.store, "rb_event").ok();
            s.timer_fn = instance.get_typed_func(&s.store, "rb_timer").ok();
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
        let r = match self.store.set_fuel(FUEL_PER_CALL) {
            Ok(()) => f(self),
            Err(e) => Err(e.to_string()),
        };
        std::mem::swap(&mut self.store.data_mut().doc, doc);
        r
    }

    pub fn listeners(&self) -> &[Listener] {
        &self.store.data().listeners
    }

    /// Calls listener `id` for an event on `target`.
    pub fn dispatch(
        &mut self,
        doc: &mut Document,
        id: i32,
        target: NodeId,
        key: i32,
        mods: i32,
        now: f64,
    ) -> Result<(), String> {
        let Some(f) = self.event_fn else {
            return Ok(());
        };
        self.with_doc(doc, now, |s| {
            f.call(&mut s.store, (id, target as i32, key, mods))
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
    let mut out: Vec<String> = simplecss::DeclarationTokenizer::from(style)
        .filter(|d| !d.name.eq_ignore_ascii_case(prop))
        .map(|d| format!("{}: {}", d.name, d.value))
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
          (func (export "rb_event") (param $id i32) (param $target i32) (param i32 i32)
            (if (i32.eq (local.get $id) (i32.const 7))
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
        s.dispatch(&mut doc, 7, b, 0, 0, 1.0).unwrap();
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
