// Execute shipped modules against the DOM parsed from the shipped dashboard.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';

class Node {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.className = '';
    this.text = '';
    this.attributes = new Map();
    this.dataset = {};
    this.listeners = {};
  }
  addEventListener(type, listener) { (this.listeners[type] ||= []).push(listener); }
  // Run the listeners a browser would on this node; `target` defaults to it.
  fire(type, event = {}) {
    const fired = { type, target: this, preventDefault() { this.prevented = true; }, ...event };
    for (const listener of this.listeners[type] || []) listener(fired);
    return fired;
  }
  contains(other) { return other === this || this.children.some(child => child instanceof Node && child.contains(other)); }
  focus() {
    const previous = document.activeElement;
    document.activeElement = this;
    if (previous && previous !== this) previous.fire('blur', { relatedTarget: this });
    this.fire('focus');
  }
  set textContent(value) { this.text = String(value); this.children = []; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  append(child) { this.children.push(child); }
  replaceChildren(...children) { this.text = ''; this.children = children; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  set tabIndex(value) { this.setAttribute('tabindex', value); }
  get tabIndex() { return Number(this.getAttribute('tabindex') ?? -1); }
}
// Use a real HTML parser rather than assertions over source text. The small DOM
// implements only the element/text operations used by the shipped renderer.
const parsed = spawnSync('python3', ['-c', `
import json, sys
from html.parser import HTMLParser
class Parser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.root = {"tag": "document", "attrs": {}, "children": []}
        self.stack = [self.root]
    def handle_starttag(self, tag, attrs):
        node = {"tag": tag, "attrs": dict(attrs), "children": []}
        self.stack[-1]["children"].append(node)
        if tag not in {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"}:
            self.stack.append(node)
    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, 0, -1):
            if self.stack[index]["tag"] == tag:
                self.stack = self.stack[:index]
                break
    def handle_data(self, data):
        self.stack[-1]["children"].append(data)
p = Parser()
p.feed(sys.stdin.read())
print(json.dumps(p.root))
`], { input: fs.readFileSync(new URL('../../assets/dashboard/index.html', import.meta.url), 'utf8'), encoding: 'utf8' });
assert.equal(parsed.status, 0, parsed.stderr);
const elements = [];
function buildDOM(value) {
  if (typeof value === 'string') return value;
  const node = new Node(value.tag);
  node.id = value.attrs.id;
  node.className = value.attrs.class || '';
  for (const [name, attr] of Object.entries(value.attrs)) {
    if (name.startsWith('data-')) node.dataset[name.slice(5)] = attr;
  }
  elements.push(node);
  node.children = value.children.map(buildDOM);
  return node;
}
buildDOM(JSON.parse(parsed.stdout));
const byId = id => elements.find(node => node.id === id);
const host = byId('host-block');
const descendants = node => node.children.filter(child => child instanceof Node).flatMap(child => [child, ...descendants(child)]);
const chips = () => descendants(host).filter(node => node.dataset.resource);
const topbar = elements.find(node => node.tagName === 'header' && node.className === 'topbar');
const railFoot = elements.find(node => node.className === 'rail-foot');
assert.ok(railFoot && descendants(railFoot).includes(host), 'the host block sits in the rail foot');
assert.ok(!descendants(topbar).includes(host), 'the host block is not in the top bar');
assert.equal(byId('host-resource-chips'), undefined, 'the top bar no longer has host chips');
assert.equal(byId('host-resource-name'), undefined, 'the top bar no longer names the host');
assert.equal(byId('host-resources'), undefined, 'there is no standalone host panel');
assert.ok(byId('host-detail'), 'the detail panel is in the page');
assert.deepEqual(chips().map(chip => chip.dataset.resource), ['cpu', 'memory', 'disk'], 'three host rows exist before the first fetch');
const providerHost = byId('provider-usage-chips');
const providerChips = () => providerHost.children.filter(child => child instanceof Node);
assert.ok(descendants(byId('host-resource-group')).includes(providerHost), 'provider usage sits in the selected host\'s group, so it names the same host');
assert.equal(providerChips().length, 1, 'one placeholder chip exists before the first fetch');

let now = 100000;
let nextResponse;
let resolvePending;
const urls = [];
const timers = new Map();
const document = {
  hidden: false, activeElement: null, createElement: tag => new Node(tag), getElementById: byId,
  listeners: {}, addEventListener: Node.prototype.addEventListener, fire: Node.prototype.fire,
};
const context = vm.createContext({
  URLSearchParams, window: { location: { search: '?workspace=all', hash: '' } }, document, console,
  Date: class extends Date { static now() { return now; } },
  setInterval: (callback, delay) => timers.set(delay, callback),
  setTimeout, clearTimeout, AbortController,
  fetch: async url => { urls.push(url); return nextResponse; },
});
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const providerUsage = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/provider-usage.js', import.meta.url), 'utf8'), { context });
const resource = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/host-resources.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await providerUsage.link(name => { assert.equal(name, './common.js'); return common; });
await resource.link(name => {
  assert.ok(['./common.js', './provider-usage.js'].includes(name), `unexpected dependency ${name}`);
  return name === './common.js' ? common : providerUsage;
});
await resource.evaluate();
const { renderHostResources, renderProviderUsage, fetchAndRenderHostResources, initHostResources, resetHostResources } = resource.namespace;
const reading = (percent, severity = 'ok', unknown_reason = null) => ({ percent, severity, unknown_reason });
const payload = {
  cpu: reading(193.7, 'critical'), memory: reading(82, 'elevated'), disk: { path: '/worst', ...reading(88, 'critical') },
  // A large legacy per-path payload must never produce additional chips.
  disks: Array.from({ length: 22 }, (_, index) => ({ path: `/workspace/${index}`, ...reading(20) })),
  severity: 'critical', sample_age_seconds: 2, max_age_seconds: 15, throttle: true,
  pressures: [{ resource: 'cpu' }, { resource: 'disk /held' }],
  reason: '<unsafe> cpu and disk high', thresholds: { enabled: true }, stale: false,
};
const text = chip => Object.fromEntries(['k', 'v'].map(name => [name, descendants(chip).find(node => node.className === name).textContent.replace(/\s+/g, ' ').trim()]));
const chip = name => chips().find(node => node.dataset.resource === name);
const readings = () => chips().map(text);
renderHostResources(payload);
assert.deepEqual(chips().map(node => node.dataset.resource), ['cpu', 'memory', 'disk'], 'cpu, memory and disk each keep their own chip');
assert.deepEqual(readings(), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }, { k: 'disk', v: '88%' }], 'load reads relative to cores; memory and disk read as percentages');
assert.doesNotMatch(chip('cpu').textContent, /cpu|193/, 'the cpu chip never shows load as a bare cpu percentage');
assert.deepEqual(chips().map(node => node.className), [
  'host-resource critical throttled', 'host-resource elevated', 'host-resource critical throttled',
], 'each chip carries its own severity, and only the held resources are throttled');
for (const node of chips()) {
  assert.equal(node.tabIndex, -1, 'static host readings do not add an unhelpful tab stop');
  assert.equal(node.getAttribute('role'), 'group');
  assert.equal(node.getAttribute('aria-label'), node.title, 'assistive technology reads the same detail as the title');
  assert.match(node.title, /sampled 2s ago/);
  assert.match(node.title, /Throttle verdict: held/);
  assert.match(node.title, /<unsafe> cpu and disk high/);
  assert.equal(descendants(node).some(child => child.tagName === 'unsafe'), false, 'throttle reasons never become markup');
}
assert.match(chip('cpu').title, /1-minute load average divided by online cores/, 'the load title explains the measure');
assert.match(chip('cpu').title, /193\.7% of cores \(critical\)/);
assert.match(chip('memory').title, /^memory 82\.0% \(elevated\)/);
assert.match(chip('disk').title, /^disk \/worst 88\.0% \(critical\)/, 'the disk title names the worst path');
const heldWords = node => descendants(node).filter(child => child.className === 'host-resource-held').map(child => child.textContent);
assert.deepEqual(heldWords(chip('cpu')), ['throttled'], 'the throttle verdict is accessible text on the held chip');
assert.deepEqual(heldWords(chip('memory')), [], 'a resource that is not held carries no throttled text');
assert.match(chip('cpu').title, /Throttle verdict: held on this resource/);
assert.doesNotMatch(chip('memory').title, /on this resource/);
const warn = byId('host-warn');
const panel = byId('host-detail');
assert.equal(warn.hidden, false, 'an elevated or worse reading shows one top-bar warning');
assert.equal(warn.getAttribute('hidden'), null);
assert.equal(byId('host-warn-label').textContent, 'load');
assert.equal(byId('host-warn-value').textContent, '1.9×');
assert.equal(warn.getAttribute('aria-expanded'), 'false');
assert.equal(host.getAttribute('aria-expanded'), 'false');
assert.equal(panel.getAttribute('role'), 'dialog');
assert.equal(panel.hidden, true, 'the detail panel stays closed until the block is opened');
assert.match(host.getAttribute('aria-label'), /holding work · load/);
assert.equal(descendants(topbar).some(node => node.dataset && node.dataset.resource), false, 'the top bar shows no load, mem, disk or io row');

