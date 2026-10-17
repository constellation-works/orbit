// Execute the shipped polling module with a hidden document: the light
// background poll, the failed-count title prefix and opt-in notifications.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const settle = async () => { for (let i = 0; i < 10; i++) await new Promise(resolve => setImmediate(resolve)); };

const timers = [];
const fakeSetTimeout = (fn, ms) => {
  const handle = { fn, ms, cancelled: false };
  timers.push(handle);
  return handle;
};
const fakeClearTimeout = handle => { if (handle) handle.cancelled = true; };
const pending = name => timers.filter(timer => !timer.cancelled && timer.fn.name === name);
const fire = async timer => { timer.cancelled = true; timer.fn(); await settle(); };

let failedTotal = 3;
let failedStatus = 200;
let readiness = { capacity: { drain_phase: 'draining' }, tasks: [] };
const urls = [];
const respond = (status, body) => ({
  ok: status < 400, status,
  json: async () => body, text: async () => JSON.stringify(body),
  headers: { get: () => null },
});
const fetch = async url => {
  urls.push(url);
  const path = url.split('?')[0];
  if (path === '/api/job-runs' || path === '/api/job-runs/all') {
    return failedStatus === 200 ? respond(200, { items: [], total: failedTotal }) : respond(failedStatus, { error: 'unavailable' });
  }
  if (path === '/api/workflows/auto/readiness') return respond(200, readiness);
  return respond(404, { error: `unexpected ${url}` });
};

const listeners = {};
const button = {
  hidden: true, title: '', attributes: new Map(), handlers: [],
  setAttribute(name, value) { this.attributes.set(name, String(value)); },
  addEventListener(type, handler) { if (type === 'click') this.handlers.push(handler); },
  click() { return Promise.all(this.handlers.map(handler => handler())); },
};
const document = {
  hidden: false, title: 'orbit',
  getElementById: id => (id === 'notify-btn' ? button : null),
  addEventListener: (type, handler) => { listeners[type] = handler; },
};
const setHidden = async hidden => { document.hidden = hidden; listeners.visibilitychange(); await settle(); };

const shown = [];
let permissionAnswer = 'granted';
class Notification {
  static permission = 'default';
  static async requestPermission() { Notification.permission = permissionAnswer; return permissionAnswer; }
  constructor(title, options) { shown.push({ title, body: options.body }); }
}
const store = new Map();
const localStorage = {
  getItem: key => (store.has(key) ? store.get(key) : null),
  setItem: (key, value) => store.set(key, String(value)),
  removeItem: key => store.delete(key),
};

const context = vm.createContext({
  URLSearchParams, URL, console, AbortController, TypeError, Error, Promise, Set, Number,
  window: { location: { search: '?workspace=one', hash: '' } },
  document, fetch, Notification, localStorage,
  setTimeout: fakeSetTimeout, clearTimeout: fakeClearTimeout,
});
const load = file => new vm.SourceTextModule(fs.readFileSync(new URL(`../../assets/dashboard/js/${file}`, import.meta.url), 'utf8'), { context });
const common = load('common.js');
const polling = load('polling.js');
await common.link(() => { throw new Error('unexpected common dependency'); });
await polling.link(name => { assert.equal(name, './common.js'); return common; });
await polling.evaluate();
const { startDashboardPolling, setDashboardTitle, initBackgroundNotifications } = polling.namespace;

let refreshes = 0;
const refresh = async () => { refreshes += 1; return true; };
const backgroundReads = () => urls.filter(url => url.startsWith('/api/job-runs')).length;

setDashboardTitle('Tasks · orbit');
initBackgroundNotifications();
assert.equal(button.hidden, false, 'the opt-in is offered where the browser supports notifications');
assert.equal(button.attributes.get('aria-pressed'), 'false', 'notifications start off');
startDashboardPolling(refresh);
await settle();

// A visible tab refreshes its panels on the 30 s cadence and reads nothing else.
assert.equal(refreshes, 1, 'a visible start refreshes once');
assert.equal(pending('runRefresh').length, 1);
assert.equal(pending('runRefresh')[0].ms, 30000, 'a visible tab keeps its 30 s cadence');
assert.equal(pending('runBackground').length, 0, 'a visible tab has no background poll');
assert.equal(backgroundReads(), 0, 'a visible tab makes no background read');
await fire(pending('runRefresh')[0]);
assert.equal(refreshes, 2);
assert.equal(backgroundReads(), 0);

// Hiding swaps the panel refresh for one light read that records a baseline.
const visibleTimer = pending('runRefresh')[0];
await setHidden(true);
assert.ok(visibleTimer.cancelled, 'hiding cancels the pending panel refresh');
assert.equal(pending('runRefresh').length, 0);
assert.ok(urls.includes('/api/job-runs?state=failed&limit=1&workspace=one'), 'the background read counts failed runs in the selected workspace');
assert.ok(urls.includes('/api/workflows/auto/readiness?workspace=one'), 'the background read checks the drain');
assert.equal(document.title, 'Tasks · orbit', 'the first hidden read only records a baseline');
assert.equal(pending('runBackground').length, 1);
assert.equal(pending('runBackground')[0].ms, 60000, 'a hidden tab still polls, at the reduced 60 s interval');
assert.equal(refreshes, 2, 'a hidden tab refreshes no panels');

