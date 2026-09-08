// DOM adapter for the existing shipped-module Node harness.
class Node {
  constructor(id = "") {
    this.id = id;
    this.children = [];
    this.dataset = {};
    this.style = { setProperty: () => {}, display: "" };
    this.listeners = {};
    this.className = "";
    this._text = "";
    this.parentNode = null;
    this.hidden = false;
    this.disabled = false;
    this.value = "";
    this.offsetWidth = 40;
    this.offsetLeft = 0;
  }
  appendChild(child) {
    if (child == null) return child;
    if (child.parentNode) child.parentNode.removeChild(child);
    this.children.push(child);
    child.parentNode = this;
    return child;
  }
  append(...children) { for (const child of children) this.appendChild(child); }
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
  prepend(child) { this.children.unshift(child); child.parentNode = this; return child; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this._text = String(value); this.children = []; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  scrollIntoView() {}
  get classList() {
    const self = this;
    const tokens = () => self.className.split(/\s+/).filter(Boolean);
    const add = (...names) => { self.className = [...new Set([...tokens(), ...names])].join(" "); };
    const remove = (...names) => {
      const drop = new Set(names);
      self.className = tokens().filter((token) => !drop.has(token)).join(" ");
    };
    return {
      add,
      remove,
      contains: (name) => tokens().includes(name),
      toggle: (name, on) => {
        if (on === undefined) on = !tokens().includes(name);
        if (on) add(name); else remove(name);
      },
    };
  }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
const tabs = ["tasks", "audit", "diagnostics", "operations", "knowledge"].map((tab) => Object.assign(new Node(), { dataset: { tab } }));
const panes = [...tabs, Object.assign(new Node(), { dataset: { tab: "run-detail" } })];
const tabsStrip = new Node("tabs");
tabsStrip.className = "tabs";
const wrap = get("global-id-wrap");
wrap.className = "global-id-wrap";
wrap.appendChild(get("global-task-id"));
wrap.appendChild(get("global-task-id-error"));
const documentListeners = {};
globalThis.document = {
  body: new Node("body"),
  hidden: false,
  getElementById: get,
  createElement: () => new Node(),
  createElementNS: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
  querySelectorAll: (selector) => {
    if (selector === ".tab") return tabs;
    if (selector === ".tab-pane") return panes;
    if (selector === "#task-filter .chip") return get("task-filter").children;
    if (selector === "#tasks-body .row") return get("tasks-body").children.filter((node) => String(node.className).includes("row"));
    return [];
  },
  querySelector: (selector) => {
    if (selector === ".tabs") return tabsStrip;
    const tabMatch = /^\.tab\[data-tab="([^"]+)"\]$/.exec(selector || "");
    if (tabMatch) return tabs.find((tab) => tab.dataset.tab === tabMatch[1]) || null;
    return new Node();
  },
  addEventListener: (name, fn) => { documentListeners[name] = fn; },
};
const location = new URL("http://dashboard.test/?workspace=one");
location.hash = "#tasks";
const hashListeners = [];
globalThis.window = {
  location,
  innerHeight: 900,
  addEventListener: (name, fn) => { if (name === "hashchange") hashListeners.push(fn); },
  matchMedia: () => ({ addEventListener: () => {}, matches: false }),
  localStorage: { getItem: () => null, setItem: () => {} },
};
Object.defineProperty(globalThis.window, "location", {
  configurable: true,
  get: () => location,
  set: () => {},
});
Object.defineProperty(location, "hash", {
  configurable: true,
  get() { return this._hash || ""; },
  set(value) {
    const next = String(value || "");
    const normalized = next.startsWith("#") ? next : `#${next}`;
    if (this._hash === normalized) return;
    this._hash = normalized;
    for (const fn of hashListeners) fn();
  },
});
location._hash = "#tasks";
globalThis.history = { replaceState: (_, __, url) => { const next = new URL(String(url), location.href); location.search = next.search; location.pathname = next.pathname; } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };
