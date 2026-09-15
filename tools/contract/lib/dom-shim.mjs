// A DOM small enough to read, big enough to render the dashboard.
//
// The golden-render test needs to run `dashboard.js` (a browser IIFE) in node
// and serialise what it built. It does NOT need a browser: the file touches
// exactly 21 DOM members, they are all structural, and the chart itself is a
// string the renderer concatenates rather than a tree it builds.
//
// The reason not to reach for jsdom is that the test compares two renders of
// the SAME page against each other, frozen against parameterised. Any place
// this shim is less than a browser is a place both sides are equally less than
// a browser, so the comparison still proves what it claims to prove, and the
// test gains no dependency, no lockfile and no install step. What the shim
// cannot prove is that the page looks right in a browser, and it does not
// claim to.
//
// Serialisation is deterministic: attributes come out in the order they were
// set, children in the order they were appended, and raw innerHTML is emitted
// verbatim rather than reparsed, so nothing here can normalise a difference
// away.

const VOID = new Set(['br', 'hr', 'img', 'input', 'meta', 'link']);

const esc = (s) =>
  String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');

class Raw {
  constructor(html) {
    this.html = html;
  }
}
class Text {
  constructor(text) {
    this.text = text;
  }
}

class Element {
  constructor(tag) {
    this.tagName = tag;
    this.attrs = new Map();
    this.children = [];
    this.dataset = {};
    this.listeners = [];
    this.styles = new Map();
    this.classes = new Set();
    this.style = {
      setProperty: (k, v) => this.styles.set(k, v),
      removeProperty: (k) => this.styles.delete(k),
    };
    this.classList = {
      add: (...c) => c.forEach((x) => this.classes.add(x)),
      remove: (...c) => c.forEach((x) => this.classes.delete(x)),
      contains: (c) => this.classes.has(c),
      toggle: (c, on) => (on === undefined ? (this.classes.has(c) ? this.classes.delete(c) : this.classes.add(c)) : on ? this.classes.add(c) : this.classes.delete(c)),
    };
  }

  set className(v) {
    this._className = v;
  }
  get className() {
    return this._className ?? '';
  }

  set innerHTML(v) {
    this.children = v === '' ? [] : [new Raw(String(v))];
  }
  get innerHTML() {
    return this.children.map(serializeNode).join('');
  }

  set textContent(v) {
    this.children = [new Text(String(v))];
  }
  get textContent() {
    return this.children.map((c) => (c instanceof Text ? c.text : c instanceof Raw ? c.html : c.textContent)).join('');
  }

  setAttribute(k, v) {
    this.attrs.set(k, String(v));
  }
  getAttribute(k) {
    return this.attrs.has(k) ? this.attrs.get(k) : null;
  }
  removeAttribute(k) {
    this.attrs.delete(k);
  }
  appendChild(child) {
    this.children.push(child);
    return child;
  }
  append(...kids) {
    for (const k of kids) this.children.push(typeof k === 'string' ? new Text(k) : k);
  }
  remove() {
    /* detached trees are never re-serialised, so this is a no-op by design */
  }
  addEventListener(type, fn) {
    this.listeners.push([type, fn]);
  }
  removeEventListener(type, fn) {
    this.listeners = this.listeners.filter(([t, f]) => !(t === type && f === fn));
  }
  querySelector() {
    return null;
  }
  querySelectorAll() {
    return [];
  }
}

/** Serialise one node. Attribute order is: class, then the element's own
 *  properties in a fixed order, then `data-*` in insertion order, then
 *  setAttribute()s in insertion order, then style. Fixed so a render is
 *  reproducible; the ORDER itself is arbitrary and both sides get the same. */
function serializeNode(node) {
  if (node instanceof Raw) return node.html;
  if (node instanceof Text) return esc(node.text);

  const parts = [];
  const cls = [node.className, ...node.classes].filter(Boolean).join(' ');
  if (cls) parts.push(`class="${esc(cls)}"`);
  for (const prop of ['id', 'type', 'href', 'title', 'value']) {
    if (node[prop] !== undefined) parts.push(`${prop}="${esc(node[prop])}"`);
  }
  for (const flag of ['checked', 'open', 'disabled']) {
    if (node[flag]) parts.push(flag);
  }
  for (const [k, v] of Object.entries(node.dataset)) {
    parts.push(`data-${k.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)}="${esc(v)}"`);
  }
  for (const [k, v] of node.attrs) parts.push(`${k}="${esc(v)}"`);
  if (node.styles.size > 0) {
    const s = [...node.styles].map(([k, v]) => `${k}:${v}`).join(';');
    parts.push(`style="${esc(s)}"`);
  }
  if (node.listeners.length > 0) {
    // Listeners are not markup, but "a listener was attached" is part of what
    // a render did, and a parameterisation that dropped one would otherwise
    // compare equal. Counted by type, never by identity.
    const byType = {};
    for (const [t] of node.listeners) byType[t] = (byType[t] ?? 0) + 1;
    parts.push(
      `data-shim-listeners="${Object.keys(byType).sort().map((t) => `${t}:${byType[t]}`).join(',')}"`,
    );
  }

  const open = `<${node.tagName}${parts.length ? ` ${parts.join(' ')}` : ''}>`;
  if (VOID.has(node.tagName)) return open;
  return `${open}${node.children.map(serializeNode).join('')}</${node.tagName}>`;
}

export function createDocument({ readyState = 'complete' } = {}) {
  const root = new Element('div');
  root.id = 'dashboard-root';

  const document = {
    readyState,
    createElement: (tag) => new Element(tag),
    createTextNode: (t) => new Text(t),
    querySelector: (sel) => (sel === '#dashboard-root' ? root : null),
    addEventListener: () => {},
    body: new Element('body'),
  };

  return { document, root, serialize: () => serializeNode(root) };
}

export { serializeNode, Element };
