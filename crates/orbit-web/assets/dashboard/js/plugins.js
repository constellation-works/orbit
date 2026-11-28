// Plugins tab: installed plugins, their declared panels, link tiles [§4.7],
// and secondary sub-tabs for declared plugin pages (plugin-pages design §4).
//
// One generic renderer serves every plugin panel. A panel names a render mode
// in its manifest (`kv`, `table`, `markdown`, `json`) and the server returns
// the source tool's JSON; markdown goes through the same sanitizing wrapper
// the rest of the dashboard uses, so plugin-authored text cannot introduce
// script or event handlers. Declared pages render in their own sandboxed frame
// via `plugin-page.js`.

import { el, fetchJson, getWorkspace, getWorkspaceRevision, hostWriteRefusal, isAggregateView, isHttpUrl, onWorkspaceChange, postJson, renderPanelPlaceholder, requestPanel, syncNodes } from './common.js';
import { renderMarkdown } from './markdown.js';
import { buildPluginPageHash, getActivePluginPage, updatePluginPageCatalog } from './plugin-page.js';

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
let lastPluginsRevision = null;
const pendingChanges = new Set();
const changeErrors = new Map();
let onTabsChange = () => {};
let lastTabsSignature = null;
let pluginTabLayoutKey = '';
let pluginTabStripObserver = null;
let pluginRailObserver = null;
let pluginTabMenuCloserBound = false;
let pluginTabFontsHooked = false;

// A pinned title that cannot take its natural width still shows a few
// characters plus the subtab's horizontal padding, never an empty control.
const PLUGIN_TAB_LABEL_FLOOR_PX = 72;

export function initPlugins({ onTabsChange: listener = () => {} } = {}) {
  onTabsChange = typeof listener === 'function' ? listener : () => {};
}

function enabledPluginPages(plugins) {
  const entries = [];
  for (const plugin of Array.isArray(plugins) ? plugins : []) {
    if (plugin?.status !== 'active') continue;
    for (const page of plugin.pages || []) {
      if (!page?.id) continue;
      entries.push({
        namespace: plugin.name,
        pageId: page.id,
        title: page.title || page.id,
        route: `plugins/${encodeURIComponent(plugin.name)}/${encodeURIComponent(page.id)}`,
      });
    }
  }
  return entries;
}

/// Sync the Plugins secondary tab bar and rail views from the active workspace's
/// plugins. Hidden when no enabled plugin declares a page and Installed is open,
/// so plugins with panels alone keep the existing Plugins tab unchanged.
export function syncPluginTabs(activePage = getActivePluginPage()) {
  const plugins = lastPluginsRevision === getWorkspaceRevision() ? lastPlugins : [];
  const pages = enabledPluginPages(plugins);
  const showBar = pages.length > 0 || Boolean(activePage);
  const activeKey = activePage ? `${activePage.namespace}\0${activePage.page}` : '';
  const signature = JSON.stringify({
    showBar,
    activeKey,
    pages: pages.map(entry => [entry.namespace, entry.pageId, entry.title]),
  });

  const bar = $('plugins-subtabs');
  const rail = $('plugins-rail-subtabs');
  if (bar) bar.hidden = !showBar;
  if (signature === lastTabsSignature) {
    onTabsChange();
    return;
  }
  lastTabsSignature = signature;

  if (bar) {
    if (!showBar) {
      bar.replaceChildren();
    } else {
      const buttons = [
        pluginTabButton({
          label: 'Installed',
          route: 'plugins',
          active: !activePage,
          inRail: false,
        }),
        ...pages.map(entry => pluginTabButton({
          label: entry.title,
          route: entry.route,
          active: activeKey === `${entry.namespace}\0${entry.pageId}`,
          inRail: false,
        })),
      ];
      mountPluginTabBar(bar, buttons);
    }
  }

  if (rail) {
    if (!pages.length) {
      rail.replaceChildren();
    } else {
      const buttons = [
        pluginTabButton({
          label: 'Installed',
          route: 'plugins',
          active: !activePage,
          inRail: true,
        }),
        ...pages.map(entry => pluginTabButton({
          label: entry.title,
          route: entry.route,
          active: activeKey === `${entry.namespace}\0${entry.pageId}`,
          inRail: true,
        })),
      ];
      rail.replaceChildren(...buttons);
      markRailPluginTabs();
      watchPluginRailTruncation(rail);
    }
  }
  onTabsChange();
}

