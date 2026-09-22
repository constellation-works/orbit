// Drives the shipped Plugins module against a fetch stub [§4.7]: the four
// render modes, link tiles, a plugin that is not active, and the markdown
// XSS fixture. Nothing here names a plugin the dashboard knows about — the
// fixture data is the only input, which is the point of the generic
// renderer.
const assert = (condition, message) => { if (!condition) throw new Error(message); };
await import('./marked.umd.js');
await import('./purify.min.js');
assert(globalThis.DOMPurify?.isSupported, 'the Plugins harness must run the vendored DOMPurify runtime');
const sanitizerProbe = globalThis.DOMPurify.sanitize('<p>kept</p><script>removed()</script>');
assert(sanitizerProbe.includes('<p>kept</p>'), `DOMPurify dropped safe markdown output: ${sanitizerProbe}`);
assert(!sanitizerProbe.includes('<script'), `DOMPurify did not strip a script from markdown output: ${sanitizerProbe}`);

const { setWorkspace } = await import('./common.js');
const { fetchAndRenderPlugins } = await import('./plugins.js');

const descendants = node => [node, ...(node.children || []).flatMap(descendants)];
const hasClass = (node, name) => String(node.className || '').split(/\s+/).includes(name);
const body = () => document.getElementById('plugins-body');
const withClass = name => descendants(body()).filter(node => hasClass(node, name));
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

const XSS_MARKDOWN = '# Report\n\n<img src=x onerror="alert(1)"> <script>alert(2)</script>\n';

const plugins = [
  {
    name: 'graph',
    version: '0.4.1',
    status: 'active',
    enabled: true,
    description: 'Leakage-safe recommendations.',
    pinned: true,
    unsandboxed: false,
    certified_orbit_version: '0.23.0',
    diagnostic: null,
    tools: [{ name: 'graph.status', advertised_name: 'graph_status', execution_kind: 'read_only', mcp_scope: 'workspace', active: true }],
    panels: [
      { id: 'status', title: 'Graph index', tool: 'graph.status', render: 'kv', group: 'diagnostics' },
      { id: 'files', title: 'Hot files', tool: 'graph.files', render: 'table', group: 'diagnostics' },
      { id: 'notes', title: 'Notes', tool: 'graph.notes', render: 'markdown', group: 'diagnostics' },
      { id: 'raw', title: 'Raw', tool: 'graph.raw', render: 'json', group: 'diagnostics' },
    ],
    links: [
      { title: 'Graph explorer', url: 'http://127.0.0.1:7890/' },
      { title: 'Hostile tile', url: 'javascript:alert(1)' },
    ],
  },
  {
    name: 'stale',
    version: '0.1.0',
    status: 'inactive',
    enabled: false,
    description: 'A plugin this host cannot serve.',
    pinned: false,
    unsandboxed: true,
    certified_orbit_version: null,
    diagnostic: "plugin 'stale' requests `fs` but this host has not granted it",
    tools: [],
    panels: [],
    links: [],
  },
];

const panelOutputs = {
  'graph/status': { indexed_files: 1284, last_run: '2026-09-20T02:00:00Z' },
  'graph/files': [{ path: 'src/a.rs', score: 0.91 }, { path: 'src/b.rs', score: 0.4, note: 'new' }],
  'graph/notes': XSS_MARKDOWN,
  'graph/raw': { nested: { ok: true } },
};

const requested = [];
globalThis.fetch = async path => {
  const url = String(path);
  requested.push(url);
  const panel = /\/api\/plugins\/([^/]+)\/panels\/([^?]+)/.exec(url);
  const payload = panel
    ? {
        output: panelOutputs[`${decodeURIComponent(panel[1])}/${decodeURIComponent(panel[2])}`],
        ...(decodeURIComponent(panel[2]) === 'raw'
          ? { truncated: true, diagnostic: 'Panel output exceeded the response limit.' }
          : {}),
      }
    : plugins;
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};

setWorkspace('ws_one');
await fetchAndRenderPlugins();
await tick();
await tick();