// A new failure prefixes the title; without the opt-in nothing is notified.
failedTotal = 5;
readiness = { capacity: { drain_phase: 'idle' }, tasks: [{ task_id: 'ORB-1', reason: 'baseline_red_hold' }] };
await fire(pending('runBackground')[0]);
assert.equal(document.title, '(2 failed) Tasks · orbit', 'the title counts failures that appeared while hidden');
assert.equal(shown.length, 0, 'no notification is shown before the operator opts in');
assert.equal(pending('runBackground')[0].ms, 60000, 'the background poll keeps its cadence');
setDashboardTitle('Jobs · orbit');
assert.equal(document.title, '(2 failed) Jobs · orbit', 'the prefix survives a destination change');
// A failed read changes nothing and keeps polling.
failedStatus = 503;
await fire(pending('runBackground')[0]);
assert.equal(document.title, '(2 failed) Jobs · orbit', 'an unanswered read neither clears nor grows the count');
assert.equal(pending('runBackground').length, 1);
failedStatus = 200;
failedTotal = 6;
await fire(pending('runBackground')[0]);
assert.equal(document.title, '(3 failed) Jobs · orbit', 'counting resumes from the last answered read');

// Showing the tab acknowledges the prefix and resumes the visible cadence.
const hiddenTimer = pending('runBackground')[0];
await setHidden(false);
assert.ok(hiddenTimer.cancelled, 'showing the tab cancels the background poll');
assert.equal(document.title, 'Jobs · orbit', 'the prefix clears once the tab is shown');
assert.equal(refreshes, 3, 'showing the tab refreshes once');
assert.equal(pending('runRefresh')[0].ms, 30000);

// Opting in asks for permission only on the click.
assert.equal(Notification.permission, 'default', 'permission is not requested on load');
await button.click();
assert.equal(Notification.permission, 'granted');
assert.equal(button.attributes.get('aria-pressed'), 'true', 'the opt-in reads as on');

// Visible after opting in: still only panel refreshes, no notifications.
const readsWhileVisible = backgroundReads();
readiness = { capacity: { drain_phase: 'draining' }, tasks: [{ task_id: 'ORB-1', reason: 'baseline_red_hold' }] };
failedTotal = 7;
await fire(pending('runRefresh')[0]);
assert.equal(backgroundReads(), readsWhileVisible, 'opting in does not add reads to a visible tab');
assert.equal(shown.length, 0, 'a visible tab never notifies');
assert.equal(document.title, 'Jobs · orbit');

// Hidden and opted in: a drain end and a new hold each notify once.
await setHidden(true);
readiness = { capacity: { drain_phase: 'idle' }, tasks: [
  { task_id: 'ORB-1', reason: 'baseline_red_hold' },
  { task_id: 'ORB-2', reason: 'baseline_red_hold' },
  { task_id: 'ORB-3', reason: 'capacity_saturated' },
] };
await fire(pending('runBackground')[0]);
assert.deepEqual(shown.map(entry => entry.title), ['Orbit drain ended', 'Orbit task held'], 'a drain end and a new hold notify');
assert.match(shown[1].body, /ORB-2/, 'the hold notice names the newly held task');
assert.doesNotMatch(shown[1].body, /ORB-1|ORB-3/, 'an existing hold or another wait is not announced');
await fire(pending('runBackground')[0]);
assert.equal(shown.length, 2, 'an unchanged drain and hold set does not notify again');

// Opting out stops notifications.
await setHidden(false);
await button.click();
assert.equal(button.attributes.get('aria-pressed'), 'false');
readiness = { capacity: { drain_phase: 'draining' }, tasks: [] };
await setHidden(true);
readiness = { capacity: { drain_phase: 'idle' }, tasks: [{ task_id: 'ORB-4', reason: 'baseline_red_hold' }] };
await fire(pending('runBackground')[0]);
assert.equal(shown.length, 2, 'no notification after opting out');

// A refused permission leaves notifications off and says why.
await setHidden(false);
Notification.permission = 'default';
permissionAnswer = 'denied';
await button.click();
assert.equal(button.attributes.get('aria-pressed'), 'false', 'a refused permission does not opt in');
assert.match(button.title, /blocked/, 'the control explains a refused permission');

// The aggregate view counts failures across workspaces and has no drain to read.
common.namespace.setMultiWorkspace(true);
common.namespace.setWorkspace(null);
urls.length = 0;
await setHidden(true);
assert.deepEqual(urls, ['/api/job-runs/all?state=failed&limit=1'], 'the aggregate background read is the cross-workspace failed count');

console.log('hidden dashboard polls every 60 s, prefixes the title with new failures until shown, and notifies a drain end or new hold only after opt-in');
