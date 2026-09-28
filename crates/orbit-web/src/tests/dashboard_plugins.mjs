// Drives the shipped Plugins module against a fetch stub [§4.7]: the four
// render modes, link tiles, a plugin that is not active, and the markdown
// XSS fixture. Nothing here names a plugin the dashboard knows about — the
// fixture data is the only input, which is the point of the generic
// renderer.
const assert = (condition, message) => { if (!condition) throw new Error(message); };
await import('./vendor/marked.umd.js');
await import('./vendor/purify.min.js');
assert(globalThis.DOMPurify?.isSupported, 'the Plugins harness must run the vendored DOMPurify runtime');
const sanitizerProbe = globalThis.DOMPurify.sanitize('<p>kept</p><script>removed()</script>');
assert(sanitizerProbe.includes('<p>kept</p>'), `DOMPurify dropped safe markdown output: ${sanitizerProbe}`);
assert(!sanitizerProbe.includes('<script'), `DOMPurify did not strip a script from markdown output: ${sanitizerProbe}`);

const { setWorkspace } = await import('./js/common.js');
const { fetchAndRenderPlugins } = await import('./js/plugins.js');

const descendants = node => [node, ...(node.children || []).flatMap(descendants)];
const hasClass = (node, name) => String(node.className || '').split(/\s+/).includes(name);
const body = () => document.getElementById('plugins-body');
const withClass = name => descendants(body()).filter(node => hasClass(node, name));
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

const XSS_MARKDOWN = '# Report\n\n<img src=x onerror="alert(1)"> <script>alert(2)</script>\n';
const allowed = { authorized: true, reason: null };
const denied = { authorized: false, reason: 'Operator session required' };