// One card per plugin, and the count reflects them.
const cards = withClass('plugin-card');
assert(cards.length === 2, `expected one card per plugin, got ${cards.length}`);
assert(document.getElementById('plugins-count').textContent === '2', 'the panel count states how many plugins are installed');

// Every panel was read through its own endpoint — the list response never
// carries panel output, so a mutating source could not be smuggled in.
for (const panel of ['status', 'files', 'notes', 'raw']) {
  assert(
    requested.some(url => url.includes(`/api/plugins/graph/panels/${panel}`)),
    `panel '${panel}' must be read from its own endpoint: ${requested}`,
  );
}

// kv: label/value rows from an object.
const kvKeys = withClass('plugin-kv-key').map(node => node.textContent);
const kvValues = withClass('plugin-kv-value').map(node => node.textContent);
assert(kvKeys.includes('indexed_files') && kvValues.includes('1284'), `kv panel rendered ${kvKeys} / ${kvValues}`);

// table: the union of the rows' keys becomes the columns.
const tables = withClass('plugin-table');
assert(tables.length === 1, `expected one table panel, got ${tables.length}`);
const headers = descendants(tables[0]).filter(node => node.tagName === 'TH' || node.children.length === 0 && node.textContent === 'note');
const tableText = tables[0].textContent;
for (const column of ['path', 'score', 'note']) {
  assert(tableText.includes(column), `table must carry the '${column}' column: ${tableText}`);
}
assert(tableText.includes('src/a.rs') && tableText.includes('0.91'), `table must carry its rows: ${tableText}`);
assert(headers.length === 3, `headers are rendered as cells: ${headers.length}`);

// json: the raw answer, pretty-printed.
const json = withClass('plugin-json');
assert(json.length === 1 && json[0].textContent.includes('"nested"'), `json panel rendered ${json.map(node => node.textContent)}`);
assert(body().textContent.includes('Panel output exceeded the response limit.'), 'a truncation diagnostic is visible beside the bounded panel output');

// markdown: rendered through the real vendored sanitizer loaded above. Raw
// HTML is escaped by the marked wrapper and DOMPurify is the final boundary.
const markdown = withClass('markdown-body');
assert(markdown.length === 1, `expected one markdown panel, got ${markdown.length}`);
const rendered = markdown[0].textContent;
assert(rendered.includes('Report'), `the markdown panel renders its source: ${rendered}`);
const dangerous = descendants(body()).filter(node =>
  String(node.tagName || '').toUpperCase() === 'SCRIPT' || node.onerror != null || node.onload != null);
assert(dangerous.length === 0, 'plugin markdown must never create a script element or an event handler');
assert(
  !withClass('markdown-body').some(node => (node.children || []).some(child => String(child.tagName).toUpperCase() === 'IMG')),
  'the XSS fixture renders inert',
);

// Link tiles are data, opened in a new tab with no referrer.
const tiles = withClass('plugin-link-tile');
assert(tiles.length === 1, `expected one drawable link tile, got ${tiles.length}`);
assert(tiles[0].href === 'http://127.0.0.1:7890/', `link tile href was ${tiles[0].href}`);
assert(String(tiles[0].rel).includes('noopener'), 'a plugin link opens without handing over the opener');
assert(
  !descendants(body()).some(node => String(node.href || '').toLowerCase().startsWith('javascript:')),
  'a non-http link tile is never drawn as an anchor',
);

// A plugin that is not serving its tools is listed with the reason.
const inactive = cards.find(card => card.dataset.key === 'stale');
assert(inactive && hasClass(inactive, 'plugin-inactive'), 'an inactive plugin is marked as such');
assert(inactive.textContent.includes('has not granted it'), `the diagnostic is shown: ${inactive.textContent}`);
assert(inactive.textContent.includes('unsandboxed'), 'an unsandboxed plugin says so');

// The certified version reads back from the plugin record.
const active = cards.find(card => card.dataset.key === 'graph');
assert(active.textContent.includes('certified for 0.23.0'), `certification is shown: ${active.textContent}`);

console.log('dashboard plugins panel assertions passed');