function pluginTabButton({ label, route, active, inRail }) {
  const labelNode = el('span', {
    class: inRail ? 'rail-label plugin-tab-label' : 'plugin-tab-label',
    text: label,
  });
  const full = el('span', { class: 'plugin-tab-full', text: label });
  full.setAttribute('aria-hidden', 'true');
  const button = el('button', {
    class: active ? 'subtab active' : 'subtab',
    type: 'button',
    'data-route': route,
    'aria-current': active ? 'page' : 'false',
  }, [labelNode, full]);
  bindPluginTabReveal(button);
  return button;
}

// The strip is one row. Pages that do not fit at their title width move
// under More; the open page stays in the row. The track clips, so a dense
// catalog does not widen the page.
function mountPluginTabBar(bar, buttons) {
  const track = el('div', { class: 'plugin-tab-track' });
  const more = pluginTabMoreButton();
  more.hidden = true;
  track.append(...buttons, more);
  const menu = el('div', {
    class: 'plugin-tab-menu',
    id: 'plugins-tab-menu',
    role: 'menu',
    'aria-label': 'More plugin pages',
  });
  menu.hidden = true;
  menu.addEventListener('keydown', (event) => onPluginTabMenuKeydown(bar, event));
  menu.addEventListener('click', (event) => {
    if (!event.target?.closest?.('[data-route]')) return;
    closePluginTabMenu(bar, { focusMore: false });
  });
  bar.replaceChildren(track, menu);
  pluginTabLayoutKey = '';
  layoutPluginTabStrip(bar, { force: true });
  watchPluginTabStrip(bar);
  hookPluginTabFonts(bar);
}

function pluginTabMoreButton() {
  const button = el('button', {
    class: 'subtab plugin-tab-more',
    type: 'button',
    'aria-haspopup': 'menu',
    'aria-expanded': 'false',
    'aria-controls': 'plugins-tab-menu',
  }, [
    el('span', { text: 'More' }),
    el('span', { class: 'plugin-tab-more-count', text: '' }),
  ]);
  button.addEventListener('click', (event) => {
    event.stopPropagation();
    const bar = button.closest('#plugins-subtabs');
    const menu = bar?.querySelector('.plugin-tab-menu');
    if (!bar || !menu) return;
    if (menu.hidden) openPluginTabMenu(bar);
    else closePluginTabMenu(bar, { focusMore: false });
  });
  button.addEventListener('keydown', (event) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    const bar = button.closest('#plugins-subtabs');
    if (!bar) return;
    event.preventDefault();
    openPluginTabMenu(bar, { focusEnd: event.key === 'ArrowUp' });
  });
  return button;
}

