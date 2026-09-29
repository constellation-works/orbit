// Plugins tab: installed plugins, their declared panels, and their link
// tiles [§4.7].
//
// One generic renderer serves every plugin. A panel names a render mode in
// its manifest (`kv`, `table`, `markdown`, `json`) and the server returns the
// source tool's JSON; nothing here knows a plugin's name, and no plugin ships
// JavaScript. Markdown goes through the same sanitizing wrapper the rest of
// the dashboard uses, so plugin-authored text cannot introduce script or
// event handlers.

import { el, fetchJson, getWorkspace, getWorkspaceRevision, isAggregateView, isHttpUrl, postJson, renderPanelPlaceholder, requestPanel, syncNodes } from './common.js';
import { renderMarkdown } from './markdown.js';

const $ = (id) => document.getElementById(id);

// Last good panel read for this workspace visit, keyed
// `<revision>\0<ns>/<id>`. A 30 s refresh repaints that read instead of
// blanking. Another visit — including a return to the same workspace —
// misses, so A's output cannot appear under B while B is pending, failed,
// or already showing its own read. `panelBodies` names the body still
// mounted under the retained plugin card. `panelReads` is the newest
// in-flight read for a cache key; a response that loses the race does not
// write the cache or that body.
const panelCache = new Map();
const panelBodies = new Map();
const panelReads = new Map();
let lastPlugins = [];
const pendingChanges = new Set();
const changeErrors = new Map();

export async function fetchAndRenderPlugins() {
  if (isAggregateView()) {
    renderPanelPlaceholder('plugins-body');
    $('plugins-count').textContent = '—';
    return;
  }
  await requestPanel(
    'plugins-body',
    `plugins:${getWorkspace() || ''}`,
    () => fetchJson('/api/plugins'),
    payload => {
      lastPlugins = Array.isArray(payload) ? payload : [];
      panelBodies.clear();
      render(lastPlugins);
      for (const plugin of lastPlugins) {
        for (const panel of plugin.panels || []) loadPanel(plugin, panel);
      }
    },
    'plugins-count',
  );
}

function render(plugins) {
  retainCurrentPanelCache();
  const body = $('plugins-body');
  if (!body) return;
  const count = $('plugins-count');
  if (count) count.textContent = String(plugins.length);
  if (!plugins.length) {
    syncNodes(body, [el('div', { class: 'panel-placeholder', text: 'No plugins are installed on this machine. `orbit plugin add <source>` installs one.' })]);
    panelBodies.clear();
    return;
  }
  syncNodes(body, plugins.map(pluginCard));
  // A matching metadata hash keeps the previous card and discards the one
  // just built. Register the bodies that remain in this container.
  bindLivePanelBodies(body);
}

function bindLivePanelBodies(root) {
  panelBodies.clear();
  const visit = (node) => {
    for (const child of node.children || []) {
      const key = child.dataset && child.dataset.panel;
      if (key) {
        const panelBody = Array.from(child.children || []).find(candidate =>
          String(candidate.className || '').split(/\s+/).includes('plugin-panel-body'));
        if (panelBody) panelBodies.set(key, panelBody);
      }
      visit(child);
    }
  };
  visit(root);
}

function pluginCard(plugin) {
  const card = el('div', { class: `plugin-card plugin-${plugin.status}` });
  card.dataset.key = plugin.name;
  card.dataset.hash = JSON.stringify([plugin.status, plugin.version, plugin.diagnostic, plugin.certified_orbit_version, plugin.host_enabled, plugin.workspace_toggle, plugin.disabled_by, plugin.capabilities, pendingChanges.has(plugin.name), changeErrors.get(plugin.name), (plugin.panels || []).map(panel => panel.id), (plugin.links || []).map(link => link.url)]);
  card.appendChild(el('div', { class: 'plugin-head' }, [
    el('span', { class: 'plugin-name', text: plugin.name }),
    el('span', { class: 'plugin-version', text: `v${plugin.version || '—'}` }),
    el('span', { class: `plugin-status status-${plugin.status}`, text: plugin.status }),
    plugin.pinned ? el('span', { class: 'plugin-chip', text: 'pinned' }) : null,
    plugin.unsandboxed ? el('span', { class: 'plugin-chip plugin-chip-warn', text: 'unsandboxed' }) : null,
    plugin.certified_orbit_version ? el('span', { class: 'plugin-chip', text: `certified for ${plugin.certified_orbit_version}` }) : null,
  ].filter(Boolean)));
  if (plugin.description) card.appendChild(el('p', { class: 'plugin-description', text: plugin.description }));
  // The diagnostic is the whole reason a non-active plugin is listed at all.
  if (plugin.diagnostic) card.appendChild(el('p', { class: 'plugin-diagnostic', text: plugin.diagnostic }));
  card.appendChild(pluginEnablement(plugin));
  if (changeErrors.has(plugin.name)) card.appendChild(el('p', { class: 'plugin-change-error', text: changeErrors.get(plugin.name) }));
  const tools = plugin.tools || [];
  if (tools.length) {
    card.appendChild(el('div', { class: 'plugin-tools' }, tools.map(tool =>
      el('span', { class: `plugin-chip ${tool.active ? '' : 'plugin-chip-idle'}`, title: `${tool.execution_kind} · MCP ${tool.advertised_name || 'not advertised'}`, text: tool.name }))));
  }
  // The manifest refuses non-http(s) links at validation; this is the second
  // half of that rule, for a record written before it or by a hand-edited store.
  const links = (plugin.links || []).filter(link => isHttpUrl(link.url));
  if (links.length) {
    card.appendChild(el('div', { class: 'plugin-links' }, links.map(link => {
      const tile = el('a', { class: 'plugin-link-tile', text: link.title });
      tile.href = link.url;
      tile.rel = 'noreferrer noopener';
      tile.target = '_blank';
      return tile;
    })));
  }
  for (const panel of plugin.panels || []) card.appendChild(panelNode(plugin, panel));
  return card;
}

