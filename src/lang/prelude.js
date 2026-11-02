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

  // A URL attribute as a property: resolved against the page.
  function absolute(v) {
    if (v === null || v === undefined) return '';
    try {
      return new URL(v, document.baseURI).href;
    } catch (_) {
      return v;
    }
  }

  function wrap(h) {
    if (h === null || h === undefined || h < 0) return null;
    let n = wrappers.get(h);
    if (!n) {
      const t = rb.nodeType(h);
      const custom = t === 1 && customRegistry.get(rb.tagName(h));
      if (custom) return upgrade(h, custom);
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
      if (this.__h === undefined) {
        // Not a DOM node (XMLHttpRequest, AbortSignal…): listeners live here.
        if (!this._ls) Object.defineProperty(this, '_ls', { value: [] });
        if (!this._ls.some((l) => l.type === type && l.fn === fn)) this._ls.push({ type, fn });
        return;
      }
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
      if (this.__h === undefined) {
        if (this._ls) this._ls.splice(0, this._ls.length, ...this._ls.filter((l) => !(l.type === type && l.fn === fn)));
        return;
      }
      for (const [id, l] of listeners) {
        if (l.node === this && l.type === type && l.fn === fn) {
          listeners.delete(id);
          rb.unlisten(this.__h, type, id);
        }
      }
    }
  }

  EventTarget.prototype.dispatchEvent = function (event) {
    event.target = event.target || this;
    event.currentTarget = this;
    for (const l of (this._ls || []).filter((l) => l.type === String(event.type).toLowerCase())) {
      call(typeof l.fn === 'function' ? l.fn : l.fn.handleEvent, typeof l.fn === 'function' ? this : l.fn, [event]);
    }
    return !event.defaultPrevented;
  };

  class Node extends EventTarget {
    constructor(h) {
      super();
      // A custom element's constructor runs while it is being upgraded.
      Object.defineProperty(this, '__h', { value: h === undefined ? upgrading : h });
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
      if (this instanceof DocumentFragment) {
        const f = document.createDocumentFragment();
        if (deep) for (const c of this.childNodes) f.appendChild(c.cloneNode(true));
        return f;
      }
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

  // getComputedStyle: values from the browser's cascade, read-only.
  function cssName(prop) {
    if (prop === 'cssFloat') return 'float';
    const name = camelToKebab(prop);
    return /^(webkit|moz|ms)-/.test(name) ? '-' + name : name;
  }
  function computedStyle(el) {
    const get = (name) => (el && el.nodeType === 1 && rb.computedStyle(el.__h, name)) || '';
    return new Proxy({}, {
      get(_, prop) {
        if (prop === 'getPropertyValue') return (p) => get(String(p).trim());
        if (prop === 'getPropertyPriority') return () => '';
        if (prop === 'setProperty' || prop === 'removeProperty') return () => {
          throw new Error('NoModificationAllowedError: computed styles are read-only');
        };
        if (prop === 'cssText') return '';
        if (typeof prop !== 'string' || prop === 'then') return undefined;
        return get(cssName(prop));
      },
      set() { return true; },
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
    attachShadow() {
      // Shadow trees are not rendered; scripts still get a working root.
      const root = document.createDocumentFragment();
      Object.defineProperty(root, 'host', { value: this });
      Object.defineProperty(this, 'shadowRoot', { value: root, configurable: true });
      return root;
    }
    animate() {
      return { finished: Promise.resolve(), cancel() {}, finish() {}, play() {}, pause() {}, onfinish: null };
    }
    isEqualNode(other) { return !!other && this.outerHTML === other.outerHTML; }
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
    set type(v) { this.setAttribute('type', v); }
    get elements() { return this.querySelectorAll('input, select, textarea, button'); }
    submit() { rb.formAction(this.__h, 0); }
    reset() { rb.formAction(this.__h, 1); }
    get hidden() { return this.hasAttribute('hidden'); }
    set hidden(v) { if (v) this.setAttribute('hidden', ''); else this.removeAttribute('hidden'); }
    get disabled() { return this.hasAttribute('disabled'); }
    set disabled(v) { if (v) this.setAttribute('disabled', ''); else this.removeAttribute('disabled'); }
    get href() { return absolute(this.getAttribute('href')); }
    set href(v) { this.setAttribute('href', v); }
    get src() { return absolute(this.getAttribute('src')); }
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
    get baseURI() {
      const base = rb.query('base[href]');
      if (base < 0) return location.href;
      try {
        return new URL(rb.getAttr(base, 'href'), location.href).href;
      } catch (_) {
        return location.href;
      }
    }
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
    createElementNS(ns, tag) { return this.createElement(String(tag).replace(/^.*:/, '')); }
    get implementation() { return implementation; }
    get currentScript() { return currentScript; }
    write(...text) { documentWrite(text.join('')); }
    writeln(...text) { documentWrite(text.join('') + '\n'); }
    open() { return this; }
    close() {}
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    createElement(tag) {
      const h = rb.createElement(String(tag).toLowerCase());
      if (h < 0) throw new Error('InvalidCharacterError: ' + tag);
      return wrap(h);
    }
    createTextNode(text) { return wrap(rb.createText(String(text))); }
    importNode(node, deep) { return node.cloneNode(!!deep); }
    adoptNode(node) { node.remove(); return node; }
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
    const head = err instanceof Error ? err.name + ': ' + err.message : String(err);
    const msg = err && err.stack && !String(err.stack).startsWith(head) ? head + '\n' + err.stack : (err && err.stack) || head;
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
    // Single-page apps: the state is kept, the address does not change.
    state: null,
    length: 1,
    scrollRestoration: 'auto',
    pushState(state) { this.state = state; },
    replaceState(state) { this.state = state; },
  };

  // ----- custom elements -----------------------------------------------------

  const customRegistry = new Map(); // tag -> class
  const definedWaiters = new Map(); // tag -> [resolve]
  let upgrading;

  // Turns element `h` into an instance of custom element class `cls`.
  function upgrade(h, cls) {
    let el;
    upgrading = h;
    try {
      el = Reflect.construct(cls, []);
    } catch (err) {
      reportError(err);
      el = Object.create(cls.prototype);
      Object.defineProperty(el, '__h', { value: h });
    } finally {
      upgrading = undefined;
    }
    wrappers.set(h, el);
    const observed = cls.observedAttributes || [];
    if (typeof el.attributeChangedCallback === 'function') {
      for (const a of observed) {
        const v = el.getAttribute(a);
        if (v !== null) call(el.attributeChangedCallback, el, [a, null, v]);
      }
    }
    if (typeof el.connectedCallback === 'function' && el.isConnected) {
      call(el.connectedCallback, el, []);
    }
    return el;
  }

  const customElements = {
    define(name, cls) {
      name = String(name).toLowerCase();
      if (customRegistry.has(name)) return;
      customRegistry.set(name, cls);
      // Upgrade the elements already in the document.
      for (const h of rb.queryAll(name)) {
        const old = wrappers.get(h);
        if (!old || !(old instanceof cls)) upgrade(h, cls);
      }
      for (const resolve of definedWaiters.get(name) || []) resolve(cls);
      definedWaiters.delete(name);
    },
    get(name) { return customRegistry.get(String(name).toLowerCase()); },
    whenDefined(name) {
      name = String(name).toLowerCase();
      if (customRegistry.has(name)) return Promise.resolve(customRegistry.get(name));
      return new Promise((resolve) => {
        if (!definedWaiters.has(name)) definedWaiters.set(name, []);
        definedWaiters.get(name).push(resolve);
      });
    },
    upgrade() {},
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

  // URL: absolute URLs, or relative ones resolved against a base.
  class URL {
    constructor(url, base) {
      url = String(url);
      const abs = /^[a-z][a-z0-9+.-]*:/i.test(url);
      if (!abs) {
        if (base === undefined) throw new TypeError('Invalid URL: ' + url);
        url = URL.resolve(String(base instanceof URL ? base.href : base), url);
      }
      const m = /^([a-z][a-z0-9+.-]*:)(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(url);
      if (!m) throw new TypeError('Invalid URL: ' + url);
      this.protocol = m[1].toLowerCase();
      const auth = m[2] || '';
      this.host = auth.replace(/^.*@/, '').toLowerCase();
      this.pathname = m[3] || (m[2] !== undefined ? '/' : '');
      this.search = m[4] && m[4].length > 1 ? m[4] : '';
      this.hash = m[5] && m[5].length > 1 ? m[5] : '';
      this.username = '';
      this.password = '';
    }
    static resolve(base, rel) {
      const m = /^([a-z][a-z0-9+.-]*:)(\/\/[^/?#]*)?([^?#]*)(\?[^#]*)?/i.exec(base) || ['', '', '', '/', ''];
      const [, scheme, authority = '', path = '/', query = ''] = m;
      if (rel.startsWith('//')) return scheme + rel;
      if (rel.startsWith('#')) return scheme + authority + path + query + rel;
      if (rel.startsWith('?')) return scheme + authority + path + rel;
      let p = rel.startsWith('/') ? rel : path.replace(/[^/]*$/, '') + rel;
      const [pathPart, ...rest] = p.split(/(?=[?#])/);
      const out = [];
      for (const seg of pathPart.split('/')) {
        if (seg === '..') out.pop();
        else if (seg !== '.') out.push(seg);
      }
      let joined = out.join('/');
      if (!joined.startsWith('/')) joined = '/' + joined;
      return scheme + authority + joined + rest.join('');
    }
    static canParse(url, base) { try { new URL(url, base); return true; } catch (_) { return false; } }
    get hostname() { return this.host.replace(/:\d+$/, ''); }
    get port() { const m = /:(\d+)$/.exec(this.host); return m ? m[1] : ''; }
    get origin() { return this.host ? this.protocol + '//' + this.host : 'null'; }
    get href() { return this.protocol + (this.host || this.protocol === 'file:' ? '//' + this.host : '') + this.pathname + this.search + this.hash; }
    set href(v) { Object.assign(this, new URL(v)); }
    get searchParams() {
      const params = new URLSearchParams(this.search);
      const url = this;
      for (const m of ['set', 'append', 'delete']) {
        const orig = params[m].bind(params);
        params[m] = (...a) => { orig(...a); const q = params.toString(); url.search = q ? '?' + q : ''; };
      }
      return params;
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }
  URL.createObjectURL = () => 'blob:';
  URL.revokeObjectURL = () => {};

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s = '') {
      const bytes = [];
      for (const ch of String(s)) {
        let c = ch.codePointAt(0);
        if (c < 0x80) bytes.push(c);
        else if (c < 0x800) bytes.push(0xc0 | (c >> 6), 0x80 | (c & 63));
        else if (c < 0x10000) bytes.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
        else bytes.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      }
      return new Uint8Array(bytes);
    }
  }
  class TextDecoder {
    get encoding() { return 'utf-8'; }
    decode(buf) {
      if (!buf) return '';
      const b = buf instanceof Uint8Array ? buf : new Uint8Array(buf.buffer || buf);
      let out = '';
      for (let i = 0; i < b.length;) {
        const c = b[i];
        let cp, n;
        if (c < 0x80) { cp = c; n = 1; }
        else if (c >= 0xf0) { cp = ((c & 7) << 18) | ((b[i + 1] & 63) << 12) | ((b[i + 2] & 63) << 6) | (b[i + 3] & 63); n = 4; }
        else if (c >= 0xe0) { cp = ((c & 15) << 12) | ((b[i + 1] & 63) << 6) | (b[i + 2] & 63); n = 3; }
        else { cp = ((c & 31) << 6) | (b[i + 1] & 63); n = 2; }
        out += String.fromCodePoint(cp);
        i += n;
      }
      return out;
    }
  }

  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  function btoa(s) {
    s = String(s);
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
      const [a, b, c] = [s.charCodeAt(i), s.charCodeAt(i + 1), s.charCodeAt(i + 2)];
      if (a > 255 || b > 255 || c > 255) throw new Error('InvalidCharacterError');
      const n = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += B64[n >> 18] + B64[(n >> 12) & 63] + (i + 1 < s.length ? B64[(n >> 6) & 63] : '=') + (i + 2 < s.length ? B64[n & 63] : '=');
    }
    return out;
  }
  function atob(s) {
    s = String(s).replace(/[\s=]/g, '');
    let out = '', bits = 0, acc = 0;
    for (const ch of s) {
      const v = B64.indexOf(ch === '-' ? '+' : ch === '_' ? '/' : ch);
      if (v < 0) throw new Error('InvalidCharacterError');
      acc = (acc << 6) | v;
      bits += 6;
      if (bits >= 8) { bits -= 8; out += String.fromCharCode((acc >> bits) & 255); }
    }
    return out;
  }

  const crypto = {
    getRandomValues(arr) {
      const max = 2 ** (8 * (arr.BYTES_PER_ELEMENT || 1));
      for (let i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * max);
      return arr;
    },
    randomUUID() {
      const h = '0123456789abcdef';
      let s = '';
      for (let i = 0; i < 36; i++) {
        s += i === 8 || i === 13 || i === 18 || i === 23 ? '-' : i === 14 ? '4' : i === 19 ? h[8 + Math.floor(Math.random() * 4)] : h[Math.floor(Math.random() * 16)];
      }
      return s;
    },
  };

  function structuredClone(v) {
    if (v === null || typeof v !== 'object') return v;
    if (v instanceof Date) return new Date(v.getTime());
    if (v instanceof Map) return new Map([...v].map(([k, x]) => [structuredClone(k), structuredClone(x)]));
    if (v instanceof Set) return new Set([...v].map(structuredClone));
    if (Array.isArray(v)) return v.map(structuredClone);
    const out = {};
    for (const k of Object.keys(v)) out[k] = structuredClone(v[k]);
    return out;
  }

  class AbortSignal extends EventTarget {
    constructor() { super(); this.aborted = false; this.reason = undefined; this.onabort = null; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static timeout() { return new AbortSignal(); }
    static abort(reason) { const s = new AbortSignal(); s.aborted = true; s.reason = reason; return s; }
    static any() { return new AbortSignal(); }
  }
  // Workers and channels exist but never deliver messages (no threads
  // here): enough for feature checks and for pages that use them optionally.
  class MessagePort extends EventTarget {
    postMessage() {}
    start() {}
    close() {}
  }
  class Worker extends EventTarget {
    constructor(url) { super(); this.url = String(url); }
    postMessage() {}
    terminate() {}
  }
  class SharedWorker extends EventTarget {
    constructor(url) { super(); this.port = new MessagePort(); }
  }
  class MessageChannel {
    constructor() { this.port1 = new MessagePort(); this.port2 = new MessagePort(); }
  }
  class BroadcastChannel extends MessagePort {
    constructor(name) { super(); this.name = String(name); }
  }

  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) {
      if (this.signal.aborted) return;
      this.signal.aborted = true;
      this.signal.reason = reason === undefined ? new Error('AbortError') : reason;
      if (typeof this.signal.onabort === 'function') call(this.signal.onabort, this.signal, [{ type: 'abort' }]);
    }
  }

  // HTTP from scripts goes through the browser (same origin only); the
  // response arrives in __rb_http.
  class Headers {
    constructor(init) { this._m = new Map(); if (init) for (const [k, v] of Object.entries(init)) this.set(k, v); }
    get(k) { return this._m.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this._m.set(String(k).toLowerCase(), String(v)); }
    append(k, v) { this.set(k, v); }
    has(k) { return this._m.has(String(k).toLowerCase()); }
    delete(k) { this._m.delete(String(k).toLowerCase()); }
    forEach(fn) { this._m.forEach((v, k) => fn(v, k, this)); }
    [Symbol.iterator]() { return this._m.entries(); }
  }
  class Request { constructor(url, init) { this.url = String(url); this.method = (init && init.method) || 'GET'; this.headers = new Headers(init && init.headers); } }
  class Response {
    constructor(body, init) { this._body = body == null ? '' : String(body); this.status = (init && init.status) || 200; this.ok = this.status < 300; this.headers = new Headers(init && init.headers); }
    text() { return Promise.resolve(this._body); }
    json() { return Promise.resolve(JSON.parse(this._body)); }
  }
  const httpPending = new Map(); // id -> (status, body) => void
  global.__rb_http = function (id, status) {
    const done = httpPending.get(id);
    httpPending.delete(id);
    if (done) done(status, status ? rb.httpBody(id) : '');
  };
  function httpRequest(method, url, body, contentType, done) {
    const id = rb.http(String(url), String(method || 'GET'), body == null ? '' : String(body), contentType || '');
    httpPending.set(id, done);
  }
  function fetch(input, init) {
    init = init || {};
    const url = input instanceof Request ? input.url : String(input instanceof URL ? input.href : input);
    const method = init.method || (input instanceof Request ? input.method : 'GET');
    let body = init.body;
    let ctype = '';
    if (body instanceof URLSearchParams) { ctype = 'application/x-www-form-urlencoded;charset=UTF-8'; body = body.toString(); }
    else if (body instanceof FormData) { ctype = 'application/x-www-form-urlencoded;charset=UTF-8'; body = new URLSearchParams(Object.fromEntries(body)).toString(); }
    const headers = new Headers(init.headers && !(init.headers instanceof Headers) ? init.headers : undefined);
    if (init.headers instanceof Headers) init.headers.forEach((v, k) => headers.set(k, v));
    if (headers.has('content-type')) ctype = headers.get('content-type');
    return new Promise((resolve, reject) => {
      if (init.signal && init.signal.aborted) { reject(init.signal.reason); return; }
      httpRequest(method, url, body, ctype, (status, text) => {
        if (!status) reject(new TypeError('Failed to fetch'));
        else { const r = new Response(text, { status }); r.url = new URL(url, location.href).href; resolve(r); }
      });
    });
  }
  class XMLHttpRequest extends EventTarget {
    constructor() { super(); this.readyState = 0; this.status = 0; this.responseText = ''; this.response = null; this.responseType = ''; this._headers = {}; }
    open(method, url) { this._method = method; this._url = url; this.readyState = 1; }
    setRequestHeader(k, v) { this._headers[String(k).toLowerCase()] = String(v); }
    getResponseHeader() { return null; }
    getAllResponseHeaders() { return ''; }
    abort() { this._aborted = true; }
    send(body) {
      httpRequest(this._method, this._url, body, this._headers['content-type'], (status, text) => {
        if (this._aborted) return;
        this.readyState = 4;
        this.status = status;
        this.responseText = text;
        try { this.response = this.responseType === 'json' ? JSON.parse(text) : text; } catch (_) { this.response = null; }
        const fire = (name) => {
          const e = { type: name, target: this, currentTarget: this };
          if (typeof this['on' + name] === 'function') call(this['on' + name], this, [e]);
          this.dispatchEvent(Object.assign(new Event(name), e));
        };
        fire('readystatechange');
        fire(status ? 'load' : 'error');
        fire('loadend');
      });
    }
  }
  class FormData {
    constructor(form) {
      this._p = [];
      if (form && form.elements) for (const el of form.elements) if (el.name) this._p.push([el.name, el.value]);
    }
    append(k, v) { this._p.push([String(k), v]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { const p = this._p.find(([n]) => n === k); return p ? p[1] : null; }
    getAll(k) { return this._p.filter(([n]) => n === k).map(([, v]) => v); }
    has(k) { return this._p.some(([n]) => n === k); }
    delete(k) { this._p = this._p.filter(([n]) => n !== k); }
    entries() { return this._p[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }
  class Blob {
    constructor(parts, opts) { this._t = (parts || []).join(''); this.type = (opts && opts.type) || ''; this.size = this._t.length; }
    text() { return Promise.resolve(this._t); }
  }

  // ----- <canvas> ------------------------------------------------------------
  // The 2D context sends each call to the browser, which draws (see the
  // `canvas` import in doc/abi.md).

  const SEP = '\u0001';
  function canvasCmd(canvas, ...args) {
    return rb.canvas(canvas.__h, args.map(String).join(SEP));
  }

  class CanvasGradient {
    constructor(canvas, id) { this._c = canvas; this._id = id; }
    addColorStop(offset, color) {
      if (!(offset >= 0 && offset <= 1)) throw new RangeError('IndexSizeError');
      canvasCmd(this._c, 'addColorStop', this._id, offset, color);
    }
  }
  class CanvasPattern {
    constructor(id) { this._id = id; }
    setTransform() {}
  }

  // Path2D records its commands; fill / stroke / clip replay them.
  class Path2D {
    constructor(init) {
      this._cmds = init instanceof Path2D ? init._cmds : '';
      if (typeof init === 'string') this._svg = init;
    }
    _add(name, ...n) { this._cmds += name + ',' + n.map(Number).join(',') + ';'; }
    addPath(p) { if (p instanceof Path2D) this._cmds += p._cmds; }
    moveTo(x, y) { this._add('moveTo', x, y); }
    lineTo(x, y) { this._add('lineTo', x, y); }
    closePath() { this._add('closePath'); }
    quadraticCurveTo(a, b, c, d) { this._add('quadraticCurveTo', a, b, c, d); }
    bezierCurveTo(a, b, c, d, e, f) { this._add('bezierCurveTo', a, b, c, d, e, f); }
    arc(x, y, r, a0, a1, ccw) { this._add('arc', x, y, r, a0, a1, ccw ? 1 : 0); }
    arcTo(a, b, c, d, r) { this._add('arcTo', a, b, c, d, r); }
    ellipse(x, y, rx, ry, rot, a0, a1, ccw) { this._add('ellipse', x, y, rx, ry, rot, a0, a1, ccw ? 1 : 0); }
    rect(x, y, w, h) { this._add('rect', x, y, w, h); }
    roundRect(x, y, w, h, r) { this._add('roundRect', x, y, w, h, Array.isArray(r) ? r[0] || 0 : r || 0); }
  }

  class CanvasRenderingContext2D {
    constructor(canvas) {
      this.canvas = canvas;
      this._s = {
        fillStyle: '#000000', strokeStyle: '#000000', lineWidth: 1, lineCap: 'butt', lineJoin: 'miter',
        miterLimit: 10, lineDashOffset: 0, globalAlpha: 1, globalCompositeOperation: 'source-over',
        font: '10px sans-serif', textAlign: 'start', textBaseline: 'alphabetic', direction: 'inherit',
        shadowBlur: 0, shadowColor: 'rgba(0, 0, 0, 0)', shadowOffsetX: 0, shadowOffsetY: 0,
        imageSmoothingEnabled: true, imageSmoothingQuality: 'low', filter: 'none',
        letterSpacing: '0px', wordSpacing: '0px', fontKerning: 'auto',
      };
      this._dash = [];
      this._t = [1, 0, 0, 1, 0, 0];
      this._stack = [];
    }
    _cmd(...args) { return canvasCmd(this.canvas, ...args); }
    getContextAttributes() { return { alpha: true, colorSpace: 'srgb', desynchronized: false, willReadFrequently: false }; }

    save() { this._stack.push([{ ...this._s }, this._dash.slice(), this._t.slice()]); this._cmd('save'); }
    restore() {
      const top = this._stack.pop();
      if (top) [this._s, this._dash, this._t] = top;
      this._cmd('restore');
    }
    reset() { this._stack = []; this._t = [1, 0, 0, 1, 0, 0]; this._dash = []; this._cmd('reset'); }

    // Transforms (kept here too for getTransform).
    setTransform(a, b, c, d, e, f) {
      if (typeof a === 'object' && a) ({ a, b, c, d, e, f } = { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, ...a });
      this._t = [a, b, c, d, e, f].map(Number);
      this._cmd('setTransform', ...this._t);
    }
    resetTransform() { this.setTransform(1, 0, 0, 1, 0, 0); }
    transform(a, b, c, d, e, f) {
      const [A, B, C, D, E, F] = this._t;
      this._t = [A * a + C * b, B * a + D * b, A * c + C * d, B * c + D * d, A * e + C * f + E, B * e + D * f + F];
      this._cmd('transform', a, b, c, d, e, f);
    }
    translate(x, y) { this.transform(1, 0, 0, 1, x, y); }
    scale(x, y) { this.transform(x, 0, 0, y, 0, 0); }
    rotate(t) { const c = Math.cos(t), s = Math.sin(t); this.transform(c, s, -s, c, 0, 0); }
    getTransform() {
      const [a, b, c, d, e, f] = this._t;
      return { a, b, c, d, e, f, m11: a, m12: b, m21: c, m22: d, m41: e, m42: f, is2D: true, isIdentity: a === 1 && b === 0 && c === 0 && d === 1 && e === 0 && f === 0 };
    }

    setLineDash(segments) {
      if (!Array.isArray(segments) || segments.some((v) => !(v >= 0) || !isFinite(v))) return;
      this._dash = segments.slice();
      this._cmd('setLineDash', ...segments);
    }
    getLineDash() { return this._dash.length % 2 ? this._dash.concat(this._dash) : this._dash.slice(); }

    createLinearGradient(x0, y0, x1, y1) { return new CanvasGradient(this.canvas, this._cmd('linearGradient', x0, y0, x1, y1)); }
    createRadialGradient(x0, y0, r0, x1, y1, r1) {
      if (r0 < 0 || r1 < 0) throw new RangeError('IndexSizeError');
      return new CanvasGradient(this.canvas, this._cmd('radialGradient', x0, y0, r0, x1, y1, r1));
    }
    createConicGradient(angle, x, y) { return new CanvasGradient(this.canvas, this._cmd('conicGradient', angle, x, y)); }
    createPattern(image, repetition) {
      if (!image || image.__h === undefined) return null;
      const id = this._cmd('createPattern', image.__h, repetition || 'repeat');
      return id === null ? null : new CanvasPattern(id);
    }

    fillRect(x, y, w, h) { this._cmd('fillRect', x, y, w, h); }
    strokeRect(x, y, w, h) { this._cmd('strokeRect', x, y, w, h); }
    clearRect(x, y, w, h) { this._cmd('clearRect', x, y, w, h); }

    beginPath() { this._cmd('beginPath'); }
    closePath() { this._cmd('closePath'); }
    moveTo(x, y) { this._cmd('moveTo', x, y); }
    lineTo(x, y) { this._cmd('lineTo', x, y); }
    quadraticCurveTo(a, b, c, d) { this._cmd('quadraticCurveTo', a, b, c, d); }
    bezierCurveTo(a, b, c, d, e, f) { this._cmd('bezierCurveTo', a, b, c, d, e, f); }
    arc(x, y, r, a0, a1, ccw) {
      if (r < 0) throw new RangeError('IndexSizeError');
      this._cmd('arc', x, y, r, a0, a1, ccw ? 1 : 0);
    }
    arcTo(a, b, c, d, r) { this._cmd('arcTo', a, b, c, d, r); }
    ellipse(x, y, rx, ry, rot, a0, a1, ccw) { this._cmd('ellipse', x, y, rx, ry, rot, a0, a1, ccw ? 1 : 0); }
    rect(x, y, w, h) { this._cmd('rect', x, y, w, h); }
    roundRect(x, y, w, h, r) { this._cmd('roundRect', x, y, w, h, Array.isArray(r) ? r[0] || 0 : r || 0); }

    // fill([path], [rule]), stroke([path]), clip([path], [rule])
    _pathArgs(args) {
      const out = [];
      if (args[0] instanceof Path2D) out.push('path:' + args.shift()._cmds);
      if (typeof args[0] === 'string') out.push(args[0]);
      return out;
    }
    fill(...args) { this._cmd('fill', ...this._pathArgs(args)); }
    stroke(...args) { this._cmd('stroke', ...this._pathArgs(args)); }
    clip(...args) { this._cmd('clip', ...this._pathArgs(args)); }
    isPointInPath(...args) {
      const nums = args.filter((a) => typeof a === 'number');
      const rest = this._pathArgs(args.filter((a) => typeof a !== 'number'));
      return this._cmd('isPointInPath', ...rest, ...nums) === '1';
    }
    isPointInStroke() { return false; }

    fillText(text, x, y, maxWidth) { this._cmd('fillText', text, x, y, maxWidth === undefined ? '' : maxWidth); }
    strokeText(text, x, y, maxWidth) { this._cmd('strokeText', text, x, y, maxWidth === undefined ? '' : maxWidth); }
    measureText(text) {
      const [width, ascent, descent] = String(this._cmd('measureText', text) || '0,0,0').split(',').map(Number);
      return {
        width,
        actualBoundingBoxLeft: 0,
        actualBoundingBoxRight: width,
        actualBoundingBoxAscent: ascent,
        actualBoundingBoxDescent: descent,
        fontBoundingBoxAscent: ascent,
        fontBoundingBoxDescent: descent,
        emHeightAscent: ascent,
        emHeightDescent: descent,
        alphabeticBaseline: 0,
        hangingBaseline: ascent * 0.8,
        ideographicBaseline: -descent,
      };
    }

    drawImage(image, ...n) {
      if (!image) throw new TypeError('drawImage: no image');
      if (image.__h === undefined) return;
      this._cmd('drawImage', image.__h, ...n);
    }

    createImageData(w, h) {
      if (typeof w === 'object') ({ width: w, height: h } = w);
      return new ImageData(w, h);
    }
    getImageData(x, y, w, h) {
      if (!w || !h) throw new RangeError('IndexSizeError');
      const data = atob(this._cmd('getImageData', x, y, w, h) || '');
      const img = new ImageData(Math.abs(w), Math.abs(h));
      for (let i = 0; i < data.length; i++) img.data[i] = data.charCodeAt(i);
      return img;
    }
    putImageData(img, x, y) {
      let s = '';
      for (let i = 0; i < img.data.length; i++) s += String.fromCharCode(img.data[i]);
      this._cmd('putImageData', x, y, img.width, btoa(s));
    }
  }
  // State properties: kept here, sent to the browser when set.
  for (const name of ['fillStyle', 'strokeStyle', 'lineWidth', 'lineCap', 'lineJoin', 'miterLimit',
    'lineDashOffset', 'globalAlpha', 'globalCompositeOperation', 'font', 'textAlign', 'textBaseline',
    'direction', 'shadowBlur', 'shadowColor', 'shadowOffsetX', 'shadowOffsetY', 'imageSmoothingEnabled',
    'imageSmoothingQuality', 'filter', 'letterSpacing', 'wordSpacing', 'fontKerning']) {
    Object.defineProperty(CanvasRenderingContext2D.prototype, name, {
      configurable: true,
      get() { return this._s[name]; },
      set(v) {
        let sent = v;
        if (v instanceof CanvasGradient) sent = 'g:' + v._id;
        else if (v instanceof CanvasPattern) sent = 'p:' + v._id;
        this._s[name] = v;
        this._cmd(name, sent);
      },
    });
  }

  class ImageData {
    constructor(a, b, c) {
      if (a instanceof Uint8ClampedArray) {
        this.data = a;
        this.width = b;
        this.height = c || a.length / 4 / b;
      } else {
        this.width = a;
        this.height = b;
        this.data = new Uint8ClampedArray(a * b * 4);
      }
      this.colorSpace = 'srgb';
    }
  }

  // <canvas> element: getContext('2d'); toDataURL; toBlob.
  const contexts = new WeakMap();
  Object.assign(Element.prototype, {
    getContext(kind) {
      if (this.localName !== 'canvas' || String(kind).toLowerCase() !== '2d') return null;
      let ctx = contexts.get(this);
      if (!ctx) contexts.set(this, (ctx = new CanvasRenderingContext2D(this)));
      return ctx;
    },
    toDataURL() {
      if (this.localName !== 'canvas') return undefined;
      return rb.canvas(this.__h, 'toDataURL') || 'data:,';
    },
    toBlob(cb) {
      const url = this.toDataURL();
      const bin = atob(url.split(',')[1] || '');
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      addTimer(() => cb(new Blob([bytes], { type: 'image/png' })), 0, [], false);
    },
    transferControlToOffscreen() { throw new Error('NotSupportedError'); },
  });

  // ----- <video> / <audio> ---------------------------------------------------
  // HTMLMediaElement: the browser plays; state is read back with `info`.

  function mediaInfo(el) {
    const v = String(rb.media(el.__h, 'info') || '').split(',');
    return {
      paused: v[0] !== '0', ended: v[1] === '1', currentTime: Number(v[2]) || 0,
      duration: v[3] === undefined ? NaN : Number(v[3]), readyState: Number(v[4]) || 0,
      volume: v[5] === undefined ? 1 : Number(v[5]), muted: v[6] === '1',
      videoWidth: Number(v[7]) || 0, videoHeight: Number(v[8]) || 0,
      error: Number(v[9]) || 0, src: v.slice(10).join(','),
    };
  }
  class TimeRanges {
    constructor(ranges) { this._r = ranges; }
    get length() { return this._r.length; }
    start(i) { return this._r[i][0]; }
    end(i) { return this._r[i][1]; }
  }
  const mediaMethods = {
    play() {
      const r = rb.media(this.__h, 'play');
      if (r === 'ok') return Promise.resolve();
      const e = new Error(r === 'NotAllowedError'
        ? "play() failed because the user didn't interact with the document first."
        : 'The element has no supported sources.');
      e.name = r || 'NotSupportedError';
      return Promise.reject(e);
    },
    pause() { rb.media(this.__h, 'pause'); },
    load() { rb.media(this.__h, 'load'); },
    canPlayType(type) { return rb.media(this.__h, 'canPlayType' + SEP + String(type)) || ''; },
    fastSeek(t) { this.currentTime = t; },
    addTextTrack() { return { mode: 'disabled', cues: [], addCue() {} }; },
    captureStream() { throw new Error('NotSupportedError'); },
  };
  for (const [name, fn] of Object.entries(mediaMethods)) {
    Object.defineProperty(Element.prototype, name, {
      configurable: true,
      writable: true,
      value: function (...a) {
        if (this.localName !== 'video' && this.localName !== 'audio') {
          throw new TypeError(name + ' is not a function');
        }
        return fn.apply(this, a);
      },
    });
  }
  const mediaProps = {
    paused: (i) => i.paused,
    ended: (i) => i.ended,
    duration: (i) => i.duration,
    readyState: (i) => i.readyState,
    videoWidth: (i) => i.videoWidth,
    videoHeight: (i) => i.videoHeight,
    currentSrc: (i) => i.src,
    error: (i) => (i.error ? { code: i.error, message: '' } : null),
    networkState: (i) => (i.error ? 3 : i.src ? 1 : 0),
    seeking: () => false,
    playbackRate: () => 1,
    defaultPlaybackRate: () => 1,
    buffered: (i) => new TimeRanges(isFinite(i.duration) ? [[0, i.duration]] : []),
    seekable: (i) => new TimeRanges(isFinite(i.duration) ? [[0, i.duration]] : []),
    played: () => new TimeRanges([]),
    textTracks: () => [],
    audioTracks: () => [],
    videoTracks: () => [],
  };
  for (const [name, get] of Object.entries(mediaProps)) {
    Object.defineProperty(Element.prototype, name, {
      configurable: true,
      get() { return this.localName === 'video' || this.localName === 'audio' ? get(mediaInfo(this)) : undefined; },
      set() {},
    });
  }
  Object.defineProperty(Element.prototype, 'currentTime', {
    configurable: true,
    get() { return mediaInfo(this).currentTime; },
    set(v) { rb.media(this.__h, 'seek' + SEP + Number(v)); },
  });
  Object.defineProperty(Element.prototype, 'volume', {
    configurable: true,
    get() { return mediaInfo(this).volume; },
    set(v) {
      if (!(v >= 0 && v <= 1)) throw new RangeError('IndexSizeError');
      rb.media(this.__h, 'volume' + SEP + Number(v));
    },
  });
  Object.defineProperty(Element.prototype, 'muted', {
    configurable: true,
    get() { return mediaInfo(this).muted; },
    set(v) { rb.media(this.__h, 'muted' + SEP + (v ? 1 : 0)); },
  });
  for (const [prop, attr] of [['controls'], ['loop'], ['autoplay'], ['defaultMuted', 'muted'], ['playsInline', 'playsinline']]) {
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() { return this.hasAttribute(attr || prop); },
      set(v) {
        this.toggleAttribute(attr || prop, !!v);
        if (prop === 'loop') rb.media(this.__h, 'loop' + SEP + (v ? 1 : 0));
      },
    });
  }
  Object.defineProperty(Element.prototype, 'poster', {
    configurable: true,
    get() { return absolute(this.getAttribute('poster')); },
    set(v) { this.setAttribute('poster', v); },
  });

  // A document that is not shown (DOMParser, createHTMLDocument): its
  // elements live detached in the page's DOM; scripts in it never run.
  function detachedDocument(markup) {
    const html = document.createElement('html');
    html.innerHTML = String(markup);
    let head = html.querySelector('head');
    if (!head) html.prepend(head = document.createElement('head'));
    let body = html.querySelector('body');
    if (!body) {
      body = document.createElement('body');
      for (const n of [...html.childNodes]) if (n !== head) body.append(n);
      html.append(body);
    }
    const doc = {
      nodeType: 9,
      nodeName: '#document',
      documentElement: html,
      head,
      body,
      get title() { const t = html.querySelector('title'); return t ? t.textContent : ''; },
      createElement: (t) => document.createElement(t),
      createElementNS: (ns, t) => document.createElementNS(ns, t),
      createTextNode: (t) => document.createTextNode(t),
      createComment: (t) => document.createComment(t),
      createDocumentFragment: () => document.createDocumentFragment(),
      importNode: (n, deep) => n.cloneNode(deep),
      adoptNode: (n) => n,
      querySelector: (s) => html.querySelector(s),
      querySelectorAll: (s) => html.querySelectorAll(s),
      getElementById: (id) => html.querySelector('[id="' + id + '"]'),
      getElementsByTagName: (t) => html.getElementsByTagName(t),
      getElementsByClassName: (c) => html.getElementsByClassName(c),
      addEventListener() {},
      removeEventListener() {},
    };
    doc.implementation = implementation;
    return doc;
  }

  class DOMParser {
    parseFromString(text) {
      return detachedDocument(text);
    }
  }

  const implementation = {
    createHTMLDocument(title) {
      const doc = detachedDocument('');
      if (title !== undefined) {
        const t = document.createElement('title');
        t.textContent = String(title);
        doc.head.append(t);
      }
      return doc;
    },
    createDocument() { return detachedDocument(''); },
    createDocumentType(name) { return { nodeType: 10, name: String(name) }; },
    hasFeature() { return true; },
  };

  // Intl without locale data: enough for formatting numbers and dates.
  const Intl = {
    NumberFormat: class {
      constructor(locale, opts) { this.opts = opts || {}; }
      format(n) {
        const d = this.opts.maximumFractionDigits;
        const s = d !== undefined ? Number(n).toFixed(d).replace(/\.?0+$/, '') : String(n);
        const [i, f] = s.split('.');
        return i.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (f ? '.' + f : '');
      }
      formatToParts(n) { return [{ type: 'integer', value: this.format(n) }]; }
      resolvedOptions() { return { locale: 'en-US', ...this.opts }; }
    },
    DateTimeFormat: class {
      constructor(locale, opts) { this.opts = opts || {}; }
      format(d) {
        d = d === undefined ? new Date() : new Date(d);
        const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
        return months[d.getMonth()] + ' ' + d.getDate() + ', ' + d.getFullYear();
      }
      formatToParts(d) { return [{ type: 'literal', value: this.format(d) }]; }
      resolvedOptions() { return { locale: 'en-US', timeZone: 'UTC', ...this.opts }; }
    },
    RelativeTimeFormat: class {
      format(v, unit) { const n = Math.abs(v); const u = n === 1 ? unit.replace(/s$/, '') : unit.replace(/s?$/, 's'); return v < 0 ? n + ' ' + u + ' ago' : 'in ' + n + ' ' + u; }
    },
    PluralRules: class { select(n) { return n === 1 ? 'one' : 'other'; } },
    Collator: class { compare(a, b) { return a < b ? -1 : a > b ? 1 : 0; } },
    ListFormat: class { format(l) { return [...l].join(', '); } },
    Segmenter: class { segment(s) { return [...String(s)].map((segment, index) => ({ segment, index })); } },
    getCanonicalLocales: (l) => (l ? [].concat(l) : []),
  };

  class PerformanceObserver {
    observe() {}
    disconnect() {}
  }
  PerformanceObserver.supportedEntryTypes = [];

  // matchMedia: evaluated by the browser against the current viewport each
  // time `matches` is read (change events are not sent yet).
  function matchMedia(query) {
    const media = String(query);
    return {
      get matches() { return rb.mediaMatches(media); },
      media,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() { return true; },
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
  for (const name of ['load', 'domcontentloaded', 'keydown', 'click', 'message']) handlerProperty(global, name);

  // window.postMessage: a `message` event at this window, later. (There
  // are no other windows to talk to: frames run no scripts.)
  global.postMessage = function (data) {
    const copy = structuredClone(data);
    addTimer(() => {
      const e = new Event('message');
      e.data = copy;
      e.origin = location.origin;
      e.source = global;
      e.ports = [];
      document.dispatchEvent(e);
    }, 0, [], false);
  };

  // Properties reflecting attributes: strings, then booleans.
  // <template>.content: a fragment with the parsed contents (moved there
  // on first use); `content` of other elements is the attribute (<meta>).
  const templateContents = new WeakMap();
  Object.defineProperty(Element.prototype, 'content', {
    configurable: true,
    get() {
      if (this.localName !== 'template' || this instanceof DocumentFragment) {
        return this.getAttribute('content') || '';
      }
      let f = templateContents.get(this);
      if (!f) {
        f = document.createDocumentFragment();
        for (const c of [...this.childNodes]) f.appendChild(c);
        templateContents.set(this, f);
      }
      return f;
    },
    set(v) { this.setAttribute('content', v); },
  });
  for (const [prop, attr] of [['rel'], ['as'], ['media'], ['crossOrigin', 'crossorigin'], ['charset'],
    ['integrity'], ['alt'], ['target'], ['lang'], ['dir'], ['placeholder'],
    ['httpEquiv', 'http-equiv'], ['htmlFor', 'for'], ['download'], ['referrerPolicy', 'referrerpolicy'],
    ['loading'], ['srcset'], ['sizes'], ['action'], ['method'], ['nonce'], ['role'], ['autocomplete'],
    ['decoding'], ['fetchPriority', 'fetchpriority'], ['enctype'], ['accept'], ['pattern'], ['label']]) {
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() { return this.getAttribute(attr || prop) || ''; },
      set(v) { if (v === null) this.removeAttribute(attr || prop); else this.setAttribute(attr || prop, v); },
    });
  }
  for (const [prop, attr] of [['async'], ['defer'], ['noModule', 'nomodule'], ['required'], ['readOnly', 'readonly'],
    ['multiple'], ['autofocus'], ['open'], ['selected'], ['inert'], ['noValidate', 'novalidate']]) {
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() { return this.hasAttribute(attr || prop); },
      set(v) { this.toggleAttribute(attr || prop, !!v); },
    });
  }
  for (const [prop, attr, initial] of [['width', 'width', 0], ['height', 'height', 0], ['tabIndex', 'tabindex', -1],
    ['colSpan', 'colspan', 1], ['rowSpan', 'rowspan', 1], ['maxLength', 'maxlength', -1]]) {
    if (Object.getOwnPropertyDescriptor(Element.prototype, prop)) continue;
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() {
        const v = parseInt(this.getAttribute(attr), 10);
        if (!isNaN(v)) return v;
        // A canvas is 300 × 150 by default; an image reports its shown size.
        if (this.localName === 'canvas' && (prop === 'width' || prop === 'height')) return prop === 'width' ? 300 : 150;
        if (this.localName === 'img' && (prop === 'width' || prop === 'height')) {
          return Math.round(rb.rect(this.__h, prop === 'width' ? 2 : 3));
        }
        return initial;
      },
      set(v) { this.setAttribute(attr, String(v)); },
    });
  }

  // `element.onclick = …` and friends.
  for (const name of ['load', 'error', 'click', 'dblclick', 'contextmenu', 'input', 'change', 'submit',
    'reset', 'keydown', 'keyup', 'keypress', 'focus', 'blur', 'mousedown', 'mouseup', 'mousemove',
    'mouseover', 'mouseout', 'mouseenter', 'mouseleave', 'wheel', 'scroll', 'pointerdown', 'pointerup',
    'animationend', 'transitionend', 'play', 'pause', 'playing', 'ended', 'timeupdate',
    'loadedmetadata', 'loadeddata', 'canplay', 'canplaythrough', 'durationchange', 'volumechange',
    'seeking', 'seeked', 'progress', 'waiting', 'loadstart', 'emptied', 'stalled', 'suspend']) {
    Object.defineProperty(Element.prototype, 'on' + name, {
      configurable: true,
      enumerable: true,
      get() { return (this.__on && this.__on[name] && this.__on[name].fn) || null; },
      set(fn) {
        const on = this.__on || (this.__on = {});
        if (on[name]) this.removeEventListener(name, on[name].listener);
        on[name] = null;
        if (typeof fn !== 'function') return;
        const listener = function (event) {
          if (fn.call(this, event) === false) event.preventDefault();
        };
        this.addEventListener(name, listener);
        on[name] = { fn, listener };
      },
    });
  }

  // document.currentScript, set by the browser around each script.
  let currentScript = null;
  let currentInserted = false;
  // document.write from a script the parser found: the markup goes right
  // after that <script> when it ends (so markup split over several writes
  // stays whole). Written scripts run, unlike innerHTML ones.
  let written = '';
  global.__rb_current = function (h, inserted) {
    if (h < 0 && currentScript && written) {
      const html = written;
      written = '';
      const holder = document.createElement('div');
      holder.innerHTML = html;
      for (const old of holder.querySelectorAll('script')) {
        const s = document.createElement('script');
        for (const name of old.getAttributeNames()) s.setAttribute(name, old.getAttribute(name));
        s.textContent = old.textContent;
        old.replaceWith(s);
      }
      currentScript.after(...holder.childNodes);
    }
    currentScript = h < 0 ? null : wrap(h);
    currentInserted = !!inserted;
  };
  function documentWrite(text) {
    if (currentScript && !currentInserted) {
      written += text;
    } else {
      console.warn("Failed to execute 'write' on 'Document': It isn't possible to write into a " +
        'document from an asynchronously-loaded external script unless it is explicitly opened.');
    }
  }

  // window.length: the number of child frames (window.frames is window).
  Object.defineProperty(global, 'length', {
    configurable: true,
    get: () => rb.queryAll('iframe').length,
  });
  for (const [name, which] of [['innerWidth', 2], ['innerHeight', 3], ['outerWidth', 2], ['outerHeight', 3]]) {
    Object.defineProperty(global, name, { configurable: true, get: () => rb.rect(0, which) });
  }
  Object.assign(global, {
    window: global,
    self: global,
    frames: global,
    top: global,
    parent: global,
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
    URL,
    customElements,
    TextEncoder,
    TextDecoder,
    btoa,
    atob,
    crypto,
    structuredClone,
    AbortController,
    AbortSignal,
    Worker,
    SharedWorker,
    MessageChannel,
    MessagePort,
    BroadcastChannel,
    Headers,
    Request,
    Response,
    fetch,
    XMLHttpRequest,
    FormData,
    Blob,
    DOMParser,
    Intl: typeof global.Intl === 'object' ? global.Intl : Intl,
    PerformanceObserver,
    HTMLInputElement: Element,
    HTMLAnchorElement: Element,
    HTMLButtonElement: Element,
    HTMLFormElement: Element,
    HTMLImageElement: Element,
    HTMLCanvasElement: Element,
    HTMLMediaElement: Element,
    HTMLVideoElement: Element,
    HTMLAudioElement: Element,
    HTMLSourceElement: Element,
    TimeRanges,
    Audio: function Audio(src) {
      const a = document.createElement('audio');
      if (src !== undefined) a.src = src;
      return a;
    },
    CanvasRenderingContext2D,
    CanvasGradient,
    CanvasPattern,
    Path2D,
    ImageData,
    HTMLDivElement: Element,
    HTMLTemplateElement: Element,
    HTMLScriptElement: Element,
    HTMLSelectElement: Element,
    HTMLTextAreaElement: Element,
    SVGElement: Element,
    ShadowRoot: DocumentFragment,
    Image: function Image(w, h) { const i = document.createElement('img'); if (w) i.width = w; if (h) i.height = h; return i; },
    Option: function Option(text, value) { const o = document.createElement('option'); o.textContent = text || ''; if (value !== undefined) o.value = value; return o; },
    requestIdleCallback: (fn) => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 50 }), 1, [], false),
    cancelIdleCallback: clearTimer,
    reportError: (e) => reportError(e),
    getSelection: () => ({ rangeCount: 0, removeAllRanges() {}, addRange() {}, toString: () => '' }),
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
    getComputedStyle: (el) => computedStyle(el),
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, args, false),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, args, true),
    clearTimeout: clearTimer,
    clearInterval: clearTimer,
    requestAnimationFrame: (fn) => addTimer(() => fn(rb.now()), 16, [], false),
    cancelAnimationFrame: clearTimer,
    queueMicrotask: (fn) => Promise.resolve().then(fn),
    alert: (msg) => rb.log('alert: ' + String(msg)),
    performance: {
      now: () => rb.now(),
      timeOrigin: 0,
      mark() {},
      measure() {},
      clearMarks() {},
      clearMeasures() {},
      getEntriesByType: () => [],
      getEntriesByName: () => [],
      getEntries: () => [],
    },
    addEventListener: (type, fn, opts) => document.addEventListener(type, fn, opts),
    removeEventListener: (type, fn) => document.removeEventListener(type, fn),
  });
})(__rb);