// The verdict flipping changes each chip's state, never its visible text, so
// the bar's width and height cannot move with it.
renderHostResources({ ...payload, throttle: false, pressures: [] });
assert.deepEqual(chips().map(node => node.className), ['host-resource critical', 'host-resource elevated', 'host-resource critical']);
assert.deepEqual(readings(), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }, { k: 'disk', v: '88%' }]);
assert.ok(chips().every(node => heldWords(node).length === 0));
assert.match(chip('disk').title, /Throttle verdict: open/);

renderHostResources({ ...payload, pressures: [{ resource: 'memory' }] });
assert.deepEqual(chips().map(node => node.className), ['host-resource critical', 'host-resource elevated throttled', 'host-resource critical'], 'the held resource is the one marked throttled');

renderHostResources({ ...payload, disk: null });
assert.deepEqual(readings()[2], { k: 'disk', v: '–' }, 'an unknown disk reads as a dash');
assert.equal(chip('disk').className, 'host-resource unknown throttled', 'a held resource stays marked while its reading is unknown');
assert.match(chip('disk').title, /^disk unavailable/);
assert.deepEqual(readings().slice(0, 2), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }], 'an unknown disk does not hide the known readings');
renderHostResources({ ...payload, memory: { percent: null, severity: 'unknown', unknown_reason: 'meminfo unreadable' } });
assert.deepEqual(readings()[1], { k: 'mem', v: '–' });
assert.match(chip('memory').title, /^memory meminfo unreadable/, 'the unknown reason reaches the title');
renderHostResources({ ...payload, stale: true });
assert.deepEqual(readings(), [{ k: 'load', v: '–' }, { k: 'mem', v: '–' }, { k: 'disk', v: '–' }]);
assert.ok(chips().every(node => node.className === 'host-resource unknown'), 'a stale sample makes every chip unknown and none held');
assert.ok(chips().every(node => /stale · .*Throttle verdict: unknown/.test(node.title)));
assert.equal(warn.hidden, true, 'a stale sample leaves the top bar without a host warning');

