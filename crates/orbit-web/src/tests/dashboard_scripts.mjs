// Runs in Chromium via dashboard_operations_browser.mjs.
// Scripts grant feedback is visit-scoped: a grant started in one
// host/workspace visit never paints another visit (including A→B→A, which is
// a new visit), the request stays bound to its reviewed digest and workspace,
// and a successful POST with a failed readback keeps the grant outcome with a
// separate refresh warning instead of "Grant refused" [ORB-21565].
const { setHost, setRegisteredHosts, setWorkspace } = await import('./js/common.js');
const { initOperations } = await import('./js/operations-shared.js');
const { fetchAndRenderScripts } = await import('./js/operations-scripts.js');
const { initRouter, setActiveTab } = await import('./js/router.js');

const get = (id) => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const tick = async (n = 20) => { for (let i = 0; i < n; i++) await new Promise((r) => setTimeout(r, 0)); };
const response = (payload, status = 200) => ({
  ok: status === 200,
  status,
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});

const allowed = { authorized: true, reason: null };
const capabilities = { grant: allowed };
const DIGEST_A = 'a'.repeat(64);
const DIGEST_B = 'b'.repeat(64);

const scriptFor = (digest) => ({
  name: 'deploy',
  state: 'stale',
  entry: 'tools/deploy.sh',
  detail: '',
  current_digest: digest,
  granted_digest: '0'.repeat(64),
  requested: { network: 'none', host_role: 'owner' },
  granted: { network: 'none', host_role: 'owner' },
  open_request: null,
});

// After a successful grant the readback reports the script granted.
let grantedDigest = null;
let failReadback = false;
let holdGrant = null;
const requests = [];
const grantPosts = [];

globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  const pathname = url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/');
  const workspace = url.searchParams.get('workspace');
  const method = options.method || 'GET';
  requests.push({ method, pathname, workspace, body: options.body ? JSON.parse(options.body) : null });
  if (pathname === '/api/scripts/deploy/grant' && method === 'POST') {
    const body = JSON.parse(options.body);
    grantPosts.push({ workspace, pathname: '/api/scripts/deploy/grant', body });
    if (holdGrant) {
      await new Promise((resolve) => { holdGrant.release = resolve; });
      holdGrant = null;
    }
    if (body?.digest === 'refused-digest') return response({ error: 'digest changed since review' }, 409);
    grantedDigest = body?.digest ?? null;
    return response({ grant: { digest: grantedDigest, provenance: 'dashboard' } });
  }
  if (pathname === '/api/scripts' && method === 'GET') {
    if (failReadback) throw new Error('refresh unavailable');
    const digest = workspace === 'ws-b' ? DIGEST_B : DIGEST_A;
    const current = grantedDigest === digest ? digest : digest;
    const state = grantedDigest === digest ? 'granted' : 'stale';
    return response({ scripts: [{ ...scriptFor(digest), current_digest: current, state }], capabilities });
  }
  return response({});
};

const workspaces = [
  { id: 'ws-a', name: 'ws-a', status: 'active' },
  { id: 'ws-b', name: 'ws-b', status: 'active' },
];
setRegisteredHosts([{ name: 'fixture-host', local: true }]);
setWorkspace('ws-a');
initOperations({ getWorkspaces: () => workspaces });
initRouter({
  getRunId: () => null, setRunId: () => {},
  getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
  setRunDetail: () => {}, setRunEvents: () => {}, setRunLogs: () => {},
  getRunSubtab: () => 'steps', setRunSubtab: () => {},
  setTab: () => {}, getOperationsSubtab: () => 'scripts', setOperationsSubtab: () => {},
  refreshDashboard: () => {},
});
setActiveTab('operations/scripts');

for (const id of ['scripts-body', 'scripts-count', 'script-operation-feedback']) {
  assert(get(id), `scripts fixture needs #${id}`);
}

const feedbackText = () => get('script-operation-feedback').textContent;
const grantButton = () => Array.from(get('scripts-body').querySelectorAll('button')).find((node) => node.textContent.startsWith('Grant '));
const grantedBodies = () => requests.filter((r) => r.method === 'POST');
// Force a scope round-trip so feedback is cleared even when the target equals
// the current workspace (setWorkspace is a no-op for the same id).
const cleanTo = async (id) => {
  const other = id === 'ws-a' ? 'ws-b' : 'ws-a';
  setWorkspace(other);
  await fetchAndRenderScripts();
  await tick();
  setWorkspace(id);
  await fetchAndRenderScripts();
  await tick();
  grantPosts.length = 0;
  requests.length = 0;
};