function layoutPluginTabStrip(bar, { force = false } = {}) {
  if (!bar || bar.hidden) return;
  const track = bar.querySelector('.plugin-tab-track');
  const more = track?.querySelector('.plugin-tab-more');
  const menu = bar.querySelector('.plugin-tab-menu');
  if (!track || !more || !menu) return;
  const tabs = [...track.querySelectorAll('.subtab[data-route]')];
  if (!tabs.length) return;
  const available = track.clientWidth;
  if (available <= 0) return;

  const activeIndex = Math.max(0, tabs.findIndex((tab) => tab.classList.contains('active')));
  const key = `${available}|${activeIndex}|${tabs.map((tab) => tab.dataset.route).join('\n')}`;
  if (!force && key === pluginTabLayoutKey) return;

  const wasOpen = !menu.hidden;
  const gap = trackGap(track);
  for (const tab of tabs) {
    tab.hidden = false;
    tab.style.maxWidth = '';
  }
  more.hidden = true;
  const natural = tabs.map((tab) => tab.offsetWidth);
  const all = tabs.map((_, index) => index);
  if (rowWidth(all, natural, gap, 0) <= available + 1) {
    applyPluginTabLayout(tabs, more, menu, new Set(), natural, natural);
    pluginTabLayoutKey = key;
    if (wasOpen) closePluginTabMenu(bar, { focusMore: false });
    return;
  }

  const optional = all.filter((index) => index !== 0 && index !== activeIndex);
  const must = activeIndex === 0 ? [0] : [0, activeIndex];
  more.hidden = false;
  setPluginTabMoreCount(more, optional.length);
  let moreWidth = more.offsetWidth;
  const widths = natural.slice();
  if (activeIndex > 0) {
    const others = rowWidth(must, widths, gap, moreWidth) - widths[activeIndex];
    const room = available - others;
    if (room < widths[activeIndex]) {
      widths[activeIndex] = Math.max(PLUGIN_TAB_LABEL_FLOOR_PX, Math.min(widths[activeIndex], room));
    }
  }

  const hidden = new Set(optional);
  const shown = [];
  const admit = () => {
    for (const index of optional) {
      if (!hidden.has(index)) continue;
      const trial = rowWidth([...must, ...shown, index], widths, gap, moreWidth);
      if (trial <= available + 1) {
        shown.push(index);
        hidden.delete(index);
      }
    }
  };
  admit();
  setPluginTabMoreCount(more, Math.max(hidden.size, 1));
  moreWidth = more.offsetWidth;
  admit();

  if (hidden.size === 0 && rowWidth(all, widths, gap, 0) <= available + 1) {
    applyPluginTabLayout(tabs, more, menu, new Set(), widths, natural);
    pluginTabLayoutKey = key;
    if (wasOpen) closePluginTabMenu(bar, { focusMore: false });
    return;
  }

  applyPluginTabLayout(tabs, more, menu, hidden, widths, natural);
  pluginTabLayoutKey = key;
  if (hidden.size && wasOpen) {
    const menuNow = bar.querySelector('.plugin-tab-menu');
    const moreNow = bar.querySelector('.plugin-tab-more');
    if (menuNow && moreNow) {
      menuNow.hidden = false;
      moreNow.setAttribute('aria-expanded', 'true');
      placePluginTabMenu(menuNow, moreNow);
      for (const item of menuNow.querySelectorAll('.subtab')) markPluginTabTruncation(item);
    }
  }
}

function applyPluginTabLayout(tabs, more, menu, hidden, widths, natural) {
  tabs.forEach((tab, index) => {
    tab.hidden = hidden.has(index);
    if (!tab.hidden && widths[index] < natural[index] - 1) {
      tab.style.maxWidth = `${Math.floor(widths[index])}px`;
    } else {
      tab.style.maxWidth = '';
    }
    if (!tab.hidden) markPluginTabTruncation(tab);
    else tab.classList.remove('is-truncated');
  });
  const overflow = tabs.filter((_, index) => hidden.has(index));
  if (!overflow.length) {
    more.hidden = true;
    more.setAttribute('aria-expanded', 'false');
    more.removeAttribute('aria-label');
    menu.hidden = true;
    menu.replaceChildren();
    return;
  }
  more.hidden = false;
  setPluginTabMoreCount(more, overflow.length);
  const label = overflow.length === 1 ? '1 more page' : `${overflow.length} more pages`;
  more.setAttribute('aria-label', label);
  fillPluginTabMenu(menu, overflow);
  if (menu.hidden) more.setAttribute('aria-expanded', 'false');
}

function fillPluginTabMenu(menu, tabs) {
  const items = tabs.map((tab) => {
    const label = tab.querySelector('.plugin-tab-label')?.textContent || '';
    const button = pluginTabButton({
      label,
      route: tab.dataset.route,
      active: tab.classList.contains('active'),
      inRail: false,
    });
    button.classList.add('plugin-tab-menu-item');
    button.setAttribute('role', 'menuitem');
    return button;
  });
  menu.replaceChildren(...items);
}

function rowWidth(indexes, widths, gap, moreWidth) {
  let total = 0;
  for (const index of indexes) total += widths[index];
  const count = indexes.length + (moreWidth ? 1 : 0);
  if (!count) return 0;
  return total + (moreWidth || 0) + gap * (count - 1);
}

function trackGap(track) {
  const style = getComputedStyle(track);
  const gap = parseFloat(style.columnGap || style.gap);
  return Number.isFinite(gap) ? gap : 0;
}