nextResponse = { ok: true, json: async () => payload };
await fetchAndRenderHostResources();
assert.equal(urls[0], '/api/host/resources', 'workspace selection must not augment a host request');
initHostResources();
// Staleness advances even while the next poll is pending.
nextResponse = new Promise(resolve => { resolvePending = resolve; });
const pendingPoll = timers.get(5000)();
now += 14000;
timers.get(1000)();
assert.deepEqual(readings(), [{ k: 'load', v: '–' }, { k: 'mem', v: '–' }, { k: 'disk', v: '–' }]);
assert.match(chip('disk').title, /sampled 16s ago/);
assert.match(chip('disk').title, /Throttle verdict: unknown.*Resource sample expired/);
const recovered = { ...payload, throttle: false, severity: 'ok', cpu: reading(20), memory: reading(42), disk: { path: '/known', ...reading(50) }, pressures: [], reason: 'recovered' };
resolvePending({ ok: true, json: async () => recovered });
await pendingPoll;
assert.deepEqual(readings(), [{ k: 'load', v: '0.2× cores' }, { k: 'mem', v: '42%' }, { k: 'disk', v: '50%' }]);
assert.ok(chips().every(node => node.className === 'host-resource ok'));
assert.equal(warn.hidden, true, 'normal readings leave the top bar without a host element');
assert.match(chip('disk').title, /Throttle verdict: open.*recovered/);
renderHostResources({ ...recovered, thresholds: { enabled: false } });
assert.match(chip('cpu').title, /Throttle verdict: disabled/);
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.equal(chips().length, 3);
assert.deepEqual(readings(), [{ k: 'load', v: '–' }, { k: 'mem', v: '–' }, { k: 'disk', v: '–' }]);
assert.match(chip('disk').title, /age unknown.*Throttle verdict: unknown.*Resource API unavailable/);

// Disk I/O stall: a host that reports it gains an io chip after disk. Once
// reported, the chip stays through a sample without it and a failed poll, so
// the bar does not shift; a host switch forgets it.
renderHostResources({ ...recovered, io: reading(23.4, 'critical'), throttle: true, pressures: [{ resource: 'io full' }] });
assert.deepEqual(chips().map(node => node.dataset.resource), ['cpu', 'memory', 'disk', 'io'], 'a host with I/O pressure shows an io chip after disk');
assert.deepEqual(readings()[3], { k: 'io', v: '23%' });
assert.equal(chip('io').className, 'host-resource critical throttled', 'an io full pressure holds the io chip');
assert.equal(chip('disk').className, 'host-resource ok', 'io pressure does not mark the disk chip held');
assert.match(chip('io').title, /stalled on disk I\/O .* Now 23\.4% \(critical\)/, 'the io title explains the measure');
renderHostResources({ ...recovered, io: null });
assert.deepEqual(readings()[3], { k: 'io', v: '–' }, 'a sample without io keeps the chip and reads a dash');
renderHostResources(null);
assert.equal(chips().length, 4, 'a failed poll keeps the io chip');
resetHostResources();
assert.deepEqual(chips().map(node => node.dataset.resource), ['cpu', 'memory', 'disk'], 'a host switch forgets the io chip');