const plugins = [
  {
    name: 'graph',
    version: '0.4.1',
    status: 'active',
    enabled: true,
    host_enabled: true,
    workspace_toggle: null,
    disabled_by: null,
    capabilities: { enable: allowed, disable: allowed },
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
    host_enabled: false,
    workspace_toggle: false,
    disabled_by: 'host',
    capabilities: { enable: allowed, disable: allowed },
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
const confirmations = [];
const panelFailures = new Set();
const deferredPanels = [];
let deferPanelReads = false;
window.confirm = message => { confirmations.push(message); return true; };
globalThis.fetch = async (path, options = {}) => {
  const url = String(path);
  requested.push(url);
  const change = /\/api\/plugins\/([^/?]+)\/(enable|disable)/.exec(url);
  if (change && options.method === 'POST') {
    const plugin = plugins.find(item => item.name === decodeURIComponent(change[1]));
    const { scope } = JSON.parse(options.body);
    assert(plugin, 'mutation names a listed plugin');
    if (scope === 'host') plugin.host_enabled = change[2] === 'enable';
    else plugin.workspace_toggle = change[2] === 'enable';
    return { ok: true, status: 200, text: async () => JSON.stringify({ plugin }) };
  }
  const panel = /\/api\/plugins\/([^/]+)\/panels\/([^?]+)/.exec(url);
  if (panel) {
    const key = `${decodeURIComponent(panel[1])}/${decodeURIComponent(panel[2])}`;
    const workspace = new URL(url, 'http://dashboard.test').searchParams.get('workspace');
    const output = panelOutputs[key];
    const payload = decodeURIComponent(panel[2]) === 'raw'
      ? { output, truncated: true, diagnostic: 'Panel output exceeded the response limit.' }
      : { output };
    const response = panelFailures.has(key)
      ? { ok: false, status: 503, json: async () => ({ error: 'panel source unavailable' }), text: async () => JSON.stringify({ error: 'panel source unavailable' }) }
      : { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
    // Snapshot the payload now. A later workspace's output must not rewrite
    // a read that is still in flight.
    if (deferPanelReads) {
      return new Promise(resolve => {
        deferredPanels.push({ workspace, key, release: () => resolve(response) });
      });
    }
    return response;
  }
  return { ok: true, status: 200, json: async () => plugins, text: async () => JSON.stringify(plugins) };
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

// Each scope shows its state and an operator control. A write sends exactly
// the selected scope and rereads the list so the button follows the new state.
const button = (name, label) => descendants(withClass('plugin-card').find(card => card.dataset.key === name))
  .find(node => node.tagName === 'BUTTON' && node.textContent === label);
assert(button('graph', 'Disable host') && button('graph', 'Disable workspace'), 'operator sees controls for both enabled scopes');
assert(button('stale', 'Enable host') && button('stale', 'Enable workspace'), 'operator sees controls for both disabled scopes');
assert(inactive.textContent.includes('disabled by host'), 'the effective disabled layer is visible');
button('graph', 'Disable workspace').click();
await tick();
await tick();
assert(requested.some(url => url.includes('/api/plugins/graph/disable')), 'workspace disable posts to plugin endpoint');
assert(button('graph', 'Enable workspace'), 'workspace control refreshes to enable after write');
button('graph', 'Disable host').click();
await tick();
await tick();
assert(confirmations[0].includes('every workspace'), 'host disable asks about its host-wide effect');
assert(button('graph', 'Enable host'), 'host state refreshes without a page restart');

for (const plugin of plugins) plugin.capabilities = { enable: denied, disable: denied };
await fetchAndRenderPlugins();
assert(withClass('plugin-toggle').length === 0, 'non-operator session sees no mutation controls');

// A refresh whose plugin metadata is unchanged keeps the mounted card.
// Only the panel read changes, and that read has to land in the card the
// document still shows.
const mountedPanel = (panelId) => {
  const section = descendants(body()).find(node => node.dataset && node.dataset.panel === panelId);
  assert(section, `panel ${panelId} is mounted in the live document`);
  const panelBody = (section.children || []).find(node => hasClass(node, 'plugin-panel-body'));
  assert(panelBody, `panel ${panelId} has a body in the live document`);
  return { section, panelBody };
};
const retainedCard = withClass('plugin-card').find(card => card.dataset.key === 'graph');
const retainedStatus = mountedPanel('graph/status');
assert(retainedStatus.panelBody.textContent.includes('1284'), `status panel starts from its first read: ${retainedStatus.panelBody.textContent}`);

panelOutputs['graph/status'] = { indexed_files: 2048, last_run: '2026-09-21T02:00:00Z' };
await fetchAndRenderPlugins();
await tick();
await tick();

const refreshedCard = withClass('plugin-card').find(card => card.dataset.key === 'graph');
const refreshedStatus = mountedPanel('graph/status');
assert(refreshedCard === retainedCard, 'unchanged plugin metadata retains the plugin card');
assert(refreshedStatus.section === retainedStatus.section, 'unchanged plugin metadata retains the panel section');
assert(refreshedStatus.panelBody === retainedStatus.panelBody, 'panel output refreshes in the retained body');
assert(refreshedStatus.panelBody.textContent.includes('2048'), `live panel shows the new output: ${refreshedStatus.panelBody.textContent}`);
assert(!refreshedStatus.panelBody.textContent.includes('1284'), `live panel dropped the previous output: ${refreshedStatus.panelBody.textContent}`);
assert(
  descendants(body()).find(node => node.dataset && node.dataset.panel === 'graph/files').textContent.includes('src/a.rs'),
  'a panel whose output did not change stays visible on the retained card',
);

panelFailures.add('graph/status');
await fetchAndRenderPlugins();
await tick();
await tick();
const failedStatus = mountedPanel('graph/status');
assert(failedStatus.panelBody === retainedStatus.panelBody, 'a failed refresh addresses the retained body');
assert(failedStatus.panelBody.textContent.includes('panel source unavailable'), `live panel shows the refresh failure: ${failedStatus.panelBody.textContent}`);
assert(!failedStatus.panelBody.textContent.includes('2048'), `the failure replaces the previous output on the live body: ${failedStatus.panelBody.textContent}`);

panelFailures.delete('graph/status');
panelOutputs['graph/status'] = { indexed_files: 4096, last_run: '2026-09-22T02:00:00Z' };
await fetchAndRenderPlugins();
await tick();
await tick();
const recoveredStatus = mountedPanel('graph/status');
assert(recoveredStatus.panelBody === retainedStatus.panelBody, 'recovery paints the same live body');
assert(recoveredStatus.panelBody.textContent.includes('4096'), `live panel shows the recovered output: ${recoveredStatus.panelBody.textContent}`);
assert(!recoveredStatus.panelBody.textContent.includes('panel source unavailable'), `recovery clears the failure diagnostic: ${recoveredStatus.panelBody.textContent}`);

const cardBeforeMetadataChange = refreshedCard;
plugins.find(plugin => plugin.name === 'graph').version = '0.4.2';
await fetchAndRenderPlugins();
await tick();
await tick();
const cardAfterMetadataChange = withClass('plugin-card').find(card => card.dataset.key === 'graph');
const replacedStatus = mountedPanel('graph/status');
assert(cardAfterMetadataChange !== cardBeforeMetadataChange, 'a metadata change replaces the plugin card');
assert(!descendants(body()).includes(cardBeforeMetadataChange), 'the replaced card leaves the live document');
assert(replacedStatus.panelBody !== retainedStatus.panelBody, 'a replaced card mounts a new panel body');
assert(replacedStatus.panelBody.textContent.includes('4096'), `the replacement card shows the current panel output: ${replacedStatus.panelBody.textContent}`);

// Workspace identity. The same plugin panel exists in A and B. A's cached
// read and a response that was already in flight must not appear on B,
// including while B is still pending or its read fails. A→B→A drops the
// first visit's response as well.
const settle = async () => { await tick(); await tick(); };
const panelText = (panelId) => mountedPanel(panelId).panelBody.textContent;
const takeDeferred = () => deferredPanels.splice(0);
const releaseDeferred = (items) => { for (const item of items) item.release(); };
const graph = plugins.find(plugin => plugin.name === 'graph');

panelOutputs['graph/status'] = { indexed_files: 1111, last_run: 'workspace-a-cache' };
panelOutputs['graph/files'] = [{ path: 'from-workspace-a.rs', score: 0.11 }];
await fetchAndRenderPlugins();
await settle();
assert(panelText('graph/status').includes('1111'), `workspace A cached its status read: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('from-workspace-a.rs'), `workspace A cached its files read: ${panelText('graph/files')}`);

deferPanelReads = true;
panelFailures.add('graph/status');
panelOutputs['graph/status'] = { indexed_files: 3333, last_run: 'workspace-b' };
panelOutputs['graph/files'] = [{ path: 'from-workspace-b.rs', score: 0.33 }];
setWorkspace('ws_two');
await fetchAndRenderPlugins();
assert(panelText('graph/status').includes('Loading'), `pending B status must wait for its own read: ${panelText('graph/status')}`);
assert(!panelText('graph/status').includes('1111') && !panelText('graph/status').includes('workspace-a-cache'), `pending B status painted A's cache: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('Loading'), `pending B files must wait for its own read: ${panelText('graph/files')}`);
assert(!panelText('graph/files').includes('from-workspace-a.rs'), `pending B files painted A's cache: ${panelText('graph/files')}`);
const heldFailure = takeDeferred();
assert(heldFailure.some(item => item.workspace === 'ws_two' && item.key === 'graph/status'), 'B status read is the one still in flight');
releaseDeferred(heldFailure);
await settle();
assert(panelText('graph/status').includes('panel source unavailable'), `failed B status shows its own error: ${panelText('graph/status')}`);
assert(!panelText('graph/status').includes('1111') && !panelText('graph/status').includes('workspace-a-cache') && !panelText('graph/status').includes('3333'), `failed B status kept another workspace's result: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('from-workspace-b.rs'), `B files shows its own read after the sibling status failure: ${panelText('graph/files')}`);
assert(!panelText('graph/files').includes('from-workspace-a.rs'), `B files kept A's rows: ${panelText('graph/files')}`);
panelFailures.delete('graph/status');

// Hold A's next read, let B render completely, then deliver A. B's live
// panel and the cache a remount paints from both stay on B.
deferPanelReads = true;
panelOutputs['graph/status'] = { indexed_files: 2222, last_run: 'late-a' };
panelOutputs['graph/files'] = [{ path: 'late-a.rs', score: 0.22 }];
setWorkspace('ws_one');
await fetchAndRenderPlugins();
const lateA = takeDeferred();
assert(lateA.some(item => item.workspace === 'ws_one' && item.key === 'graph/status'), 'A status read is held');
panelOutputs['graph/status'] = { indexed_files: 3333, last_run: 'live-b' };
panelOutputs['graph/files'] = [{ path: 'live-b.rs', score: 0.33 }];
deferPanelReads = false;
setWorkspace('ws_two');
await fetchAndRenderPlugins();
await settle();
assert(panelText('graph/status').includes('3333'), `B rendered its own status: ${panelText('graph/status')}`);
assert(!panelText('graph/status').includes('2222') && !panelText('graph/status').includes('1111'), `B rendered before A's late read: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('live-b.rs'), `B rendered its own files: ${panelText('graph/files')}`);
releaseDeferred(lateA);
await settle();
assert(panelText('graph/status').includes('3333') && !panelText('graph/status').includes('2222') && !panelText('graph/status').includes('late-a'), `late A did not change B's live status: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('live-b.rs') && !panelText('graph/files').includes('late-a.rs'), `late A did not change B's live files: ${panelText('graph/files')}`);
graph.version = '0.4.3';
deferPanelReads = true;
await fetchAndRenderPlugins();
assert(panelText('graph/status').includes('3333') && !panelText('graph/status').includes('2222') && !panelText('graph/status').includes('late-a'), `remount paints B's cache, not late A: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('live-b.rs') && !panelText('graph/files').includes('late-a.rs'), `remount paints B's files cache, not late A: ${panelText('graph/files')}`);
releaseDeferred(takeDeferred());
await settle();

// A→B→A. The first visit's response is still in flight across both
// switches and must not fill the return visit, before or after that visit's
// own read lands.
deferPanelReads = true;
panelOutputs['graph/status'] = { indexed_files: 4444, last_run: 'a-first' };
panelOutputs['graph/files'] = [{ path: 'a-first.rs', score: 0.44 }];
setWorkspace('ws_one');
await fetchAndRenderPlugins();
const aFirst = takeDeferred();
panelOutputs['graph/status'] = { indexed_files: 5555, last_run: 'b-middle' };
panelOutputs['graph/files'] = [{ path: 'b-middle.rs', score: 0.55 }];
setWorkspace('ws_two');
await fetchAndRenderPlugins();
const bMiddle = takeDeferred();
assert(!panelText('graph/status').includes('4444') && !panelText('graph/status').includes('a-first'), `B pending during A→B→A hid A: ${panelText('graph/status')}`);
releaseDeferred(bMiddle);
await settle();
assert(panelText('graph/status').includes('5555'), `middle B rendered: ${panelText('graph/status')}`);
panelOutputs['graph/status'] = { indexed_files: 6666, last_run: 'a-second' };
panelOutputs['graph/files'] = [{ path: 'a-second.rs', score: 0.66 }];
setWorkspace('ws_one');
await fetchAndRenderPlugins();
const aSecond = takeDeferred();
assert(panelText('graph/status').includes('Loading'), `return visit waits for its own status read: ${panelText('graph/status')}`);
assert(!panelText('graph/status').includes('4444') && !panelText('graph/status').includes('5555'), `return visit hid both earlier reads: ${panelText('graph/status')}`);
assert(!panelText('graph/files').includes('a-first.rs') && !panelText('graph/files').includes('b-middle.rs'), `return visit hid both earlier file reads: ${panelText('graph/files')}`);
releaseDeferred(aFirst);
await settle();
assert(panelText('graph/status').includes('Loading') && !panelText('graph/status').includes('4444') && !panelText('graph/status').includes('a-first'), `stale A did not paint the return visit: ${panelText('graph/status')}`);
assert(!panelText('graph/files').includes('a-first.rs'), `stale A did not paint the return visit's files: ${panelText('graph/files')}`);
graph.version = '0.4.4';
await fetchAndRenderPlugins();
assert(panelText('graph/status').includes('Loading') && !panelText('graph/status').includes('4444') && !panelText('graph/status').includes('5555'), `return-visit remount did not revive a stale read: ${panelText('graph/status')}`);
assert(!panelText('graph/files').includes('a-first.rs') && !panelText('graph/files').includes('b-middle.rs'), `return-visit remount did not revive stale files: ${panelText('graph/files')}`);
const aReturn = takeDeferred();
releaseDeferred(aSecond);
await settle();
assert(panelText('graph/status').includes('Loading') && !panelText('graph/status').includes('6666'), `superseded return-visit read does not paint over the newer mount: ${panelText('graph/status')}`);
releaseDeferred(aReturn);
await settle();
assert(panelText('graph/status').includes('6666') && !panelText('graph/status').includes('4444') && !panelText('graph/status').includes('5555'), `return visit shows its own status read: ${panelText('graph/status')}`);
assert(panelText('graph/files').includes('a-second.rs') && !panelText('graph/files').includes('a-first.rs') && !panelText('graph/files').includes('b-middle.rs'), `return visit shows its own files read: ${panelText('graph/files')}`);
deferPanelReads = false;

console.log('dashboard plugins panel assertions passed');