function setPluginTabMoreCount(more, count) {
  const node = more.querySelector('.plugin-tab-more-count');
  if (node) node.textContent = String(count);
}

function openPluginTabMenu(bar, { focusEnd = false } = {}) {
  const menu = bar.querySelector('.plugin-tab-menu');
  const more = bar.querySelector('.plugin-tab-more');
  if (!menu || !more || more.hidden || !menu.childElementCount) return;
  menu.hidden = false;
  more.setAttribute('aria-expanded', 'true');
  for (const tab of bar.querySelectorAll('.subtab.is-revealed')) tab.classList.remove('is-revealed');
  placePluginTabMenu(menu, more);
  for (const item of menu.querySelectorAll('.subtab')) markPluginTabTruncation(item);
  const items = [...menu.querySelectorAll('[role="menuitem"]')];
  (focusEnd ? items[items.length - 1] : items[0])?.focus();
  ensurePluginTabMenuCloser();
}

function closePluginTabMenu(bar, { focusMore = false } = {}) {
  const menu = bar?.querySelector('.plugin-tab-menu');
  const more = bar?.querySelector('.plugin-tab-more');
  if (menu) menu.hidden = true;
  if (more) more.setAttribute('aria-expanded', 'false');
  if (focusMore) more?.focus();
}

function placePluginTabMenu(menu, more) {
  const gutter = 8;
  const rect = more.getBoundingClientRect();
  const bottomReserve = window.innerWidth <= 760 ? 72 : gutter;
  const width = menu.offsetWidth;
  const height = menu.offsetHeight;
  let left = rect.right - width;
  left = Math.max(gutter, Math.min(left, window.innerWidth - width - gutter));
  let top = rect.bottom + 4;
  if (top + height > window.innerHeight - bottomReserve) {
    const above = rect.top - height - 4;
    top = above >= gutter ? above : Math.max(gutter, window.innerHeight - bottomReserve - height);
  }
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;
}

function onPluginTabMenuKeydown(bar, event) {
  const menu = bar.querySelector('.plugin-tab-menu');
  const items = [...menu.querySelectorAll('[role="menuitem"]')];
  if (!items.length) return;
  const index = items.indexOf(document.activeElement);
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    items[(index + 1) % items.length].focus();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    items[index <= 0 ? items.length - 1 : index - 1].focus();
  } else if (event.key === 'Home') {
    event.preventDefault();
    items[0].focus();
  } else if (event.key === 'End') {
    event.preventDefault();
    items[items.length - 1].focus();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    event.stopPropagation();
    closePluginTabMenu(bar, { focusMore: true });
  }
}

function ensurePluginTabMenuCloser() {
  if (pluginTabMenuCloserBound) return;
  pluginTabMenuCloserBound = true;
  document.addEventListener('pointerdown', (event) => {
    const bar = $('plugins-subtabs');
    const menu = bar?.querySelector('.plugin-tab-menu');
    if (!menu || menu.hidden) return;
    const target = event.target;
    if (target instanceof Node && target.closest('.plugin-tab-more, .plugin-tab-menu')) return;
    closePluginTabMenu(bar, { focusMore: false });
  });
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    const bar = $('plugins-subtabs');
    const menu = bar?.querySelector('.plugin-tab-menu');
    if (!menu || menu.hidden) return;
    if (menu.contains(document.activeElement)) return;
    event.preventDefault();
    closePluginTabMenu(bar, { focusMore: true });
  });
}

function watchPluginTabStrip(bar) {
  if (typeof ResizeObserver === 'undefined') return;
  if (!pluginTabStripObserver) {
    pluginTabStripObserver = new ResizeObserver(() => layoutPluginTabStrip(bar));
  }
  pluginTabStripObserver.observe(bar);
}

function hookPluginTabFonts(bar) {
  if (pluginTabFontsHooked || !document.fonts?.ready) return;
  pluginTabFontsHooked = true;
  document.fonts.ready.then(() => {
    pluginTabLayoutKey = '';
    layoutPluginTabStrip(bar, { force: true });
    markRailPluginTabs();
  }).catch(() => {});
}

