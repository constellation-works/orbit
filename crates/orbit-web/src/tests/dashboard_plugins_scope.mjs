// A late plugin toggle must never repaint a workspace we have left, including
// the aggregate placeholder and a new workspace whose list read is pending.
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const { setWorkspace, setMultiWorkspace } = await import('./js/common.js');
const { fetchAndRenderPlugins } = await import('./js/plugins.js');
const descendants = node => [node, ...(node.children || []).flatMap(descendants)];
const body = () => document.getElementById('plugins-body');
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const flush = async () => { for (let i = 0; i < 5; i++) await tick(); };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const plugin = workspace => ({ name: 'fixture', version: '1', status: 'active', host_enabled: true, workspace_toggle: true, description: `${workspace} metadata`, capabilities: { disable: { authorized: true }, enable: { authorized: true } }, panels: [], links: [], tools: [] });
let finishChange;
let finishList;
let deferList = false;
globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  if (options.method === 'POST') return new Promise(resolve => { finishChange = resolve; });
  if (deferList) return new Promise(resolve => { finishList = resolve; });
  return response([plugin(url.searchParams.get('workspace'))]);
};
const toggle = () => descendants(body()).find(node => node.tagName === 'BUTTON' && node.textContent === 'Disable workspace');
setMultiWorkspace(true);
setWorkspace('A');
await fetchAndRenderPlugins();
toggle().click();
assert(finishChange, 'the toggle reaches the mutation endpoint');
setWorkspace(null);
await fetchAndRenderPlugins();
assert(body().textContent === 'Select a workspace to view this panel', 'aggregate scope begins with its placeholder');
finishChange(response({}));
await flush();
assert(body().textContent === 'Select a workspace to view this panel', `late success must preserve aggregate scope: ${body().textContent}`);

setWorkspace('A');
await fetchAndRenderPlugins();
toggle().click();
setWorkspace('B');
deferList = true;
const listRead = fetchAndRenderPlugins();
finishChange(response({ error: 'A mutation failed' }, 500));
await flush();
assert(!body().textContent.includes('A metadata'), `a late failure must not restore A while B loads: ${body().textContent}`);
assert(!body().textContent.includes('A mutation failed'), 'A mutation error must not be presented as a failure in B');
finishList(response([plugin('B')]));
await listRead;
assert(body().textContent.includes('B metadata'), 'B renders its own metadata');
assert(!body().textContent.includes('A mutation failed'), 'B remains free of A mutation errors after its read lands');