try {
  // Normal same-visit grant success paints and reads back.
  grantedDigest = null;
  failReadback = false;
  await fetchAndRenderScripts();
  await tick();
  let button = grantButton();
  assert(button && !button.disabled, 'same-visit grant offers an enabled Grant button');
  button.click();
  await tick();
  assert(feedbackText().includes(`Granted deploy at ${DIGEST_A.slice(0, 12)}.`), `same-visit grant reports success: ${feedbackText()}`);
  assert(grantedBodies().at(-1)?.workspace === 'ws-a' && grantedBodies().at(-1)?.body.digest === DIGEST_A, 'same-visit grant posts the reviewed digest to its workspace');

  // Normal same-visit refusal reports Grant refused and re-enables.
  grantedDigest = null;
  await fetchAndRenderScripts();
  await tick();
  // Force the next POST to refuse by posting a stale digest directly through
  // the rendered control's digest: temporarily rewrite the fixture digest.
  button = grantButton();
  assert(button && !button.disabled, 'refusal case starts from an enabled Grant button');
  // Drive the refusal path through the same rendered control by holding the
  // test at the POST: the fixture refuses the sentinel digest below.
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (path, options = {}) => {
    const url = new URL(path, 'http://dashboard.test');
    if (url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/') === '/api/scripts/deploy/grant') {
      return response({ error: 'grant denied in ws-a' }, 403);
    }
    return originalFetch(path, options);
  };
  button.click();
  await tick();
  assert(feedbackText().startsWith('Grant refused:'), `same-visit refusal reports Grant refused: ${feedbackText()}`);
  button = grantButton();
  assert(button && !button.disabled, 'a refused grant re-enables its button for a retry');
  globalThis.fetch = originalFetch;

  // Late success after A→B cannot paint B or start its readback.
  grantedDigest = null;
  failReadback = false;
  await cleanTo('ws-a');
  button = grantButton();
  assert(button, 'abandoned-visit case renders a Grant button in A');
  holdGrant = {};
  button.click();
  await tick(5);
  assert(grantPosts.length === 1 && grantPosts[0].workspace === 'ws-a', 'the held grant started in A and stays unsettled');
  assert(feedbackText() === '', 'no feedback lands while the grant is held');
  setWorkspace('ws-b');
  await fetchAndRenderScripts();
  await tick();
  const readsForB = requests.filter((r) => r.method === 'GET' && r.pathname === '/api/scripts').length;
  assert(feedbackText() === '', `switching to B clears completed Scripts feedback: ${JSON.stringify(feedbackText())}`);
  holdGrant.release?.();
  await tick();
  assert(feedbackText() === '', `late A success never paints B: ${JSON.stringify(feedbackText())}`);
  const readsAfter = requests.filter((r) => r.method === 'GET' && r.pathname === '/api/scripts').length;
  assert(readsAfter === readsForB, 'a stale grant starts no readback for the current visit');
  assert(grantPosts.length === 1 && grantPosts[0].workspace === 'ws-a' && grantPosts[0].body.digest === DIGEST_A, `the held grant stayed bound to A and its digest: ${JSON.stringify(grantPosts)}`);

  // Late refusal after A→B cannot paint B either.
  grantedDigest = null;
  failReadback = false;
  await cleanTo('ws-a');
  button = grantButton();
  holdGrant = {};
  const refusingFetch = globalThis.fetch;
  globalThis.fetch = async (path, options = {}) => {
    const url = new URL(path, 'http://dashboard.test');
    if (url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/') === '/api/scripts/deploy/grant') {
      grantPosts.push({ workspace: url.searchParams.get('workspace'), pathname: '/api/scripts/deploy/grant', body: JSON.parse(options.body) });
      if (holdGrant) {
        await new Promise((resolve) => { holdGrant.release = resolve; });
        holdGrant = null;
      }
      return response({ error: 'grant denied in A' }, 403);
    }
    return refusingFetch(path, options);
  };
  button.click();
  await tick(5);
  setWorkspace('ws-b');
  await fetchAndRenderScripts();
  await tick();
  holdGrant.release?.();
  await tick();
  assert(feedbackText() === '', `late A refusal never paints B: ${JSON.stringify(feedbackText())}`);
  globalThis.fetch = refusingFetch;

  // A→B→A is a new visit: the first A grant stays silent after the return.
  grantedDigest = null;
  failReadback = false;
  await cleanTo('ws-a');
  button = grantButton();
  holdGrant = {};
  button.click();
  await tick(5);
  setWorkspace('ws-b');
  await fetchAndRenderScripts();
  await tick();
  setWorkspace('ws-a');
  await fetchAndRenderScripts();
  await tick();
  const readsBeforeReturn = requests.filter((r) => r.method === 'GET').length;
  holdGrant.release?.();
  await tick();
  assert(feedbackText() === '', `returning to A does not replay the abandoned grant: ${JSON.stringify(feedbackText())}`);
  assert(requests.filter((r) => r.method === 'GET').length === readsBeforeReturn, 'the abandoned grant starts no readback after A→B→A');

  // A successful POST with a failed GET keeps the grant and warns separately.
  grantedDigest = null;
  failReadback = false;
  await cleanTo('ws-a');
  button = grantButton();
  failReadback = true;
  const readbackPosts = grantedBodies().length;
  button.click();
  await tick();
  const text = feedbackText();
  assert(text.includes(`Granted deploy at ${DIGEST_A.slice(0, 12)}.`), `readback failure keeps the grant outcome: ${text}`);
  assert(text.includes('Refresh failed:'), `readback failure warns distinctly: ${text}`);
  assert(!text.startsWith('Grant refused:'), `readback failure never reports Grant refused: ${text}`);
  assert(grantedBodies().length === readbackPosts + 1, 'the failed readback issues no duplicate grant');
  button = grantButton();
  assert(!button || button.disabled, 'the stale Grant button stays disabled after a failed readback');
  failReadback = false;

  // Scope reset clears Scripts feedback alongside the shared operations reset.
  grantedDigest = null;
  failReadback = false;
  await cleanTo('ws-a');
  button = grantButton();
  button.click();
  await tick();
  assert(feedbackText().includes('Granted deploy'), `reset case starts with grant feedback: ${feedbackText()}`);
  setWorkspace('ws-b');
  await fetchAndRenderScripts();
  await tick();
  assert(feedbackText() === '', `scope change clears Scripts feedback: ${JSON.stringify(feedbackText())}`);

  globalThis.dashboardScriptsTestsPassed = true;
} catch (error) {
  globalThis.dashboardScriptsError = error instanceof Error ? error.message : String(error);
}