// Provider usage rides the same payload: one button per provider with its name,
// its worst window's value, a meter and the weekly pressure, worst first, behind
// a summary chip that narrow bars show instead. Each opens a dialog.
const observedToday = new Date().toISOString();
const usage = extra => ({
  provider: 'claude', scope: null, window: 'five_hour', used_percent: 5, exhausted: false, threshold: 90, gating: true,
  gated: false, partial: false, stale: false, age_seconds: 30, observed_at: observedToday, until: '2026-10-18T03:00:00Z',
  resets_at: null, crews: ['opus'], ...extra,
});
const staleOff = usage({
  provider: 'codex', window: 'primary', used_percent: 100, exhausted: true, threshold: 'off', stale: true,
  age_seconds: 3 * 86400, observed_at: '2026-10-14T12:00:00Z', crews: [],
});
const unreported = usage({ scope: 'opus', window: 'seven_day', used_percent: null });
const limits = [usage({}), usage({ window: 'seven_day', used_percent: 26, resets_at: '2099-01-02T03:04:00Z' }), staleOff];
const classes = node => node.className;
const line = node => descendants(node)
  .filter(child => ['k', 'v', 'provider-usage-pressure', 'provider-usage-flag', 'provider-usage-count'].includes(child.className))
  .map(child => child.textContent.trim()).join(' ');
const kv = node => Object.fromEntries(['k', 'v'].map(name => [name, descendants(node).find(child => child.className === name)?.textContent.trim()]));
const providerItems = () => providerChips().filter(node => node.className.includes('provider-usage-item'));
const providerSummary = () => providerChips().find(node => node.className.includes('provider-usage-summary'));
const dialog = byId('provider-usage-dialog');
const dialogText = () => dialog.textContent.replace(/\s+/g, ' ').trim();
const dialogRows = () => descendants(dialog).filter(node => node.className.startsWith('usage-row ')).map(node => node.textContent.replace(/\s+/g, ' ').trim());
const pill = () => descendants(dialog).find(node => node.className.startsWith('usage-pill'));
const pin = () => descendants(dialog).find(node => node.className === 'usage-pin');
const expanded = () => providerChips().filter(node => node.getAttribute('aria-expanded') === 'true');
const outsideClick = () => document.fire('click', { target: { closest: () => null } });
// A click bubbles to the document, whose listener ignores a click on a chip.
const clickChip = node => { node.fire('click'); document.fire('click', { target: { closest: () => node } }); };
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const base = 'host-resource provider-usage provider-usage-item';
renderProviderUsage({ provider_limits: limits });
assert.deepEqual(providerItems().map(line), ['codex 100% old', 'claude 26%'],
  'one chip per provider with its worst window\'s value, worst provider first');
assert.deepEqual(providerItems().map(classes), [
  `${base} critical stale gating-off`, `${base} ok`,
], 'stale and gating-off providers are marked in their chip class');
for (const node of [...providerItems(), providerSummary()]) {
  assert.equal(node.tagName, 'button', 'a chip is a real button');
  assert.equal(node.getAttribute('aria-haspopup'), 'dialog');
  assert.equal(node.getAttribute('aria-controls'), 'provider-usage-dialog');
  assert.equal(node.getAttribute('aria-expanded'), 'false');
  assert.equal(node.title, undefined, 'the dialog carries the detail, so there is no native tooltip');
  assert.ok(node.getAttribute('aria-label'), 'the button has an accessible name');
}
assert.match(providerItems()[1].getAttribute('aria-label'), /^claude: 26% used in the 7-day window/);
assert.equal(dialog.hidden, true, 'the dialog stays closed until a chip opens it');
const [codexChip, claudeChip] = providerItems();
assert.equal(descendants(claudeChip).filter(node => node.className === 'provider-usage-flag').length, 0, 'a fresh provider carries no flag');
// The meter is decoration: the value is in the chip's text.
assert.equal(descendants(claudeChip).find(node => node.className === 'provider-usage-meter').getAttribute('aria-hidden'), 'true');
assert.match(descendants(descendants(claudeChip).find(node => node.className === 'provider-usage-meter')).at(0).getAttribute('style'), /width: 26%/);

