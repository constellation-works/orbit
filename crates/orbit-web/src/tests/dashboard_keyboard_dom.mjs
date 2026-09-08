// DOM adapter for the keyboard-operability scenarios.
//
// Unlike the other adapters here this one keeps attributes in a real map and
// dispatches to every registered listener, because the behaviour under test is
// exactly "which attributes did the row set" and "does a keydown reach the row
// handler".
class Node {
  constructor(tag = "div") {
    this.tagName = String(tag).toUpperCase();
    this.id = "";
    this.children = [];
    this.dataset = {};
    this.attributes = new Map();
    this.listeners = new Map();
    this.style = { setProperty: () => {}, display: "" };
    this.className = "";
    this.title = "";
    this.value = "";
    this.hidden = false;
    this.disabled = false;
    this.tabIndex = -1;
    this._text = "";
    this.parentNode = null;
  }

  appendChild(child) {
    if (child == null) return child;
    if (child.parentNode) child.parentNode.removeChild(child);
    this.children.push(child);
    child.parentNode = this;
    return child;
  }
  insertBefore(child, before) {
    if (child.parentNode) child.parentNode.removeChild(child);
    const index = this.children.indexOf(before);
    if (index < 0) return this.appendChild(child);
    this.children.splice(index, 0, child);
    child.parentNode = this;
    return child;
  }
  removeChild(child) {
    this.children = this.children.filter((candidate) => candidate !== child);
    child.parentNode = null;
    return child;
  }
  replaceChildren(...children) {
    this._text = "";
    this.children = [];
    for (const child of children) this.appendChild(child);
  }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }

  addEventListener(name, fn) {
    const bound = this.listeners.get(name) || [];
    bound.push(fn);
    this.listeners.set(name, bound);
  }
  // Test-only dispatcher. `overrides` can set `target` to model a key press
  // that started on a nested control and bubbled up to the row.
  dispatch(name, overrides = {}) {
    const event = { target: this, currentTarget: this, preventDefault() {}, stopPropagation() {}, ...overrides };
    for (const fn of this.listeners.get(name) || []) fn(event);
  }

  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.has(name) ? this.attributes.get(name) : null; }

  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }

  get classList() {
    const self = this;
    const tokens = () => new Set(self.className.split(/\s+/).filter(Boolean));
    const write = (set) => { self.className = [...set].join(" "); };
    return {
      add: (...names) => { const set = tokens(); for (const name of names) set.add(name); write(set); },
      remove: (...names) => { const set = tokens(); for (const name of names) set.delete(name); write(set); },
      contains: (name) => tokens().has(name),
      toggle: (name, on) => {
        const set = tokens();
        const next = on === undefined ? !set.has(name) : !!on;
        if (next) set.add(name); else set.delete(name);
        write(set);
        return next;
      },
    };
  }

  // Supports the `tag`, `.class` and `tag.class` selectors the dashboard
  // modules actually use to re-find nodes they rendered earlier.
  matches(selector) {
    const [tag, ...classes] = String(selector).split(".");
    if (tag && this.tagName !== tag.toUpperCase()) return false;
    return classes.every((name) => this.classList.contains(name));
  }
  querySelector(selector) {
    for (const child of this.children) {
      if (child.matches(selector)) return child;
      const found = child.querySelector(selector);
      if (found) return found;
    }
    return null;
  }
  querySelectorAll(selector) {
    const found = [];
    for (const child of this.children) {
      if (child.matches(selector)) found.push(child);
      found.push(...child.querySelectorAll(selector));
    }
    return found;
  }
  contains(node) { return this === node || this.children.some((child) => child.contains(node)); }
  focus() { globalThis.document.activeElement = this; }
  closest() { return null; }
  scrollIntoView() {}
}

const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, Object.assign(new Node(), { id })), byId.get(id));

// The log dock's filter pills live in index.html, so the harness stands in the
// same markup the page ships: real buttons carrying `data-filter`.
const sideDock = get("side-dock");
for (const filter of ["all", "err", "deny", "warn"]) {
  const pill = new Node("button");
  pill.className = filter === "all" ? "filter-pill on" : "filter-pill";
  pill.dataset.filter = filter;
  pill.setAttribute("aria-pressed", filter === "all" ? "true" : "false");
  sideDock.appendChild(pill);
}

globalThis.document = {
  body: new Node("body"),
  hidden: false,
  activeElement: null,
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createElementNS: (_ns, tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node("#text"), { textContent: text }),
  createDocumentFragment: () => new Node("#fragment"),
  querySelectorAll: (selector) => {
    if (selector === "#side-dock .filter-pill") return sideDock.querySelectorAll(".filter-pill");
    return [];
  },
  querySelector: () => null,
  addEventListener: () => {},
};

globalThis.window = {
  location: new URL("http://dashboard.test/"),
  innerHeight: 900,
  addEventListener: () => {},
  matchMedia: () => ({ addEventListener: () => {}, matches: false }),
  localStorage: { getItem: () => null, setItem: () => {} },
};
globalThis.history = { replaceState: () => {} };
Object.defineProperty(globalThis, "navigator", {
  value: { clipboard: { writeText: () => Promise.resolve() } },
  configurable: true,
});
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };
