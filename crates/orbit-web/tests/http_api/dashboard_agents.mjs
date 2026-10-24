// Analytics › Agents through contract-shaped `/api/agents` payloads: the role
// tabs and their ranked-row counts, the attribution strip, ranking selection
// driving the detail column (breakdown, biggest-penalty tasks linking to the
// task, shown-not-scored stats), "How the score works" keeping its state
// across polls, a newer scan re-reading the open detail, the empty and
// not_computed states, the Reviewers provisional banner, `accepts often` tag
// and verdict-outcome bars, the aggregate placeholder, and the router sending
// `#analytics/orchestrators` and old `#agents/<role>` to `#analytics/agents/<role>`.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const classesOf = node => String(node.className || '').split(/\s+/).filter(Boolean);
class Node {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attrs = {};
    this.listeners = {};
    this.className = '';
    this.own = '';
    this.hidden = false;
    this.offsetWidth = 80;
    this.offsetLeft = 0;
    this.style = { setProperty(name, value) { this[name] = value; } };
  }
  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(entry => entry !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
      toggle: (name, on) => (on ?? !classesOf(node).includes(name)) ? node.classList.add(name) : node.classList.remove(name),
    };
  }
  set textContent(value) {
    this.children.forEach(child => { child.parentNode = null; });
    this.children = [];
    this.own = String(value);
  }
  get textContent() { return this.own + this.children.map(child => child.textContent ?? '').join(''); }
  get lastElementChild() { return this.children.at(-1) || null; }
  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      if (typeof child === 'string') { this.own += child; continue; }
      if (child.parentNode) child.parentNode.removeChild(child);
      child.parentNode = this;
      this.children.push(child);
    }
  }
  appendChild(child) { this.append(child); return child; }
  removeChild(child) {
    const index = this.children.indexOf(child);
    if (index >= 0) this.children.splice(index, 1);
    if (child) child.parentNode = null;
    return child;
  }
  insertBefore(child, ref) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    const index = ref ? this.children.indexOf(ref) : -1;
    if (index < 0) this.children.push(child);
    else this.children.splice(index, 0, child);
    return child;
  }
  setAttribute(name, value) { this.attrs[name] = String(value); }
  getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null; }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
  focus() { document.activeElement = this; }
  scrollIntoView() {}
  querySelectorAll(selector) {
    const chains = selector.split(',').map(part => part.trim().split(/\s+/));
    const out = [];
    const walk = (node, ancestors) => {
      for (const child of node.children) {
        const path = [...ancestors, child];
        if (chains.some(chain => matchesChain(path, chain))) out.push(child);
        walk(child, path);
      }
    };
    walk(this, []);
    return out;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
}
function matches(node, simple) {
  if (/[:[]/.test(simple)) return false;
  const tag = simple.match(/^[a-z][a-z0-9]*/i)?.[0];
  if (tag && node.tagName !== tag.toUpperCase()) return false;
  for (const [, name] of simple.matchAll(/\.([^\s.#]+)/g)) if (!classesOf(node).includes(name)) return false;
  const id = simple.match(/#([\w-]+)/)?.[1];
  return !id || node.id === id;
}
function matchesChain(path, chain) {
  let at = path.length - 1;
  if (!matches(path[at], chain[chain.length - 1])) return false;
  for (let index = chain.length - 2; index >= 0; index -= 1) {
    at -= 1;
    while (at >= 0 && !matches(path[at], chain[index])) at -= 1;
    if (at < 0) return false;
  }
  return true;
}

const byId = {};
const make = (tag, id, parent, className = '') => {
  const node = new Node(tag);
  if (id) { node.id = id; byId[id] = node; }
  node.className = className;
  if (parent) parent.appendChild(node);
  return node;
};
const ROLES = ['orchestrators', 'implementers', 'reviewers', 'pilots'];
const body = new Node('body');
const tabs = make('div', 'tabs', body, 'tabs');
for (const tab of ['tasks', 'analytics']) make('button', null, tabs, 'tab').dataset.tab = tab;
const analyticsSubtabs = make('nav', 'analytics-subtabs', body, 'subtabs rail-subtabs');
for (const sub of ['metrics', 'reliability', 'codebase', 'resources', 'cost', 'agents']) make('button', null, analyticsSubtabs, 'subtab').dataset.subtab = sub;
make('div', 'topbar-crumb', body);
const panes = Object.fromEntries(['tasks', 'analytics'].map(tab => {
  const pane = make('section', null, body, 'tab-pane');
  pane.dataset.tab = tab;
  return [tab, pane];
}));
const roleTabs = make('nav', 'agents-roles', panes.analytics, 'subtabs view-tabs');
for (const role of ROLES) {
  const button = make('button', null, roleTabs, 'subtab');
  button.dataset.view = role;
  make('span', `agents-count-${role}`, button, 'agents-role-count');
}
const layout = make('main', 'agents-main', panes.analytics);
for (const id of ['agents-title', 'agents-count', 'agents-meta', 'agents-body']) make('div', id, layout);
const windows = make('div', 'agents-window-selector', layout);
for (const value of ['1h', '24h', '7d', '30d', 'all']) make('button', null, windows, 'scoreboard-window-seg').dataset.window = value;

const document = {
  activeElement: null,
  body,
  title: '',
  hidden: false,
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
  querySelectorAll: selector => body.querySelectorAll(selector),
  querySelector: selector => body.querySelector(selector),
  addEventListener() {},
};
const replaced = [];
const window = {
  location: { search: '?window=30d', hash: '', href: 'http://dashboard.test/?window=30d' },
  history: null,
  addEventListener() {},
};
const history = {
  replaceState(_state, _title, url) {
    replaced.push(String(url));
    const target = new URL(String(url), window.location.href);
    window.location.hash = target.hash;
    window.location.href = target.href;
  },
  pushState() {},
};
window.history = history;
const pending = [];
const response = (payload, status = 200) => ({ ok: status >= 200 && status < 300, status, headers: { get: () => null }, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = (path, options = {}) => new Promise(resolve => pending.push({ url: new URL(path, 'http://dashboard.test'), options, resolve }));
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window, history, Node, Intl,
  requestAnimationFrame: fn => fn(), HashChangeEvent: class { constructor(type) { this.type = type; } },
});
const modules = new Map();
const dir = new URL('../../assets/dashboard/js/', import.meta.url);
const load = name => {
  if (!modules.has(name)) {
    const url = new URL(name, dir);
    modules.set(name, new vm.SourceTextModule(fs.readFileSync(url, 'utf8'), { context, identifier: url.href }));
  }
  return modules.get(name);
};
await load('agents.js').link(specifier => load(specifier.replace(/^\.\//, '')));
await load('agents.js').evaluate();
const agents = modules.get('agents.js').namespace;
const common = modules.get('common.js').namespace;
const settle = async () => { for (let i = 0; i < 8; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };
const click = node => { assert.ok(node, 'the control exists'); for (const listener of node.listeners.click || []) listener({ stopPropagation() {}, preventDefault() {}, button: 0 }); };
const view = () => byId['agents-body'];
const part = key => view().children.find(node => node.dataset.key === key);
const rankingRows = () => part('ranking').querySelectorAll('tr.agents-row');
const rowNamed = crew => rankingRows().find(row => row.dataset.row.startsWith(`${crew}\0`));
const selectButton = crew => rowNamed(crew).querySelector('button.agents-row-select');
const takeAll = () => pending.splice(0);
const answer = (requests, route) => {
  for (const request of requests) request.resolve(route(request.url));
};

// Contract-shaped payloads (docs/design/agent-scorecards/2_design.md §6).
const TERMS = [
  { key: 'landed', label: 'Landed', weight: 1, definition: 'Landed tasks, one each whatever their complexity.', group: 'credit' },
  { key: 'spec_defects', label: 'Spec defects', weight: -1, definition: 'Edits after the first run, criteria blocks and escalations.', group: 'problem' },
  { key: 'spec_rework', label: 'Spec-caused rework', weight: -1, definition: 'Rework whose cause is spec.', group: 'problem' },
];
const stats = (rework) => [
  { key: 'unclassified', label: 'Unclassified rework and regressions (not scored)', value: rework[3] },
  { key: 'active_hours', label: 'Active hours', value: 30.25 },
  { key: 'rework_spec', label: 'Rework: spec (counted)', value: rework[0] },
  { key: 'rework_crew', label: 'Rework: crew (counted against implementers)', value: rework[1] },
  { key: 'rework_infrastructure', label: 'Rework: infrastructure (not scored)', value: rework[2] },
  { key: 'rework_no_cause', label: 'Rework: no cause (not scored)', value: rework[3] },
];
const row = (crew, rank, units, counts, raw, score, rework) => ({
  crew, model: 'model unknown', units, below_support: rank == null, rank,
  counts, per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, (count * 100) / units])),
  raw, score, runs_without_model: 0, provisional: null, stats: stats(rework),
});
const header = (role, computedAt, status = 'ok') => ({
  role, window: '30d', requested_window: '30d', since: '2026-09-19T00:00:00Z', min_support: 10,
  computed_at: computedAt, cache_ttl_seconds: 300, tasks_scanned: 120, status,
  unit: { orchestrators: 'owned tasks', implementers: 'implemented tasks', reviewers: 'settled reviews', pilots: 'assessed tasks' }[role],
});
const orchestrators = (computedAt = '2026-10-19T08:00:00.000Z') => ({
  ...header('orchestrators', computedAt), fleet_average: 71.43, terms: TERMS,
  rows: [
    row('opus', 1, 40, { landed: 38, spec_defects: 6, spec_rework: 2 }, 75, 73.47, [2, 5, 1, 0]),
    row('sonnet', 2, 20, { landed: 15, spec_defects: 1, spec_rework: 1 }, 65, 65, [1, 0, 0, 3]),
    row('haiku', null, 3, { landed: 3, spec_defects: 1, spec_rework: 0 }, 66.67, 66.67, [0, 0, 0, 0]),
  ],
});
const notComputed = role => ({ ...header(role, '2026-10-19T08:00:00.000Z', 'not_computed'), fleet_average: null, terms: TERMS.slice(0, 2), rows: [] });
const attribution = {
  window: '30d', requested_window: '30d', since: '2026-09-19T00:00:00Z', computed_at: '2026-10-19T08:00:00.000Z', cache_ttl_seconds: 300,
  total: 20,
  owners: { orchestrators: 12, implementers: 5, reviewers: 0, pilots: 0, not_scored: { count: 3, reasons: { infrastructure: 2, unattributed: 1 } } },
  charged: { orchestrators: [{ crew: 'opus', model: 'model unknown', problems: 8 }], implementers: [], reviewers: [], pilots: [] },
};
const task = (id, count, detail) => ({ task_id: id, title: `Task ${id}`, status: 'done', count, occurrences: [{ at: '2026-10-11T09:00:00Z', detail, run_id: null }] });
const detailFor = (scorecard, crew, tasks) => {
  const selected = scorecard.rows.find(entry => entry.crew === crew);
  return {
    ...header('orchestrators', scorecard.computed_at), fleet_average: scorecard.fleet_average, row: selected,
    terms: TERMS.map(term => ({
      key: term.key, label: term.label, weight: term.weight, group: term.group,
      count: selected.counts[term.key], per_100: selected.per_100[term.key], contribution: term.weight * selected.per_100[term.key],
      tasks: tasks[term.key] || [],
    })),
  };
};

common.setMultiWorkspace(true);
common.setWorkspace('ws-a');
common.setWindow('30d');
await settle();
agents.wireAgentControls();
assert.equal(agents.getAgentsRole(), 'orchestrators', 'Orchestrators is the default role');

// One refresh reads the role's scorecard and the attribution strip together,
// and every other role's scorecard for its tab count.
let loading = agents.fetchAndRenderAgents();
let requests = takeAll();
assert.deepEqual(requests.map(request => request.url.pathname).sort(), [
  '/api/agents/attribution',
  '/api/agents/implementers/scorecard',
  '/api/agents/orchestrators/scorecard',
  '/api/agents/pilots/scorecard',
  '/api/agents/reviewers/scorecard',
]);
for (const request of requests) {
  assert.equal(request.url.searchParams.get('workspace'), 'ws-a', `${request.url.pathname} is scoped to the workspace`);
  assert.equal(request.url.searchParams.get('window'), '30d', `${request.url.pathname} follows the shared window`);
}
const scorecardRequest = requests.find(request => request.url.pathname === '/api/agents/orchestrators/scorecard');
assert.equal(scorecardRequest.url.searchParams.get('min_support'), '10');
const routeV1 = url => {
  if (url.pathname === '/api/agents/attribution') return response(attribution);
  const role = url.pathname.split('/')[3];
  return response(role === 'orchestrators' ? orchestrators() : notComputed(role));
};
answer(requests, routeV1);
await loading;
await settle();

// Tab badges name ranked rows; the panel count names all rows and the ranked subset.
assert.equal(byId['agents-count-orchestrators'].textContent, '2 ranked', 'the tab count names its ranked-row population');
assert.equal(byId['agents-count'].textContent, '3 rows · 2 ranked', 'the panel names all rows and the ranked subset');
for (const role of ['implementers', 'reviewers', 'pilots']) assert.equal(byId[`agents-count-${role}`].textContent, '—');
assert.equal(byId['agents-title'].textContent, 'Orchestrators');
assert.match(byId['agents-meta'].textContent, /63 owned tasks in 30d · fleet average 71\.4/);

// The strip splits the window's problems by owner and links each role.
const strip = part('strip');
assert.match(strip.textContent, /20 problems in 30d/);
const legend = strip.querySelectorAll('li');
assert.deepEqual(legend.map(item => item.dataset.owner), [...ROLES, 'not_scored']);
assert.ok(legend[0].classList.contains('is-current'), 'the current role is highlighted');
assert.equal(legend[1].querySelector('a').href, '#analytics/agents/implementers?window=30d', 'a legend entry links to its role');
assert.equal(legend[4].querySelector('a'), null, 'Not scored is no role to open');
assert.match(strip.textContent, /2 rework caused by infrastructure · 1 no orchestrator attribution/);
const segments = strip.querySelectorAll('span.agents-strip-seg');
assert.equal(segments.length, 3, 'one segment per owner with problems');
assert.equal(segments[0].style.width, '60%');
assert.ok(segments[0].classList.contains('is-current'));

// The ranking: rank, crew and model, the score, support, then one column per
// term; the below-support row is greyed, tagged and unranked.
assert.deepEqual(rankingRows().map(entry => entry.dataset.row.split('\0')[0]), ['opus', 'sonnet', 'haiku']);
assert.deepEqual(part('ranking').querySelectorAll('thead th').map(th => th.textContent),
  ['Rank', 'Crew · model', 'Score', 'Support', 'Landed', 'Spec defects', 'Spec-caused rework']);
assert.ok(rowNamed('haiku').classList.contains('agents-row-below'));
assert.match(rowNamed('haiku').textContent, /below 10/);
assert.equal(rowNamed('haiku').children[0].textContent, '—', 'a below-support row has no rank');
assert.equal(rowNamed('opus').children.find(cell => cell.dataset.term === 'spec_defects').textContent, '15');
assert.ok(rowNamed('haiku').querySelector('span.agents-bar-fill.negative') === null, 'a positive score bar for the below-support row');
// Unclassified rework and regressions show beside the score, only where there are some.
assert.equal(rowNamed('sonnet').querySelector('td.agents-score-col span.agents-score-note').textContent, '3 unclassified');
assert.equal(rowNamed('opus').querySelector('span.agents-score-note'), null, 'no note without unclassified problems');

// The top row is selected and its detail asked for.
assert.equal(selectButton('opus').getAttribute('aria-pressed'), 'true', 'the top row is selected by default');
requests = takeAll();
assert.equal(requests.length, 1);
assert.equal(requests[0].url.pathname, '/api/agents/orchestrators/detail');
assert.equal(requests[0].url.searchParams.get('crew'), 'opus');
assert.equal(requests[0].url.searchParams.get('model'), 'model unknown');
assert.match(part('detail').textContent, /Loading the tasks/);
// The breakdown and the stats come from the ranking row before the detail lands.
const breakdown = () => part('detail').querySelectorAll('ul.agents-breakdown li').map(item => [item.dataset.term, item.querySelector('span.agents-term-value').textContent]);
assert.deepEqual(breakdown(), [['landed', '+95'], ['spec_defects', '−15'], ['spec_rework', '−5']]);
assert.match(part('detail').textContent, /raw 75, pulled down toward the fleet average 71\.4 over 40 units/);
assert.equal(part('detail').querySelector('p.agents-detail-note'), null);
requests[0].resolve(response(detailFor(orchestrators(), 'opus', {
  spec_defects: [task('ORB-1', 4, 'spec_edit: acceptance_criteria edited'), task('ORB-2', 2, 'criteria_block: acceptance-criterion-unmet')],
  spec_rework: [task('ORB-3', 2, 'repeat_run: spec')],
})));
await settle();
let penalty = part('detail').querySelector('section.agents-penalty');
assert.equal(penalty.dataset.term, 'spec_defects', 'the biggest penalty is the largest negative term');
assert.deepEqual(penalty.querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-1', 'ORB-2']);
assert.ok(penalty.querySelector('a.agents-task-id').href.includes('open=ORB-1'), 'a penalty task links to the task');
assert.match(penalty.textContent, /spec_edit: acceptance_criteria edited/);
assert.match(part('detail').textContent, /Active hours30\.3/, 'shown-not-scored stats sit in the detail column');

// The role panel shows each orchestrator's rework by cause.
const panel = part('panel');
assert.equal(panel.querySelector('h3').textContent, 'Rework by cause');
assert.deepEqual(panel.querySelectorAll('li.agents-cause-row').map(item => item.querySelector('span.agents-cause-total').textContent), ['8 rework', '4 rework', 'no rework']);
assert.equal(panel.querySelector('a.agents-cause-note').href, '#analytics/agents/implementers?window=30d', 'crew rework points at Implementers');

// Selecting another row moves the detail column to it.
click(selectButton('sonnet'));
assert.equal(selectButton('sonnet').getAttribute('aria-pressed'), 'true');
assert.equal(selectButton('opus').getAttribute('aria-pressed'), 'false');
assert.ok(rowNamed('sonnet').classList.contains('is-selected'));
assert.match(part('detail').querySelector('header').textContent, /#2sonnet/);
assert.deepEqual(breakdown(), [['landed', '+75'], ['spec_defects', '−5'], ['spec_rework', '−5']]);
assert.match(part('detail').textContent, /raw 65, not above the fleet average 71\.4, so not pulled/, 'a row below the average keeps its raw score');
assert.match(part('detail').querySelector('p.agents-detail-note').textContent, /^3 .*20 owned tasks .*\(15 per 100\)/, 'the detail states the unclassified rate');
requests = takeAll();
assert.equal(requests.length, 1);
assert.equal(requests[0].url.searchParams.get('crew'), 'sonnet');
requests[0].resolve(response({ error: "no orchestrators row for crew 'sonnet' in the 30d window" }, 400));
await settle();
assert.match(part('detail').querySelector('p.agents-error').textContent, /Unable to load the tasks: no orchestrators row/);

// Back to the first row: its cached detail answers without a request.
click(selectButton('opus'));
assert.equal(pending.length, 0, 'the cached detail of this scan is reused');
assert.deepEqual(part('detail').querySelector('section.agents-penalty').querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-1', 'ORB-2']);

// "How the score works" lists every term and the smoothing formula, and its
// open state survives a poll.
const how = () => part('how');
assert.equal(how().open, false);
assert.equal(how().querySelectorAll('dt').length, 3);
assert.match(how().textContent, /score = min\(raw, \(owned tasks × raw \+ 30 × fleet average\) ÷ \(owned tasks \+ 30\)\)/);
assert.equal(how().querySelectorAll('p').length, 5, 'the shared rate, formula and smoothing paragraphs, then the role\'s own two');
how().open = true;
for (const listener of how().listeners.toggle) listener();
const howBefore = how();

// A poll with the same scan keeps the selection, the detail and the disclosure.
loading = agents.fetchAndRenderAgents();
answer(takeAll(), routeV1);
await loading;
await settle();
assert.equal(pending.length, 0, 'an unchanged scan reads no detail');
assert.equal(selectButton('opus').getAttribute('aria-pressed'), 'true', 'the selection survives a poll');
assert.equal(how(), howBefore, 'the unchanged disclosure node is kept');
assert.equal(how().open, true);

// A newer scan re-reads the selected row's detail and keeps its tasks on
// screen meanwhile; the rebuilt disclosure stays open.
const routeV2 = url => {
  if (url.pathname === '/api/agents/attribution') return response(attribution);
  const role = url.pathname.split('/')[3];
  if (role !== 'orchestrators') return response(notComputed(role));
  const next = orchestrators('2026-10-19T08:05:00.000Z');
  next.terms = TERMS.map(term => ({ ...term, definition: `${term.definition} (v2)` }));
  return response(next);
};
loading = agents.fetchAndRenderAgents();
answer(takeAll(), routeV2);
await loading;
await settle();
requests = takeAll();
assert.equal(requests.length, 1, 'a newer scan re-reads the open detail');
assert.equal(requests[0].url.searchParams.get('crew'), 'opus');
assert.deepEqual(part('detail').querySelector('section.agents-penalty').querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-1', 'ORB-2'], 'the previous tasks stay while the newer ones load');
assert.notEqual(how(), howBefore, 'changed terms rebuild the disclosure');
assert.equal(how().open, true, 'the rebuilt disclosure keeps its open state');
requests[0].resolve(response(detailFor(orchestrators('2026-10-19T08:05:00.000Z'), 'opus', { spec_defects: [task('ORB-9', 6, 'spec_edit: description edited')] })));
await settle();
assert.deepEqual(part('detail').querySelector('section.agents-penalty').querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-9']);

// A role that is not computed yet shows an empty state that says so, with
// its terms in the disclosure and no ranking, detail or panel.
agents.setAgentsRole('implementers');
loading = agents.fetchAndRenderAgents();
answer(takeAll(), routeV2);
await loading;
await settle();
assert.equal(byId['agents-title'].textContent, 'Implementers');
assert.ok(part('state').classList.contains('agents-state-not-computed'));
assert.match(part('state').textContent, /Implementers are not computed yet/);
assert.equal(part('ranking'), undefined);
assert.equal(part('detail'), undefined);
assert.equal(part('panel'), undefined, 'a role without a panel module shows none');
assert.equal(how().querySelectorAll('dt').length, 2, 'the not-computed role still lists its terms');
assert.equal(how().open, true, 'the disclosure keeps its state across roles');
assert.ok(part('strip').querySelectorAll('li')[1].classList.contains('is-current'), 'the strip highlights the role on screen');
assert.equal(byId['agents-count-orchestrators'].textContent, '2 ranked', 'the other tabs keep their ranked-row counts');
assert.equal(pending.length, 0, 'a not-computed role reads no detail');

// A computed role with no rows in the window says so.
agents.setAgentsRole('orchestrators');
loading = agents.fetchAndRenderAgents();
answer(takeAll(), url => {
  if (url.pathname === '/api/agents/orchestrators/scorecard') return response({ ...orchestrators(), fleet_average: null, rows: [] });
  if (url.pathname === '/api/agents/attribution') return response({ ...attribution, total: 0, owners: { orchestrators: 0, implementers: 0, reviewers: 0, pilots: 0, not_scored: { count: 0, reasons: {} } } });
  return response(notComputed(url.pathname.split('/')[3]));
});
await loading;
await settle();
assert.ok(part('state').classList.contains('agents-state-empty'));
assert.match(part('state').textContent, /No owned tasks in the 30d window/);
assert.match(part('strip').textContent, /No problem was recorded in this window/);
assert.equal(byId['agents-count-orchestrators'].textContent, '0 ranked');
assert.equal(byId['agents-count'].textContent, '0 rows · 0 ranked');
takeAll();

// Reviewers: a banner while any review is younger than the miss lookback,
// the `accepts often` tag beside a row, yes/no stats and each reviewer's
// verdict outcomes.
const REVIEWER_TERMS = [
  { key: 'findings_held', label: 'Findings that held', weight: 1, definition: 'Findings that held, weighted by severity.', group: 'credit' },
  { key: 'false_rejects', label: 'False rejects', weight: -2, definition: 'Rejects the next run proved wrong.', group: 'problem' },
  { key: 'missed_regressions', label: 'Missed regressions', weight: -4, definition: 'review_missed regressions, whenever filed.', group: 'problem' },
];
const OUTCOME_KEYS = ['outcome_held_up', 'outcome_caught', 'outcome_false_alarm', 'outcome_missed', 'outcome_unresolved'];
const reviewerRow = (crew, rank, units, counts, score, provisional, [acceptRate, peers, often], outcomes) => ({
  crew, model: `gpt-${crew}`, units, below_support: rank == null, rank,
  counts, per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, (count * 100) / units])),
  raw: score, score, runs_without_model: 0, provisional,
  stats: [
    { key: 'accept_rate', label: 'Accept rate (%)', value: acceptRate },
    { key: 'peer_accept_rate', label: "Peers' accept rate on the same tiers (%)", value: peers },
    { key: 'accepts_often', label: 'Accepts often', value: often },
    ...OUTCOME_KEYS.map((key, index) => ({ key, label: key, value: outcomes[index] })),
  ],
});
const reviewers = (provisional, computedAt) => ({
  ...header('reviewers', computedAt), fleet_average: 5, terms: REVIEWER_TERMS, provisional,
  rows: [
    reviewerRow('vega', 1, 20, { findings_held: 30, false_rejects: 1, missed_regressions: 0 }, 95, 12, [40, 70, false], [8, 6, 1, 0, 5]),
    reviewerRow('lyra', 2, 12, { findings_held: 4, false_rejects: 0, missed_regressions: 1 }, 2.3, 4, [92, 70, true], [9, 1, 0, 1, 1]),
  ],
});
const routeReviewers = (provisional, computedAt) => url => {
  if (url.pathname === '/api/agents/attribution') return response(attribution);
  if (url.pathname === '/api/agents/reviewers/scorecard') return response(reviewers(provisional, computedAt));
  if (url.pathname === '/api/agents/reviewers/detail') return response({ error: 'no detail in this fixture' }, 400);
  return response(notComputed(url.pathname.split('/')[3]));
};
agents.setAgentsRole('reviewers');
loading = agents.fetchAndRenderAgents();
answer(takeAll(), routeReviewers(16, '2026-10-19T08:10:00.000Z'));
await loading;
await settle();
answer(takeAll(), routeReviewers(16, '2026-10-19T08:10:00.000Z'));
await settle();
assert.equal(byId['agents-count-reviewers'].textContent, '2 ranked');
assert.deepEqual(view().children.map(node => node.dataset.key).filter(Boolean), ['strip', 'banner', 'ranking', 'detail', 'how', 'panel'], 'the banner sits under the strip');
assert.match(part('banner').textContent, /Provisional\. 16 of 32 reviews in this window are younger than the 30-day miss lookback/);
assert.deepEqual(rankingRows().map(entry => entry.querySelector('span.agents-tag')?.dataset.tag ?? null), [null, 'accepts_often'], 'only the row well above its peers is tagged');
assert.match(rowNamed('lyra').querySelector('span.agents-tag').title, /92%.*70%/);
assert.match(part('detail').textContent, /Accepts oftenno/, 'a boolean stat reads as yes or no');
assert.match(part('detail').textContent, /Provisional12/);
click(selectButton('lyra'));
answer(takeAll(), routeReviewers(16, '2026-10-19T08:10:00.000Z'));
await settle();
assert.match(part('detail').textContent, /Accepts oftenyes/);
const outcomes = part('panel');
assert.equal(outcomes.querySelector('h3').textContent, 'How verdicts turned out');
assert.deepEqual(outcomes.querySelectorAll('li.agents-cause-row').map(item => item.querySelector('span.agents-cause-total').textContent), ['20 reviews', '12 reviews']);
const lyraBar = outcomes.querySelectorAll('li.agents-cause-row')[1].querySelectorAll('span.agents-cause-seg');
assert.deepEqual(lyraBar.map(segment => [segment.dataset.outcome, segment.style.width]),
  [['outcome_held_up', '45%'], ['outcome_caught', '5%'], ['outcome_missed', '5%'], ['outcome_unresolved', '5%']], 'segments scale to the busiest reviewer and skip empty outcomes');
// With no review inside the lookback the banner goes.
loading = agents.fetchAndRenderAgents();
answer(takeAll(), routeReviewers(0, '2026-10-19T08:15:00.000Z'));
await loading;
await settle();
answer(takeAll(), routeReviewers(0, '2026-10-19T08:15:00.000Z'));
await settle();
assert.equal(part('banner'), undefined, 'no banner once every review is past the lookback');
takeAll();

// The aggregate view has no workspace to rank.
common.setWorkspace('');
await agents.fetchAndRenderAgents();
assert.equal(view().textContent, common.AGGREGATE_PANEL_PLACEHOLDER);
assert.equal(byId['agents-count'].textContent, '—');
assert.equal(pending.length, 0);
common.setWorkspace('ws-a');

// Router: `#analytics/orchestrators` and old `#agents/<role>` open
// `#analytics/agents/<role>` in place, keeping the window, and a role tab
// routes to its role.
await load('router.js').link(specifier => load(specifier.replace(/^\.\//, '')));
await load('router.js').evaluate();
const router = modules.get('router.js').namespace;
let tab = 'tasks';
let fetches = 0;
let analyticsSubtab = 'metrics';
let reliabilityView = 'pipeline';
router.initRouter({
  getTab: () => tab, setTab: value => { tab = value; },
  getAnalyticsSubtab: () => analyticsSubtab, setAnalyticsSubtab: value => { analyticsSubtab = value; },
  getReliabilityView: () => reliabilityView, setReliabilityView: value => { reliabilityView = value; },
  getAgentsRole: agents.getAgentsRole, setAgentsRole: agents.setAgentsRole,
  fetchAgents: () => { fetches += 1; },
  refreshDashboard: () => {},
});
window.location.hash = '#analytics/orchestrators?window=7d';
router.setActiveTab('analytics/orchestrators?window=7d');
assert.equal(tab, 'analytics', 'the legacy route opens Analytics');
assert.equal(analyticsSubtab, 'agents', 'the legacy route opens Analytics > Agents');
assert.equal(replaced.at(-1).split('#')[1], 'analytics/agents/orchestrators?window=7d', 'the legacy hash is rewritten in place');
assert.equal(common.getWindow(), '7d', 'the redirect keeps the window');
assert.ok(panes.analytics.classList.contains('active'));
assert.ok(roleTabs.children[0].classList.contains('active'));
assert.equal(fetches, 1, 'opening a role reads it at once');
router.setActiveTab('agents/pilots');
assert.equal(tab, 'analytics', 'the old Agents route opens Analytics');
assert.equal(agents.getAgentsRole(), 'pilots', 'the old Agents route keeps its role');
assert.equal(replaced.at(-1).split('#')[1], 'analytics/agents/pilots?window=7d', 'the old Agents hash is rewritten in place');
router.setActiveTab('analytics/agents/reviewers');
assert.equal(agents.getAgentsRole(), 'reviewers');
assert.ok(roleTabs.children[2].classList.contains('active'));
assert.equal(roleTabs.children[0].classList.contains('active'), false);
assert.equal(window.location.hash, '#analytics/agents/reviewers?window=7d');
router.setActiveTab('analytics/agents/nobody');
assert.equal(agents.getAgentsRole(), 'reviewers', 'an unknown role keeps the role on screen');
assert.equal(window.location.hash, '#analytics/agents/reviewers?window=7d', 'and names it in the hash');
takeAll();
console.log('dashboard agents ok');

// Reuse this shell/DOM harness for role-specific public view behavior.
export { agents, common, answer, takeAll, response, settle, part, click, selectButton, byId, pending, notComputed, attribution };