function pluginEnablement(plugin) {
  const section = el('div', { class: 'plugin-enablement' });
  const states = [
    ['host', plugin.host_enabled === true],
    ['workspace', plugin.workspace_toggle !== false],
  ];
  for (const [scope, enabled] of states) {
    const row = el('div', { class: `plugin-scope plugin-scope-${scope}` });
    row.appendChild(el('span', { text: `${scope === 'host' ? 'Host' : 'Workspace'}: ${enabled ? 'enabled' : 'disabled'}${scope === 'workspace' && plugin.workspace_toggle == null ? ' (inherited)' : ''}` }));
    const action = enabled ? 'disable' : 'enable';
    if (plugin.capabilities?.[action]?.authorized === true) {
      const button = el('button', { class: 'plugin-toggle', text: `${action === 'enable' ? 'Enable' : 'Disable'} ${scope}` });
      button.type = 'button';
      button.disabled = pendingChanges.has(plugin.name);
      button.addEventListener('click', () => changePlugin(plugin, scope, action, section));
      row.appendChild(button);
    }
    section.appendChild(row);
  }
  if (plugin.disabled_by) section.appendChild(el('span', { class: 'plugin-disabled-by', text: `Effective: disabled by ${plugin.disabled_by}` }));
  return section;
}

async function changePlugin(plugin, scope, action, section) {
  if (pendingChanges.has(plugin.name)) return;
  if (scope === 'host' && action === 'disable' && !window.confirm(`Disable ${plugin.name} on this host? It will be unavailable in every workspace on this host.`)) return;
  pendingChanges.add(plugin.name);
  changeErrors.delete(plugin.name);
  const buttons = Array.from(section.children).flatMap(row => Array.from(row.children)).filter(node => node.tagName === 'BUTTON');
  for (const button of buttons) button.disabled = true;
  try {
    await postJson(`/api/plugins/${encodeURIComponent(plugin.name)}/${action}`, { scope });
    await fetchAndRenderPlugins();
  } catch (error) {
    changeErrors.set(plugin.name, error.message || String(error));
  } finally {
    pendingChanges.delete(plugin.name);
    for (const button of buttons) button.disabled = false;
    render(lastPlugins);
  }
}

function panelNode(plugin, panel) {
  const key = panelKey(plugin, panel);
  const node = el('section', { class: `plugin-panel plugin-panel-${panel.group}` });
  node.dataset.panel = key;
  node.appendChild(el('header', { class: 'plugin-panel-head' }, [
    el('span', { class: 'plugin-panel-title', text: panel.title || panel.id }),
    el('span', { class: 'plugin-panel-source', text: panel.tool }),
  ]));
  const body = el('div', { class: 'plugin-panel-body' });
  renderPanelBody(body, panel, panelCache.get(panelCacheKey(plugin, panel)));
  node.appendChild(body);
  return node;
}

const panelKey = (plugin, panel) => `${plugin.name}/${panel.id}`;

function panelCacheKey(plugin, panel) {
  return `${getWorkspaceRevision()}\0${panelKey(plugin, panel)}`;
}

// Forget every other visit before painting this one. A late read still
// carries its own revision and is dropped below; this only keeps the map
// from handing that visit's payload to the panel on screen.
function retainCurrentPanelCache() {
  const prefix = `${getWorkspaceRevision()}\0`;
  for (const key of panelCache.keys()) {
    if (!key.startsWith(prefix)) panelCache.delete(key);
  }
  for (const key of panelReads.keys()) {
    if (!key.startsWith(prefix)) panelReads.delete(key);
  }
}