// Click opens and pins; clicking the pinned chip closes; one dialog at a time.
clickChip(claudeChip);
assert.equal(dialog.hidden, false);
assert.equal(dialog.getAttribute('role'), 'dialog');
assert.deepEqual(expanded(), [claudeChip], 'aria-expanded tracks the open chip');
assert.equal(dialog.getAttribute('aria-label'), 'claude usage');
assert.equal(pill().textContent, 'within limits');
assert.match(dialogText(), /^claude within limits observed .* · .* ago/, 'the header names the provider, its state and the observation');
assert.match(dialogRows()[0], /^7-day 26% resets 2099-01-02 03:04 UTC · in \S+ \S+ gate at 90%$/, 'a window with a reset time says when, absolutely and relatively');
assert.doesNotMatch(dialogRows()[1], /resets/, 'a window the payload gives no reset time omits it');
const tick = descendants(dialog).find(node => node.className === 'usage-meter-tick');
assert.match(tick.getAttribute('style'), /left: 90%/, 'the meter ticks at the gate threshold');
assert.match(dialogText(), /No crews held back/);
assert.equal(pin().textContent, 'Unpin', 'a click pinned it open');
assert.equal(descendants(dialog).some(node => node.className === 'usage-pressure'), false, 'no pressure section without a pressure reading');
clickChip(claudeChip);
assert.equal(dialog.hidden, true, 'clicking the pinned chip closes it');
assert.deepEqual(expanded(), []);

// The gating-off provider: a "no gate" flag, a stale pill, and no gate on its row.
assert.deepEqual(descendants(codexChip).filter(node => node.className === 'provider-usage-flag').map(node => node.textContent), ['old']);
clickChip(codexChip);
assert.deepEqual(expanded(), [codexChip], 'opening another chip closes the first: one dialog at a time');
assert.match(pill().textContent, /^stale · 3d old$/);
assert.match(dialogRows()[0], /^primary old 100% no gate$/);
assert.equal(descendants(dialog).some(node => node.className === 'usage-meter-tick'), false, 'a window with no gate has no tick');
// Esc and an outside click close it, whether or not it is pinned.
document.fire('keydown', { key: 'Escape' });
assert.equal(dialog.hidden, true, 'Esc closes the dialog');
clickChip(codexChip);
assert.equal(dialog.hidden, false);
outsideClick();
assert.equal(dialog.hidden, true, 'a click outside closes the dialog');
// A click inside the dialog does not.
clickChip(codexChip);
document.fire('click', { target: { closest: () => dialog } });
assert.equal(dialog.hidden, false);
// The Pin button toggles the pin; the dialog follows the pointer once unpinned.
pin().fire('click');
assert.equal(pin().textContent, 'Pin open');
assert.equal(pin().getAttribute('aria-pressed'), 'false');
pin().fire('click');
assert.equal(pin().textContent, 'Unpin');
outsideClick();

// Hover opens after an intent delay, keeps open while the pointer crosses to the
// dialog, and closes once it leaves. Touch hover does nothing.
claudeChip.fire('pointerenter', { pointerType: 'touch' });
await sleep(220);
assert.equal(dialog.hidden, true, 'a touch pointer does not hover');
claudeChip.fire('pointerenter', { pointerType: 'mouse' });
assert.equal(dialog.hidden, true, 'hover waits out the intent delay');
await sleep(60);
assert.equal(dialog.hidden, true);
await sleep(200);
assert.equal(dialog.hidden, false, 'hover previews the dialog');
assert.equal(pin().textContent, 'Pin open', 'a hover preview is not pinned');
claudeChip.fire('pointerleave', { pointerType: 'mouse' });
await sleep(80);
dialog.fire('pointerenter', { pointerType: 'mouse' });
await sleep(400);
assert.equal(dialog.hidden, false, 'moving into the dialog keeps it open');
dialog.fire('pointerleave', { pointerType: 'mouse' });
await sleep(400);
assert.equal(dialog.hidden, true, 'leaving the dialog closes the preview');
// A quick pass over a chip never opens it.
claudeChip.fire('pointerenter', { pointerType: 'mouse' });
await sleep(50);
claudeChip.fire('pointerleave', { pointerType: 'mouse' });
await sleep(300);
assert.equal(dialog.hidden, true);
// Clicking a hover preview pins it, and a pinned dialog survives the pointer leaving.
claudeChip.fire('pointerenter', { pointerType: 'mouse' });
await sleep(220);
clickChip(claudeChip);
claudeChip.fire('pointerleave', { pointerType: 'mouse' });
await sleep(400);
assert.equal(dialog.hidden, false);
assert.equal(pin().textContent, 'Unpin');
outsideClick();
// Keyboard focus opens a preview, and moving focus away closes it.
claudeChip.focus();
assert.equal(dialog.hidden, false, 'focus opens the dialog');
assert.deepEqual(expanded(), [claudeChip]);
const tab = claudeChip.fire('keydown', { key: 'Tab' });
assert.equal(tab.prevented, true, 'Tab from the chip moves into the open dialog');
codexChip.focus();
assert.deepEqual(expanded(), [codexChip], 'focusing another chip swaps the dialog');
document.activeElement = null;
codexChip.fire('blur', { relatedTarget: null });
assert.equal(dialog.hidden, true, 'blurring a preview closes it');

