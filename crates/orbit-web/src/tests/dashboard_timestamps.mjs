// Execute the shipped dashboard clock under a zone away from UTC: the Rust
// harness runs this with TZ=America/Los_Angeles, so a local time rendered as
// UTC (or the reverse) cannot pass.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

assert.equal(Intl.DateTimeFormat().resolvedOptions().timeZone, 'America/Los_Angeles', 'the harness pins the zone');

const context = vm.createContext({ URLSearchParams, window: { location: { search: '', hash: '' } }, console });
const load = name => new vm.SourceTextModule(fs.readFileSync(new URL(`../../assets/dashboard/js/${name}`, import.meta.url), 'utf8'), { context, identifier: name });
const common = load('common.js');
const reliability = load('reliability.js');
await common.link(() => { throw new Error('unexpected common dependency'); });
await reliability.link(name => { assert.equal(name, './common.js'); return common; });
await reliability.evaluate();
const { formatAge, formatClock, formatDateTime, formatUtcDateTime, formatUtcRange, fmtDuration, elapsedDurationInfo } = common.namespace;
const { fmtWindowRange } = reliability.namespace;

// The incident: a 06:33Z → 06:33Z window read "Oct 5, 11:33 PM → Oct 6, 11:33 PM UTC".
assert.equal(
  fmtWindowRange({ since: '2026-10-06T06:33:00Z', until: '2026-10-07T06:33:00Z', bucket: 'hour' }),
  '2026-10-06 06:33 → 2026-10-07 06:33 UTC · hour buckets',
  'the Reliability range labelled UTC shows the UTC instants',
);
assert.equal(fmtWindowRange({ since: 'garbage', until: '2026-10-07T06:33:00Z' }), '', 'an unreadable end drops the range');
assert.equal(formatUtcRange('2026-10-06T23:00:00-07:00', '2026-10-07T00:00:00-07:00'), '2026-10-07 06:00 → 2026-10-07 07:00 UTC', 'offsets convert to UTC');
assert.equal(formatUtcDateTime('2026-10-07T06:33:00Z'), '2026-10-07 06:33 UTC');

// Absolute local times are 24-hour and name their zone, across daylight saving.
assert.equal(formatDateTime('2026-10-07T06:31:00Z'), '2026-10-06 23:31 PDT', 'task and run detail time');
assert.equal(formatDateTime('2026-12-07T06:31:09Z', { seconds: true }), '2026-12-06 22:31:09 PST', 'winter time names PST');
assert.equal(formatDateTime(''), '-');
assert.equal(formatDateTime('not a time'), 'not a time', 'unparseable values are shown as given');

// One clock for the rail, log dock and status bar: 24-hour with seconds.
assert.equal(formatClock('2026-10-07T06:32:57Z'), '23:32:57 PDT', 'the rail clock names its zone');
assert.equal(formatClock('2026-10-07T06:32:57Z', { zone: false }), '23:32:57', 'dense log columns carry the zone in a title');
assert.equal(formatClock('2026-10-07T07:05:00Z', { seconds: false }), '00:05 PDT', 'midnight is 00, never 24 or 12 AM');

const now = Date.parse('2026-10-07T12:00:00Z');
assert.equal(formatAge('2026-10-07T11:59:30Z', now), '30s');
assert.equal(formatAge('2026-10-07T09:00:00Z', now), '3h');
assert.equal(formatAge('2026-10-05T12:00:00Z', now), '2d');
assert.equal(formatAge('2026-10-07T12:00:30Z', now), '0s', 'a reading from a slightly fast clock is not negative');

// Duration unit boundaries preserve sub-hour precision and roll up longer runs.
for (const [ms, expected] of [
  [null, '-'], ['', '-'], [NaN, '-'], [Infinity, '-'], [-1, '-'],
  [0, '0ms'], [12.7, '13ms'], [999, '999ms'], [1000, '1.0s'],
  [12345, '12.3s'], [59999, '60.0s'], [60000, '1m 0s'],
  [303000, '5m 3s'], [3599999, '59m 59s'], [3600000, '1h 0m'],
  [13191607, '3h 39m'], [86399999, '23h 59m'], [86400000, '1d 0h'],
  [183660000, '2d 3h'],
]) assert.equal(fmtDuration(ms), expected, `duration ${ms} respects unit boundaries`);

// Only running records with readable starts may replace missing/zero durations.
for (const [record, expectedMs, isLive] of [
  [{ state: 'running', started_at: '2026-10-07T11:59:30Z' }, 30000, true],
  [{ state: 'running', started_at: '2026-10-07T11:59:30Z', duration_ms: 0 }, 30000, true],
  [{ state: 'running', started_at: '2026-10-07T12:00:30Z' }, 0, true],
  [{ state: 'running', started_at: 'invalid' }, undefined, false],
  [{ state: 'running' }, undefined, false],
  [{ state: 'running', started_at: '2026-10-07T11:59:30Z', duration_ms: 1000 }, 1000, false],
  [{ state: 'success', started_at: '2026-10-07T11:59:30Z', duration_ms: 1000 }, 1000, false],
  [{ state: 'pending', started_at: '2026-10-07T11:59:30Z' }, undefined, false],
]) {
  const duration = elapsedDurationInfo(record, record.duration_ms, now);
  assert.equal(duration.durationMs, expectedMs);
  assert.equal(duration.isLive, isLive);
}