async function loadPanel(plugin, panel) {
  const key = panelKey(plugin, panel);
  const revision = getWorkspaceRevision();
  const cacheKey = `${revision}\0${key}`;
  const token = {};
  panelReads.set(cacheKey, token);
  let state;
  try {
    const payload = await fetchJson(`/api/plugins/${encodeURIComponent(plugin.name)}/panels/${encodeURIComponent(panel.id)}`);
    state = {
      output: payload?.output,
      diagnostic: typeof payload?.diagnostic === 'string' ? payload.diagnostic : null,
      truncated: payload?.truncated === true,
    };
  } catch (error) {
    state = { error: error.message || String(error) };
  }
  // A→B and A→B→A both move the revision. A newer read on the same visit
  // replaces the token. Either way the late payload must not land in the
  // live body or in the cache that body will paint from.
  if (revision !== getWorkspaceRevision() || panelReads.get(cacheKey) !== token) return;
  panelCache.set(cacheKey, state);
  // The body bindLivePanelBodies left mounted for this key. The replacement
  // built for an unchanged card is not in the document, so a response must
  // not follow that discarded node.
  const node = panelBodies.get(key);
  if (node) renderPanelBody(node, panel, state);
}

function renderPanelBody(node, panel, state) {
  if (!state) {
    syncNodes(node, [el('div', { class: 'panel-placeholder', text: 'Loading…' })]);
    return;
  }
  if (state.error) {
    syncNodes(node, [el('div', { class: 'panel-placeholder action-error', text: state.error })]);
    return;
  }
  const nodes = renderNodes(panel.render, state.output);
  if (state.diagnostic) {
    nodes.unshift(el('div', {
      class: `panel-placeholder ${state.truncated ? 'action-error' : ''}`.trim(),
      text: state.diagnostic,
    }));
  }
  syncNodes(node, nodes);
}

// The four render modes of §4.7. An output that does not fit the declared
// mode falls back to `json` rather than rendering nothing: the panel is a
// diagnostic surface, and the raw answer is more useful than an empty card.
function renderNodes(mode, output) {
  switch (mode) {
    case 'kv':
      return kvNodes(output);
    case 'table':
      return tableNodes(output);
    case 'markdown':
      return markdownNodes(output);
    default:
      return jsonNodes(output);
  }
}

function kvNodes(output) {
  if (!output || typeof output !== 'object' || Array.isArray(output)) return jsonNodes(output);
  const rows = Object.entries(output).map(([key, value]) => el('div', { class: 'plugin-kv-row' }, [
    el('span', { class: 'plugin-kv-key', text: key }),
    el('span', { class: 'plugin-kv-value', text: scalarText(value) }),
  ]));
  return rows.length ? [el('div', { class: 'plugin-kv' }, rows)] : emptyNodes();
}

function tableNodes(output) {
  const rows = Array.isArray(output) ? output : Array.isArray(output?.rows) ? output.rows : null;
  if (!rows) return jsonNodes(output);
  if (!rows.length) return emptyNodes();
  const columns = [];
  for (const row of rows) {
    if (!row || typeof row !== 'object') return jsonNodes(output);
    for (const key of Object.keys(row)) if (!columns.includes(key)) columns.push(key);
  }
  const table = el('table', { class: 'plugin-table' });
  const head = el('thead');
  head.appendChild(el('tr', {}, columns.map(column => el('th', { text: column }))));
  table.appendChild(head);
  const bodyRows = rows.map(row => el('tr', {}, columns.map(column => el('td', { text: scalarText(row[column]) }))));
  table.appendChild(el('tbody', {}, bodyRows));
  return [table];
}

function markdownNodes(output) {
  const source = typeof output === 'string'
    ? output
    : typeof output?.markdown === 'string'
      ? output.markdown
      : typeof output?.text === 'string'
        ? output.text
        : null;
  if (source == null) return jsonNodes(output);
  const view = el('div', { class: 'markdown-body' });
  // `renderMarkdown` parses with the dashboard's own renderer (raw HTML in
  // the source is escaped, not passed through) and then sanitizes. A plugin
  // is untrusted text here, exactly like a task comment.
  const rendered = renderMarkdown(source);
  if (rendered == null) view.textContent = source;
  else view.innerHTML = rendered;
  return [view];
}

function jsonNodes(output) {
  if (output === undefined) return emptyNodes();
  return [el('pre', { class: 'plugin-json', text: JSON.stringify(output, null, 2) })];
}

const emptyNodes = () => [el('div', { class: 'panel-placeholder', text: 'No data.' })];

function scalarText(value) {
  if (value == null) return '—';
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}
