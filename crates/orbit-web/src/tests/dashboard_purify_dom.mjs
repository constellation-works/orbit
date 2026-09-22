// Small DOM implementation for executing the vendored DOMPurify bundle in
// Node. It deliberately implements DOM behavior, not sanitization policy: the
// production DOMPurify code still parses, walks, strips and serializes markup.
const HTML_NS = 'http://www.w3.org/1999/xhtml';
const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'track', 'wbr']);

const escapeText = value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const escapeAttr = value => escapeText(value).replace(/"/g, '&quot;');

class MiniNode {
  constructor(type, name, ownerDocument = null) {
    this._nodeType = type;
    this._nodeName = name;
    this._ownerDocument = ownerDocument;
    this._parentNode = null;
    this._childNodes = [];
  }
  get nodeType() { return this._nodeType; }
  get nodeName() { return this._nodeName; }
  get ownerDocument() { return this._ownerDocument; }
  get parentNode() { return this._parentNode; }
  get childNodes() { return this._childNodes; }
  get nextSibling() {
    if (!this._parentNode) return null;
    const siblings = this._parentNode._childNodes;
    return siblings[siblings.indexOf(this) + 1] || null;
  }
  get firstChild() { return this._childNodes[0] || null; }
  get firstElementChild() { return this._childNodes.find(node => node.nodeType === 1) || null; }
  get lastElementChild() { return [...this._childNodes].reverse().find(node => node.nodeType === 1) || null; }
  get children() { return this._childNodes.filter(node => node.nodeType === 1); }
  hasChildNodes() { return this._childNodes.length > 0; }
  appendChild(child) {
    if (child.nodeType === 11) {
      for (const nested of [...child.childNodes]) this.appendChild(nested);
      return child;
    }
    child.remove();
    this._childNodes.push(child);
    child._parentNode = this;
    return child;
  }
  append(...children) { for (const child of children) this.appendChild(child); }
  prepend(child) { this.insertBefore(child, this.firstChild); }
  insertBefore(child, before) {
    child.remove();
    const index = before == null ? -1 : this._childNodes.indexOf(before);
    if (index < 0) return this.appendChild(child);
    this._childNodes.splice(index, 0, child);
    child._parentNode = this;
    return child;
  }
  removeChild(child) {
    const index = this._childNodes.indexOf(child);
    if (index < 0) throw new Error('child not found');
    this._childNodes.splice(index, 1);
    child._parentNode = null;
    return child;
  }
  remove() { if (this._parentNode) this._parentNode.removeChild(this); }
  replaceChildren(...children) {
    for (const child of this._childNodes) child._parentNode = null;
    this._childNodes = [];
    for (const child of children) this.appendChild(child);
  }
  cloneNode(deep = false) {
    const clone = new MiniNode(this.nodeType, this.nodeName, this.ownerDocument);
    if (deep) for (const child of this.childNodes) clone.appendChild(child.cloneNode(true));
    return clone;
  }
  get textContent() { return this._childNodes.map(child => child.textContent).join(''); }
  set textContent(value) {
    this.replaceChildren();
    if (String(value)) this.appendChild(this.ownerDocument.createTextNode(String(value)));
  }
}

class MiniText extends MiniNode {
  constructor(text, ownerDocument) { super(3, '#text', ownerDocument); this.data = String(text); }
  get textContent() { return this.data; }
  set textContent(value) { this.data = String(value); }
  cloneNode() { return new MiniText(this.data, this.ownerDocument); }
}

class MiniComment extends MiniNode {
  constructor(text, ownerDocument) { super(8, '#comment', ownerDocument); this.data = String(text); }
  get textContent() { return this.data; }
  set textContent(value) { this.data = String(value); }
  cloneNode() { return new MiniComment(this.data, this.ownerDocument); }
}

class MiniFragment extends MiniNode {
  constructor(ownerDocument) { super(11, '#document-fragment', ownerDocument); }
  cloneNode(deep = false) {
    const clone = new MiniFragment(this.ownerDocument);
    if (deep) for (const child of this.childNodes) clone.appendChild(child.cloneNode(true));
    return clone;
  }
}

class MiniElement extends MiniNode {
  constructor(tag, ownerDocument) {
    super(1, String(tag).toUpperCase(), ownerDocument);
    this.namespaceURI = HTML_NS;
    this._attributes = [];
    this.dataset = {};
    this.style = { setProperty: () => {}, display: '' };
    this.listeners = {};
    this.hidden = false;
    this.disabled = false;
    this.value = '';
    this.offsetWidth = 40;
    this.offsetLeft = 0;
  }
  get tagName() { return this.nodeName; }
  get attributes() { return this._attributes; }
  get shadowRoot() { return null; }
  get className() { return this.getAttribute('class') || ''; }
  set className(value) { this.setAttribute('class', value); }
  get id() { return this.getAttribute('id') || ''; }
  set id(value) { this.setAttribute('id', value); }
  setAttribute(name, value) {
    const existing = this._attributes.find(attr => attr.name === String(name));
    if (existing) existing.value = String(value);
    else this._attributes.push({ name: String(name), value: String(value), namespaceURI: null });
  }
  setAttributeNS(namespaceURI, name, value) {
    this.setAttribute(name, value);
    this._attributes.find(attr => attr.name === String(name)).namespaceURI = namespaceURI;
  }
  getAttribute(name) { return this._attributes.find(attr => attr.name === String(name))?.value ?? null; }
  getAttributeNode(name) { return this._attributes.find(attr => attr.name === String(name)) || null; }
  hasAttribute(name) { return this.getAttributeNode(name) != null; }
  removeAttribute(name) {
    const attr = this.getAttributeNode(name);
    if (attr) this.removeAttributeNode(attr);
  }
  removeAttributeNode(attr) {
    const index = this._attributes.indexOf(attr);
    if (index >= 0) this._attributes.splice(index, 1);
    return attr;
  }
  getAttributeNames() { return this._attributes.map(attr => attr.name); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  dispatchEvent(event) { return this.listeners[event.type]?.(event); }
  click() { if (!this.disabled) return this.listeners.click?.(); }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  contains(node) { for (let next = node; next; next = next.parentNode) if (next === this) return true; return false; }
  scrollIntoView() {}
  get classList() {
    const element = this;
    const tokens = () => element.className.split(/\s+/).filter(Boolean);
    const write = values => { element.className = [...new Set(values)].join(' '); };
    return {
      add: (...names) => write([...tokens(), ...names]),
      remove: (...names) => write(tokens().filter(name => !names.includes(name))),
      contains: name => tokens().includes(name),
      toggle: (name, on) => {
        const enabled = on === undefined ? !tokens().includes(name) : Boolean(on);
        if (enabled) write([...tokens(), name]);
        else write(tokens().filter(token => token !== name));
        return enabled;
      },
    };
  }
  get innerHTML() { return this.childNodes.map(serialize).join(''); }
  set innerHTML(value) { parseInto(this, String(value)); }
  get outerHTML() { return serialize(this); }
  cloneNode(deep = false) {
    const clone = new MiniElement(this.tagName, this.ownerDocument);
    for (const attr of this.attributes) clone.setAttributeNS(attr.namespaceURI, attr.name, attr.value);
    if (deep) for (const child of this.childNodes) clone.appendChild(child.cloneNode(true));
    return clone;
  }
}

class MiniDocument extends MiniNode {
  constructor() {
    super(9, '#document', null);
    this._ownerDocument = this;
    this.documentElement = new MiniElement('body', this);
    this.body = this.documentElement;
    this._childNodes = [this.documentElement];
    this.documentElement._parentNode = this;
    this.currentScript = null;
    this.implementation = {
      createHTMLDocument: () => new MiniDocument(),
      createDocument: () => new MiniDocument(),
    };
  }
  createElement(tag) { return new MiniElement(tag, this); }
  createElementNS(_namespace, tag) { return this.createElement(tag); }
  createTextNode(text) { return new MiniText(text, this); }
  createDocumentFragment() { return new MiniFragment(this); }
  importNode(node, deep) { return node.cloneNode(Boolean(deep)); }
  createNodeIterator(root) {
    const nodes = [];
    const visit = node => { nodes.push(node); for (const child of node.childNodes || []) visit(child); };
    visit(root);
    let index = 0;
    return { nextNode: () => nodes[index++] || null };
  }
  getElementsByTagName(tag) {
    const wanted = String(tag).toUpperCase();
    const found = [];
    const visit = node => {
      if (node.nodeType === 1 && node.tagName === wanted) found.push(node);
      for (const child of node.childNodes || []) visit(child);
    };
    visit(this.documentElement);
    return found;
  }
}

function parseInto(parent, html) {
  parent.replaceChildren();
  const stack = [parent];
  const tokens = String(html).match(/<!--[\s\S]*?-->|<\/?[A-Za-z][^>]*>|[^<]+|</g) || [];
  for (const token of tokens) {
    const current = stack[stack.length - 1];
    if (token.startsWith('<!--')) {
      current.appendChild(new MiniComment(token.slice(4, -3), parent.ownerDocument));
    } else if (token.startsWith('</')) {
      if (stack.length > 1) stack.pop();
    } else if (token.startsWith('<') && token.length > 1) {
      const match = /^<([A-Za-z][\w:-]*)([\s\S]*?)\/?\s*>$/.exec(token);
      if (!match) continue;
      const element = parent.ownerDocument.createElement(match[1]);
      const attributes = match[2];
      const pattern = /([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;
      let attr;
      while ((attr = pattern.exec(attributes))) element.setAttribute(attr[1], attr[2] ?? attr[3] ?? attr[4] ?? '');
      current.appendChild(element);
      if (!token.endsWith('/>') && !VOID.has(match[1].toLowerCase())) stack.push(element);
    } else if (token) {
      current.appendChild(parent.ownerDocument.createTextNode(token));
    }
  }
}

function serialize(node) {
  if (node.nodeType === 3) return escapeText(node.data);
  if (node.nodeType === 8) return `<!--${node.data}-->`;
  if (node.nodeType === 11) return node.childNodes.map(serialize).join('');
  const tag = node.tagName.toLowerCase();
  const attrs = node.attributes.map(attr => ` ${attr.name}="${escapeAttr(attr.value)}"`).join('');
  return VOID.has(tag) ? `<${tag}${attrs}>` : `<${tag}${attrs}>${node.childNodes.map(serialize).join('')}</${tag}>`;
}

const document = new MiniDocument();
const byId = new Map();
document.getElementById = id => {
  if (!byId.has(id)) byId.set(id, document.createElement('div'));
  return byId.get(id);
};
document.querySelectorAll = () => [];
document.querySelector = () => null;
document.addEventListener = () => {};
document.hidden = false;

const location = new URL('http://dashboard.test/?workspace=one');
const window = {
  document,
  Element: MiniElement,
  Node: MiniNode,
  DocumentFragment: MiniFragment,
  NodeFilter: {
    SHOW_ELEMENT: 1,
    SHOW_TEXT: 4,
    SHOW_CDATA_SECTION: 8,
    SHOW_PROCESSING_INSTRUCTION: 64,
    SHOW_COMMENT: 128,
  },
  location,
  localStorage: { getItem: () => null, setItem: () => {} },
  addEventListener: () => {},
};

globalThis.document = document;
globalThis.window = window;
globalThis.Element = MiniElement;
globalThis.Node = MiniNode;
globalThis.DocumentFragment = MiniFragment;
globalThis.NodeFilter = window.NodeFilter;
globalThis.history = { replaceState: () => {} };
Object.defineProperty(globalThis, 'navigator', {
  value: { clipboard: { writeText: () => Promise.resolve() } },
  configurable: true,
});
globalThis.requestAnimationFrame = fn => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { close() {} };
