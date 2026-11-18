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
    // DOM: setting nodeValue replaces a text node's data; on other nodes it does nothing
    set nodeValue(v) { if (this.nodeType === 3) this.textContent = v === null ? '' : String(v); }

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
    // The browser moves focus (firing blur / focus) once the script returns.
    focus() { rb.formAction(this.__h, 3); }
    blur() { rb.formAction(this.__h, 4); }
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
    click() {
      if (this.hasAttribute('disabled') && /^(input|button|select|textarea)$/.test(this.localName)) return;
      const e = new eventInterfaces.MouseEvent('click', { bubbles: true, cancelable: true, composed: true, detail: 1 });
      dispatchSynthetic(this, 'click', e);
      // The activation behavior (a link, a file input's dialog, a submit
      // button) is the browser's.
      if (!e.defaultPrevented) rb.formAction(this.__h, 2);
    }
    // <input type=file>: the chosen files (null for other elements).
    get files() { return this.localName === 'input' ? inputFiles(this) : undefined; }
    set files(v) {}
    showPicker() {
      if (this.localName === 'input' && this.type === 'file') rb.formAction(this.__h, 2);
      else if (this.localName !== 'input' && this.localName !== 'select') throw new DOMException('showPicker() needs an input', 'NotSupportedError');
    }
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
    // DOM §4.5: quirks mode is BackCompat, otherwise CSS1Compat.
    get compatMode() { return rb.quirks() ? 'BackCompat' : 'CSS1Compat'; }
    getElementById(id) {
      const v = String(id).replace(/["\\]/g, '\\$&');
      return wrap(rb.query('[id="' + v + '"]'));
    }
    get forms() { return this.querySelectorAll('form'); }
    get images() { return this.querySelectorAll('img'); }
    get links() { return this.querySelectorAll('a[href], area[href]'); }
    get scripts() { return this.querySelectorAll('script'); }
    get activeElement() { const h = rb.focused(); return h >= 0 ? wrap(h) : this.body; }
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
      if (arguments.length === 0) throw new TypeError("Failed to construct 'Event': 1 argument required");
      this.type = String(type);
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.composed = !!(init && init.composed);
      this.defaultPrevented = false;
      this.isTrusted = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this.timeStamp = rb.now();
    }
    get srcElement() { return this.target; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
    get cancelBubble() { return !!this._stopped; }
    set cancelBubble(v) { if (v) this.stopPropagation(); }
    preventDefault() { if (this.cancelable !== false) this.defaultPrevented = true; rb.preventDefault(); }
    initEvent(type, bubbles, cancelable) { this.type = String(type); this.bubbles = !!bubbles; this.cancelable = !!cancelable; }
    stopPropagation() { this._stopped = true; rb.stopPropagation(); }
    stopImmediatePropagation() { this._stopped = true; rb.stopPropagation(); }
    composedPath() {
      const path = [];
      for (let n = this.target; n; n = n.parentNode) path.push(n);
      if (path.length) path.push(global);
      return path;
    }
  }
  Object.assign(Event, { NONE: 0, CAPTURING_PHASE: 1, AT_TARGET: 2, BUBBLING_PHASE: 3 });

  // Event interfaces with their init dictionaries (UI Events, HTML):
  // [name, parent, { member: default }].
  const eventInterfaces = {};
  const keyMods = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
  for (const [name, parent, fields] of [
    ['UIEvent', 'Event', { view: null, detail: 0, which: 0 }],
    ['FocusEvent', 'UIEvent', { relatedTarget: null }],
    ['MouseEvent', 'UIEvent', { screenX: 0, screenY: 0, clientX: 0, clientY: 0, movementX: 0, movementY: 0, button: 0, buttons: 0, relatedTarget: null, ...keyMods }],
    ['PointerEvent', 'MouseEvent', { pointerId: 0, width: 1, height: 1, pressure: 0, tangentialPressure: 0, tiltX: 0, tiltY: 0, twist: 0, pointerType: '', isPrimary: false }],
    ['WheelEvent', 'MouseEvent', { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 }],
    ['DragEvent', 'MouseEvent', { dataTransfer: null }],
    ['KeyboardEvent', 'UIEvent', { key: '', code: '', location: 0, repeat: false, isComposing: false, charCode: 0, keyCode: 0, ...keyMods }],
    ['InputEvent', 'UIEvent', { data: null, inputType: '', isComposing: false, dataTransfer: null }],
    ['CompositionEvent', 'UIEvent', { data: '' }],
    ['ProgressEvent', 'Event', { lengthComputable: false, loaded: 0, total: 0 }],
    ['ErrorEvent', 'Event', { message: '', filename: '', lineno: 0, colno: 0, error: undefined }],
    ['PopStateEvent', 'Event', { state: null }],
    ['HashChangeEvent', 'Event', { oldURL: '', newURL: '' }],
    ['MessageEvent', 'Event', { data: null, origin: '', lastEventId: '', source: null, ports: [] }],
    ['CloseEvent', 'Event', { wasClean: false, code: 0, reason: '' }],
    ['StorageEvent', 'Event', { key: null, oldValue: null, newValue: null, url: '', storageArea: null }],
    ['SubmitEvent', 'Event', { submitter: null }],
    ['AnimationEvent', 'Event', { animationName: '', elapsedTime: 0, pseudoElement: '' }],
    ['TransitionEvent', 'Event', { propertyName: '', elapsedTime: 0, pseudoElement: '' }],
    ['PageTransitionEvent', 'Event', { persisted: false }],
    ['ClipboardEvent', 'Event', { clipboardData: null }],
    ['TouchEvent', 'UIEvent', { touches: [], targetTouches: [], changedTouches: [], ...keyMods }],
    ['BeforeUnloadEvent', 'Event', {}],
  ]) {
    const Base = parent === 'Event' ? Event : eventInterfaces[parent];
    const cls = { [name]: class extends Base {
      constructor(type, init) {
        super(type, init);
        for (const [k, v] of Object.entries(fields)) this[k] = init && init[k] !== undefined ? init[k] : v;
      }
    } }[name];
    eventInterfaces[name] = cls;
  }
  Object.assign(eventInterfaces.MouseEvent.prototype, {
    get pageX() { return this.clientX + (global.scrollX || 0); },
    get pageY() { return this.clientY + (global.scrollY || 0); },
    get x() { return this.clientX; },
    get y() { return this.clientY; },
    get offsetX() { return this.clientX - (this.target && this.target.getBoundingClientRect ? this.target.getBoundingClientRect().left : 0); },
    get offsetY() { return this.clientY - (this.target && this.target.getBoundingClientRect ? this.target.getBoundingClientRect().top : 0); },
  });
  for (const name of ['MouseEvent', 'KeyboardEvent']) {
    eventInterfaces[name].prototype.getModifierState = function (k) {
      return { Control: this.ctrlKey, Shift: this.shiftKey, Alt: this.altKey, Meta: this.metaKey }[k] || false;
    };
  }
  Object.assign(eventInterfaces.KeyboardEvent, { DOM_KEY_LOCATION_STANDARD: 0, DOM_KEY_LOCATION_LEFT: 1, DOM_KEY_LOCATION_RIGHT: 2, DOM_KEY_LOCATION_NUMPAD: 3 });
  Object.assign(eventInterfaces.WheelEvent, { DOM_DELTA_PIXEL: 0, DOM_DELTA_LINE: 1, DOM_DELTA_PAGE: 2 });
  // The interface of an event the browser dispatches, by type.
  function eventClass(type) {
    if (/^(click|dblclick|auxclick|contextmenu|mouse)/.test(type)) return eventInterfaces.MouseEvent;
    if (/^pointer/.test(type)) return eventInterfaces.PointerEvent;
    if (/^touch/.test(type)) return eventInterfaces.TouchEvent;
    if (/^key/.test(type)) return eventInterfaces.KeyboardEvent;
    if (/^(focus|blur)/.test(type)) return eventInterfaces.FocusEvent;
    if (type === 'wheel') return eventInterfaces.WheelEvent;
    if (type === 'input' || type === 'beforeinput') return eventInterfaces.InputEvent;
    if (type === 'submit') return eventInterfaces.SubmitEvent;
    return Event;
  }

  function currentEvent() {
    const type = rb.eventString(0);
    // UI events from the browser bubble and can be canceled.
    const e = new (eventClass(type))(type, { bubbles: true, cancelable: true });
    e.isTrusted = true;
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
    if (e instanceof eventInterfaces.KeyboardEvent) e.repeat = rb.eventInt(5) === 1;
    const touch = rb.eventString(2) || '';
    if (e instanceof eventInterfaces.TouchEvent) {
      // "<touches>|<changed>", each "id,x,y,target;…" (Touch Events)
      const [all, changed] = touch.split('|').map((list) => (list ? list.split(';') : []).map((t) => {
        const [id, x, y, target] = t.split(',').map(Number);
        return new Touch({ identifier: id, target: wrap(target), clientX: x, clientY: y });
      }));
      e.touches = touchList(all);
      e.changedTouches = touchList(changed);
      e.targetTouches = touchList(all.filter((t) => t.target === e.target));
      if (changed.length) Object.assign(e, { clientX: changed[0].clientX, clientY: changed[0].clientY });
    }
    if (e instanceof eventInterfaces.WheelEvent) Object.assign(e, { deltaX: rb.eventPos(2), deltaY: rb.eventPos(3), deltaMode: 0 });
    if (!(e instanceof eventInterfaces.MouseEvent)) {
      e.pageX = e.clientX;
      e.pageY = e.clientY;
    } else {
      // legacy `which` of mouse events is the button + 1 (UI Events §8.1)
      e.which = e.button + 1;
      // which buttons are held (the browser reports presses and releases)
      const bit = [1, 4, 2][e.button] || 0;
      if (type === 'mousedown' || type === 'pointerdown') pressedButtons |= bit;
      if (type === 'mouseup' || type === 'pointerup') pressedButtons &= ~bit;
      e.buttons = pressedButtons;
      const finger = /^touch (\d+) (\d)/.exec(touch);
      if (finger) {
        // a finger: pressed from pointerdown to pointerup, its own pointerId
        const up = /(up|cancel)$/.test(type);
        e.buttons = up ? 0 : 1;
        if (e instanceof eventInterfaces.PointerEvent) Object.assign(e, { pointerId: 2 + Number(finger[1]), pointerType: 'touch', isPrimary: finger[2] === '1', pressure: up ? 0 : 0.5, width: 20, height: 20 });
      } else if (e instanceof eventInterfaces.PointerEvent) Object.assign(e, { pointerId: 1, pointerType: 'mouse', isPrimary: true, pressure: pressedButtons ? 0.5 : 0 });
    }
    return e;
  }
  let pressedButtons = 0;

  // Touch Events: a finger on the screen, and the lists of them
  class Touch {
    constructor(init) {
      init = init || {};
      this.identifier = init.identifier ?? 0;
      this.target = init.target ?? null;
      this.clientX = init.clientX ?? 0;
      this.clientY = init.clientY ?? 0;
      this.screenX = init.screenX ?? this.clientX;
      this.screenY = init.screenY ?? this.clientY;
      this.pageX = init.pageX ?? this.clientX + (global.scrollX || 0);
      this.pageY = init.pageY ?? this.clientY + (global.scrollY || 0);
      this.radiusX = init.radiusX ?? 10;
      this.radiusY = init.radiusY ?? 10;
      this.rotationAngle = init.rotationAngle ?? 0;
      this.force = init.force ?? 0.5;
    }
  }
  function touchList(list) {
    const l = list.slice();
    l.item = (i) => l[i] ?? null;
    return l;
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
      if (a instanceof Error) return a.name + ': ' + a.message + (a.stack ? '\n' + a.stack : '');
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
    // A new fragment of this document (no reload; the page hears hashchange).
    set hash(v) {
      let h = String(v);
      if (h && h[0] !== '#') h = '#' + h;
      rb.navigate(rb.location().split('#')[0] + h);
    },
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

  // HTMLHyperlinkElementUtils (HTML §4.6.6): the parts of an <a> or
  // <area>'s URL. Other elements keep them as ordinary properties.
  {
    const isLink = (el) => el.localName === 'a' || el.localName === 'area';
    const linkUrl = (el) => {
      const v = el.getAttribute('href');
      if (v === null) return null;
      try { return new URL(v, document.baseURI); } catch (_) { return null; }
    };
    const setters = {
      protocol: (u, v) => { u.protocol = v.replace(/:.*$/, '').toLowerCase() + ':'; },
      username: (u, v) => { u.username = v; },
      password: (u, v) => { u.password = v; },
      host: (u, v) => { u.host = v.toLowerCase(); },
      hostname: (u, v) => { u.host = v.toLowerCase() + (u.port ? ':' + u.port : ''); },
      port: (u, v) => { u.host = u.hostname + (v === '' ? '' : ':' + v); },
      pathname: (u, v) => { u.pathname = v.startsWith('/') ? v : '/' + v; },
      search: (u, v) => { u.search = v === '' ? '' : v.startsWith('?') ? v : '?' + v; },
      hash: (u, v) => { u.hash = v === '' ? '' : v.startsWith('#') ? v : '#' + v; },
    };
    for (const part of [...Object.keys(setters), 'origin']) {
      Object.defineProperty(Element.prototype, part, {
        configurable: true,
        get() {
          if (!isLink(this)) return undefined;
          const u = linkUrl(this);
          if (u) return u[part];
          return part === 'protocol' ? ':' : '';
        },
        set(v) {
          if (!isLink(this)) {
            Object.defineProperty(this, part, { value: v, writable: true, enumerable: true, configurable: true });
            return;
          }
          const u = linkUrl(this);
          if (!u || !setters[part]) return;
          setters[part](u, String(v));
          this.setAttribute('href', u.href);
        },
      });
    }
  }

  // CSSOM (CSS Object Model): the sheets of <style> and <link> elements,
  // their rules, insertRule / deleteRule. Changes go to the browser as the
  // sheet's new CSS (rb.setSheet); the element's text stays as it was.
  {
    // Top-level pieces of CSS: rules (or, in a declaration block,
    // declarations and nested rules), without comments.
    function splitRules(css) {
      const out = [];
      let depth = 0, parens = 0, start = 0, quote = '', text = '';
      for (let i = 0; i < css.length; i++) {
        const c = css[i];
        if (quote) {
          if (c === '\\') i++;
          else if (c === quote) quote = '';
          continue;
        }
        if (c === '/' && css[i + 1] === '*') {
          text += css.slice(start, i);
          const end = css.indexOf('*/', i + 2);
          i = end < 0 ? css.length : end + 1;
          start = i + 1;
          continue;
        }
        if (c === '"' || c === "'") quote = c;
        else if (c === '(') parens++;
        else if (c === ')') parens = Math.max(0, parens - 1);
        else if (c === '{') depth++;
        else if (c === '}' && depth > 0 && --depth === 0) {
          out.push(text + css.slice(start, i + 1));
          text = '';
          start = i + 1;
        } else if (c === ';' && depth === 0 && parens === 0) {
          out.push(text + css.slice(start, i + 1));
          text = '';
          start = i + 1;
        }
      }
      out.push(text + css.slice(start));
      return out.map((r) => r.trim()).filter((r) => r && r !== ';');
    }

    function ruleList(rules) {
      Object.defineProperty(rules, 'item', { value: (i) => rules[i] ?? null });
      return rules;
    }

    function syntaxError(text) {
      return new DOMException("Failed to parse the rule '" + text + "'.", 'SyntaxError');
    }

    class CSSRule {
      constructor(sheet, parent) {
        this.parentStyleSheet = sheet;
        this.parentRule = parent;
      }
      _sync() { if (this.parentStyleSheet) this.parentStyleSheet._sync(); }
    }
    Object.assign(CSSRule, {
      STYLE_RULE: 1, CHARSET_RULE: 2, IMPORT_RULE: 3, MEDIA_RULE: 4, FONT_FACE_RULE: 5,
      PAGE_RULE: 6, KEYFRAMES_RULE: 7, KEYFRAME_RULE: 8, NAMESPACE_RULE: 10, SUPPORTS_RULE: 12,
    });

    // A rule kept as text (@font-face, @keyframes, @import, …).
    class CSSTextRule extends CSSRule {
      constructor(sheet, parent, text, type) {
        super(sheet, parent);
        this._text = text;
        this.type = type;
      }
      get cssText() { return this._text; }
    }
    class CSSImportRule extends CSSTextRule {
      get href() { return (/^@import\s+(?:url\()?\s*["']?([^"')\s;]*)/i.exec(this._text) || [])[1] || ''; }
    }
    class CSSFontFaceRule extends CSSTextRule {}
    class CSSKeyframesRule extends CSSTextRule {
      get name() { return (/^@[-\w]*keyframes\s+([^\s{]+)/i.exec(this._text) || [])[1] || ''; }
    }
    const textRules = {
      'import': [CSSImportRule, 3], 'charset': [CSSTextRule, 2], 'font-face': [CSSFontFaceRule, 5],
      'page': [CSSTextRule, 6], 'keyframes': [CSSKeyframesRule, 7], '-webkit-keyframes': [CSSKeyframesRule, 7],
      'namespace': [CSSTextRule, 10],
    };

    const kebab = (p) => p.startsWith('--') ? p : p.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());

    // The declarations of a style rule.
    class CSSRuleStyle {
      constructor(rule, decls) {
        this._rule = rule;
        this._decls = decls;
        return new Proxy(this, {
          get(t, p) {
            if (typeof p !== 'string' || p in t || /^\d+$/.test(p)) return /^\d+$/.test(p) ? t.item(+p) : t[p];
            return t.getPropertyValue(kebab(p));
          },
          set(t, p, v) {
            if (typeof p !== 'string' || p.startsWith('_') || p in CSSRuleStyle.prototype) t[p] = v;
            else t.setProperty(kebab(p), v);
            return true;
          },
        });
      }
      get length() { return this._decls.length; }
      item(i) { return this._decls[i] ? this._decls[i].name : ''; }
      getPropertyValue(name) {
        const d = this._decls.find((d) => d.name === String(name).trim());
        return d ? d.value : '';
      }
      getPropertyPriority(name) {
        const d = this._decls.find((d) => d.name === String(name).trim());
        return d && d.important ? 'important' : '';
      }
      setProperty(name, value, priority = '') {
        name = String(name).trim();
        if (!name.startsWith('--')) name = name.toLowerCase();
        value = value === null || value === undefined ? '' : String(value).trim();
        const i = this._decls.findIndex((d) => d.name === name);
        if (value === '') {
          if (i >= 0) this._decls.splice(i, 1);
        } else {
          const d = { name, value, important: String(priority).toLowerCase() === 'important' };
          if (i >= 0) this._decls[i] = d;
          else this._decls.push(d);
        }
        this._rule._sync();
      }
      removeProperty(name) {
        const old = this.getPropertyValue(name);
        this.setProperty(name, '');
        return old;
      }
      get cssText() {
        return this._decls.map((d) => d.name + ': ' + d.value + (d.important ? ' !important' : '') + ';').join(' ');
      }
      set cssText(v) {
        this._decls = parseDecls(splitRules(String(v)));
        this._rule._sync();
      }
    }

    function parseDecls(parts) {
      const decls = [];
      for (let part of parts) {
        part = part.replace(/;$/, '');
        const colon = part.indexOf(':');
        if (colon < 0) continue;
        let name = part.slice(0, colon).trim();
        if (!name.startsWith('--')) name = name.toLowerCase();
        let value = part.slice(colon + 1).trim();
        const important = /!\s*important$/i.test(value);
        if (important) value = value.replace(/\s*!\s*important$/i, '');
        if (name) decls.push({ name, value, important });
      }
      return decls;
    }

    class CSSStyleRule extends CSSRule {
      constructor(sheet, parent, selector, body) {
        super(sheet, parent);
        this._selector = selector;
        const parts = splitRules(body);
        this._nested = parts.filter((p) => p.includes('{'));
        this._style = new CSSRuleStyle(this, parseDecls(parts.filter((p) => !p.includes('{'))));
      }
      get type() { return 1; }
      get selectorText() { return this._selector; }
      set selectorText(v) { this._selector = String(v).trim(); this._sync(); }
      get style() { return this._style; }
      set style(v) { this._style.cssText = v; }
      get cssText() {
        const body = [this._style.cssText, ...this._nested].filter(Boolean).join(' ');
        return this._selector + ' { ' + (body ? body + ' ' : '') + '}';
      }
    }

    // @media, @supports, @layer, @container … blocks.
    class CSSGroupingRule extends CSSRule {
      constructor(sheet, parent, name, prelude, body) {
        super(sheet, parent);
        this._name = name;
        this._prelude = prelude;
        this._rules = ruleList(splitRules(body).map((t) => parseRule(t, sheet, this)));
      }
      get type() { return 0; }
      get cssRules() { return this._rules; }
      insertRule(rule, index = 0) {
        insertInto(this._rules, rule, index, this.parentStyleSheet, this);
        this._sync();
        return index;
      }
      deleteRule(index) {
        removeFrom(this._rules, index);
        this._sync();
      }
      get cssText() {
        const head = '@' + this._name + (this._prelude ? ' ' + this._prelude : '');
        const body = this._rules.map((r) => r.cssText).join(' ');
        return head + ' { ' + (body ? body + ' ' : '') + '}';
      }
    }
    class CSSConditionRule extends CSSGroupingRule {
      get conditionText() { return this._prelude; }
    }
    class CSSMediaRule extends CSSConditionRule {
      get type() { return 4; }
      get media() {
        const rule = this;
        return {
          get mediaText() { return rule._prelude; },
          set mediaText(v) { rule._prelude = String(v).trim(); rule._sync(); },
          get length() { return rule._prelude ? rule._prelude.split(',').length : 0; },
          toString() { return rule._prelude; },
        };
      }
    }
    class CSSSupportsRule extends CSSConditionRule {
      get type() { return 12; }
    }
    const groupingRules = {
      'media': CSSMediaRule, 'supports': CSSSupportsRule, 'container': CSSConditionRule,
      'layer': CSSGroupingRule, 'scope': CSSGroupingRule, 'starting-style': CSSGroupingRule,
      'document': CSSGroupingRule, '-moz-document': CSSGroupingRule,
    };

    function parseRule(text, sheet, parent) {
      const brace = text.indexOf('{');
      if (text[0] === '@') {
        const name = ((/^@([-\w]+)/.exec(text) || [])[1] || '').toLowerCase();
        const prelude = (brace < 0 ? text.replace(/;$/, '') : text.slice(0, brace)).slice(name.length + 1).trim();
        const Group = groupingRules[name];
        if (Group && brace >= 0) {
          return new Group(sheet, parent, name, prelude, text.slice(brace + 1, text.lastIndexOf('}')));
        }
        const [Rule, type] = textRules[name] || [CSSTextRule, 0];
        return new Rule(sheet, parent, text, type);
      }
      if (brace <= 0 || !text.endsWith('}')) throw syntaxError(text);
      return new CSSStyleRule(sheet, parent, text.slice(0, brace).trim(), text.slice(brace + 1, -1));
    }

    function insertInto(rules, text, index, sheet, parent) {
      index = Number(index) >>> 0;
      if (index > rules.length) {
        throw new DOMException('The index provided (' + index + ') is larger than the maximum index (' + rules.length + ').', 'IndexSizeError');
      }
      const pieces = splitRules(String(text));
      if (pieces.length !== 1) throw syntaxError(String(text));
      rules.splice(index, 0, parseRule(pieces[0], sheet, parent));
    }

    function removeFrom(rules, index) {
      index = Number(index) >>> 0;
      if (index >= rules.length) {
        throw new DOMException('The index provided (' + index + ') is outside the range [0, ' + rules.length + ').', 'IndexSizeError');
      }
      rules.splice(index, 1);
    }

    const crossOrigin = (href) => {
      try { return new URL(href, location.href).origin !== location.origin; } catch (_) { return true; }
    };

    class StyleSheet {}
    class CSSStyleSheet extends StyleSheet {
      constructor() {
        super();
        this._owner = null;
        this._text = null;
        this._rules = ruleList([]);
        this.disabled = false;
      }
      get type() { return 'text/css'; }
      get ownerNode() { return this._owner; }
      get ownerRule() { return null; }
      get parentStyleSheet() { return null; }
      get href() { return this._owner && this._owner.localName === 'link' ? this._owner.href : null; }
      get title() { return this._owner ? this._owner.getAttribute('title') : null; }
      get media() {
        const text = (this._owner && this._owner.getAttribute('media')) || '';
        return { mediaText: text, length: text ? text.split(',').length : 0, toString: () => text };
      }
      get cssRules() {
        const owner = this._owner;
        if (owner) {
          if (owner.localName === 'link' && crossOrigin(owner.href)) {
            throw new DOMException("Failed to read the 'cssRules' property from 'CSSStyleSheet': Cannot access rules", 'SecurityError');
          }
          const text = rb.sheetText(owner.__h);
          if (text !== null && text !== this._text) {
            this._text = text;
            this._rules = ruleList(splitRules(text).map((t) => parseRule(t, this, null)));
          }
        }
        return this._rules;
      }
      get rules() { return this.cssRules; }
      insertRule(rule, index = 0) {
        insertInto(this.cssRules, rule, index, this, null);
        this._sync();
        return Number(index) >>> 0;
      }
      deleteRule(index) {
        removeFrom(this.cssRules, index);
        this._sync();
      }
      addRule(selector = 'undefined', style = 'undefined', index) {
        this.insertRule(selector + ' { ' + style + ' }', index === undefined ? this.cssRules.length : index);
        return -1;
      }
      removeRule(index = 0) { this.deleteRule(index); }
      replaceSync(text) {
        // Constructed sheets do not load @import rules.
        const pieces = splitRules(String(text)).filter((t) => !/^@import\b/i.test(t));
        this._rules = ruleList(pieces.map((t) => parseRule(t, this, null)));
        this._sync();
      }
      replace(text) {
        try {
          this.replaceSync(text);
          return Promise.resolve(this);
        } catch (e) {
          return Promise.reject(e);
        }
      }
      _sync() {
        this._text = this._rules.map((r) => r.cssText).join('\n');
        if (this._owner) rb.setSheet(this._owner.__h, this._text);
      }
    }

    // HTMLStyleElement / HTMLLinkElement .sheet: one per element, null
    // until the element is in the document (and a link has loaded).
    const sheets = new Map();
    Object.defineProperty(Element.prototype, 'sheet', {
      configurable: true,
      get() {
        const name = this.localName;
        if (name !== 'style' && name !== 'link') return undefined;
        if (!this.isConnected) return null;
        if (name === 'link' && !/(^|\s)stylesheet(\s|$)/i.test(this.getAttribute('rel') || '')) return null;
        if (rb.sheetText(this.__h) === null) return null;
        let sheet = sheets.get(this.__h);
        if (!sheet) {
          sheet = new CSSStyleSheet();
          sheet._owner = this;
          sheets.set(this.__h, sheet);
        }
        return sheet;
      },
    });
    Object.defineProperty(Document.prototype, 'styleSheets', {
      configurable: true,
      get() {
        const list = [];
        for (const el of this.querySelectorAll('style, link')) {
          const sheet = el.sheet;
          if (sheet) list.push(sheet);
        }
        return ruleList(list);
      },
    });

    Object.assign(global, {
      StyleSheet, CSSStyleSheet, CSSRule, CSSStyleRule, CSSGroupingRule, CSSConditionRule,
      CSSMediaRule, CSSSupportsRule, CSSImportRule, CSSFontFaceRule, CSSKeyframesRule,
    });
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

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s = '') { return rb.encodeUtf8(String(s)); }
    encodeInto(s, dest) {
      const b = rb.encodeUtf8(String(s));
      const n = Math.min(b.length, dest.length);
      // Whole characters only.
      let end = n;
      if (end < b.length) while (end > 0 && (b[end] & 0xc0) === 0x80) end--;
      dest.set(b.subarray(0, end));
      return { read: rb.decodeUtf8(b.subarray(0, end)).length, written: end };
    }
  }
  class TextDecoder {
    constructor(label) { this._label = label === undefined ? 'utf-8' : String(label).trim().toLowerCase(); }
    get encoding() { return this._label === 'utf8' || this._label === 'unicode-1-1-utf-8' ? 'utf-8' : this._label; }
    decode(buf) {
      if (!buf) return '';
      const b = sourceBytes(buf);
      if (!b) throw new TypeError("Failed to execute 'decode' on 'TextDecoder': parameter 1 is not of type 'ArrayBuffer'.");
      return decodeText(b, this._label);
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
    // Signals an abort: `onabort` and `abort` listeners run once.
    _abort(reason) {
      if (this.aborted) return;
      this.aborted = true;
      this.reason = reason === undefined ? new DOMException('signal is aborted without reason', 'AbortError') : reason;
      const e = new Event('abort');
      if (typeof this.onabort === 'function') call(this.onabort, this, [e]);
      this.dispatchEvent(e);
    }
    static timeout(ms) {
      const s = new AbortSignal();
      addTimer(() => s._abort(new DOMException('signal timed out', 'TimeoutError')), ms, [], false);
      return s;
    }
    static abort(reason) { const s = new AbortSignal(); s._abort(reason); return s; }
    static any(signals) {
      const s = new AbortSignal();
      for (const t of signals) {
        if (t.aborted) { s._abort(t.reason); break; }
        t.addEventListener('abort', () => s._abort(t.reason));
      }
      return s;
    }
  }
  // DOMException (WebIDL): an Error with a name and its legacy code.
  const DOM_CODES = { IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20, URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25 };
  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(String(message));
      Object.defineProperty(this, 'name', { value: String(name), configurable: true, writable: true });
      Object.defineProperty(this, 'code', { value: DOM_CODES[name] || 0, configurable: true });
    }
  }
  for (const [n, c] of Object.entries(DOM_CODES)) {
    DOMException[n.replace(/Error$/, '').replace(/([a-z])([A-Z])/g, '$1_$2').toUpperCase() + '_ERR'] = c;
  }

  // Message ports (HTML "Channel messaging"): a message posted on one port
  // of a channel arrives at the other as a `message` event in a later task
  // once that port is enabled (`start()`, or setting `onmessage`).
  class MessagePort extends EventTarget {
    constructor() {
      super();
      Object.defineProperty(this, '_q', { value: [], writable: true });
      this._other = null;
      this._started = false;
      this._closed = false;
      this._onmessage = null;
    }
    get onmessage() { return this._onmessage; }
    set onmessage(f) { this._onmessage = typeof f === 'function' ? f : null; this.start(); }
    postMessage(data) {
      const to = this._other;
      if (this._closed || !to || to._closed) return;
      const copy = structuredClone(data);
      addTimer(() => to._deliver(copy), 0, [], false);
    }
    _deliver(data) {
      if (this._closed) return;
      if (!this._started) { this._q.push(data); return; }
      const e = new Event('message');
      e.data = data;
      e.origin = '';
      e.ports = [];
      e.source = null;
      if (this._onmessage) call(this._onmessage, this, [e]);
      this.dispatchEvent(e);
    }
    start() {
      if (this._started) return;
      this._started = true;
      const q = this._q;
      this._q = [];
      for (const d of q) addTimer(() => this._deliver(d), 0, [], false);
    }
    close() { this._closed = true; }
  }
  // Workers exist but never run (no threads here): enough for feature
  // checks and for pages that use them optionally.
  class Worker extends EventTarget {
    constructor(url) { super(); this.url = String(url); }
    postMessage() {}
    terminate() {}
  }
  class SharedWorker extends EventTarget {
    constructor(url) { super(); this.port = new MessagePort(); }
  }
  class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }
  // BroadcastChannel: messages reach the other channels of the same name
  // (in this page; there are no other same-origin contexts).
  const broadcastChannels = new Set();
  class BroadcastChannel extends EventTarget {
    constructor(name) {
      super();
      this.name = String(name);
      this.onmessage = null;
      this._closed = false;
      broadcastChannels.add(this);
    }
    postMessage(data) {
      if (this._closed) throw new DOMException('BroadcastChannel is closed', 'InvalidStateError');
      const copy = structuredClone(data);
      for (const c of broadcastChannels) {
        if (c === this || c.name !== this.name) continue;
        addTimer(() => {
          if (c._closed) return;
          const e = new Event('message');
          e.data = structuredClone(copy);
          e.origin = location.origin;
          if (typeof c.onmessage === 'function') call(c.onmessage, c, [e]);
          c.dispatchEvent(e);
        }, 0, [], false);
      }
    }
    close() { this._closed = true; broadcastChannels.delete(this); }
  }

  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) { this.signal._abort(reason); }
  }

  // ----- Blob, File, FileList, FileReader (File API) -------------------------
  // Contents are Uint8Arrays; the browser has the chosen files' bytes and
  // those of blob: URLs (for <img>, links and downloads).

  function concatBytes(chunks) {
    let n = 0;
    for (const c of chunks) n += c.length;
    const out = new Uint8Array(n);
    let at = 0;
    for (const c of chunks) { out.set(c, at); at += c.length; }
    return out;
  }
  // A BufferSource's bytes (a view of them), or null.
  function sourceBytes(v) {
    if (v instanceof ArrayBuffer) return new Uint8Array(v);
    if (ArrayBuffer.isView(v)) return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    return null;
  }
  class Blob {
    constructor(parts, opts) {
      if (parts !== undefined && (parts === null || typeof parts !== 'object' || typeof parts[Symbol.iterator] !== 'function')) {
        throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.");
      }
      const chunks = [];
      for (const p of parts || []) {
        if (p instanceof Blob) chunks.push(p._data);
        else chunks.push(sourceBytes(p) || rb.encodeUtf8(String(p)));
      }
      this._data = chunks.length === 1 ? chunks[0].slice() : concatBytes(chunks);
      const t = opts && opts.type !== undefined ? String(opts.type) : '';
      this._type = /^[\x20-\x7e]*$/.test(t) ? t.toLowerCase() : '';
    }
    get size() { return this._data.length; }
    get type() { return this._type; }
    slice(start, end, contentType) {
      const size = this.size;
      const rel = (v, d) => {
        if (v === undefined) return d;
        v = Math.trunc(Number(v)) || 0;
        return v < 0 ? Math.max(size + v, 0) : Math.min(v, size);
      };
      const from = rel(start, 0), to = rel(end, size);
      const b = new Blob([], { type: contentType === undefined ? '' : contentType });
      b._data = this._data.slice(from, Math.max(from, to));
      return b;
    }
    text() { return Promise.resolve(rb.decodeUtf8(this._data)); }
    arrayBuffer() { return Promise.resolve(this._data.slice().buffer); }
    bytes() { return Promise.resolve(this._data.slice()); }
    stream() {
      let done = false;
      const data = this._data;
      return {
        locked: false,
        getReader() {
          return {
            read: () => Promise.resolve(done ? { done: true, value: undefined } : (done = true, { done: false, value: data.slice() })),
            cancel: () => { done = true; return Promise.resolve(); },
            releaseLock() {},
            closed: Promise.resolve(),
          };
        },
      };
    }
    get [Symbol.toStringTag]() { return 'Blob'; }
  }
  class File extends Blob {
    constructor(bits, name, opts) {
      if (arguments.length < 2) throw new TypeError("Failed to construct 'File': 2 arguments required, but only " + arguments.length + ' present.');
      super(bits, opts);
      this._name = String(name);
      this._modified = opts && opts.lastModified !== undefined ? Math.trunc(Number(opts.lastModified)) || 0 : Date.now();
    }
    get name() { return this._name; }
    get lastModified() { return this._modified; }
    get lastModifiedDate() { return new Date(this._modified); }
    get webkitRelativePath() { return ''; }
    get [Symbol.toStringTag]() { return 'File'; }
  }
  class FileList {
    constructor(files) {
      Object.defineProperty(this, '_f', { value: files });
      files.forEach((f, i) => Object.defineProperty(this, i, { value: f, enumerable: true }));
    }
    get length() { return this._f.length; }
    item(i) { return this._f[i] || null; }
    [Symbol.iterator]() { return this._f[Symbol.iterator](); }
    get [Symbol.toStringTag]() { return 'FileList'; }
  }
  // <input type=file>.files: the same FileList until the selection changes.
  const fileLists = new Map(); // handle -> [serial, FileList]
  function inputFiles(el) {
    const info = rb.files(el.__h);
    if (info === null) return null;
    const lines = info.split('\n');
    const serial = lines.shift();
    const cached = fileLists.get(el.__h);
    if (cached && cached[0] === serial) return cached[1];
    const files = lines.map((line, i) => {
      const [name, type, , modified] = line.split('\t');
      const f = new File([], name, { type, lastModified: Number(modified) });
      f._data = rb.fileData(el.__h, i) || new Uint8Array(0);
      return f;
    });
    const list = new FileList(files);
    fileLists.set(el.__h, [serial, list]);
    return list;
  }

  // Single-byte decodings FileReader.readAsText accepts besides UTF-8.
  function decodeText(bytes, label) {
    const enc = String(label || 'utf-8').trim().toLowerCase();
    if (/^(iso-8859-1|latin1|l1|us-ascii|ascii|windows-1252|cp1252)$/.test(enc)) {
      let s = '';
      for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));
      return s;
    }
    return rb.decodeUtf8(bytes);
  }
  function binaryString(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));
    return s;
  }

  // Fires `type` at a non-DOM target: its on<type> handler, then listeners.
  function fireAt(target, type, fields) {
    const e = Object.assign(new eventInterfaces.ProgressEvent(type, fields), { target, currentTarget: target });
    if (typeof target['on' + type] === 'function') call(target['on' + type], target, [e]);
    target.dispatchEvent(e);
  }

  // The element interfaces of HTML (§ of each element) and SVGElement: they
  // share Element's methods, and `x instanceof HTMLIFrameElement` holds for
  // the elements of that interface only.
  const SVG_NS = 'http://www.w3.org/2000/svg';
  const HTML_NS = 'http://www.w3.org/1999/xhtml';
  if (!Object.getOwnPropertyDescriptor(Element.prototype, 'namespaceURI')) {
    // From the tree: inside <svg> (but not inside its <foreignObject>) or <math>.
    Object.defineProperty(Element.prototype, 'namespaceURI', {
      configurable: true,
      get() {
        for (let n = this; n && n.nodeType === 1; n = n.parentNode) {
          const t = n.localName;
          if (t === 'foreignObject' && n !== this) return HTML_NS;
          if (t === 'svg') return SVG_NS;
          if (t === 'math') return 'http://www.w3.org/1998/Math/MathML';
        }
        return HTML_NS;
      },
    });
  }
  const elementInterfaces = {};
  for (const [name, tags] of Object.entries({
    HTMLAnchorElement: 'a', HTMLAreaElement: 'area', HTMLAudioElement: 'audio', HTMLBRElement: 'br', HTMLBaseElement: 'base',
    HTMLBodyElement: 'body', HTMLButtonElement: 'button', HTMLCanvasElement: 'canvas', HTMLDListElement: 'dl', HTMLDataElement: 'data',
    HTMLDataListElement: 'datalist', HTMLDetailsElement: 'details', HTMLDialogElement: 'dialog', HTMLDivElement: 'div',
    HTMLEmbedElement: 'embed', HTMLFieldSetElement: 'fieldset', HTMLFormElement: 'form', HTMLHRElement: 'hr', HTMLHeadElement: 'head',
    HTMLHeadingElement: 'h1 h2 h3 h4 h5 h6', HTMLHtmlElement: 'html', HTMLIFrameElement: 'iframe', HTMLImageElement: 'img',
    HTMLInputElement: 'input', HTMLLIElement: 'li', HTMLLabelElement: 'label', HTMLLegendElement: 'legend', HTMLLinkElement: 'link',
    HTMLMapElement: 'map', HTMLMediaElement: 'audio video', HTMLMenuElement: 'menu', HTMLMetaElement: 'meta', HTMLMeterElement: 'meter',
    HTMLModElement: 'ins del', HTMLOListElement: 'ol', HTMLObjectElement: 'object', HTMLOptGroupElement: 'optgroup',
    HTMLOptionElement: 'option', HTMLOutputElement: 'output', HTMLParagraphElement: 'p', HTMLPictureElement: 'picture',
    HTMLPreElement: 'pre listing xmp', HTMLProgressElement: 'progress', HTMLQuoteElement: 'q blockquote', HTMLScriptElement: 'script',
    HTMLSelectElement: 'select', HTMLSlotElement: 'slot', HTMLSourceElement: 'source', HTMLSpanElement: 'span', HTMLStyleElement: 'style',
    HTMLTableCaptionElement: 'caption', HTMLTableCellElement: 'td th', HTMLTableColElement: 'col colgroup', HTMLTableElement: 'table',
    HTMLTableRowElement: 'tr', HTMLTableSectionElement: 'thead tbody tfoot', HTMLTemplateElement: 'template',
    HTMLTextAreaElement: 'textarea', HTMLTimeElement: 'time', HTMLTitleElement: 'title', HTMLTrackElement: 'track',
    HTMLUListElement: 'ul', HTMLVideoElement: 'video', SVGElement: null,
  })) {
    const list = tags && tags.split(' ');
    const F = { [name]: function () { throw new TypeError('Illegal constructor'); } }[name];
    F.prototype = Element.prototype;
    Object.defineProperty(F, Symbol.hasInstance, {
      value: (x) => x instanceof Element && (list ? x.namespaceURI !== SVG_NS && list.includes(x.localName) : x.namespaceURI === SVG_NS),
    });
    elementInterfaces[name] = F;
  }

  // ----- Web Audio (a subset: oscillators, gain, AudioParam automation) -----
  // The graph renders in the browser (media::webaudio); here each call is a
  // command, fields separated by U+0001.
  const audioCmd = (...f) => rb.audio(f.join('\u0001'));

  class AudioParam {
    constructor(node, name, value, min, max) {
      Object.defineProperties(this, {
        _n: { value: node }, _name: { value: name },
        defaultValue: { value, enumerable: true }, minValue: { value: min, enumerable: true }, maxValue: { value: max, enumerable: true },
      });
      this._value = value;
    }
    _send(op, value, time, tau) {
      const n = this._n;
      audioCmd('param', n.context.__id, n.__id, this._name, op, Number(value), Number(time) || 0, Number(tau) || 0);
      return this;
    }
    get value() {
      const v = audioCmd('param-value', this._n.context.__id, this._n.__id, this._name);
      return Number.isFinite(v) ? v : this._value;
    }
    set value(v) { this._value = Number(v); this._send('value', v); }
    setValueAtTime(v, t) { return this._send('set', v, t); }
    linearRampToValueAtTime(v, t) { return this._send('linear', v, t); }
    exponentialRampToValueAtTime(v, t) {
      if (!Number(v)) throw new RangeError('exponentialRampToValueAtTime: the value must not be 0');
      return this._send('exponential', v, t);
    }
    setTargetAtTime(v, t, tau) { return this._send('target', v, t, tau); }
    cancelScheduledValues(t) { return this._send('cancel', 0, t); }
    cancelAndHoldAtTime(t) { return this._send('hold', 0, t); }
  }

  class AudioNode extends EventTarget {
    constructor(context, id) {
      super();
      Object.defineProperties(this, { context: { value: context, enumerable: true }, __id: { value: id } });
    }
    get numberOfInputs() { return 1; }
    get numberOfOutputs() { return 1; }
    connect(dest) {
      // (connections into an AudioParam are not supported)
      if (dest instanceof AudioNode) audioCmd('connect', this.context.__id, this.__id, dest.__id);
      return dest;
    }
    disconnect(dest) { audioCmd('disconnect', this.context.__id, this.__id, dest instanceof AudioNode ? dest.__id : ''); }
  }
  class AudioDestinationNode extends AudioNode {
    get maxChannelCount() { return 2; }
    get numberOfOutputs() { return 0; }
  }
  class GainNode extends AudioNode {
    constructor(context, options) {
      super(context, audioCmd('gain', context.__id));
      Object.defineProperty(this, 'gain', { value: new AudioParam(this, 'gain', 1, -3.4e38, 3.4e38), enumerable: true });
      if (options && options.gain !== undefined) this.gain.value = options.gain;
    }
  }
  const endedOscillators = new Map(); // `${context}:${id}` -> node, until it ends
  class OscillatorNode extends AudioNode {
    constructor(context, options) {
      super(context, audioCmd('oscillator', context.__id));
      Object.defineProperties(this, {
        frequency: { value: new AudioParam(this, 'frequency', 440, -24000, 24000), enumerable: true },
        detune: { value: new AudioParam(this, 'detune', 0, -153600, 153600), enumerable: true },
      });
      this._type = 'sine';
      this.onended = null;
      const o = options || {};
      if (o.type) this.type = o.type;
      if (o.frequency !== undefined) this.frequency.value = o.frequency;
      if (o.detune !== undefined) this.detune.value = o.detune;
    }
    get numberOfInputs() { return 0; }
    get type() { return this._type; }
    set type(t) {
      t = String(t);
      if (!['sine', 'square', 'sawtooth', 'triangle'].includes(t)) return;
      this._type = t;
      audioCmd('type', this.context.__id, this.__id, t);
    }
    start(when) {
      if (this._started) throw new DOMException('start() was called already', 'InvalidStateError');
      this._started = true;
      audioCmd('start', this.context.__id, this.__id, Number(when) || 0);
      endedOscillators.set(this.context.__id + ':' + this.__id, this);
      watchEnded();
    }
    stop(when) {
      if (!this._started) throw new DOMException('stop() before start()', 'InvalidStateError');
      audioCmd('stop', this.context.__id, this.__id, Number(when) || 0);
    }
    setPeriodicWave() {}
  }
  // `ended` of oscillators: asked while some are playing.
  let endedTimer = 0;
  function watchEnded() {
    if (endedTimer) return;
    endedTimer = setInterval(() => {
      const contexts = new Set([...endedOscillators.values()].map((n) => n.context.__id));
      for (const c of contexts) {
        for (let id; (id = audioCmd('ended', c)) > 0;) {
          const node = endedOscillators.get(c + ':' + id);
          endedOscillators.delete(c + ':' + id);
          if (!node) continue;
          const e = Object.assign(new Event('ended'), { target: node, currentTarget: node });
          if (typeof node.onended === 'function') call(node.onended, node, [e]);
          node.dispatchEvent(e);
        }
      }
      if (!endedOscillators.size) {
        clearInterval(endedTimer);
        endedTimer = 0;
      }
    }, 50);
  }

  class BaseAudioContext extends EventTarget {
    constructor() {
      super();
      Object.defineProperty(this, '__id', { value: audioCmd('context') });
      Object.defineProperty(this, 'destination', { value: new AudioDestinationNode(this, 1), enumerable: true });
      this.onstatechange = null;
    }
    get currentTime() { return audioCmd('time', this.__id) || 0; }
    get sampleRate() { return audioCmd('rate', this.__id) || 48000; }
    get state() { return this._closed ? 'closed' : audioCmd('running', this.__id) === 1 ? 'running' : 'suspended'; }
    get baseLatency() { return 0.01; }
    get listener() { return {}; }
    createOscillator() { return new OscillatorNode(this); }
    createGain() { return new GainNode(this); }
    _state(cmd) {
      const before = this.state;
      if (!this._closed) audioCmd(cmd, this.__id);
      if (this.state !== before) {
        const e = Object.assign(new Event('statechange'), { target: this, currentTarget: this });
        if (typeof this.onstatechange === 'function') call(this.onstatechange, this, [e]);
        this.dispatchEvent(e);
      }
      return Promise.resolve();
    }
    // (sound starts only after the user interacted with the page)
    resume() { return this._state('resume'); }
    suspend() { return this._state('suspend'); }
    close() { this._state('close'); this._closed = true; return Promise.resolve(); }
  }
  class AudioContext extends BaseAudioContext {}

  class FileReader extends EventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.result = null;
      this.error = null;
      for (const t of ['loadstart', 'progress', 'load', 'abort', 'error', 'loadend']) this['on' + t] = null;
    }
    _read(blob, kind, label) {
      if (!(blob instanceof Blob)) throw new TypeError("Failed to execute 'read' on 'FileReader': parameter 1 is not of type 'Blob'.");
      if (this.readyState === 1) throw new DOMException('The object is already busy reading Blobs.', 'InvalidStateError');
      this.readyState = 1;
      this.result = null;
      this.error = null;
      const token = this._token = {};
      const total = blob.size;
      addTimer(() => {
        if (this._token !== token) return;
        fireAt(this, 'loadstart', { lengthComputable: true, loaded: 0, total });
        const bytes = blob._data;
        let result;
        if (kind === 'text') result = decodeText(bytes, label);
        else if (kind === 'url') result = 'data:' + (blob.type || 'application/octet-stream') + ';base64,' + rb.base64(bytes);
        else if (kind === 'buffer') result = bytes.slice().buffer;
        else result = binaryString(bytes);
        if (this._token !== token) return;
        this.readyState = 2;
        this.result = result;
        const done = { lengthComputable: true, loaded: total, total };
        fireAt(this, 'progress', done);
        fireAt(this, 'load', done);
        if (this.readyState === 2) fireAt(this, 'loadend', done);
      }, 0, [], false);
    }
    readAsText(blob, encoding) { this._read(blob, 'text', encoding); }
    readAsDataURL(blob) { this._read(blob, 'url'); }
    readAsArrayBuffer(blob) { this._read(blob, 'buffer'); }
    readAsBinaryString(blob) { this._read(blob, 'binary'); }
    abort() {
      if (this.readyState !== 1) return;
      this._token = null;
      this.readyState = 2;
      this.result = null;
      fireAt(this, 'abort', {});
      fireAt(this, 'loadend', {});
    }
  }
  Object.assign(FileReader, { EMPTY: 0, LOADING: 1, DONE: 2 });

  // blob: URLs (File API): the Blob for fetch here, its bytes in the
  // browser for everything else.
  const blobUrls = new Map();
  URL.createObjectURL = (obj) => {
    const origin = location.origin;
    const url = 'blob:' + (origin && origin !== 'null' ? origin : 'null') + '/' + crypto.randomUUID();
    if (obj instanceof Blob) {
      blobUrls.set(url, obj);
      rb.blobUrl(url, obj.type, obj._data);
    }
    return url;
  };
  URL.revokeObjectURL = (url) => {
    url = String(url);
    if (blobUrls.delete(url)) rb.blobRevoke(url);
  };

  // ----- FormData ------------------------------------------------------------

  // An entry value (XHR standard: "create an entry").
  function formEntry(value, filename) {
    if (!(value instanceof Blob)) return String(value);
    if (value instanceof File && filename === undefined) return value;
    const f = new File([], filename !== undefined ? String(filename) : value instanceof File ? value.name : 'blob',
      { type: value.type, lastModified: value instanceof File ? value.lastModified : undefined });
    f._data = value._data;
    return f;
  }
  // The entry list of a form (HTML: "constructing the entry list").
  function formEntries(form, submitter) {
    const out = [];
    for (const el of form.elements) {
      const name = el.getAttribute('name');
      if (!name || el.hasAttribute('disabled')) continue;
      const tag = el.localName, type = el.type;
      if (tag === 'button' || (tag === 'input' && ['submit', 'image', 'reset', 'button'].includes(type))) {
        if (el === submitter && type === 'submit') out.push([name, el.value]);
        continue;
      }
      if ((type === 'checkbox' || type === 'radio') && !el.checked) continue;
      if (tag === 'input' && type === 'file') {
        const files = el.files;
        if (!files || !files.length) out.push([name, new File([], '', { type: 'application/octet-stream' })]);
        else for (const f of files) out.push([name, f]);
        continue;
      }
      out.push([name, tag === 'input' && (type === 'checkbox' || type === 'radio') && !el.hasAttribute('value') ? 'on' : el.value]);
    }
    return out;
  }
  class FormData {
    constructor(form, submitter) {
      this._p = form ? formEntries(form, submitter) : [];
    }
    append(k, v, filename) { this._p.push([String(k), formEntry(v, filename)]); }
    set(k, v, filename) {
      k = String(k);
      const entry = [k, formEntry(v, filename)];
      const i = this._p.findIndex(([n]) => n === k);
      this._p = this._p.filter(([n], j) => n !== k || j === i);
      if (i < 0) this._p.push(entry);
      else this._p[this._p.findIndex(([n]) => n === k)] = entry;
    }
    get(k) { const p = this._p.find(([n]) => n === String(k)); return p ? p[1] : null; }
    getAll(k) { return this._p.filter(([n]) => n === String(k)).map(([, v]) => v); }
    has(k) { return this._p.some(([n]) => n === String(k)); }
    delete(k) { this._p = this._p.filter(([n]) => n !== String(k)); }
    entries() { return this._p.map(([n, v]) => [n, v])[Symbol.iterator](); }
    keys() { return this._p.map(([n]) => n)[Symbol.iterator](); }
    values() { return this._p.map(([, v]) => v)[Symbol.iterator](); }
    forEach(fn, thisArg) { for (const [n, v] of this._p) fn.call(thisArg, v, n, this); }
    [Symbol.iterator]() { return this.entries(); }
    get [Symbol.toStringTag]() { return 'FormData'; }
  }

  // multipart/form-data (RFC 7578, as the HTML standard encodes it).
  function multipartBody(fd) {
    const crlf = (s) => s.replace(/\r\n|\r|\n/g, '\r\n');
    const esc = (s) => s.replace(/\r/g, '%0D').replace(/\n/g, '%0A').replace(/"/g, '%22');
    let boundary = '----RubrowserFormBoundary';
    for (let i = 0; i < 16; i++) boundary += 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'[Math.floor(Math.random() * 62)];
    const chunks = [];
    for (const [name, value] of fd._p) {
      let head = '--' + boundary + '\r\nContent-Disposition: form-data; name="' + esc(crlf(name)) + '"';
      if (value instanceof File) {
        head += '; filename="' + esc(value.name) + '"\r\nContent-Type: ' + (value.type || 'application/octet-stream') + '\r\n\r\n';
        chunks.push(rb.encodeUtf8(head), value._data, rb.encodeUtf8('\r\n'));
      } else {
        chunks.push(rb.encodeUtf8(head + '\r\n\r\n' + crlf(value) + '\r\n'));
      }
    }
    chunks.push(rb.encodeUtf8('--' + boundary + '--\r\n'));
    return { body: concatBytes(chunks), type: 'multipart/form-data; boundary=' + boundary };
  }

  // Fetch standard "extract a body": bytes (or a string, sent as UTF-8)
  // and the Content-Type it implies.
  function extractBody(body) {
    if (body === undefined || body === null) return { body: '', type: null };
    if (body instanceof URLSearchParams) return { body: body.toString(), type: 'application/x-www-form-urlencoded;charset=UTF-8' };
    if (body instanceof FormData) return multipartBody(body);
    if (body instanceof Blob) return { body: body._data, type: body.type || null };
    const bytes = sourceBytes(body);
    if (bytes) return { body: bytes, type: null };
    return { body: String(body), type: 'text/plain;charset=UTF-8' };
  }
  function bodyBytes(b) { return typeof b === 'string' ? rb.encodeUtf8(b) : b; }

  // ----- fetch, XMLHttpRequest -----------------------------------------------
  // HTTP from scripts goes through the browser (same origin only); the
  // response arrives in __rb_http. blob: and data: URLs are answered here.
  class Headers {
    constructor(init) {
      this._m = new Map();
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init) for (const [k, v] of Object.entries(init)) this.append(k, v);
    }
    get(k) { return this._m.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this._m.set(String(k).toLowerCase(), String(v)); }
    append(k, v) { k = String(k).toLowerCase(); this._m.set(k, this._m.has(k) ? this._m.get(k) + ', ' + String(v) : String(v)); }
    has(k) { return this._m.has(String(k).toLowerCase()); }
    delete(k) { this._m.delete(String(k).toLowerCase()); }
    forEach(fn) { this._m.forEach((v, k) => fn(v, k, this)); }
    entries() { return [...this._m.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))[Symbol.iterator](); }
    keys() { return [...this.entries()].map(([k]) => k)[Symbol.iterator](); }
    values() { return [...this.entries()].map(([, v]) => v)[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }
  // Headers from the browser's `name: value` lines.
  function parseHeaders(text) {
    const h = new Headers();
    for (const line of String(text || '').split('\r\n')) {
      const i = line.indexOf(':');
      if (i > 0) h.append(line.slice(0, i).trim(), line.slice(i + 1).trim());
    }
    return h;
  }
  class Request {
    constructor(input, init) {
      init = init || {};
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : new URL(String(input instanceof URL ? input.href : input), location.href).href;
      this.method = String(init.method || (from ? from.method : 'GET')).toUpperCase();
      this.headers = new Headers(init.headers || (from ? from.headers : undefined));
      this._body = init.body !== undefined ? init.body : from ? from._body : null;
      this.signal = init.signal || (from ? from.signal : null);
    }
    clone() { return new Request(this); }
  }
  class Response {
    constructor(body, init) {
      init = init || {};
      const x = extractBody(body);
      this._bytes = bodyBytes(x.body);
      this.status = init.status === undefined ? 200 : init.status;
      this.statusText = init.statusText || '';
      this.ok = this.status >= 200 && this.status < 300;
      this.headers = new Headers(init.headers);
      if (x.type && !this.headers.has('content-type')) this.headers.set('content-type', x.type);
      this.type = 'basic';
      this.redirected = false;
      this.url = '';
      this.bodyUsed = false;
    }
    _consume() {
      if (this.bodyUsed) return Promise.reject(new TypeError('Body has already been consumed.'));
      this.bodyUsed = true;
      return Promise.resolve(this._bytes);
    }
    text() { return this._consume().then((b) => rb.decodeUtf8(b)); }
    json() { return this.text().then((t) => JSON.parse(t)); }
    arrayBuffer() { return this._consume().then((b) => b.slice().buffer); }
    bytes() { return this._consume().then((b) => b.slice()); }
    blob() { return this._consume().then((b) => new Blob([b], { type: this.headers.get('content-type') || '' })); }
    clone() {
      const r = new Response(this._bytes, { status: this.status, statusText: this.statusText, headers: this.headers });
      r.url = this.url;
      return r;
    }
    static json(data, init) {
      const r = new Response(JSON.stringify(data), init);
      r.headers.set('content-type', 'application/json');
      return r;
    }
    static error() { const r = new Response(null, { status: 0 }); r.type = 'error'; return r; }
  }
  const httpPending = new Map(); // id -> (status, bytes, headers) => void
  global.__rb_http = function (id, status) {
    const done = httpPending.get(id);
    httpPending.delete(id);
    if (done) done(status, status ? rb.httpBytes(id) : new Uint8Array(0), status ? rb.httpHeaders(id) : '');
  };
  // WebSocket (HTML § 9.3, RFC 6455): the browser holds the connection and
  // reports to __rb_ws (1 open, 2 text, 3 binary, 4 closed, 5 error).
  const sockets = new Map(); // id -> WebSocket
  class WebSocket extends EventTarget {
    constructor(url, protocols) {
      super();
      let u;
      try { u = new URL(String(url), location.href); } catch (e) { throw new DOMException(`Failed to construct 'WebSocket': The URL '${url}' is invalid.`, 'SyntaxError'); }
      if (u.protocol === 'http:') u.protocol = 'ws:';
      else if (u.protocol === 'https:') u.protocol = 'wss:';
      if (u.protocol !== 'ws:' && u.protocol !== 'wss:') throw new DOMException(`Failed to construct 'WebSocket': The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '${u.protocol}' is not allowed.`, 'SyntaxError');
      if (u.hash) throw new DOMException("Failed to construct 'WebSocket': The URL contains a fragment identifier.", 'SyntaxError');
      const list = protocols === undefined ? [] : [].concat(protocols).map(String);
      if (new Set(list).size !== list.length || list.some((p) => !/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(p))) throw new DOMException("Failed to construct 'WebSocket': The subprotocol is invalid.", 'SyntaxError');
      this.url = u.href;
      this.readyState = 0;
      this.protocol = '';
      this.extensions = '';
      this.bufferedAmount = 0;
      this.binaryType = 'blob';
      for (const t of ['open', 'message', 'error', 'close']) this['on' + t] = null;
      this._id = rb.wsOpen(this.url, list.join(','));
      sockets.set(this._id, this);
    }
    send(data) {
      if (this.readyState === 0) throw new DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", 'InvalidStateError');
      if (this.readyState !== 1) return;
      if (data instanceof Blob) rb.wsSend(this._id, data._data, 0);
      else {
        const bytes = sourceBytes(data);
        rb.wsSend(this._id, bytes ? bytes.slice() : String(data), 0);
      }
    }
    close(code, reason) {
      if (code !== undefined && code !== 1000 && !(code >= 3000 && code <= 4999)) throw new DOMException(`Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. ${code} is neither.`, 'InvalidAccessError');
      reason = reason === undefined ? '' : String(reason);
      if (rb.encodeUtf8(reason).length > 123) throw new DOMException("Failed to execute 'close' on 'WebSocket': The close reason must not be greater than 123 UTF-8 bytes.", 'SyntaxError');
      if (this.readyState >= 2) return;
      this.readyState = 2;
      rb.wsClose(this._id, code === undefined ? (reason ? 1000 : 1005) : code, reason);
    }
    _fire(e) {
      Object.defineProperty(e, 'target', { value: this, configurable: true });
      Object.defineProperty(e, 'currentTarget', { value: this, configurable: true });
      if (typeof this['on' + e.type] === 'function') call(this['on' + e.type], this, [e]);
      this.dispatchEvent(e);
    }
  }
  Object.assign(WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  Object.assign(WebSocket.prototype, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  global.__rb_ws = function (id, kind) {
    const ws = sockets.get(id);
    if (!ws) return;
    const origin = new URL(ws.url).origin;
    if (kind === 1) {
      ws.readyState = 1;
      ws.protocol = rb.httpHeaders(id);
      ws._fire(new Event('open'));
    } else if (kind === 2 || kind === 3) {
      if (ws.readyState !== 1) return;
      const bytes = rb.httpBytes(id);
      const data = kind === 2 ? rb.decodeUtf8(bytes) : ws.binaryType === 'arraybuffer' ? bytes.slice().buffer : new Blob([bytes]);
      ws._fire(new eventInterfaces.MessageEvent('message', { data, origin }));
    } else if (kind === 4) {
      const m = /^(\d+) (\d) ([\s\S]*)$/.exec(rb.httpHeaders(id)) || [0, 1006, 0, ''];
      sockets.delete(id);
      const failed = m[2] === '0';
      ws.readyState = 3;
      if (failed) ws._fire(new Event('error'));
      ws._fire(new eventInterfaces.CloseEvent('close', { wasClean: !failed, code: Number(m[1]), reason: m[3] }));
    }
  };

  function httpRequest(method, url, body, contentType, done) {
    const href = new URL(String(url), location.href).href;
    const local = localResponse(href, method);
    if (local) { addTimer(() => done(...local), 0, [], false); return; }
    const id = rb.http(href, String(method || 'GET'), body, contentType || '');
    httpPending.set(id, done);
  }
  // [status, bytes, headers] for blob: and data: URLs (Fetch: "scheme
  // fetch"), [0, …] when they fail; null for other URLs.
  function localResponse(url, method) {
    const failed = [0, new Uint8Array(0), ''];
    if (url.startsWith('blob:')) {
      const blob = blobUrls.get(url.split('#')[0]);
      if (!blob || String(method).toUpperCase() !== 'GET') return failed;
      return [200, blob._data, 'content-type: ' + blob.type + '\r\ncontent-length: ' + blob.size + '\r\n'];
    }
    if (url.startsWith('data:')) {
      const m = /^data:([^,]*?)(;base64)?,([\s\S]*)$/.exec(url.split('#')[0]);
      if (!m) return failed;
      let bytes;
      if (m[2]) {
        const bin = atob(decodeURIComponent(m[3]));
        bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      } else {
        bytes = rb.encodeUtf8(decodeURIComponent(m[3]));
      }
      return [200, bytes, 'content-type: ' + (m[1] || 'text/plain;charset=US-ASCII') + '\r\n'];
    }
    return null;
  }
  function fetch(input, init) {
    let req;
    try { req = new Request(input, init); } catch (e) { return Promise.reject(e); }
    const x = extractBody(req._body);
    const ctype = req.headers.get('content-type') || x.type || '';
    return new Promise((resolve, reject) => {
      const signal = req.signal;
      if (signal && signal.aborted) { reject(signal.reason); return; }
      httpRequest(req.method, req.url, x.body, ctype, (status, bytes, headers) => {
        if (signal && signal.aborted) { reject(signal.reason); return; }
        if (!status) { reject(new TypeError('Failed to fetch')); return; }
        const r = new Response(bytes, { status, headers: parseHeaders(headers) });
        r.url = req.url;
        resolve(r);
      });
    });
  }
  class XMLHttpRequest extends EventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.status = 0;
      this.statusText = '';
      this.responseText = '';
      this.response = '';
      this.responseType = '';
      this.responseURL = '';
      this.timeout = 0;
      this.withCredentials = false;
      this._headers = {};
      this._resp = '';
      this.upload = new EventTarget();
    }
    open(method, url) {
      this._method = String(method).toUpperCase();
      this._url = new URL(String(url), location.href).href;
      this._aborted = false;
      this.readyState = 1;
      fireAt(this, 'readystatechange', {});
    }
    setRequestHeader(k, v) { this._headers[String(k).toLowerCase()] = String(v); }
    overrideMimeType() {}
    getResponseHeader(k) { return this.readyState >= 2 ? parseHeaders(this._resp).get(k) : null; }
    getAllResponseHeaders() {
      if (this.readyState < 2) return '';
      return [...parseHeaders(this._resp)].map(([k, v]) => k + ': ' + v + '\r\n').join('');
    }
    abort() {
      if (this._aborted || this.readyState === 4 || this.readyState === 0) return;
      this._aborted = true;
      this.readyState = 4;
      fireAt(this, 'readystatechange', {});
      fireAt(this, 'abort', {});
      fireAt(this, 'loadend', {});
      this.readyState = 0;
    }
    send(body) {
      const x = extractBody(this._method === 'GET' || this._method === 'HEAD' ? null : body);
      const sent = typeof x.body === 'string' ? rb.encodeUtf8(x.body).length : x.body.length;
      const ctype = this._headers['content-type'] || x.type || '';
      fireAt(this, 'loadstart', { loaded: 0, total: 0 });
      if (sent) fireAt(this.upload, 'loadstart', { lengthComputable: true, loaded: 0, total: sent });
      httpRequest(this._method, this._url, x.body, ctype, (status, bytes, headers) => {
        if (this._aborted) return;
        if (sent && status) {
          const up = { lengthComputable: true, loaded: sent, total: sent };
          for (const t of ['progress', 'load', 'loadend']) fireAt(this.upload, t, up);
        }
        this.status = status;
        this.statusText = status === 200 ? 'OK' : '';
        this.responseURL = status ? this._url : '';
        this._resp = headers;
        const type = parseHeaders(headers).get('content-type') || '';
        const text = () => decodeText(bytes, (/charset=([^;]+)/i.exec(type) || [])[1]);
        switch (this.responseType) {
          case 'arraybuffer': this.response = bytes.slice().buffer; break;
          case 'blob': this.response = new Blob([bytes], { type }); break;
          case 'json': try { this.response = JSON.parse(text()); } catch (_) { this.response = null; } break;
          default: this.responseText = text(); this.response = this.responseText;
        }
        this.readyState = 4;
        const done = { lengthComputable: true, loaded: bytes.length, total: bytes.length };
        fireAt(this, 'readystatechange', {});
        if (status) fireAt(this, 'progress', done);
        fireAt(this, status ? 'load' : 'error', done);
        fireAt(this, 'loadend', done);
      });
    }
  }
  Object.assign(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });

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

  // Intl (ECMA-402) with English (en-US) data, plus Traditional Chinese
  // (zh-TW) dates: other locales format as en-US, which
  // `resolvedOptions().locale` reports.
  const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July',
    'August', 'September', 'October', 'November', 'December'];
  const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
  const group = (i) => i.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const CURRENCY = { USD: '$', EUR: '€', GBP: '£', JPY: '¥', CNY: 'CN¥', TWD: 'NT$', KRW: '₩', INR: '₹', CAD: 'CA$', AUD: 'A$', HKD: 'HK$' };
  const UNITS = { percent: '%', kilobyte: ' kB', megabyte: ' MB', gigabyte: ' GB', byte: ' byte', second: ' sec', minute: ' min', hour: ' hr', day: ' day', kilometer: ' km', meter: ' m', celsius: '°C', fahrenheit: '°F' };
  const toParts = (pieces) => pieces.filter((p) => p.value !== '');

  class NumberFormat {
    constructor(locale, opts) {
      const o = Object.assign({}, opts);
      this.style = o.style || 'decimal';
      this.currency = o.currency ? String(o.currency).toUpperCase() : undefined;
      if (this.style === 'currency' && !this.currency) throw new TypeError('Currency code is required');
      this.currencyDisplay = o.currencyDisplay || 'symbol';
      this.unit = o.unit;
      this.unitDisplay = o.unitDisplay || 'short';
      this.notation = o.notation || 'standard';
      this.compactDisplay = o.compactDisplay || 'short';
      this.signDisplay = o.signDisplay || 'auto';
      this.useGrouping = o.useGrouping === undefined ? (this.notation === 'compact' ? 'min2' : 'auto') : o.useGrouping;
      const curDigits = this.currency === 'JPY' || this.currency === 'KRW' ? 0 : 2;
      const minDefault = this.style === 'currency' && this.notation !== 'compact' ? curDigits : 0;
      const maxDefault = this.style === 'currency' ? curDigits : this.style === 'percent' ? 0 : 3;
      this.minimumIntegerDigits = o.minimumIntegerDigits || 1;
      this.minimumFractionDigits = o.minimumFractionDigits !== undefined ? +o.minimumFractionDigits : Math.min(minDefault, o.maximumFractionDigits !== undefined ? +o.maximumFractionDigits : minDefault);
      this.maximumFractionDigits = o.maximumFractionDigits !== undefined ? +o.maximumFractionDigits : Math.max(maxDefault, this.minimumFractionDigits);
      if (this.minimumFractionDigits > this.maximumFractionDigits) throw new RangeError('maximumFractionDigits value is out of range.');
      this.maximumSignificantDigits = o.maximumSignificantDigits;
      this.minimumSignificantDigits = o.minimumSignificantDigits;
    }
    formatToParts(n) {
      if (typeof n === 'bigint') n = Number(n);
      n = Number(n);
      const parts = [];
      const neg = n < 0 || Object.is(n, -0);
      if (Number.isNaN(n)) return [{ type: 'nan', value: 'NaN' }];
      let x = Math.abs(n);
      if (this.style === 'percent') x *= 100;
      let suffix = '';
      let maxF = this.maximumFractionDigits;
      let minF = this.minimumFractionDigits;
      if (this.notation === 'compact' && Number.isFinite(x)) {
        const steps = this.compactDisplay === 'long'
          ? [[1e12, ' trillion'], [1e9, ' billion'], [1e6, ' million'], [1e3, ' thousand']]
          : [[1e12, 'T'], [1e9, 'B'], [1e6, 'M'], [1e3, 'K']];
        for (const [v, name] of steps) {
          if (Math.round(x) >= v) { x /= v; suffix = name; break; }
        }
        if (this.maximumSignificantDigits === undefined && x < 100) { maxF = x < 10 ? 1 : 0; minF = 0; }
        else if (this.maximumSignificantDigits === undefined) { maxF = 0; minF = 0; }
      }
      let digits;
      if (!Number.isFinite(x)) digits = '∞';
      else if (this.maximumSignificantDigits !== undefined) {
        digits = String(Number(x.toPrecision(this.maximumSignificantDigits)));
        if (digits.includes('e')) digits = Number(digits).toFixed(0);
      } else {
        digits = (Math.round(x * 10 ** maxF) / 10 ** maxF).toFixed(maxF);
        if (maxF > minF) {
          digits = digits.replace(/0+$/, '');
          const f = digits.split('.')[1] || '';
          digits = digits.replace(/\.$/, '') + (f.length < minF ? (f.length ? '' : '.') + '0'.repeat(minF - f.length) : '');
        }
      }
      let [int, frac] = digits.split('.');
      if (int !== '∞') int = int.padStart(this.minimumIntegerDigits, '0');
      const grouped = this.useGrouping === false || (this.useGrouping === 'min2' && int.length < 5) ? int : group(int);
      const zero = Number(digits) === 0;
      const sd = this.signDisplay;
      let sign = '';
      if (neg && sd !== 'never' && !(zero && (sd === 'exceptZero' || sd === 'negative'))) sign = '-';
      else if (!neg && (sd === 'always' || (sd === 'exceptZero' && !zero))) sign = '+';
      if (sign) parts.push({ type: sign === '-' ? 'minusSign' : 'plusSign', value: sign });
      if (this.style === 'currency') {
        const sym = this.currencyDisplay === 'code' ? this.currency + '\u00a0' : this.currencyDisplay === 'name' ? '' :
          (this.currencyDisplay === 'narrowSymbol' ? (CURRENCY[this.currency] || '').replace(/^[A-Z]+/, '') : CURRENCY[this.currency]) || this.currency + '\u00a0';
        if (sym) parts.push({ type: 'currency', value: sym });
      }
      if (int === '∞') parts.push({ type: 'infinity', value: '∞' });
      else {
        grouped.split(',').forEach((g, i) => {
          if (i) parts.push({ type: 'group', value: ',' });
          parts.push({ type: 'integer', value: g });
        });
      }
      if (frac) parts.push({ type: 'decimal', value: '.' }, { type: 'fraction', value: frac });
      if (suffix.startsWith(' ')) parts.push({ type: 'literal', value: ' ' });
      if (suffix) parts.push({ type: 'compact', value: suffix.trim() });
      if (this.style === 'percent') parts.push({ type: 'percentSign', value: '%' });
      if (this.style === 'currency' && this.currencyDisplay === 'name') parts.push({ type: 'literal', value: ' ' }, { type: 'currency', value: this.currency });
      if (this.style === 'unit' && this.unit) {
        const u = String(this.unit);
        if (this.unitDisplay === 'long') parts.push({ type: 'literal', value: ' ' }, { type: 'unit', value: Number(digits) === 1 ? u : u + 's' });
        else {
          const short = UNITS[u] || ' ' + u;
          if (short.startsWith(' ')) parts.push({ type: 'literal', value: ' ' });
          parts.push({ type: 'unit', value: short.trim() });
        }
      }
      return parts;
    }
    format(n) { return this.formatToParts(n).map((p) => p.value).join(''); }
    formatRange(a, b) { return this.format(a) + '–' + this.format(b); }
    resolvedOptions() {
      return {
        locale: 'en-US', numberingSystem: 'latn', style: this.style, currency: this.currency,
        minimumIntegerDigits: this.minimumIntegerDigits, minimumFractionDigits: this.minimumFractionDigits,
        maximumFractionDigits: this.maximumFractionDigits, useGrouping: this.useGrouping,
        notation: this.notation, signDisplay: this.signDisplay,
      };
    }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }
  // `format` is a bound getter in ECMA-402 (`array.map(nf.format)`).
  Object.defineProperty(NumberFormat.prototype, 'format', {
    configurable: true,
    get() { const f = (n) => this.formatToParts(n).map((p) => p.value).join(''); return f; },
  });

  // Time zones come from the browser's IANA database (__rb.tzOffset gives a
  // zone's offset in minutes at a time, NaN for an unknown zone).
  let localZone;
  const hostZone = () => localZone || (localZone = (global.__rb && __rb.tzLocal && __rb.tzLocal()) || 'UTC');
  const zoneOffset = (zone, ms) => (zone === 'UTC' ? 0 : __rb.tzOffset(zone, ms));
  function canonicalZone(z) {
    const s = String(z);
    if (/^(utc|etc\/utc|gmt|etc\/gmt|z|etc\/universal|universal)$/i.test(s)) return 'UTC';
    if (!global.__rb || !__rb.tzOffset || Number.isNaN(__rb.tzOffset(s, 0))) throw new RangeError('Invalid time zone specified: ' + s);
    return s;
  }
  // Traditional Chinese (zh-TW), Swedish (sv-SE, its numeric dates are
  // ISO 8601's y-MM-dd) and Canadian English (en-CA) data; every other
  // locale formats as en-US.
  const ZH_DAYS = ['日', '一', '二', '三', '四', '五', '六'];
  const SV_MONTHS = ['januari', 'februari', 'mars', 'april', 'maj', 'juni', 'juli', 'augusti', 'september', 'oktober', 'november', 'december'];
  const SV_MONTHS_SHORT = ['jan.', 'feb.', 'mars', 'apr.', 'maj', 'juni', 'juli', 'aug.', 'sep.', 'okt.', 'nov.', 'dec.'];
  const SV_DAYS = ['söndag', 'måndag', 'tisdag', 'onsdag', 'torsdag', 'fredag', 'lördag'];
  const SV_DAYS_SHORT = ['sön', 'mån', 'tis', 'ons', 'tors', 'fre', 'lör'];
  const isZh = (l) => /^zh(-|$)/i.test(l);
  const dateLocale = (l) => (isZh(l) ? 'zh-TW' : /^sv(-|$)/i.test(l) ? 'sv-SE' : /^en-CA$/i.test(l) ? 'en-CA' : 'en-US');

  class DateTimeFormat {
    constructor(locale, opts) {
      const o = Object.assign({}, opts);
      const req = [].concat(locale === undefined ? [] : locale).map(String);
      this.locale = req.length ? dateLocale(req[0]) : 'en-US';
      this.timeZone = o.timeZone === undefined ? hostZone() : canonicalZone(o.timeZone);
      const fields = ['weekday', 'era', 'year', 'month', 'day', 'hour', 'minute', 'second', 'fractionalSecondDigits', 'timeZoneName', 'dayPeriod'];
      this.o = {};
      for (const f of fields) if (o[f] !== undefined) this.o[f] = o[f];
      this.dateStyle = o.dateStyle;
      this.timeStyle = o.timeStyle;
      if ((this.dateStyle || this.timeStyle) && Object.keys(this.o).length) throw new TypeError('Can\'t set option ' + Object.keys(this.o)[0] + ' when dateStyle is used');
      if (!this.dateStyle && !this.timeStyle && !Object.keys(this.o).some((k) => k !== 'timeZoneName' && k !== 'era')) {
        this.o.year = 'numeric'; this.o.month = 'numeric'; this.o.day = 'numeric';
      }
      const zh = this.locale === 'zh-TW';
      const sv = this.locale === 'sv-SE';
      const ds = { full: { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' }, long: { year: 'numeric', month: 'long', day: 'numeric' },
        medium: { year: 'numeric', month: zh ? 'long' : 'short', day: 'numeric' }, short: { year: this.locale === 'en-US' ? '2-digit' : 'numeric', month: 'numeric', day: 'numeric' } };
      const ts = { full: { hour: 'numeric', minute: '2-digit', second: '2-digit', timeZoneName: 'long' }, long: { hour: 'numeric', minute: '2-digit', second: '2-digit', timeZoneName: 'short' },
        medium: { hour: 'numeric', minute: '2-digit', second: '2-digit' }, short: { hour: 'numeric', minute: '2-digit' } };
      if (this.dateStyle) Object.assign(this.o, ds[this.dateStyle]);
      if (this.timeStyle) Object.assign(this.o, ts[this.timeStyle]);
      this.hour12 = o.hour12 !== undefined ? !!o.hour12 : o.hourCycle ? o.hourCycle === 'h12' || o.hourCycle === 'h11' : !sv;
    }
    formatToParts(date) {
      const d = date === undefined ? new Date() : new Date(typeof date === 'object' ? date.valueOf() : date);
      if (Number.isNaN(d.getTime())) throw new RangeError('Invalid time value');
      // the wall-clock time in this.timeZone, read with the UTC getters
      const off = zoneOffset(this.timeZone, d.getTime()) || 0;
      const w = new Date(d.getTime() + off * 60000);
      const get = (f) => w['getUTC' + f]();
      const o = this.o;
      const zh = this.locale === 'zh-TW';
      const two = (n) => String(n).padStart(2, '0');
      const out = [];
      const lit = (v) => out.push({ type: 'literal', value: v });
      const year = get('FullYear'), month = get('Month'), day = get('Date');
      const yv = o.year === '2-digit' ? two(year % 100) : String(year);
      const dv = o.day === '2-digit' ? two(day) : String(day);
      const textMonth = o.month === 'long' || o.month === 'short' || o.month === 'narrow';
      if (this.locale === 'sv-SE') {
        // CLDR sv patterns: y-MM-dd, y-MM, d/M; EEEE d MMMM y; HH:mm:ss
        if (o.weekday) {
          out.push({ type: 'weekday', value: (o.weekday === 'long' ? SV_DAYS : SV_DAYS_SHORT)[get('Day')] });
          if (o.year || o.month || o.day) lit(' ');
        }
        if (textMonth) {
          if (o.day) out.push({ type: 'day', value: dv });
          if (o.month) { if (o.day) lit(' '); out.push({ type: 'month', value: (o.month === 'long' ? SV_MONTHS : SV_MONTHS_SHORT)[month] }); }
          if (o.year) { if (o.month || o.day) lit(' '); out.push({ type: 'year', value: yv }); }
        } else if (o.year && (o.month || o.day)) {
          out.push({ type: 'year', value: yv });
          if (o.month) { lit('-'); out.push({ type: 'month', value: two(month + 1) }); }
          if (o.day) { lit('-'); out.push({ type: 'day', value: two(day) }); }
        } else {
          const seq = [];
          if (o.year) seq.push({ type: 'year', value: yv });
          if (o.day) seq.push({ type: 'day', value: dv });
          if (o.month) seq.push({ type: 'month', value: o.month === '2-digit' ? two(month + 1) : String(month + 1) });
          seq.forEach((p, i) => { if (i) lit('/'); out.push(p); });
        }
        if (o.hour || o.minute || o.second) {
          if (out.length) lit(' ');
          let h = get('Hours');
          const pm = h >= 12;
          if (this.hour12) h = h % 12 || 12;
          const t = [];
          if (o.hour) t.push({ type: 'hour', value: this.hour12 && o.hour !== '2-digit' ? String(h) : two(h) });
          if (o.minute) t.push({ type: 'minute', value: two(get('Minutes')) });
          if (o.second) t.push({ type: 'second', value: two(get('Seconds')) });
          t.forEach((p, i) => { if (i) lit(':'); out.push(p); });
          if (o.fractionalSecondDigits) { lit(','); out.push({ type: 'fractionalSecond', value: String(get('Milliseconds')).padStart(3, '0').slice(0, o.fractionalSecondDigits) }); }
          if (o.hour && this.hour12) { lit(' '); out.push({ type: 'dayPeriod', value: pm ? 'em' : 'fm' }); }
        }
      } else if (zh) {
        // CLDR zh-Hant patterns: y年M月d日 (text month, or numeric with a
        // weekday), y/M/d, M/d（E）; dateStyle/timeStyle use flexible day
        // periods (凌晨…晚上), single fields 上午/下午.
        const date = [];
        const md = (v) => (o.month === '2-digit' ? two(v) : String(v));
        const ymd = textMonth || (o.weekday && o.year) || (o.year && !o.month && !o.day) || this.dateStyle === 'full' || this.dateStyle === 'long' || this.dateStyle === 'medium';
        if (ymd) {
          if (o.year) date.push({ type: 'year', value: yv }, { type: 'literal', value: '年' });
          if (o.month) date.push({ type: 'month', value: md(month + 1) }, { type: 'literal', value: '月' });
          if (o.day) date.push({ type: 'day', value: dv }, { type: 'literal', value: '日' });
        } else {
          const seq = [];
          if (o.year) seq.push({ type: 'year', value: yv });
          if (o.month) seq.push({ type: 'month', value: md(month + 1) });
          if (o.day) seq.push({ type: 'day', value: dv });
          seq.forEach((p, i) => { if (i) date.push({ type: 'literal', value: '/' }); date.push(p); });
        }
        out.push(...date);
        if (o.weekday) {
          const wd = ZH_DAYS[get('Day')];
          const v = { type: 'weekday', value: o.weekday === 'long' ? '星期' + wd : o.weekday === 'short' ? '週' + wd : wd };
          if (!out.length) out.push(v);
          else if (!ymd) { lit('（'); out.push(v); lit('）'); }
          else { if (!textMonth || this.dateStyle) lit(' '); out.push(v); }
        }
        if (o.hour || o.minute || o.second) {
          if (out.length && !(o.weekday && !o.year && !o.month && !o.day)) lit(' ');
          let h = get('Hours');
          if (o.hour && this.hour12) {
            const flexible = this.timeStyle !== undefined;
            const period = !flexible ? (h < 12 ? '上午' : '下午')
              : h < 5 ? '凌晨' : h < 8 ? '清晨' : h < 12 ? '上午' : h < 13 ? '中午' : h < 19 ? '下午' : '晚上';
            out.push({ type: 'dayPeriod', value: period });
            h = h % 12 || 12;
          }
          const t = [];
          if (o.hour) t.push({ type: 'hour', value: o.hour === '2-digit' || (!this.hour12 && o.minute) ? two(h) : String(h) });
          if (o.minute) t.push({ type: 'minute', value: o.hour || o.minute === '2-digit' ? two(get('Minutes')) : String(get('Minutes')) });
          if (o.second) t.push({ type: 'second', value: o.minute || o.second === '2-digit' ? two(get('Seconds')) : String(get('Seconds')) });
          t.forEach((p, i) => { if (i) lit(':'); out.push(p); });
          if (o.hour && !o.minute && !o.second) lit('時');
          if (o.fractionalSecondDigits) { lit('.'); out.push({ type: 'fractionalSecond', value: String(get('Milliseconds')).padStart(3, '0').slice(0, o.fractionalSecondDigits) }); }
        }
      } else {
        const mv = o.month === 'long' ? MONTHS[month] : o.month === 'short' ? MONTHS[month].slice(0, 3) : o.month === 'narrow' ? MONTHS[month][0] :
          o.month === '2-digit' ? two(month + 1) : String(month + 1);
        if (o.weekday) {
          const wd = DAYS[get('Day')];
          out.push({ type: 'weekday', value: o.weekday === 'long' ? wd : o.weekday === 'short' ? wd.slice(0, 3) : wd[0] });
          if (o.year || o.month || o.day || o.hour) lit(', ');
        }
        if (textMonth) {
          if (o.month) out.push({ type: 'month', value: mv });
          if (o.day) { if (o.month) lit(' '); out.push({ type: 'day', value: dv }); }
          if (o.year) { if (o.month || o.day) lit(o.day ? ', ' : ' '); out.push({ type: 'year', value: yv }); }
        } else if (this.locale === 'en-CA' && (o.month || o.year)) {
          // en-CA numeric dates: y-MM-dd, y-MM, MM-dd
          const seq = [];
          if (o.year) seq.push({ type: 'year', value: yv });
          if (o.month) seq.push({ type: 'month', value: two(month + 1) });
          if (o.day) seq.push({ type: 'day', value: two(day) });
          seq.forEach((p, i) => { if (i) lit('-'); out.push(p); });
        } else {
          const seq = [];
          if (o.month) seq.push({ type: 'month', value: mv });
          if (o.day) seq.push({ type: 'day', value: dv });
          if (o.year) seq.push({ type: 'year', value: yv });
          seq.forEach((p, i) => { if (i) lit('/'); out.push(p); });
        }
        if (o.hour || o.minute || o.second) {
          if (out.length && !(o.weekday && !o.year && !o.month && !o.day)) lit((this.dateStyle === 'full' || this.dateStyle === 'long') && this.timeStyle ? ' at ' : ', ');
          let h = get('Hours');
          const pm = h >= 12;
          if (this.hour12) h = h % 12 || 12;
          const t = [];
          if (o.hour) t.push({ type: 'hour', value: o.hour === '2-digit' || (!this.hour12 && o.minute) ? two(h) : String(h) });
          if (o.minute) t.push({ type: 'minute', value: o.hour || o.minute === '2-digit' ? two(get('Minutes')) : String(get('Minutes')) });
          if (o.second) t.push({ type: 'second', value: o.minute || o.second === '2-digit' ? two(get('Seconds')) : String(get('Seconds')) });
          t.forEach((p, i) => { if (i) lit(':'); out.push(p); });
          if (o.fractionalSecondDigits) { lit('.'); out.push({ type: 'fractionalSecond', value: String(get('Milliseconds')).padStart(3, '0').slice(0, o.fractionalSecondDigits) }); }
          const ca = this.locale === 'en-CA';
          if (o.hour && this.hour12) { lit(' '); out.push({ type: 'dayPeriod', value: pm ? (ca ? 'p.m.' : 'PM') : ca ? 'a.m.' : 'AM' }); }
        }
      }
      if (o.timeZoneName) {
        lit(' ');
        const name = off === 0 ? (o.timeZoneName === 'long' && !zh ? 'Coordinated Universal Time' : 'UTC')
          : 'GMT' + (off > 0 ? '+' : '-') + Math.floor(Math.abs(off) / 60) + (Math.abs(off) % 60 ? ':' + two(Math.abs(off) % 60) : '');
        out.push({ type: 'timeZoneName', value: name });
      }
      return out;
    }
    formatRange(a, b) { return this.format(a) + ' – ' + this.format(b); }
    resolvedOptions() {
      return Object.assign({ locale: this.locale, calendar: 'gregory', numberingSystem: 'latn', timeZone: this.timeZone, hour12: this.hour12, hourCycle: this.hour12 ? 'h12' : 'h23' },
        this.dateStyle ? { dateStyle: this.dateStyle } : {}, this.timeStyle ? { timeStyle: this.timeStyle } : {}, this.dateStyle || this.timeStyle ? {} : this.o);
    }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }
  Object.defineProperty(DateTimeFormat.prototype, 'format', {
    configurable: true,
    get() { return (d) => this.formatToParts(d).map((p) => p.value).join(''); },
  });

  class RelativeTimeFormat {
    constructor(locale, opts) { const o = Object.assign({}, opts); this.numeric = o.numeric || 'always'; this.style = o.style || 'long'; }
    formatToParts(v, unit) {
      v = Number(v);
      let u = String(unit).replace(/s$/, '');
      if (!['second', 'minute', 'hour', 'day', 'week', 'month', 'quarter', 'year'].includes(u)) throw new RangeError('Invalid unit argument for format() \'' + unit + '\'');
      if (this.numeric === 'auto') {
        const special = { day: { '-1': 'yesterday', 0: 'today', 1: 'tomorrow' }, second: { 0: 'now' } }[u];
        if (special && special[v] !== undefined) return [{ type: 'literal', value: special[v] }];
        if (['week', 'month', 'quarter', 'year'].includes(u) && Math.abs(v) <= 1 && Number.isInteger(v)) {
          return [{ type: 'literal', value: (v < 0 ? 'last ' : v > 0 ? 'next ' : 'this ') + u }];
        }
        if (['hour', 'minute'].includes(u) && v === 0) return [{ type: 'literal', value: 'this ' + u }];
      }
      if (this.style !== 'long') u = { second: 'sec.', minute: 'min.', hour: 'hr.', day: 'day', week: 'wk.', month: 'mo.', quarter: 'qtr.', year: 'yr.' }[u];
      const n = Math.abs(v);
      const name = n === 1 || u.endsWith('.') ? u : u + 's';
      const num = new NumberFormat().format(n);
      const neg = v < 0 || Object.is(v, -0);
      return neg
        ? [{ type: 'integer', value: num, unit: u }, { type: 'literal', value: ' ' + name + ' ago' }]
        : [{ type: 'literal', value: 'in ' }, { type: 'integer', value: num, unit: u }, { type: 'literal', value: ' ' + name }];
    }
    format(v, unit) { return this.formatToParts(v, unit).map((p) => p.value).join(''); }
    resolvedOptions() { return { locale: 'en-US', style: this.style, numeric: this.numeric, numberingSystem: 'latn' }; }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }

  class PluralRules {
    constructor(locale, opts) { this.type = (opts && opts.type) || 'cardinal'; }
    select(n) {
      n = Number(n);
      if (this.type === 'ordinal') {
        const t = n % 10, h = n % 100;
        return t === 1 && h !== 11 ? 'one' : t === 2 && h !== 12 ? 'two' : t === 3 && h !== 13 ? 'few' : 'other';
      }
      return n === 1 ? 'one' : 'other';
    }
    resolvedOptions() { return { locale: 'en-US', type: this.type, pluralCategories: this.type === 'ordinal' ? ['few', 'one', 'two', 'other'] : ['one', 'other'] }; }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }

  class Collator {
    constructor(locale, opts) { const o = Object.assign({}, opts); this.sensitivity = o.sensitivity || 'variant'; this.numeric = !!o.numeric; }
    resolvedOptions() { return { locale: 'en-US', usage: 'sort', sensitivity: this.sensitivity, numeric: this.numeric, ignorePunctuation: false, caseFirst: 'false', collation: 'default' }; }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }
  // Roughly the root collation: case and accents break ties only.
  Object.defineProperty(Collator.prototype, 'compare', {
    configurable: true,
    get() {
      return (a, b) => {
        a = String(a); b = String(b);
        const base = (s) => s.normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
        const key = (s) => (this.numeric ? s.replace(/\d+/g, (m) => m.padStart(20, '0')) : s);
        const [x, y] = [key(base(a)), key(base(b))];
        if (x !== y) return x < y ? -1 : 1;
        if (this.sensitivity === 'base' || a === b) return 0;
        const [p, q] = [a.normalize('NFD'), b.normalize('NFD')];
        // Accents, then case (lowercase first).
        const [pl, ql] = [p.toLowerCase(), q.toLowerCase()];
        if (pl !== ql) return pl < ql ? -1 : 1;
        if (this.sensitivity === 'accent') return 0;
        for (let i = 0; i < p.length; i++) {
          if (p[i] !== q[i]) return p[i] === pl[i] ? -1 : 1;
        }
        return 0;
      };
    },
  });

  class ListFormat {
    constructor(locale, opts) { const o = Object.assign({}, opts); this.type = o.type || 'conjunction'; this.style = o.style || 'long'; }
    formatToParts(list) {
      const items = [...list].map(String);
      const word = this.type === 'disjunction' ? 'or' : this.style === 'narrow' && this.type === 'conjunction' ? '' : this.style === 'short' ? '&' : 'and';
      const out = [];
      items.forEach((v, i) => {
        if (i) {
          const last = i === items.length - 1;
          const sep = this.type === 'unit' ? (this.style === 'narrow' ? ' ' : ', ')
            : last ? (items.length > 2 ? ',' : '') + (word ? ' ' + word + ' ' : ' ') : ', ';
          out.push({ type: 'literal', value: this.type !== 'unit' && last && !word ? ', ' : sep });
        }
        out.push({ type: 'element', value: v });
      });
      return out;
    }
    format(list) { return this.formatToParts(list).map((p) => p.value).join(''); }
    resolvedOptions() { return { locale: 'en-US', type: this.type, style: this.style }; }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }

  class Segmenter {
    constructor(locale, opts) { this.granularity = (opts && opts.granularity) || 'grapheme'; }
    segment(input) {
      const s = String(input);
      const g = this.granularity;
      const list = [];
      if (g === 'word') {
        const re = /[\p{L}\p{N}_'’]+|\s+|[^\p{L}\p{N}_\s]/gu;
        let m;
        while ((m = re.exec(s))) list.push({ segment: m[0], index: m.index, input: s, isWordLike: /[\p{L}\p{N}]/u.test(m[0]) });
      } else if (g === 'sentence') {
        const re = /[^.!?]+(?:[.!?]+\s*|$)/g;
        let m;
        while ((m = re.exec(s)) && m[0]) list.push({ segment: m[0], index: m.index, input: s });
      } else {
        // Graphemes: a base character with its combining marks, joiners
        // and variation selectors.
        const re = /\P{M}\p{M}*(?:\u200d\P{M}\p{M}*)*[\ufe0e\ufe0f]?|\p{M}+/gu;
        let m;
        while ((m = re.exec(s))) list.push({ segment: m[0], index: m.index, input: s });
      }
      const segments = {
        [Symbol.iterator]: () => list[Symbol.iterator](),
        containing(i) { return list.find((x) => i >= x.index && i < x.index + x.segment.length); },
      };
      return segments;
    }
    resolvedOptions() { return { locale: 'en-US', granularity: this.granularity }; }
    static supportedLocalesOf(l) { return l ? [].concat(l).slice(0, 1).map(String) : []; }
  }

  class DisplayNames {
    constructor(locale, opts) { this.type = (opts && opts.type) || 'language'; }
    of(code) {
      const names = { en: 'English', zh: 'Chinese', ja: 'Japanese', ko: 'Korean', fr: 'French', de: 'German', es: 'Spanish', 'zh-TW': 'Chinese (Taiwan)', 'en-US': 'American English', US: 'United States', TW: 'Taiwan', JP: 'Japan', GB: 'United Kingdom', USD: 'US Dollar', EUR: 'Euro' };
      return names[code] || code;
    }
    resolvedOptions() { return { locale: 'en-US', type: this.type, style: 'long', fallback: 'code' }; }
  }

  class Locale {
    constructor(tag) {
      const t = String(tag);
      const [language, ...rest] = t.split(/[-_]/);
      this.baseName = t; this.language = language.toLowerCase();
      this.region = rest.find((p) => /^[A-Za-z]{2}$|^\d{3}$/.test(p));
      this.script = rest.find((p) => /^[A-Za-z]{4}$/.test(p));
    }
    toString() { return this.baseName; }
    maximize() { return this; }
    minimize() { return this; }
  }

  // ECMA-402: DateTimeFormat, NumberFormat and Collator may also be called
  // without `new`.
  const callable = (C) => {
    const F = function (locales, options) { return new C(locales, options); };
    F.prototype = C.prototype;
    Object.defineProperty(C.prototype, 'constructor', { value: F, writable: true, configurable: true });
    for (const k of Object.getOwnPropertyNames(C)) if (!['length', 'name', 'prototype'].includes(k)) F[k] = C[k];
    Object.defineProperty(F, 'name', { value: C.name });
    return F;
  };
  const Intl = {
    NumberFormat: callable(NumberFormat), DateTimeFormat: callable(DateTimeFormat), RelativeTimeFormat, PluralRules, Collator: callable(Collator), ListFormat, Segmenter, DisplayNames, Locale,
    getCanonicalLocales: (l) => (l ? [].concat(l).map(String) : []),
    supportedValuesOf: (k) => (k === 'timeZone' ? ((global.__rb && __rb.tzNames && __rb.tzNames()) || 'UTC').split('\n') : k === 'currency' ? Object.keys(CURRENCY) : []),
  };
  // Locale-sensitive methods of built-ins go through Intl (boa without its
  // `intl` feature formats them like toString).
  const localeOptions = (o, defaults) => {
    o = Object.assign({}, o);
    const any = ['weekday', 'year', 'month', 'day', 'hour', 'minute', 'second', 'dateStyle', 'timeStyle'].some((k) => o[k] !== undefined);
    return any ? o : Object.assign(o, defaults);
  };
  const dateFormatter = (defaults) => function (locales, opts) {
    if (Number.isNaN(this.getTime())) return 'Invalid Date';
    return new DateTimeFormat(locales, localeOptions(opts, defaults)).format(this);
  };
  Object.assign(Date.prototype, {
    toLocaleString: dateFormatter({ year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' }),
    toLocaleDateString: dateFormatter({ year: 'numeric', month: 'numeric', day: 'numeric' }),
    toLocaleTimeString: dateFormatter({ hour: 'numeric', minute: '2-digit', second: '2-digit' }),
  });
  Number.prototype.toLocaleString = function (locales, opts) { return new NumberFormat(locales, opts).format(Number(this)); };
  if (typeof BigInt === 'function') BigInt.prototype.toLocaleString = function (locales, opts) { return new NumberFormat(locales, opts).format(this); };
  Array.prototype.toLocaleString = function (locales, opts) { return this.map((x) => (x == null ? '' : x.toLocaleString(locales, opts))).join(','); };
  String.prototype.localeCompare = function (that, locales, opts) { return new Collator(locales, opts).compare(String(this), String(that)); };

  class PerformanceObserver {
    observe() {}
    disconnect() {}
  }
  PerformanceObserver.supportedEntryTypes = [];

  // The CSS namespace (CSSOM, CSS Conditional Rules 3): supports() asks the
  // browser's @supports evaluation; escape() serializes an identifier.
  const CSS = {
    supports(prop, value) {
      if (arguments.length === 0) throw new TypeError('CSS.supports: 1 argument required');
      const cond = arguments.length > 1 ? `(${prop}: ${value})` : String(prop);
      return rb.cssSupports(cond) || (arguments.length === 1 && rb.cssSupports(`(${cond})`));
    },
    escape(ident) {
      if (arguments.length === 0) throw new TypeError('CSS.escape: 1 argument required');
      const s = String(ident);
      let out = '';
      for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i);
        if (c === 0) out += '\uFFFD';
        else if ((c >= 1 && c <= 0x1f) || c === 0x7f || (i === 0 && c >= 0x30 && c <= 0x39) || (i === 1 && c >= 0x30 && c <= 0x39 && s.charCodeAt(0) === 0x2d)) out += '\\' + c.toString(16) + ' ';
        else if (i === 0 && s.length === 1 && c === 0x2d) out += '\\' + s[i];
        else if (c >= 0x80 || c === 0x2d || c === 0x5f || (c >= 0x30 && c <= 0x39) || (c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a)) out += s[i];
        else out += '\\' + s[i];
      }
      return out;
    },
  };

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
    get languages() {
      const l = rb.languages().split(',').filter(Boolean);
      return Object.freeze(l.length ? l : ['en-US', 'en']);
    },
    get language() { return this.languages[0]; },
    platform: '',
    cookieEnabled: false,
    onLine: true,
    hardwareConcurrency: 1,
    maxTouchPoints: 0,
    sendBeacon: () => true,
  };

  // DOM traversal (DOM standard §6): NodeFilter, TreeWalker, NodeIterator.
  const NodeFilter = {
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
    SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4,
    SHOW_CDATA_SECTION: 0x8, SHOW_ENTITY_REFERENCE: 0x10, SHOW_ENTITY: 0x20,
    SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100,
    SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400, SHOW_NOTATION: 0x800,
  };
  // "Filter" a node: whatToShow, then the filter (a function or an object
  // with acceptNode).
  function traversalFilter(t, node) {
    if (t._active) throw new DOMException('Filter is running', 'InvalidStateError');
    if (!((1 << (node.nodeType - 1)) & t.whatToShow)) return NodeFilter.FILTER_SKIP;
    const f = t.filter;
    if (f == null) return NodeFilter.FILTER_ACCEPT;
    t._active = true;
    try {
      const r = typeof f === 'function' ? f.call(undefined, node) : f.acceptNode(node);
      return Number(r);
    } finally {
      t._active = false;
    }
  }
  class TreeWalker {
    constructor(root, whatToShow, filter) {
      this.root = root;
      this.whatToShow = whatToShow === undefined ? NodeFilter.SHOW_ALL : whatToShow >>> 0;
      this.filter = filter == null ? null : filter;
      this.currentNode = root;
      this._active = false;
    }
    parentNode() {
      let node = this.currentNode;
      while (node && node !== this.root) {
        node = node.parentNode;
        if (node && traversalFilter(this, node) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
      }
      return null;
    }
    _children(first) {
      let node = first ? this.currentNode.firstChild : this.currentNode.lastChild;
      while (node) {
        const r = traversalFilter(this, node);
        if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
        if (r === NodeFilter.FILTER_SKIP) {
          const child = first ? node.firstChild : node.lastChild;
          if (child) { node = child; continue; }
        }
        while (node) {
          const sibling = first ? node.nextSibling : node.previousSibling;
          if (sibling) { node = sibling; break; }
          const parent = node.parentNode;
          if (!parent || parent === this.root || parent === this.currentNode) return null;
          node = parent;
        }
      }
      return null;
    }
    firstChild() { return this._children(true); }
    lastChild() { return this._children(false); }
    _siblings(next) {
      let node = this.currentNode;
      if (node === this.root) return null;
      for (;;) {
        let sibling = next ? node.nextSibling : node.previousSibling;
        while (sibling) {
          node = sibling;
          const r = traversalFilter(this, node);
          if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
          sibling = next ? node.firstChild : node.lastChild;
          if (r === NodeFilter.FILTER_REJECT || !sibling) sibling = next ? node.nextSibling : node.previousSibling;
        }
        node = node.parentNode;
        if (!node || node === this.root) return null;
        if (traversalFilter(this, node) === NodeFilter.FILTER_ACCEPT) return null;
      }
    }
    nextSibling() { return this._siblings(true); }
    previousSibling() { return this._siblings(false); }
    previousNode() {
      let node = this.currentNode;
      while (node !== this.root) {
        let sibling = node.previousSibling;
        while (sibling) {
          node = sibling;
          let r = traversalFilter(this, node);
          while (r !== NodeFilter.FILTER_REJECT && node.lastChild) {
            node = node.lastChild;
            r = traversalFilter(this, node);
          }
          if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
          sibling = node.previousSibling;
        }
        if (node === this.root || !node.parentNode) return null;
        node = node.parentNode;
        if (traversalFilter(this, node) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
      }
      return null;
    }
    nextNode() {
      let node = this.currentNode;
      let r = NodeFilter.FILTER_ACCEPT;
      for (;;) {
        while (r !== NodeFilter.FILTER_REJECT && node.firstChild) {
          node = node.firstChild;
          r = traversalFilter(this, node);
          if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
        }
        let sibling = null;
        let temp = node;
        while (temp) {
          if (temp === this.root) return null;
          sibling = temp.nextSibling;
          if (sibling) { node = sibling; break; }
          temp = temp.parentNode;
        }
        if (!temp) return null;
        r = traversalFilter(this, node);
        if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = node);
      }
    }
  }
  class NodeIterator {
    constructor(root, whatToShow, filter) {
      this.root = root;
      this.whatToShow = whatToShow === undefined ? NodeFilter.SHOW_ALL : whatToShow >>> 0;
      this.filter = filter == null ? null : filter;
      this.referenceNode = root;
      this.pointerBeforeReferenceNode = true;
      this._active = false;
    }
    // The node after / before `node` in tree order, within the root.
    _following(node) {
      if (node.firstChild) return node.firstChild;
      while (node && node !== this.root) {
        if (node.nextSibling) return node.nextSibling;
        node = node.parentNode;
      }
      return null;
    }
    _preceding(node) {
      if (node === this.root) return null;
      let prev = node.previousSibling;
      if (!prev) return node.parentNode;
      while (prev.lastChild) prev = prev.lastChild;
      return prev;
    }
    _traverse(next) {
      let node = this.referenceNode;
      let before = this.pointerBeforeReferenceNode;
      for (;;) {
        if (next) {
          if (!before) {
            node = this._following(node);
            if (!node) return null;
          } else before = false;
        } else if (before) {
          node = this._preceding(node);
          if (!node) return null;
        } else before = true;
        if (traversalFilter(this, node) === NodeFilter.FILTER_ACCEPT) break;
      }
      this.referenceNode = node;
      this.pointerBeforeReferenceNode = before;
      return node;
    }
    nextNode() { return this._traverse(true); }
    previousNode() { return this._traverse(false); }
    detach() {}
  }
  Object.assign(Document.prototype, {
    createTreeWalker(root, whatToShow, filter) {
      if (!(root instanceof Node)) throw new TypeError('createTreeWalker: root is not a Node');
      return new TreeWalker(root, whatToShow, filter);
    },
    createNodeIterator(root, whatToShow, filter) {
      if (!(root instanceof Node)) throw new TypeError('createNodeIterator: root is not a Node');
      return new NodeIterator(root, whatToShow, filter);
    },
  });

  const document = wrap(0);
  wrappers.set(0, document);
  Object.defineProperty(document, 'defaultView', { configurable: true, get: () => global });
  if (!('readyState' in document)) {
    Object.defineProperty(document, 'readyState', { configurable: true, get: () => 'complete' });
  }
  // Cookies set by scripts last for the page.
  let cookieJar = '';
  // document.domain: the host (setting it is ignored, as in browsers
  // with origin-keyed agent clusters).
  Object.defineProperty(document, 'domain', {
    configurable: true,
    get: () => location.hostname,
    set: () => {},
  });
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
  // The event handler attributes of Window (HTML §8.1.8.2): pages test
  // `'onwheel' in window` and the like to choose the events they listen to.
  for (const name of ['load', 'domcontentloaded', 'keydown', 'keyup', 'keypress', 'click', 'dblclick', 'contextmenu', 'message', 'hashchange',
    'popstate', 'selectstart', 'wheel', 'scroll', 'resize', 'mousedown', 'mouseup', 'mousemove', 'mouseover', 'mouseout', 'pointerdown',
    'pointerup', 'pointermove', 'pointercancel', 'focus', 'blur', 'input', 'change', 'submit', 'error', 'beforeunload', 'unload',
    'pagehide', 'pageshow', 'online', 'offline', 'storage', 'visibilitychange']) handlerProperty(global, name);

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

  // <img>: whether its image is done (loaded, broken or none) and its size
  // (HTML's `complete`, `naturalWidth`, `naturalHeight`).
  for (const [prop, get] of [
    ['complete', (h) => rb.imageInfo(h, 0) !== 0],
    ['naturalWidth', (h) => (rb.imageInfo(h, 0) === 1 ? rb.imageInfo(h, 1) : 0)],
    ['naturalHeight', (h) => (rb.imageInfo(h, 0) === 1 ? rb.imageInfo(h, 2) : 0)],
  ]) {
    if (Object.getOwnPropertyDescriptor(Element.prototype, prop)) continue;
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() { return this.localName === 'img' ? get(this.__h) : undefined; },
    });
  }

  // `element.onclick = …` and friends.
  for (const name of ['load', 'error', 'click', 'dblclick', 'contextmenu', 'selectstart', 'input', 'change', 'submit',
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
    ...eventInterfaces,
    Touch,
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
    NodeFilter,
    TreeWalker,
    NodeIterator,
    DOMException,
    MessagePort,
    BroadcastChannel,
    Headers,
    Request,
    Response,
    fetch,
    XMLHttpRequest,
    FormData,
    Blob,
    File,
    FileList,
    FileReader,
    DOMParser,
    Intl: typeof global.Intl === 'object' ? global.Intl : Intl,
    PerformanceObserver,
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
    ShadowRoot: DocumentFragment,
    ...elementInterfaces,
    AudioContext, webkitAudioContext: AudioContext, BaseAudioContext, AudioNode, AudioParam, AudioDestinationNode, GainNode, OscillatorNode,
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
    CSS,
    WebSocket,
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
