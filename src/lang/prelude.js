// rubrowser browser environment for JavaScript.
//
// The JS engine (boa) runs inside wasm and talks to the page through the
// native `__rb` object, a thin mirror of the rb DOM ABI (doc/abi.md). This
// prelude builds the familiar web APIs on top of it: document, elements,
// events, timers, console, location.
(function (rb) {
  'use strict';
  const global = globalThis;

  // ----- nodes -------------------------------------------------------------

  const wrappers = new Map();

  function wrap(h) {
    if (h === null || h === undefined || h < 0) return null;
    let n = wrappers.get(h);
    if (!n) {
      const t = rb.nodeType(h);
      n = t === 1 ? new Element(h) : t === 3 ? new Text(h) : t === 9 ? new Document(h) : new Node(h);
      wrappers.set(h, n);
    }
    return n;
  }

  function handle(n) {
    if (n === null || n === undefined) return -1;
    if (typeof n === 'string') return rb.createText(n);
    if (!(n instanceof Node)) throw new TypeError('not a Node');
    return n.__h;
  }

  function list(handles) {
    return handles.map(wrap);
  }

  const listeners = new Map(); // id -> { fn, node, type, once }
  let nextListener = 1;

  class EventTarget {
    addEventListener(type, fn, options) {
      if (typeof fn !== 'function' && !(fn && typeof fn.handleEvent === 'function')) return;
      type = String(type).toLowerCase();
      for (const l of listeners.values()) {
        if (l.node === this && l.type === type && l.fn === fn) return;
      }
      const id = nextListener++;
      const once = !!(options && typeof options === 'object' && options.once);
      listeners.set(id, { fn, node: this, type, once });
      rb.listen(this.__h, type, id);
    }

    removeEventListener(type, fn) {
      type = String(type).toLowerCase();
      for (const [id, l] of listeners) {
        if (l.node === this && l.type === type && l.fn === fn) {
          listeners.delete(id);
          rb.unlisten(this.__h, type, id);
        }
      }
    }
  }

  class Node extends EventTarget {
    constructor(h) {
      super();
      Object.defineProperty(this, '__h', { value: h });
    }

    get nodeType() { return rb.nodeType(this.__h); }
    get nodeName() { return this.nodeType === 3 ? '#text' : this.nodeType === 9 ? '#document' : '#node'; }
    get parentNode() { return wrap(rb.relative(this.__h, 0)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get firstChild() { return wrap(rb.relative(this.__h, 1)); }
    get lastChild() { return wrap(rb.relative(this.__h, 2)); }
    get nextSibling() { return wrap(rb.relative(this.__h, 3)); }
    get previousSibling() { return wrap(rb.relative(this.__h, 4)); }
    get childNodes() {
      const out = [];
      const n = rb.childCount(this.__h);
      for (let i = 0; i < n; i++) out.push(wrap(rb.childAt(this.__h, i)));
      return out;
    }
    get children() { return this.childNodes.filter((c) => c.nodeType === 1); }
    get childElementCount() { return this.children.length; }
    get firstElementChild() { return this.children[0] || null; }
    get lastElementChild() { const c = this.children; return c[c.length - 1] || null; }
    get nextElementSibling() {
      let n = this.nextSibling;
      while (n && n.nodeType !== 1) n = n.nextSibling;
      return n;
    }
    get previousElementSibling() {
      let n = this.previousSibling;
      while (n && n.nodeType !== 1) n = n.previousSibling;
      return n;
    }
    get isConnected() {
      let n = this;
      while (n.parentNode) n = n.parentNode;
      return n.nodeType === 9;
    }
    get textContent() { return rb.getText(this.__h); }
    set textContent(v) { rb.setText(this.__h, v === null || v === undefined ? '' : String(v)); }
    get nodeValue() { return this.nodeType === 3 ? this.textContent : null; }

    hasChildNodes() { return rb.childCount(this.__h) > 0; }
    contains(other) {
      for (let n = other; n; n = n.parentNode) if (n === this) return true;
      return false;
    }
    appendChild(child) { insert(this.__h, child, -1); return child; }
    append(...nodes) { for (const n of nodes) insert(this.__h, n, -1); }
    prepend(...nodes) {
      const first = rb.relative(this.__h, 1);
      for (const n of nodes) insert(this.__h, n, first);
    }
    insertBefore(child, ref) { insert(this.__h, child, ref ? handle(ref) : -1); return child; }
    removeChild(child) { rb.remove(handle(child)); return child; }
    replaceChild(child, old) { insert(this.__h, child, handle(old)); rb.remove(handle(old)); return old; }
    remove() { rb.remove(this.__h); }
    replaceWith(...nodes) {
      const p = this.parentNode;
      if (!p) return;
      for (const n of nodes) insert(p.__h, n, this.__h);
      this.remove();
    }
    before(...nodes) { const p = this.parentNode; if (p) for (const n of nodes) insert(p.__h, n, this.__h); }
    after(...nodes) {
      const p = this.parentNode;
      if (!p) return;
      const next = rb.relative(this.__h, 3);
      for (const n of nodes) insert(p.__h, n, next);
    }
    get ownerDocument() { return this.nodeType === 9 ? null : wrap(0); }
    compareDocumentPosition(other) {
      if (other === this) return 0;
      const chain = (n) => { const c = []; for (; n; n = n.parentNode) c.unshift(n); return c; };
      const a = chain(this), b = chain(other);
      if (a[0] !== b[0]) return 1 | 32 | (this.__h < other.__h ? 4 : 2);
      if (b.includes(this)) return 16 | 4;
      if (a.includes(other)) return 8 | 2;
      let i = 0;
      while (a[i] === b[i]) i++;
      const kids = a[i - 1].childNodes;
      return kids.indexOf(b[i]) > kids.indexOf(a[i]) ? 4 : 2;
    }
    dispatchEvent(event) {
      dispatchSynthetic(this, event.type, event);
      return !event.defaultPrevented;
    }
    cloneNode(deep) {
      if (this.nodeType === 3) return wrap(rb.createText(this.textContent));
      const holder = wrap(rb.createElement('div'));
      rb.setHtml(holder.__h, deep ? this.outerHTML : this.outerHTML.replace(/>[\s\S]*<\/[^>]+>$/, '></' + this.localName + '>'));
      return holder.firstChild;
    }
    querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }
    querySelectorAll(sel) {
      return this.nodeType === 9 ? list(rb.queryAll(String(sel))) : list(rb.queryWithin(this.__h, String(sel)));
    }
    getElementsByTagName(tag) { return this.querySelectorAll(tag === '*' ? '*' : String(tag).toLowerCase()); }
    getElementsByClassName(names) {
      const sel = String(names).trim().split(/\s+/).map((c) => '.' + c).join('');
      return sel ? this.querySelectorAll(sel) : [];
    }
  }

  class Text extends Node {
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
    get length() { return this.textContent.length; }
  }

  function camelToKebab(s) {
    return s.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());
  }

  function styleProxy(el) {
    return new Proxy({}, {
      get(_, prop) {
        if (prop === 'setProperty') return (p, v) => rb.setStyle(el.__h, String(p), v === null || v === undefined ? '' : String(v));
        if (prop === 'removeProperty') return (p) => rb.setStyle(el.__h, String(p), '');
        if (prop === 'getPropertyValue') return (p) => readStyle(el, String(p));
        if (prop === 'cssText') return el.getAttribute('style') || '';
        if (typeof prop !== 'string') return undefined;
        return readStyle(el, camelToKebab(prop));
      },
      set(_, prop, value) {
        if (prop === 'cssText') { el.setAttribute('style', String(value)); return true; }
        rb.setStyle(el.__h, camelToKebab(String(prop)), value === null || value === undefined ? '' : String(value));
        return true;
      },
    });
  }

  function readStyle(el, prop) {
    const css = el.getAttribute('style') || '';
    for (const decl of css.split(';')) {
      const i = decl.indexOf(':');
      if (i > 0 && decl.slice(0, i).trim().toLowerCase() === prop) return decl.slice(i + 1).trim();
    }
    return '';
  }

  class DOMTokenList {
    constructor(el) { this.el = el; }
    add(...cs) { for (const c of cs) rb.classOp(this.el.__h, 1, String(c)); }
    remove(...cs) { for (const c of cs) rb.classOp(this.el.__h, 2, String(c)); }
    toggle(c, force) {
      if (force === true) { this.add(c); return true; }
      if (force === false) { this.remove(c); return false; }
      return rb.classOp(this.el.__h, 3, String(c));
    }
    contains(c) { return rb.classOp(this.el.__h, 0, String(c)); }
    replace(a, b) { if (!this.contains(a)) return false; this.remove(a); this.add(b); return true; }
    get length() { return this.values().length; }
    item(i) { return this.values()[i] || null; }
    values() { return (this.el.className || '').split(/\s+/).filter(Boolean); }
    forEach(fn) { this.values().forEach(fn); }
    [Symbol.iterator]() { return this.values()[Symbol.iterator](); }
    toString() { return this.el.className; }
  }

  function datasetProxy(el) {
    const attr = (p) => 'data-' + camelToKebab(String(p));
    return new Proxy({}, {
      get(_, p) { return typeof p === 'string' ? (el.getAttribute(attr(p)) ?? undefined) : undefined; },
      set(_, p, v) { el.setAttribute(attr(p), String(v)); return true; },
      deleteProperty(_, p) { el.removeAttribute(attr(p)); return true; },
    });
  }

  class Element extends Node {
    get localName() { return rb.tagName(this.__h); }
    get tagName() { return this.localName.toUpperCase(); }
    get nodeName() { return this.tagName; }
    get id() { return this.getAttribute('id') || ''; }
    set id(v) { this.setAttribute('id', v); }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() { return new DOMTokenList(this); }
    get style() { return styleProxy(this); }
    get dataset() { return datasetProxy(this); }
    // Geometry from the last layout.
    getBoundingClientRect() {
      const x = rb.rect(this.__h, 0), y = rb.rect(this.__h, 1);
      const width = rb.rect(this.__h, 2), height = rb.rect(this.__h, 3);
      return { x, y, left: x, top: y, width, height, right: x + width, bottom: y + height, toJSON() { return this; } };
    }
    getClientRects() { return [this.getBoundingClientRect()]; }
    get offsetWidth() { return Math.round(rb.rect(this.__h, 2)); }
    get offsetHeight() { return Math.round(rb.rect(this.__h, 3)); }
    get offsetLeft() { return Math.round(rb.rect(this.__h, 0)); }
    get offsetTop() { return Math.round(rb.rect(this.__h, 1)); }
    get offsetParent() { return this.parentElement; }
    get clientWidth() { return Math.round(rb.rect(this.__h, 4)); }
    get clientHeight() { return Math.round(rb.rect(this.__h, 5)); }
    get clientTop() { return 0; }
    get clientLeft() { return 0; }
    get scrollWidth() { return this.clientWidth; }
    get scrollHeight() { return this.clientHeight; }
    get scrollTop() { return 0; }
    set scrollTop(v) {}
    get scrollLeft() { return 0; }
    set scrollLeft(v) {}
    scrollIntoView() {}
    focus() {}
    blur() {}
    get innerHTML() { return rb.getHtml(this.__h, 0); }
    set innerHTML(v) { rb.setHtml(this.__h, String(v)); }
    get outerHTML() { return rb.getHtml(this.__h, 1); }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    get value() { return rb.getValue(this.__h); }
    set value(v) { rb.setValue(this.__h, String(v)); }
    get checked() { return rb.getChecked(this.__h); }
    set checked(v) { rb.setChecked(this.__h, !!v); }
    get selectedIndex() { return rb.getSelected(this.__h); }
    set selectedIndex(i) { rb.setSelected(this.__h, Number(i)); }
    get options() { return this.querySelectorAll('option'); }
    get form() { return this.closest('form'); }
    get name() { return this.getAttribute('name') || ''; }
    set name(v) { this.setAttribute('name', v); }
    get type() {
      const t = this.getAttribute('type');
      if (t) return t.toLowerCase();
      return this.localName === 'button' ? 'submit' : this.localName === 'input' ? 'text' : '';
    }
    get elements() { return this.querySelectorAll('input, select, textarea, button'); }
    submit() { rb.formAction(this.__h, 0); }
    reset() { rb.formAction(this.__h, 1); }
    get hidden() { return this.hasAttribute('hidden'); }
    set hidden(v) { if (v) this.setAttribute('hidden', ''); else this.removeAttribute('hidden'); }
    get disabled() { return this.hasAttribute('disabled'); }
    set disabled(v) { if (v) this.setAttribute('disabled', ''); else this.removeAttribute('disabled'); }
    get href() { return this.getAttribute('href') || ''; }
    set href(v) { this.setAttribute('href', v); }
    get src() { return this.getAttribute('src') || ''; }
    set src(v) { this.setAttribute('src', v); }
    get title() { return this.getAttribute('title') || ''; }
    set title(v) { this.setAttribute('title', v); }
    get attributes() {
      return rb.attrNames(this.__h).map((name) => ({ name, value: this.getAttribute(name) }));
    }

    getAttribute(name) { return rb.getAttr(this.__h, String(name).toLowerCase()); }
    setAttribute(name, value) { rb.setAttr(this.__h, String(name).toLowerCase(), String(value)); }
    removeAttribute(name) { rb.removeAttr(this.__h, String(name).toLowerCase()); }
    hasAttribute(name) { return this.getAttribute(name) !== null; }
    toggleAttribute(name, force) {
      const has = this.hasAttribute(name);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.setAttribute(name, '');
      if (!want && has) this.removeAttribute(name);
      return want;
    }
    getAttributeNames() { return rb.attrNames(this.__h); }
    matches(sel) { return rb.matches(this.__h, String(sel)); }
    closest(sel) {
      for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(sel)) return n;
      return null;
    }
    insertAdjacentHTML(where, html) {
      const holder = wrap(rb.createElement('div'));
      holder.innerHTML = html;
      const nodes = holder.childNodes;
      switch (String(where).toLowerCase()) {
        case 'beforebegin': this.before(...nodes); break;
        case 'afterbegin': this.prepend(...nodes); break;
        case 'beforeend': this.append(...nodes); break;
        case 'afterend': this.after(...nodes); break;
      }
    }
    click() { dispatchSynthetic(this, 'click'); }
    focus() {}
    blur() {}
  }

  // A fragment is a detached <template> element whose children move when it
  // is inserted.
  class DocumentFragment extends Element {
    get nodeType() { return 11; }
    get nodeName() { return '#document-fragment'; }
    getElementById(id) { return this.querySelectorAll('[id]').find((e) => e.id === String(id)) || null; }
  }

  function insert(parent, child, ref) {
    if (child instanceof DocumentFragment) {
      for (const c of child.childNodes) rb.insert(parent, c.__h, ref);
    } else {
      rb.insert(parent, handle(child), ref);
    }
  }

  class Document extends Node {
    get documentElement() { return wrap(rb.special(1)); }
    get head() { return wrap(rb.special(2)); }
    get body() { return wrap(rb.special(3)); }
    get title() {
      const t = this.querySelector('title');
      return t ? t.textContent.trim().replace(/\s+/g, ' ') : '';
    }
    set title(v) {
      let t = this.querySelector('title');
      if (!t) {
        t = this.createElement('title');
        (this.head || this.documentElement).appendChild(t);
      }
      t.textContent = v;
    }
    get location() { return location; }
    get readyState() { return 'complete'; }
    getElementById(id) {
      const v = String(id).replace(/["\\]/g, '\\$&');
      return wrap(rb.query('[id="' + v + '"]'));
    }
    get forms() { return this.querySelectorAll('form'); }
    get images() { return this.querySelectorAll('img'); }
    get links() { return this.querySelectorAll('a[href], area[href]'); }
    get scripts() { return this.querySelectorAll('script'); }
    get activeElement() { return this.body; }
    hasFocus() { return true; }
    createEvent(kind) { return new (/custom/i.test(kind) ? CustomEvent : Event)(''); }
    createComment(text) { return this.createTextNode(''); }
    createElement(tag) {
      const h = rb.createElement(String(tag).toLowerCase());
      if (h < 0) throw new Error('InvalidCharacterError: ' + tag);
      return wrap(h);
    }
    createTextNode(text) { return wrap(rb.createText(String(text))); }
    createDocumentFragment() {
      const h = rb.createElement('template');
      const f = new DocumentFragment(h);
      wrappers.set(h, f);
      return f;
    }
  }

  // ----- events --------------------------------------------------------------

  class Event {
    constructor(type, init) {
      this.type = type;
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.defaultPrevented = false;
      this.timeStamp = rb.now();
    }
    preventDefault() { this.defaultPrevented = true; rb.preventDefault(); }
    initEvent(type, bubbles, cancelable) { this.type = type; this.bubbles = !!bubbles; this.cancelable = !!cancelable; }
    stopPropagation() { rb.stopPropagation(); }
    stopImmediatePropagation() { rb.stopPropagation(); }
  }

  function currentEvent() {
    const e = new Event(rb.eventString(0));
    const mods = rb.eventInt(3);
    Object.assign(e, {
      target: wrap(rb.eventInt(0)),
      currentTarget: wrap(rb.eventInt(1)),
      key: rb.eventString(1),
      keyCode: rb.eventInt(2),
      which: rb.eventInt(2),
      shiftKey: !!(mods & 1),
      ctrlKey: !!(mods & 2),
      altKey: !!(mods & 4),
      metaKey: !!(mods & 8),
      clientX: rb.eventPos(0),
      clientY: rb.eventPos(1),
      button: rb.eventInt(4),
    });
    e.pageX = e.clientX;
    e.pageY = e.clientY;
    return e;
  }

  function call(fn, self, args) {
    try {
      if (typeof fn === 'function') fn.apply(self, args);
      else if (fn && typeof fn.handleEvent === 'function') fn.handleEvent(...args);
    } catch (err) {
      reportError(err);
    }
  }

  // Called by the runtime for each listener (the browser does the bubbling).
  global.__rb_dispatch = function (id) {
    const l = listeners.get(id);
    if (!l) return;
    if (l.once) {
      listeners.delete(id);
      rb.unlisten(l.node.__h, l.type, id);
    }
    const e = currentEvent();
    call(l.fn, e.currentTarget, [e]);
  };

  // Inline handler attributes (onclick="…"), registered by the browser.
  global.__rb_inline = function (h, type, code) {
    const el = wrap(h);
    let fn;
    try {
      fn = new Function('event', code);
    } catch (err) {
      reportError(err);
      return;
    }
    el.addEventListener(type, function (event) {
      if (fn.call(this, event) === false) event.preventDefault();
    });
  };

  // `window.onload = …` and friends.
  function handlerProperty(target, name) {
    let current = null;
    Object.defineProperty(target, 'on' + name, {
      configurable: true,
      enumerable: true,
      get: () => current,
      set: (fn) => {
        if (current) document.removeEventListener(name, current);
        current = typeof fn === 'function' ? fn : null;
        if (current) document.addEventListener(name, current);
      },
    });
  }

  // element.click() from script: run click listeners up the tree in JS.
  function dispatchSynthetic(target, type, event) {
    let stopped = false;
    const e = event || new Event(type, { bubbles: true });
    e.target = target;
    e.stopPropagation = () => { stopped = true; };
    e.stopImmediatePropagation = () => { stopped = true; };
    e.preventDefault = () => { e.defaultPrevented = true; };
    for (let n = target; n && !stopped; n = e.bubbles ? n.parentNode : null) {
      e.currentTarget = n;
      for (const l of [...listeners.values()]) {
        if (l.node === n && l.type === type) call(l.fn, n, [e]);
      }
    }
  }

  // ----- timers --------------------------------------------------------------

  const timers = new Map(); // id -> { fn, args, interval }
  let nextTimer = 1;

  function addTimer(fn, ms, args, interval) {
    const id = nextTimer++;
    const delay = Math.max(0, Number(ms) || 0);
    timers.set(id, { fn, args, interval: interval ? delay : -1 });
    rb.setTimeout(delay, id);
    return id;
  }

  global.__rb_timer = function (id) {
    const t = timers.get(id);
    if (!t) return;
    if (t.interval >= 0) rb.setTimeout(t.interval, id);
    else timers.delete(id);
    if (typeof t.fn === 'function') call(t.fn, global, t.args);
    else (0, eval)(String(t.fn));
  };

  function clearTimer(id) {
    if (timers.delete(id)) rb.clearTimeout(id);
  }

  // ----- globals -------------------------------------------------------------

  function format(args) {
    return args.map((a) => {
      if (typeof a === 'string') return a;
      if (a instanceof Error) return a.name + ': ' + a.message;
      if (a instanceof Node) return a.nodeType === 1 ? '<' + a.localName + '>' : a.nodeName;
      try { return JSON.stringify(a) ?? String(a); } catch (_) { return String(a); }
    }).join(' ');
  }

  function reportError(err) {
    const msg = err && err.stack ? err.stack : err instanceof Error ? err.name + ': ' + err.message : String(err);
    rb.log('Uncaught ' + msg);
  }
  global.__rb_report = reportError;

  const console = {
    log: (...a) => rb.log(format(a)),
    info: (...a) => rb.log(format(a)),
    debug: (...a) => rb.log(format(a)),
    warn: (...a) => rb.log('warning: ' + format(a)),
    error: (...a) => rb.log('error: ' + format(a)),
  };

  function parseUrl() {
    const href = rb.location();
    const m = /^([a-z][a-z0-9+.-]*:)(\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(href) || [];
    return {
      protocol: m[1] || '',
      host: m[3] || '',
      pathname: m[4] || '',
      search: m[5] && m[5].length > 1 ? m[5] : '',
      hash: m[6] && m[6].length > 1 ? m[6] : '',
    };
  }

  class URLSearchParams {
    constructor(init) {
      this._pairs = [];
      if (typeof init === 'string') {
        for (const part of init.replace(/^\?/, '').split('&')) {
          if (!part) continue;
          const i = part.indexOf('=');
          const dec = (s) => decodeURIComponent(s.replace(/\+/g, ' '));
          this._pairs.push(i < 0 ? [dec(part), ''] : [dec(part.slice(0, i)), dec(part.slice(i + 1))]);
        }
      } else if (init && typeof init === 'object') {
        for (const [k, v] of Object.entries(init)) this._pairs.push([String(k), String(v)]);
      }
    }
    get(k) { const p = this._pairs.find(([n]) => n === k); return p ? p[1] : null; }
    getAll(k) { return this._pairs.filter(([n]) => n === k).map(([, v]) => v); }
    has(k) { return this._pairs.some(([n]) => n === k); }
    set(k, v) { this.delete(k); this._pairs.push([k, String(v)]); }
    append(k, v) { this._pairs.push([k, String(v)]); }
    delete(k) { this._pairs = this._pairs.filter(([n]) => n !== k); }
    entries() { return this._pairs[Symbol.iterator](); }
    keys() { return this._pairs.map(([k]) => k)[Symbol.iterator](); }
    forEach(fn) { this._pairs.forEach(([k, v]) => fn(v, k, this)); }
    [Symbol.iterator]() { return this.entries(); }
    toString() {
      const enc = (s) => encodeURIComponent(s).replace(/%20/g, '+');
      return this._pairs.map(([k, v]) => enc(k) + '=' + enc(v)).join('&');
    }
  }

  const location = {
    get protocol() { return parseUrl().protocol; },
    get host() { return parseUrl().host; },
    get hostname() { return parseUrl().host.replace(/:\d+$/, ''); },
    get pathname() { return parseUrl().pathname; },
    get search() { return parseUrl().search; },
    get hash() { return parseUrl().hash; },
    get origin() { const u = parseUrl(); return u.host ? u.protocol + '//' + u.host : 'null'; },
    get href() { return rb.location(); },
    set href(v) { rb.navigate(String(v)); },
    assign(v) { rb.navigate(String(v)); },
    replace(v) { rb.navigate(String(v)); },
    reload() { rb.navigate(rb.location()); },
    toString() { return rb.location(); },
  };

  const history = {
    back() { rb.historyGo(-1); },
    forward() { rb.historyGo(1); },
    go(n) { rb.historyGo(Number(n) || 0); },
  };

  // ----- web platform stand-ins ---------------------------------------------

  // Storage lasts for the page (nothing is written to disk).
  class Storage {
    constructor() { Object.defineProperty(this, '_m', { value: new Map() }); }
    get length() { return this._m.size; }
    key(i) { return [...this._m.keys()][i] ?? null; }
    getItem(k) { return this._m.has(String(k)) ? this._m.get(String(k)) : null; }
    setItem(k, v) { this._m.set(String(k), String(v)); }
    removeItem(k) { this._m.delete(String(k)); }
    clear() { this._m.clear(); }
  }

  class CustomEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.detail = init && 'detail' in init ? init.detail : null;
    }
  }

  // Observers: nothing changes size or mutates behind scripts' backs here,
  // and everything counts as visible (so lazy loaders load at once).
  class MutationObserver {
    constructor(cb) { this.cb = cb; }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
  }
  class ResizeObserver {
    constructor(cb) { this.cb = cb; }
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  class IntersectionObserver {
    constructor(cb, options) { this.cb = cb; this.root = (options && options.root) || null; }
    observe(target) {
      const entry = {
        target,
        isIntersecting: true,
        intersectionRatio: 1,
        time: rb.now(),
        boundingClientRect: {},
        intersectionRect: {},
        rootBounds: null,
      };
      addTimer(() => this.cb([entry], this), 0, [], false);
    }
    unobserve() {}
    disconnect() {}
    takeRecords() { return []; }
  }

  function matchMedia(query) {
    return {
      matches: false,
      media: String(query),
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
    };
  }

  const navigator = {
    userAgent: 'Mozilla/5.0 rubrowser',
    language: 'en-US',
    languages: ['en-US', 'en'],
    platform: '',
    cookieEnabled: false,
    onLine: true,
    hardwareConcurrency: 1,
    maxTouchPoints: 0,
    sendBeacon: () => true,
  };

  const document = wrap(0);
  wrappers.set(0, document);
  Object.defineProperty(document, 'defaultView', { configurable: true, get: () => global });
  if (!('readyState' in document)) {
    Object.defineProperty(document, 'readyState', { configurable: true, get: () => 'complete' });
  }
  // Cookies set by scripts last for the page.
  let cookieJar = '';
  Object.defineProperty(document, 'cookie', {
    configurable: true,
    get: () => cookieJar,
    set: (v) => {
      const pair = String(v).split(';')[0];
      const name = pair.split('=')[0].trim();
      const rest = cookieJar.split('; ').filter((c) => c && c.split('=')[0] !== name);
      cookieJar = [...rest, pair.trim()].join('; ');
    },
  });
  for (const name of ['load', 'domcontentloaded', 'keydown', 'click']) handlerProperty(global, name);

  for (const [name, which] of [['innerWidth', 2], ['innerHeight', 3], ['outerWidth', 2], ['outerHeight', 3]]) {
    Object.defineProperty(global, name, { configurable: true, get: () => rb.rect(0, which) });
  }
  Object.assign(global, {
    window: global,
    self: global,
    document,
    console,
    location,
    history,
    Node,
    Text,
    Element,
    HTMLElement: Element,
    Document,
    Event,
    EventTarget,
    URLSearchParams,
    Storage,
    localStorage: new Storage(),
    sessionStorage: new Storage(),
    CustomEvent,
    DocumentFragment,
    MutationObserver,
    ResizeObserver,
    IntersectionObserver,
    matchMedia,
    navigator,
    devicePixelRatio: 1,
    scrollX: 0,
    scrollY: 0,
    pageXOffset: 0,
    pageYOffset: 0,
    screen: { width: 1920, height: 1080, availWidth: 1920, availHeight: 1080, colorDepth: 24 },
    scrollTo() {},
    scrollBy() {},
    scroll() {},
    getComputedStyle: (el) => (el && el.style) || {},
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, args, false),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, args, true),
    clearTimeout: clearTimer,
    clearInterval: clearTimer,
    requestAnimationFrame: (fn) => addTimer(() => fn(rb.now()), 16, [], false),
    cancelAnimationFrame: clearTimer,
    queueMicrotask: (fn) => Promise.resolve().then(fn),
    alert: (msg) => rb.log('alert: ' + String(msg)),
    performance: { now: () => rb.now() },
    addEventListener: (type, fn, opts) => document.addEventListener(type, fn, opts),
    removeEventListener: (type, fn) => document.removeEventListener(type, fn),
  });
})(__rb);
