// Run the actual bundled script with a deterministic host/DOM fixture.
// This is automated bridge behavior evidence, not a native desktop pass.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { test } from 'node:test';
const source = process.env.ORBIT_PANEL_RESOURCE
  ? readFileSync(process.env.ORBIT_PANEL_RESOURCE, 'utf8').match(/<script>([\s\S]*?)<\/script>/)?.[1]
  : readFileSync(new URL('../task-panel.js', import.meta.url), 'utf8');
assert.equal(typeof source, 'string', 'the served bundle must contain executable panel JavaScript');
const flush = () => new Promise((resolve) => setImmediate(resolve));
function fixture() {
  const nodes = new Map();
  const posted = [], timers = new Map();
  let message;
  const parent = { postMessage: (value) => posted.push(value) };
  const get = (id) => {
    if (!nodes.has(id)) nodes.set(id, {
      textContent: '', value: '', disabled: ['read', 'send'].includes(id), handlers: {},
      set innerHTML(_) { throw new Error('unsafe HTML rendering'); },
      addEventListener(type, handler) { this.handlers[type] = handler; },
    });
    return nodes.get(id);
  };
  vm.runInNewContext(source, {
    window: { parent, addEventListener: (_, handler) => { message = handler; } },
    document: { getElementById: get },
    setTimeout: (fn) => { const id = timers.size + 1; timers.set(id, fn); return id; },
    clearTimeout: (id) => timers.delete(id), Date, Map, JSON, Error,
  });
  const receive = (data, sender = parent) => message({ source: sender, data });
  const answer = (request, result, error) => receive({ jsonrpc: '2.0', id: request.id, result, error });
  const submit = (workspace = 'hm_demo/ws_tasks', id = 'TST-1') => {
    get('workspace').value = workspace; get('task-key').value = id;
    get('selection').handlers.submit({ preventDefault() {} });
    return posted.at(-1);
  };
  const task = (id = 'TST-1') => ({ id, title: '<img src=x onerror=alert(1)>', updated_at: '2026-10-03T00:00:00Z', status: 'proposed', description: '<script>bad()</script>', acceptance_criteria: ['<a href=javascript:bad()>'] });
  const initialize = async (caps = { serverTools: {}, updateModelContext: {} }) => {
    answer(posted[0], { protocolVersion: '2026-01-26', hostCapabilities: caps }); await flush();
  };
  return { get, posted, receive, answer, submit, task, initialize, timers };
}

test('initializes before reading; renders unsafe content as text and sends a bounded reference only on click', async () => {
  const f = fixture();
  assert.equal(f.posted[0].method, 'ui/initialize');
  assert.equal(f.get('read').disabled, true);
  await f.initialize();
  assert.equal(f.posted[1].method, 'ui/notifications/initialized');
  assert.equal(f.get('read').disabled, false);
  const read = f.submit();
  assert.equal(read.method, 'tools/call'); assert.equal(read.params.name, 'orbit_task_show');
  assert.equal(read.params.arguments.workspace, 'hm_demo/ws_tasks');
  f.answer(read, { structuredContent: f.task(), isError: false }); await flush();
  assert.equal(f.get('title').textContent, f.task().title);
  assert.ok(f.get('details').textContent.includes('<script>bad()</script>'));
  assert.equal(f.get('send').disabled, false);
  assert.ok(!f.posted.some((m) => m.method === 'ui/update-model-context'));
  f.get('send').handlers.click();
  const context = f.posted.at(-1); assert.equal(context.method, 'ui/update-model-context');
  const reference = JSON.parse(context.params.content[0].text);
  assert.equal(reference.workspace, read.params.arguments.workspace);
  assert.equal(reference.id, 'TST-1'); assert.equal(reference.authority, 'none');
  assert.ok(!('description' in reference));
  f.answer(context, {}); await flush();
});

test('late, failed, mismatched, and changed selections cannot supply fresh context', async () => {
  const f = fixture(); await f.initialize();
  const old = f.submit('first', 'TST-1'); const current = f.submit('second', 'TST-2');
  f.answer(current, { structuredContent: f.task('TST-2') }); await flush();
  f.answer(old, { structuredContent: f.task() }); await flush();
  assert.ok(f.get('identity').textContent.startsWith('second'));
  const failed = f.submit('second', 'TST-2');
  f.answer(failed, { isError: true, structuredContent: { message: 'offline' } }); await flush();
  assert.equal(f.get('send').disabled, true); assert.ok(f.get('state').textContent.includes('stale'));
  const mismatch = f.submit(); f.answer(mismatch, { structuredContent: f.task('TST-2') }); await flush();
  assert.equal(f.get('send').disabled, true);
  const good = f.submit(); f.answer(good, { structuredContent: f.task() }); await flush();
  f.get('workspace').handlers.input(); assert.equal(f.get('send').disabled, true);
  assert.equal(f.get('reference').textContent, '');
});

test('untrusted frames cannot initialize or render; missing context support provides a copy reference', async () => {
  const f = fixture();
  f.receive({ jsonrpc: '2.0', id: f.posted[0].id, result: { protocolVersion: '2026-01-26', hostCapabilities: { serverTools: {} } } }, {});
  assert.equal(f.get('read').disabled, true);
  await f.initialize({ serverTools: {} });
  const read = f.submit(); f.answer(read, { structuredContent: f.task() }); await flush();
  f.get('send').handlers.click(); await flush();
  assert.equal(f.posted.at(-1).method, 'tools/call');
  assert.equal(JSON.parse(f.get('reference').textContent).authority, 'none');
  const title = f.get('title').textContent;
  f.receive({ jsonrpc: '2.0', method: 'ui/notifications/tool-result', params: { structuredContent: { schema_version: 1, workspace: 'attacker', task: f.task('TST-2') } } }, {});
  assert.equal(f.get('title').textContent, title);
});

test('timeouts, missing tool capability and teardown refuse reads/context', async () => {
  const missing = fixture(); await missing.initialize({ updateModelContext: {} });
  assert.equal(missing.get('read').disabled, true);
  const f = fixture(); await f.initialize();
  f.submit(); for (const timer of [...f.timers.values()]) timer(); await flush();
  assert.equal(f.get('send').disabled, true);
  const good = f.submit(); f.answer(good, { structuredContent: f.task() }); await flush();
  const pendingRead = f.submit();
  f.receive({ jsonrpc: '2.0', id: pendingRead.id, method: 'ui/resource-teardown' });
  await flush();
  assert.equal(f.get('read').disabled, true); assert.equal(f.get('send').disabled, true);
  assert.equal(f.posted.at(-1).id, pendingRead.id);
  assert.ok('result' in f.posted.at(-1), 'teardown is acknowledged even when its id collides with the active read');
  f.answer(pendingRead, { structuredContent: f.task() }); await flush();
  assert.equal(f.get('send').disabled, true);
});


test('teardown racing initialization cannot re-enable a disposed panel', async () => {
  const f = fixture();
  f.answer(f.posted[0], { protocolVersion: '2026-01-26', hostCapabilities: { serverTools: {} } });
  f.receive({ jsonrpc: '2.0', id: 99, method: 'ui/resource-teardown' });
  await flush();
  assert.equal(f.get('read').disabled, true);
  assert.equal(f.get('send').disabled, true);
  assert.ok(!f.posted.some((m) => m.method === 'ui/notifications/initialized'));
});