function bindPluginTabReveal(button) {
  const place = () => placePluginTabFullName(button);
  button.addEventListener('mouseenter', place);
  button.addEventListener('focus', place);
  button.addEventListener('pointerdown', (event) => {
    if (event.pointerType === 'mouse') return;
    button.classList.add('is-revealed');
    place();
    window.setTimeout(() => button.classList.remove('is-revealed'), 1500);
  });
}

function placePluginTabFullName(button) {
  if (!button.classList.contains('is-truncated')) return;
  const bubble = button.querySelector('.plugin-tab-full');
  if (!bubble) return;
  const gutter = 8;
  const anchor = button.getBoundingClientRect();
  const width = Math.min(bubble.scrollWidth, window.innerWidth - gutter * 2);
  const height = Math.max(bubble.scrollHeight, bubble.getBoundingClientRect().height);
  let left = anchor.left;
  if (left + width > window.innerWidth - gutter) {
    left = Math.max(gutter, window.innerWidth - gutter - width);
  }
  const bottomReserve = window.innerWidth <= 760 ? 72 : gutter;
  let top = anchor.bottom + 4;
  if (top + height > window.innerHeight - bottomReserve) {
    top = Math.max(gutter, anchor.top - height - 4);
  }
  button.style.setProperty('--plugin-tab-full-left', `${Math.round(left)}px`);
  button.style.setProperty('--plugin-tab-full-top', `${Math.round(top)}px`);
}

function markPluginTabTruncation(tab) {
  const label = tab.querySelector('.plugin-tab-label');
  if (!label || tab.hidden || label.clientWidth === 0) {
    tab.classList.remove('is-truncated');
    return;
  }
  tab.classList.toggle('is-truncated', label.scrollWidth > label.clientWidth + 1);
}

function markRailPluginTabs() {
  const rail = $('plugins-rail-subtabs');
  if (!rail) return;
  for (const tab of rail.querySelectorAll('.subtab[data-route]')) markPluginTabTruncation(tab);
}

function watchPluginRailTruncation(rail) {
  if (!rail || typeof ResizeObserver === 'undefined') return;
  if (!pluginRailObserver) {
    pluginRailObserver = new ResizeObserver(() => markRailPluginTabs());
  }
  pluginRailObserver.observe(rail);
}

onWorkspaceChange(() => {
  changeErrors.clear();
  lastPlugins = [];
  lastPluginsRevision = null;
  lastTabsSignature = null;
  syncPluginTabs();
});

