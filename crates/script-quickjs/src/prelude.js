// The slice 1 DOM API (spec 0004, child 0006, DOM API), built in JavaScript
// over the natives `bindings.rs` provides. Evaluated once per page; it
// returns the internals the engine calls into (timers, events, sweeping).
//
// Nodes cross the boundary as string keys. `wrap` turns a key into the one
// wrapper object for that node: the cache maps keys to weak references, so
// identity holds while a wrapper lives and a collected wrapper unpins its
// node. Fallible natives return a number code on failure, which `check`
// throws as a `DOMException` with the standard name.
(function (native) {
  'use strict';
  const global = globalThis;

  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(message);
      this.name = name;
    }
  }

  // By the code `bindings.rs` returns.
  const ERRORS = [
    null,
    ['HierarchyRequestError', 'The node cannot be inserted there.'],
    ['NotFoundError', 'The node was not found.'],
    ['InvalidStateError', 'The node is not in a state that allows this.'],
    ['InvalidCharacterError', 'The name is not valid.'],
    ['SyntaxError', 'The selector is not valid.'],
    ['NotSupportedError', 'The operation is not supported.'],
  ];

  const check = (result) => {
    if (typeof result === 'number') {
      const [name, message] = ERRORS[result] || ['Error', ''];
      throw new DOMException(message, name);
    }
    return result;
  };

  const cache = new Map();
  const protos = {};

  const wrap = (key) => {
    if (key === null || key === undefined) return null;
    const live = cache.get(key)?.deref();
    if (live !== undefined) return live;
    const kind = native.kind(key);
    if (kind === null || kind === undefined) return null;
    const node = native.instantiate(key, protos[kind]);
    cache.set(key, new WeakRef(node));
    return node;
  };
  const wrapAll = (keys) => keys.map(wrap).filter((node) => node !== null);

  // Event listeners by target object, then by type (AC-33). Element
  // listeners are kept but fire from feature 14.
  const listeners = new WeakMap();
  const listen = (target, type, callback) => {
    if (typeof callback !== 'function' && !(callback && typeof callback.handleEvent === 'function')) {
      return;
    }
    type = String(type);
    let types = listeners.get(target);
    if (!types) listeners.set(target, (types = new Map()));
    const list = types.get(type) || [];
    if (!list.includes(callback)) list.push(callback);
    types.set(type, list);
  };
  const unlisten = (target, type, callback) => {
    const list = listeners.get(target)?.get(String(type));
    const at = list ? list.indexOf(callback) : -1;
    if (at >= 0) list.splice(at, 1);
  };
  const report = (error) => {
    const text = error && error.stack ? `${error}\n${error.stack}` : String(error);
    native.console('error', `Uncaught ${text}`);
  };
  // Calls every listener on each target of `path` in turn; one that
  // throws is reported and the rest still run.
  const dispatch = (path, type, bubbles) => {
    const event = {
      type,
      target: path[0],
      currentTarget: null,
      bubbles,
      defaultPrevented: false,
      preventDefault() {
        this.defaultPrevented = true;
      },
    };
    for (const current of path) {
      event.currentTarget = current;
      const list = listeners.get(current)?.get(type);
      for (const callback of list ? [...list] : []) {
        try {
          if (typeof callback === 'function') callback.call(current, event);
          else callback.handleEvent(event);
        } catch (error) {
          report(error);
        }
      }
    }
  };

  const illegal = function () {
    throw new TypeError('Illegal constructor');
  };
  const makeClass = (name, parent) => {
    const constructor = { [name]: function () { illegal(); } }[name];
    if (parent) {
      constructor.prototype = Object.create(parent.prototype, {
        constructor: { value: constructor, writable: true, configurable: true },
      });
    }
    global[name] = constructor;
    return constructor;
  };
  const getters = (proto, members) => {
    for (const [name, get] of Object.entries(members)) {
      Object.defineProperty(proto, name, { get, configurable: true, enumerable: true });
    }
  };
  const accessors = (proto, members) => {
    for (const [name, [get, set]] of Object.entries(members)) {
      Object.defineProperty(proto, name, { get, set, configurable: true, enumerable: true });
    }
  };
  const methods = (proto, members) => {
    for (const [name, value] of Object.entries(members)) {
      Object.defineProperty(proto, name, { value, writable: true, configurable: true });
    }
  };

  const Node = makeClass('Node');
  const Element = makeClass('Element', Node);
  const CharacterData = makeClass('CharacterData', Node);
  const Text = makeClass('Text', CharacterData);
  const Comment = makeClass('Comment', CharacterData);
  const Document = makeClass('Document', Node);
  const DocumentType = makeClass('DocumentType', Node);
  const DocumentFragment = makeClass('DocumentFragment', Node);
  global.HTMLElement = Element;
  Object.assign(protos, {
    Element: Element.prototype,
    Text: Text.prototype,
    Comment: Comment.prototype,
    Document: Document.prototype,
    DocumentType: DocumentType.prototype,
    DocumentFragment: DocumentFragment.prototype,
  });
  for (const [name, value] of Object.entries({
    ELEMENT_NODE: 1, TEXT_NODE: 3, COMMENT_NODE: 8, DOCUMENT_NODE: 9,
    DOCUMENT_TYPE_NODE: 10, DOCUMENT_FRAGMENT_NODE: 11,
  })) {
    Object.defineProperty(Node, name, { value });
    Object.defineProperty(Node.prototype, name, { value });
  }

  const query = (scope, selectors) => check(native.query(scope, String(selectors)));

  getters(Node.prototype, {
    nodeType() { return native.nodeType(this); },
    nodeName() { return native.nodeName(this); },
    parentNode() { return wrap(native.parent(this)); },
    childNodes() { return wrapAll(native.childNodes(this)); },
    firstChild() { return wrap(native.firstChild(this)); },
    lastChild() { return wrap(native.lastChild(this)); },
    nextSibling() { return wrap(native.nextSibling(this)); },
    previousSibling() { return wrap(native.previousSibling(this)); },
  });
  accessors(Node.prototype, {
    textContent: [
      function () { return native.textContent(this) ?? null; },
      function (text) { check(native.setTextContent(this, text === null ? '' : String(text))); },
    ],
  });
  methods(Node.prototype, {
    appendChild(child) { check(native.insertBefore(this, child, null)); return child; },
    insertBefore(child, reference) {
      check(native.insertBefore(this, child, reference ?? null));
      return child;
    },
    removeChild(child) { check(native.removeChild(this, child)); return child; },
    replaceChild(newChild, oldChild) {
      check(native.replaceChild(this, newChild, oldChild));
      return oldChild;
    },
    contains(other) {
      for (let node = other ?? null; node !== null; node = node.parentNode) {
        if (node === this) return true;
      }
      return false;
    },
    addEventListener(type, callback) { listen(this, type, callback); },
    removeEventListener(type, callback) { unlisten(this, type, callback); },
  });

  // `style`: any property by camelCase name, read and written through the
  // `style` attribute.
  const kebab = (name) => name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);
  const parseStyle = (text) => {
    const declarations = new Map();
    for (const part of (text || '').split(';')) {
      const colon = part.indexOf(':');
      if (colon < 0) continue;
      const name = part.slice(0, colon).trim().toLowerCase();
      const value = part.slice(colon + 1).trim();
      if (name) declarations.set(name, value);
    }
    return declarations;
  };
  const serializeStyle = (declarations) =>
    [...declarations].map(([name, value]) => `${name}: ${value};`).join(' ');
  const styleOf = (element) => new Proxy({}, {
    get(_, property) {
      if (typeof property !== 'string') return undefined;
      const text = element.getAttribute('style') || '';
      if (property === 'cssText') return text;
      return parseStyle(text).get(kebab(property)) ?? '';
    },
    set(_, property, value) {
      if (typeof property !== 'string') return true;
      if (property === 'cssText') {
        element.setAttribute('style', String(value));
        return true;
      }
      const declarations = parseStyle(element.getAttribute('style'));
      const name = kebab(property);
      if (value === null || value === undefined || value === '') declarations.delete(name);
      else declarations.set(name, String(value));
      element.setAttribute('style', serializeStyle(declarations));
      return true;
    },
  });

  const classes = (element) => (element.getAttribute('class') || '').split(/\s+/).filter(Boolean);
  const classListOf = (element) => ({
    contains(name) { return classes(element).includes(String(name)); },
    add(...names) {
      const list = classes(element);
      for (const name of names.map(String)) if (!list.includes(name)) list.push(name);
      element.setAttribute('class', list.join(' '));
    },
    remove(...names) {
      const gone = names.map(String);
      element.setAttribute('class', classes(element).filter((c) => !gone.includes(c)).join(' '));
    },
    toggle(name, force) {
      name = String(name);
      const has = classes(element).includes(name);
      const want = force === undefined ? !has : Boolean(force);
      if (want && !has) this.add(name);
      if (!want && has) this.remove(name);
      return want;
    },
  });

  getters(Element.prototype, {
    tagName() { return native.nodeName(this); },
    classList() { return classListOf(this); },
    children() { return this.childNodes.filter((node) => node.nodeType === 1); },
  });
  accessors(Element.prototype, {
    id: [
      function () { return this.getAttribute('id') ?? ''; },
      function (value) { this.setAttribute('id', value); },
    ],
    className: [
      function () { return this.getAttribute('class') ?? ''; },
      function (value) { this.setAttribute('class', value); },
    ],
    style: [
      function () { return styleOf(this); },
      function (value) { this.setAttribute('style', value); },
    ],
  });
  methods(Element.prototype, {
    getAttribute(name) { return native.getAttribute(this, String(name)) ?? null; },
    setAttribute(name, value) { check(native.setAttribute(this, String(name), String(value))); },
    removeAttribute(name) { check(native.removeAttribute(this, String(name))); },
    hasAttribute(name) { return (native.getAttribute(this, String(name)) ?? null) !== null; },
    querySelector(selectors) { return wrap(query(this, selectors)[0]); },
    querySelectorAll(selectors) { return wrapAll(query(this, selectors)); },
  });

  getters(Document.prototype, {
    documentElement() { return wrap(native.documentElement()); },
    body() { return wrap(native.body()); },
    head() { return wrap(native.head()); },
    readyState() { return native.readyState(); },
  });
  methods(Document.prototype, {
    getElementById(id) { return wrap(native.elementById(String(id))); },
    querySelector(selectors) { return wrap(query(this, selectors)[0]); },
    querySelectorAll(selectors) { return wrapAll(query(this, selectors)); },
    createElement(name) { return wrap(check(native.createElement(String(name)))); },
    createTextNode(data) { return wrap(native.createTextNode(String(data))); },
  });

  const document = wrap(native.document());
  global.window = global;
  global.self = global;
  global.document = document;
  global.DOMException = DOMException;
  global.addEventListener = (type, callback) => listen(global, type, callback);
  global.removeEventListener = (type, callback) => unlisten(global, type, callback);

  const log = (level) => (...args) => native.console(level, args.map(String).join(' '));
  global.console = {
    log: log('log'),
    info: log('info'),
    warn: log('warn'),
    error: log('error'),
    debug: log('debug'),
  };

  const timers = new Map();
  global.setTimeout = function (handler, delay, ...args) {
    const id = native.setTimeout(Number(delay) || 0);
    timers.set(id, [handler, args]);
    return id;
  };
  global.clearTimeout = function (id) {
    id = Number(id);
    if (timers.delete(id)) native.clearTimeout(id);
  };

  return {
    runTimer(id) {
      const timer = timers.get(id);
      if (!timer) return;
      timers.delete(id);
      const [handler, args] = timer;
      if (typeof handler === 'function') handler.apply(global, args);
      else (0, eval)(String(handler));
    },
    fire(target, type) {
      if (target === 'document') dispatch([document, global], type, true);
      else dispatch([global], type, false);
    },
    sweep() {
      for (const [key, ref] of cache) {
        if (ref.deref() === undefined) cache.delete(key);
      }
    },
  };
})