// A gated provider shows when it is held until, and which crews are held back.
renderProviderUsage({ provider_limits: [
  usage({ used_percent: 5 }), usage({ window: 'seven_day', used_percent: 85 }),
  usage({ provider: 'codex', window: 'primary', used_percent: 92, gated: true, crews: ['sol', 'luna'], until: '2099-10-18T03:00:00Z' }),
] });
assert.deepEqual(providerItems().map(classes), [`${base} critical`, `${base} elevated`]);
assert.deepEqual(providerItems().map(line), ['codex 92%', 'claude 85%']);
clickChip(providerItems()[0]);
assert.equal(pill().textContent, 'gated until 2099-10-18 03:00 UTC');
assert.match(dialogText(), /Held back: sol, luna/);
assert.match(dialogRows()[0], /^primary gated 92% gate at 90%$/);
outsideClick();
clickChip(providerItems()[1]);
assert.equal(pill().textContent, 'near limit');
outsideClick();

// A scoped window is named by its scope, with the window as its sublabel; a
// provider with many windows lists them all, most used first.
const scoped = (scope, window, used) => usage({ provider: 'antigravity', scope, window, used_percent: used });
renderProviderUsage({
  provider_limits: [
    scoped('argon', 'gemini-argon-wtu-weekly', 0), scoped('claude', '3p-weekly', 0), scoped('flash', 'gemini-weekly', 5),
    scoped('gpt', '3p-weekly', 0), scoped('pro', 'gemini-weekly', 5.4),
  ],
  provider_pressures: [{ provider: 'antigravity', pressure: 1, pressure_window: null }],
});
assert.deepEqual(providerItems().map(line), ['antigravity 5% ×1']);
clickChip(providerItems()[0]);
assert.deepEqual(dialogRows().map(row => row.split(' gate')[0]), [
  'pro gemini · weekly 5%', 'flash gemini · weekly 5%', 'argon gemini · wtu · weekly 0%', 'claude 3p · weekly 0%', 'gpt 3p · weekly 0%',
], 'scope-named rows with the window as sublabel, most used first');
assert.doesNotMatch(dialogText(), /gemini-argon-wtu-weekly/, 'no raw scoped window id');
assert.match(dialogText(), /Weekly pressure ×1 Above ×1 this provider's weekly allowance would expire unused, so its crews are pulled sooner among otherwise equal work; below ×1 they are pulled later; at ×1 nothing changes\. It only orders work and never holds any back\. No live weekly usage, so this reads neutral\./);
outsideClick();

// The weekly pressure shows its basis.
renderProviderUsage({ provider_limits: [usage({ window: 'seven_day', used_percent: 11 })], provider_pressures: [
  { provider: 'claude', pressure: 1.09, pressure_window: { window: 'seven_day', used_percent: 11, resets_at: '2099-01-02T03:04:00Z' } },
] });
clickChip(providerItems()[0]);
assert.match(dialogText(), /Weekly pressure ×1\.09 .* 7-day · 11% used · resets 2099-01-02 03:04 UTC · in \S+ \S+/);
outsideClick();

// A scope with a missing usage reads as a dash and sorts after known readings.
renderProviderUsage({ provider_limits: [unreported, usage({})] });
assert.deepEqual(providerItems().map(line), ['claude 5%', 'claude opus -']);
assert.equal(providerItems()[1].className, `${base} unknown`);
clickChip(providerItems()[1]);
assert.equal(pill().textContent, 'no usage reported');
assert.match(dialogRows()[0], /^opus 7-day -/);
outsideClick();

// The summary chip shows the worst provider and opens the all-providers
// dialog, as does the `+N more` chip; every provider is listed worst first.
renderProviderUsage({ provider_limits: limits });
assert.deepEqual(kv(providerSummary()), { k: 'codex', v: '100%' }, 'the summary chip shows the worst provider');
assert.equal(providerSummary().className, 'host-resource provider-usage provider-usage-summary critical stale gating-off');
assert.match(providerSummary().getAttribute('aria-label'), /^Provider usage: 2 providers, worst codex: 100% used/);
clickChip(providerSummary());
assert.equal(dialog.getAttribute('aria-label'), 'Provider usage, every provider');
assert.deepEqual(dialogRows(), ['codex old 100% primary', 'claude 26% 7-day'], 'every provider, worst first, with its worst window');
assert.deepEqual(expanded(), [providerSummary()]);
outsideClick();

// Beyond three providers the rest fold into one chip that opens the same
// dialog, and the dialog lists every provider, not just the hidden ones.
renderProviderUsage({ provider_limits: [
  ...limits, unreported, usage({ window: 'weekly_extra', used_percent: 60 }),
  usage({ provider: 'gemini', window: 'primary', used_percent: 10 }),
] });
assert.equal(providerItems().length, 4);
assert.deepEqual(providerItems().slice(0, 3).map(line), ['codex 100% old', 'claude 60%', 'gemini 10%']);
const more = providerItems().at(-1);
assert.deepEqual(kv(more), { k: '+1 more', v: undefined });
assert.equal(more.className, `${base} provider-usage-more unknown`);
clickChip(more);
assert.deepEqual(dialogRows().map(row => row.split(' ')[0]), ['codex', 'claude', 'gemini', 'claude'], 'the dialog lists all providers');
assert.equal(dialogRows().length, 4);
assert.match(dialogRows()[1], /^claude 60% weekly extra$/);
assert.deepEqual(expanded(), [more], 'only the chip that opened the dialog is expanded');
outsideClick();

// A poll that changes nothing leaves the chips (and focus and hover on them)
// alone; one that changes the open provider's reading refreshes its dialog in place.
renderProviderUsage({ provider_limits: limits });
const stable = providerChips();
clickChip(providerItems()[1]);
renderProviderUsage({ provider_limits: limits });
assert.ok(providerChips().every((node, index) => node === stable[index]), 'an unchanged poll keeps the same chips');
renderProviderUsage({ provider_limits: [usage({}), usage({ window: 'seven_day', used_percent: 40 }), staleOff] });
assert.equal(dialog.hidden, false, 'an open dialog survives a poll');
assert.match(dialogRows()[0], /^7-day 40%/, 'and shows the new reading');
assert.equal(expanded().length, 1);
renderProviderUsage({ provider_limits: [staleOff] });
assert.equal(dialog.hidden, true, 'a dialog whose provider is gone closes');
outsideClick();

// No readings keep one placeholder, and say why.
const placeholder = payload => { renderProviderUsage(payload); const chips = providerChips(); assert.equal(chips.length, 1); assert.equal(chips[0].className, 'host-resource provider-usage unknown'); return chips[0].title; };
assert.match(placeholder({}), /not reported by this host/, 'an older host sends no field');
assert.match(placeholder({ provider_limits: null }), /not reported by this host/);
assert.match(placeholder({ provider_limits: [] }), /No provider usage readings/);
assert.match(placeholder(null), /unavailable/);
renderProviderUsage({ provider_limits: limits });
clickChip(providerItems()[0]);
renderProviderUsage(null);
assert.equal(dialog.hidden, true, 'losing the readings closes the dialog');

// The readings follow the host poll: a fetch fills them, the 1s staleness tick
// leaves them alone, a failed poll or a host switch clears them.
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: limits }) };
await fetchAndRenderHostResources();
assert.equal(providerItems().length, 2, 'the poll renders the selected host\'s readings');
const shown = providerChips()[0];
timers.get(1000)();
assert.equal(providerChips()[0], shown, 'the staleness tick does not rebuild the provider chips');
resetHostResources();
assert.match(providerChips()[0].title, /unavailable/, 'switching host drops the previous host\'s readings');
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: limits }) };
await fetchAndRenderHostResources();
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.match(providerChips()[0].title, /unavailable/, 'a failed poll does not leave stale readings');
// The other host's readings replace the previous host's, and a hidden tab
// requests nothing: the readings have no poll of their own.
const otherHost = [usage({ provider: 'codex', window: 'primary', used_percent: 61 })];
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: otherHost }) };
await fetchAndRenderHostResources();
assert.deepEqual(providerItems().map(line), ['codex 61%']);
document.hidden = true;
const requests = urls.length;
await timers.get(5000)();
assert.equal(urls.length, requests, 'a hidden tab does not poll host resources or provider usage');
document.hidden = false;