export async function fetchAndRenderPlugins() {
  const revision = getWorkspaceRevision();
  if (isAggregateView()) {
    renderPanelPlaceholder('plugins-body');
    $('plugins-count').textContent = '—';
    lastPlugins = [];
    lastPluginsRevision = revision;
    syncPluginTabs();
    updatePluginPageCatalog([], { revision, aggregate: true });
    return;
  }
  try {
    await requestPanel(
      'plugins-body',
      `plugins:${getWorkspace() || ''}`,
      () => fetchJson('/api/plugins'),
      payload => {
        lastPlugins = Array.isArray(payload) ? payload : [];
        lastPluginsRevision = getWorkspaceRevision();
        panelBodies.clear();
        syncPluginTabs();
        updatePluginPageCatalog(lastPlugins, { revision: lastPluginsRevision });
        render(lastPlugins);
        if (!getActivePluginPage()) {
          for (const plugin of lastPlugins) {
            for (const panel of plugin.panels || []) loadPanel(plugin, panel);
          }
        }
      },
      'plugins-count',
    );
  } catch (error) {
    if (revision === getWorkspaceRevision()) {
      updatePluginPageCatalog(null, { revision, error });
    }
    throw error;
  }
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
  const card = el('div', { class: `card card-dense plugin-card plugin-${plugin.status}` });
  card.dataset.key = plugin.name;
  // A card renders the listing's metadata, not just version and enablement.
  // Pins, sandbox status, tool activation and manifest labels may change while
  // the version stays the same; retaining that card would display stale state.
  card.dataset.hash = JSON.stringify([plugin, pendingChanges.has(plugin.name), changeErrors.get(plugin.name)]);
  card.appendChild(el('div', { class: 'plugin-head' }, [
    el('span', { class: 'plugin-name', text: plugin.name }),
    el('span', { class: 'plugin-version', text: `v${plugin.version || '—'}` }),
    el('span', { class: `plugin-status status-${plugin.status}`, text: plugin.status }),
    plugin.pinned ? el('span', { class: 'badge', text: 'pinned' }) : null,
    plugin.unsandboxed ? el('span', { class: 'badge', 'data-tone': 'warning', text: 'unsandboxed' }) : null,
    certificationChip(plugin),
  ].filter(Boolean)));
  if (plugin.description) card.appendChild(el('p', { class: 'plugin-description', text: plugin.description }));
  // The diagnostic is the whole reason a non-active plugin is listed at all.
  if (plugin.diagnostic) card.appendChild(el('p', { class: 'plugin-diagnostic', text: plugin.diagnostic }));
  card.appendChild(pluginEnablement(plugin));
  if (changeErrors.has(plugin.name)) card.appendChild(el('p', { class: 'plugin-change-error', text: changeErrors.get(plugin.name) }));
  const tools = plugin.tools || [];
  if (tools.length) card.appendChild(toolInventory(plugin, tools));
  const pages = (plugin.pages || []).filter(page => page && page.id);
  if (pages.length) {
    card.appendChild(el('div', { class: 'plugin-pages' }, pages.map(page => {
      const inactive = page.state !== 'active';
      const tile = el('a', {
        class: `plugin-page-tile${inactive ? ' is-inactive' : ''}`,
      }, [
        el('span', { class: 'plugin-page-tile-title', text: page.title || page.id }),
        inactive ? el('span', { class: 'badge', 'data-tone': 'warning', text: page.state || 'inactive' }) : null,
      ].filter(Boolean));
      tile.href = buildPluginPageHash(plugin.name, page.id);
      return tile;
    })));
    if (plugin.status === 'active') {
      for (const page of pages) {
        if (page.diagnostic) card.appendChild(el('p', { class: 'plugin-diagnostic', text: page.diagnostic }));
      }
    }
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

const toolChip = tool =>
  el('span', { class: 'badge mono', 'data-tone': tool.active ? null : 'muted', title: `${tool.name} · ${tool.execution_kind} · MCP ${tool.advertised_name || 'not advertised'}`, text: tool.name });

// A plugin can expose dozens of tools. Past TOOL_PREVIEW the card shows the
// first few, a filter, and a toggle that reveals the rest inside a bounded,
// scrollable list, so one plugin cannot push its neighbours out of reach.
// Disclosure and filter state live in `toolViews`, keyed by plugin name,
// because a metadata change rebuilds the card.
const TOOL_PREVIEW = 8;
const toolViews = new Map();
let toolListSeq = 0;

function toolInventory(plugin, tools) {
  if (tools.length <= TOOL_PREVIEW) return el('div', { class: 'plugin-tools' }, tools.map(toolChip));
  const view = toolViews.get(plugin.name) || { expanded: false, query: '' };
  toolViews.set(plugin.name, view);
  const listId = `plugin-tools-${toolListSeq++}`;
  const list = el('div', { class: 'plugin-tools', id: listId, role: 'group', 'aria-label': `${plugin.name} tools` });
  const status = el('span', { class: 'plugin-tools-status', role: 'status' });
  const filter = el('input', { class: 'plugin-tools-filter', type: 'search', 'aria-label': `Filter ${plugin.name} tools` });
  filter.placeholder = 'Filter tools';
  filter.value = view.query;
  filter.autocomplete = 'off';
  const toggle = el('button', { class: 'plugin-tools-toggle', type: 'button', 'aria-controls': listId });

  const paint = () => {
    const query = view.query.trim().toLowerCase();
    const matches = query
      ? tools.filter(tool => `${tool.name} ${tool.advertised_name || ''}`.toLowerCase().includes(query))
      : tools;
    const shown = view.expanded ? matches : matches.slice(0, TOOL_PREVIEW);
    list.replaceChildren(...shown.map(toolChip));
    // A scrollable region must be keyboard-focusable to be scrolled.
    list.classList.toggle('plugin-tools-scroll', view.expanded);
    if (view.expanded) list.tabIndex = 0;
    else list.removeAttribute('tabindex');
    toggle.setAttribute('aria-expanded', String(view.expanded));
    toggle.textContent = view.expanded ? 'Show fewer' : `Show all ${tools.length} tools`;
    status.textContent = query
      ? `${matches.length} of ${tools.length} tools match`
      : view.expanded ? `${tools.length} tools` : `${shown.length} of ${tools.length} tools`;
  };
  filter.addEventListener('input', () => {
    view.query = filter.value;
    // Matches beyond the preview must be visible, so filtering opens the list.
    if (view.query.trim()) view.expanded = true;
    paint();
  });
  toggle.addEventListener('click', () => {
    view.expanded = !view.expanded;
    if (!view.expanded) {
      view.query = '';
      filter.value = '';
    }
    paint();
  });
  paint();
  return el('div', { class: 'plugin-tool-inventory' }, [
    el('div', { class: 'plugin-tools-bar' }, [filter, toggle, status]),
    list,
  ]);
}

// Compare SemVer precedence, including release candidates; build metadata
// does not make a certification older. Unknown versions cannot prove lag.
function certificationBehind(certified, host) {
  const parse = value => {
    const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*))?(?:\+[\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*)?$/.exec(value || '');
    if (!match) return null;
    const pre = match[4]?.split('.') || [];
    if (pre.some(part => /^\d+$/.test(part) && part.length > 1 && part.startsWith('0'))) return null;
    return { core: match.slice(1, 4).map(BigInt), pre };
  };
  const a = parse(certified), b = parse(host);
  if (!a || !b) return false;
  for (let i = 0; i < a.core.length; i++) {
    if (a.core[i] !== b.core[i]) return a.core[i] < b.core[i];
  }
  if (!a.pre.length || !b.pre.length) return Boolean(a.pre.length && !b.pre.length);
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    const left = a.pre[i], right = b.pre[i];
    if (left === right) continue;
    if (left === undefined || right === undefined) return left === undefined;
    const leftNumeric = /^\d+$/.test(left), rightNumeric = /^\d+$/.test(right);
    if (leftNumeric !== rightNumeric) return leftNumeric;
    return leftNumeric ? BigInt(left) < BigInt(right) : left < right;
  }
  return false;
}

