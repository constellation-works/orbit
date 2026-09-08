// The same full-app scenarios run in the Node harness and a real browser.
const node = id => document.getElementById(id);
const check = (condition, message) => { if (!condition) throw new Error(message); };
const settle = async () => { for (let i = 0; i < 5; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
let heldPath = '/api/tasks';
let networkDown = false;
let metricsError = false;
let marker = 'first';
let summaryReads = 0;
const pendingReads = [];
const list = items => ({ items, total: items.length, limit: 50, truncated: false });
function fixture(url) {
  const workspace = url.searchParams.get('workspace');
  switch (url.pathname) {
    case '/api/workspaces': return ['one', 'two'].map(id => ({ id, name: id, status: 'active', is_default: id === 'one' }));
    case '/api/tasks': return list([{ id: 'TEST-1', title: marker, status: 'in-progress', priority: 'medium' }]);
    case '/api/job-runs': return list([{ run_id: marker, job_id: 'fixture', state: 'failed' }]);
    case '/api/diagnostics/errors': return [{ message: marker, source: 'fixture' }];
    case '/api/routines': return { host_id: marker, routines: [{ name: marker, source: workspace, enabled: true }], clock: {} };
    case '/api/auto-tasks': return { definitions: [] };
    case '/api/audit/summary': summaryReads++; return { events: summaryReads };
    default: return [];
  }
}
globalThis.fetch = async path => {
  const url = new URL(path, 'http://dashboard.test');
  if (networkDown) throw new TypeError('Fixture network unavailable');
  if (metricsError && url.pathname === '/api/diagnostics/metrics') return response({ error: 'Metrics fixture failure' }, 500);
  const payload = fixture(url);
  if (url.pathname === heldPath) return new Promise((resolve, reject) => pendingReads.push({ payload, resolve, reject }));
  return response(payload);
};
await import('./app.js');
await settle();
const { setWorkspace } = await import('./common.js');
const { setActiveTab } = await import('./router.js');
const refresh = () => {
  const button = node('refresh-btn');
  if (button.listeners) button.listeners.click(); else button.click();
};
const release = (request, payload = request.payload) => request.resolve(response(payload));
const text = id => node(id).textContent;
const busy = id => node(id).getAttribute ? node(id).getAttribute('aria-busy') : node(id)['aria-busy'];
check(text('tasks-body').includes('Loading'), 'cold Tasks must visibly load');
check(!text('tasks-body').includes('No tasks'), 'cold Tasks cannot claim empty');
for (const request of pendingReads.splice(0)) release(request, list([]));
await settle();
check(text('tasks-body').includes('No tasks'), 'successful empty Tasks produces empty state');

const surfaces = [
  { route: 'tasks', body: 'tasks-body', path: '/api/tasks', empty: list([]), emptyText: 'No tasks' },
  { route: 'diagnostics/runs', body: 'runs-body', path: '/api/job-runs', empty: list([]), emptyText: 'No job runs' },
  { route: 'diagnostics/errors', body: 'diag-body', path: '/api/diagnostics/errors', empty: [], emptyText: 'No error events' },
  { route: 'operations/routines', body: 'routines-body', path: '/api/routines', empty: { routines: [], clock: {} }, emptyText: 'No routines' },
];
for (const surface of surfaces) {
  heldPath = surface.path;
  setWorkspace('one');
  setWorkspace('two');
  marker = 'current-scope-data';
  setActiveTab(surface.route);
  await settle();
  if (!pendingReads.length) { refresh(); await settle(); }
  check(text(surface.body).includes('Loading'), `${surface.route}: cold load visible`);
  check(!text(surface.body).includes(surface.emptyText), `${surface.route}: cold load cannot be empty`);
  check(busy(surface.body) === 'true', `${surface.route}: loading marked busy`);
  const status = Array.from(node(surface.body).children).find(child => child.dataset.panelStatus);
  const attribute = name => status?.getAttribute ? status.getAttribute(name) : status?.[name];
  check(attribute('role') === 'status' && attribute('aria-live') === 'polite', `${surface.route}: accessible loading feedback`);
  if (status.getBoundingClientRect) {
    const box = status.getBoundingClientRect();
    check(box.width > 0 && box.height > 0 && box.top < window.innerHeight, `${surface.route}: loading feedback visible in browser`);
  }
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  check(text(surface.body).includes(marker), `${surface.route}: loaded data visible`);
  check(text(surface.body).includes('Updated.'), `${surface.route}: success visible`);

  marker = 'late-old-scope';
  refresh(); await settle();
  const old = pendingReads.splice(0);
  check(text(surface.body).includes('Refreshing') && text(surface.body).includes('current-scope-data'), `${surface.route}: refresh retains labeled data`);
  check(!node('refresh-btn').disabled, `${surface.route}: refresh remains usable`);
  setWorkspace('one');
  check(!text(surface.body).includes('current-scope-data'), `${surface.route}: workspace change clears synchronously`);
  marker = 'new-scope-data';
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  for (const request of old) release(request);
  await settle();
  check(text(surface.body).includes('new-scope-data') && !text(surface.body).includes('late-old-scope'), `${surface.route}: old scope response discarded`);

  marker = 'older-overlap'; refresh(); await settle();
  const earlier = pendingReads.splice(0);
  marker = 'newer-overlap'; refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  for (const request of earlier) release(request);
  await settle();
  check(text(surface.body).includes('newer-overlap') && !text(surface.body).includes('older-overlap'), `${surface.route}: latest request wins`);

  marker = 'late-before-roundtrip'; refresh(); await settle();
  const roundtrip = pendingReads.splice(0);
  setWorkspace('two'); setWorkspace('one');
  for (const request of roundtrip) release(request);
  await settle();
  check(text(surface.body).includes('Loading') && !text(surface.body).includes('late-before-roundtrip'), `${surface.route}: A→B→A rejects old response`);
  marker = 'newer-overlap'; refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) request.reject(new TypeError('Fixture refresh failure'));
  await settle();
  check(text(surface.body).includes('stale data') && text(surface.body).includes('newer-overlap'), `${surface.route}: refresh failure retains labeled stale data`);
  check(busy(surface.body) === 'false', `${surface.route}: failure clears busy`);
  setWorkspace('two'); refresh(); await settle();
  for (const request of pendingReads.splice(0)) request.reject(new TypeError('Fixture cold failure'));
  await settle();
  check(text(surface.body).includes('Unable to load') && !text(surface.body).includes(surface.emptyText), `${surface.route}: cold error differs from empty`);
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request, surface.empty);
  await settle();
  check(text(surface.body).includes(surface.emptyText) && !text(surface.body).includes('Unable to load'), `${surface.route}: empty success recovers`);
}
heldPath = null;
metricsError = true;
const priorSummaryReads = summaryReads;
setActiveTab('diagnostics/metrics');
await settle();
check(text('diag-body').includes('Metrics fixture failure'), 'HTTP failure visible in Metrics panel');
check(node('conn-status').className.includes('green') && !text('meta-text').includes('offline'), 'one HTTP panel error does not imply offline');
check(summaryReads > priorSummaryReads && text('tile-events-value') === String(summaryReads), 'audit summary renders updated data independently of failed panel');
networkDown = true;
refresh(); await settle();
check(node('conn-status').className.includes('red') && text('meta-text').includes('offline'), 'network outage reports offline');
networkDown = false;
metricsError = false;
refresh(); await settle();
check(node('conn-status').className.includes('green'), 'connection recovers');
const realTimeout = globalThis.setTimeout;
const realFetch = globalThis.fetch;
globalThis.setTimeout = (fn, ms, ...args) => realTimeout(fn, ms === 30000 ? 1 : ms, ...args);
globalThis.fetch = (_path, options) => new Promise((_resolve, reject) => {
  options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
});
refresh(); await settle();
check(busy('diag-body') === 'false' && text('diag-body').includes('timed out'), 'hung request times out and clears busy state');
check(!node('refresh-btn').disabled, 'timeout leaves retry available');
globalThis.setTimeout = realTimeout;
globalThis.fetch = realFetch;
const realClearTimeout = globalThis.clearTimeout;
const scheduledPolls = [];
globalThis.setTimeout = (fn, ms, ...args) => {
  if (ms === 0) return realTimeout(fn, ms, ...args);
  const handle = { cancelled: false, unref: () => {} };
  scheduledPolls.push({ fn, ms, args, handle });
  return handle;
};
globalThis.clearTimeout = (handle) => {
  if (typeof handle === 'object') handle.cancelled = true;
  else realClearTimeout(handle);
};
const activePoll = () => scheduledPolls.filter(entry => !entry.handle.cancelled).at(-1);
const runPoll = async () => {
  const poll = activePoll();
  check(poll, 'refresh poll must be scheduled');
  poll.handle.cancelled = true;
  poll.fn(...poll.args);
  await settle();
};

// Pausing a dashboard removes its pending poll. Returning to it refreshes once,
// then failed polls double their delay and a successful retry restores 30s.
document.hidden = true;
documentListeners.visibilitychange();
const hiddenSummaryReads = summaryReads;
await settle();
check(summaryReads === hiddenSummaryReads, 'hidden dashboard makes no audit-summary request');
document.hidden = false;
documentListeners.visibilitychange();
await settle();
check(summaryReads === hiddenSummaryReads + 1, 'visible dashboard refreshes exactly once');
check(activePoll().ms === 30000, 'successful refresh schedules the normal 30s interval');
networkDown = true;
await runPoll();
check(activePoll().ms === 60000, 'first failed refresh doubles the interval');
await runPoll();
check(activePoll().ms === 120000, 'consecutive failures continue exponential backoff');
networkDown = false;
await runPoll();
check(activePoll().ms === 30000, 'successful retry restores the 30s interval');
globalThis.setTimeout = realTimeout;
globalThis.clearTimeout = realClearTimeout;
globalThis.loadingTestsPassed = true;
console.log('Dashboard loading, ordering, empty, error and recovery scenarios passed.');