console.log('topbar host chips (load, mem, disk, io where reported): per-chip severity and held state, many paths, fixed text across throttle, unknown/stale and polling, and per-provider usage chips and dialogs (open, pin, close, hover, focus, windows, gate, reset, held crews, stale, gating-off, missing usage, fold, summary, host reset) passed');

// Settings Hosts shares only a concurrent/fresh topbar read for the same host.
const { fetchHostResourcePayloadFor } = resource.namespace;
const identity = { name: 'serving-name', machine_id: 'local', local: true };
common.namespace.setRegisteredHosts([identity]);
resetHostResources();
nextResponse = new Promise(resolve => { resolvePending = resolve; });
const sharedStart = urls.length;
const topbarRead = fetchAndRenderHostResources();
const hostsRead = fetchHostResourcePayloadFor('serving-name', { shareSelected: true, identity });
assert.equal(urls.length, sharedStart + 1, 'concurrent Hosts and topbar reads issue one resource request');
resolvePending({ ok: true, json: async () => recovered });
await topbarRead;
assert.equal(await hostsRead, recovered);
assert.equal(await fetchHostResourcePayloadFor('serving-name', { shareSelected: true, identity }), recovered);
assert.equal(urls.length, sharedStart + 1, 'the just-completed topbar result is shared');
nextResponse = { ok: true, json: async () => payload };
await fetchHostResourcePayloadFor('other-host', { shareSelected: true });
assert.equal(urls.at(-1), '/api/on/other-host/host/resources', 'another host never inherits the selected snapshot');
now += 1001;
assert.equal(await fetchHostResourcePayloadFor('serving-name', { shareSelected: true, identity }), payload);
assert.equal(urls.at(-1), '/api/on/serving-name/host/resources', 'a later panel refresh reads resources again');
resetHostResources();
nextResponse = new Promise(resolve => { resolvePending = resolve; });
const retired = fetchAndRenderHostResources();
resetHostResources();
nextResponse = { ok: true, json: async () => recovered };
const afterReset = urls.length;
await fetchHostResourcePayloadFor('serving-name', { shareSelected: true, identity });
assert.equal(urls.length, afterReset + 1, 'a host switch invalidates even an in-flight shared read');
resolvePending({ ok: true, json: async () => payload });
await retired;
console.log('resource request sharing: concurrent, fresh, expired, different host and reset passed');