function certificationChip(plugin) {
  if (!plugin.certified_orbit_version) return null;
  const behind = certificationBehind(plugin.certified_orbit_version, plugin.host_orbit_version);
  return el('span', {
    class: 'badge plugin-certification', 'data-tone': behind ? 'warning' : null,
    text: `certified for ${plugin.certified_orbit_version}${behind ? ' · behind host' : ''}`,
    title: behind
      ? `Certification predates this host's Orbit ${plugin.host_orbit_version}; compatibility with this version has not been certified.`
      : `Conformance certification for Orbit ${plugin.certified_orbit_version}${plugin.host_orbit_version ? `; host runs ${plugin.host_orbit_version}` : '; host version unavailable'}.`,
  });
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
    if (plugin.capabilities?.[action]?.authorized === true && !hostWriteRefusal()) {
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
  const revision = getWorkspaceRevision();
  pendingChanges.add(plugin.name);
  changeErrors.delete(plugin.name);
  const buttons = Array.from(section.children).flatMap(row => Array.from(row.children)).filter(node => node.tagName === 'BUTTON');
  for (const button of buttons) button.disabled = true;
  try {
    try {
      await postJson(`/api/plugins/${encodeURIComponent(plugin.name)}/${action}`, { scope });
    } catch (error) {
      if (revision === getWorkspaceRevision()) changeErrors.set(plugin.name, error.message || String(error));
      return;
    }
    try {
      await fetchAndRenderPlugins();
    } catch (_) {
      // requestPanel reports a failed refresh as stale panel data.
    }
  } finally {
    pendingChanges.delete(plugin.name);
    for (const button of buttons) button.disabled = false;
    // The mutation may settle after a scope change. Only repaint a listing
    // read for this visit; an aggregate placeholder or a pending new workspace
    // must not be replaced by the previous workspace's retained cards.
    if (!isAggregateView() && lastPluginsRevision === getWorkspaceRevision()) render(lastPlugins);
  }
}

function panelNode(plugin, panel) {
  const key = panelKey(plugin, panel);
  const node = el('section', { class: `card card-flush plugin-panel plugin-panel-${panel.group}` });
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