common.namespace.setRegisteredHosts([{ name: 'remote', machine_id: 'remote-id', ssh: 'old-target' }]);
common.namespace.setHost('remote');
nextResponse = { ok: true, json: async () => recovered };
await fetchAndRenderHostResources();
const beforeRetarget = urls.length;
await fetchHostResourcePayloadFor('remote', { shareSelected: true, identity: { name: 'remote', machine_id: 'remote-id', ssh: 'new-target' } });
assert.equal(urls.length, beforeRetarget + 1, 'a retargeted SSH destination cannot borrow the previous destination reading');
console.log('resource request sharing: registry retarget invalidates the selected snapshot');

// Pressure is host-computed, uncapped, and grouped by credential account.
const pressures = [
  { provider: 'claude', pressure: 25.2, pressure_window: { window: 'seven_day', used_percent: 10, resets_at: '2026-10-24T18:00:00Z' } },
  { provider: 'claude', account: 'credits', pressure: 0.5, pressure_window: { window: 'seven_day', used_percent: 50, resets_at: '2026-10-30T18:00:00Z' } },
  { provider: 'gemini', pressure: 1, pressure_window: null },
];
renderProviderUsage({ provider_limits: [usage({ used_percent: 10, window: 'seven_day' }), usage({ account: 'credits', used_percent: 50, window: 'seven_day' })], provider_pressures: pressures });
assert.deepEqual(providerItems().map(line), ['claude 10% ×25.2', 'claude/credits 50% ×0.5', 'gemini - ×1'], 'one chip per account, with the raw pressure');
clickChip(providerItems()[0]);
assert.match(dialogText(), /Weekly pressure ×25\.2 .* 7-day · 10% used · resets 2026-10-24 18:00 UTC/);
outsideClick();
clickChip(providerItems()[2]);
assert.match(dialogText(), /No live weekly usage/);
assert.equal(pill().textContent, 'no usage reported');
outsideClick();
assert.deepEqual(kv(providerSummary()), { k: 'claude', v: '10%' });
// A model-scoped window names its scope once: `seven_day_opus` under scope
// `opus` is the row `opus`, sublabel `7-day`; an account-wide window keeps its name.
renderProviderUsage({ provider_limits: [
  usage({ used_percent: 10, window: 'seven_day' }), usage({ scope: 'opus', used_percent: 40, window: 'seven_day_opus' }),
  usage({ account: 'credits', scope: 'sonnet', used_percent: 20, window: 'seven_day_sonnet' }),
], provider_pressures: pressures });
assert.deepEqual(providerItems().map(line), ['claude 40% ×25.2', 'claude/credits 20% ×0.5', 'gemini - ×1']);
clickChip(providerItems()[0]);
assert.deepEqual(dialogRows().map(row => row.split(' gate')[0]), ['opus 7-day 40%', '7-day 10%']);
outsideClick();
renderProviderUsage({ provider_limits: [], provider_pressures: [{ provider: 'codex', pressure: 0, pressure_window: null }] });
assert.deepEqual(providerItems().map(line), ['codex - ×0'], 'zero pressure is displayed, not treated as missing');
console.log('weekly pressure: raw values, account isolation, neutral and zero display passed');
