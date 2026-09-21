// Drives the shipped Config module against a fetch stub [ORB-12724]: what it
// paints for a layered payload, and what it writes when a row is edited.
const { setWorkspace } = await import('./common.js');
const { fetchAndRenderConfig, initConfig, setConfigSubtab } = await import('./config.js');

const panel = id => document.getElementById(id);
const descendants = node => [node, ...Array.from(node.children || []).flatMap(descendants)];
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const withClass = (id, name) => descendants(panel(id)).filter(node => String(node.className || '').split(/\s+/).includes(name));
const button = (id, label) => descendants(panel(id)).find(node => node.textContent === label && node.type === 'button');

const requests = [];
let workspaceBaseBranch = 'agent-main';
let refusal = null;

const effective = () => ({
  scope: 'effective',
  config_set: { authorized: true, reason: null },
  crew_fields: ['provider', 'model', 'effort', 'tags', 'description'],
  workspace_file_exists: true,
  write_scope_default: 'workspace',
  layers: {
    built_in: { label: 'built-in' },
    global: { path: '/home/op/.orbit/config.toml', exists: true },
    workspace: { path: '/repo/.orbit/config.toml', exists: true },
    execution_not_inherited: true,
    not_inherited_keys: ['execution.codex.sandbox'],
  },
  workspace_binding: {
    base_branch: 'agent-main',
    ship_mode: 'pr',
    owner_machine_id: 'hm_1',
    repo_root: '/repo',
    source: 'workspace-registry',
    workflow_base_branch: workspaceBaseBranch,
    base_branch_matches_workflow: workspaceBaseBranch === 'agent-main',
  },
  sections: [
    {
      token: 'delivery',
      title: 'Delivery (workflow.*)',
      blurb: 'how tasks are shipped',
      key_prefix: 'workflow',
      kind: 'keys',
      counts: { set: 1, default: 1, unset: 0, total: 2 },
      not_inherited: 0,
      keys: [
        {
          key: 'workflow.base_branch',
          label: 'base_branch',
          value: workspaceBaseBranch,
          value_type: 'string',
          options: [],
          state: 'set',
          section: 'delivery',
          description: 'Config fallback for the ship base branch.',
          source: { layer: 'workspace', path: '/repo/.orbit/config.toml' },
          shadowed_by: [{ layer: 'global', value: 'trunk', reason: 'overridden', note: 'overrides global: trunk' }],
        },
        {
          key: 'workflow.auto_ship',
          label: 'auto_ship',
          value: false,
          value_type: 'bool',
          options: [],
          state: 'default',
          section: 'delivery',
          description: 'Opt-in for unattended ship dispatch.',
          source: { layer: 'built-in', path: null },
          shadowed_by: [],
        },
      ],
    },
    { token: 'crews', title: 'Crews (crews.*)', blurb: 'named provider/model assignments', key_prefix: 'crews', kind: 'crews', counts: { set: 0, default: 0, unset: 0, total: 0 }, not_inherited: 0, keys: [] },
    {
      token: 'execution',
      title: 'Execution (execution.*)',
      blurb: 'how agent subprocesses run',
      key_prefix: 'execution',
      kind: 'keys',
      counts: { set: 0, default: 1, unset: 0, total: 1 },
      not_inherited: 1,
      keys: [
        {
          key: 'execution.codex.sandbox',
          label: 'codex.sandbox',
          value: 'workspace-write',
          value_type: 'string',
          options: ['read-only', 'workspace-write', 'danger-full-access'],
          state: 'default',
          section: 'execution',
          description: 'Codex sandbox mode.',
          source: { layer: 'built-in', path: null },
          shadowed_by: [{
            layer: 'global',
            value: 'danger-full-access',
            reason: 'not-inherited',
            note: 'global sets danger-full-access — not inherited while a workspace file exists',
          }],
        },
      ],
    },
    {
      token: 'operation',
      title: 'Operation mode (operation.*)',
      blurb: 'unattended-operation policy',
      key_prefix: 'operation',
      kind: 'keys',
      counts: { set: 0, default: 0, unset: 1, total: 1 },
      not_inherited: 0,
      keys: [
        {
          key: 'operation.preset',
          label: 'preset',
          value: null,
          value_type: 'string',
          options: ['supervised', 'autonomous'],
          state: 'unset',
          section: 'operation',
          description: 'Operation-mode preset.',
          source: { layer: 'built-in', path: null },
          shadowed_by: [],
        },
      ],
    },
    { token: 'housekeeping', title: 'Housekeeping', blurb: 'logs, scoring, ids, and PR links', key_prefix: null, kind: 'keys', counts: { set: 0, default: 0, unset: 0, total: 0 }, not_inherited: 0, keys: [] },
  ],
  crews: [
    { name: 'opus', provider: 'claude', model: 'opus', effort: null, tags: [], description: null, source: 'built-in', referenced_by: ['workflow.default_crew'] },
  ],
  paths: [{ label: 'global root', value: '/home/op/.orbit' }],
});

const response = (payload, status = 200) => ({
  ok: status === 200,
  status,
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});

globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  const body = options.body ? JSON.parse(options.body) : null;
  requests.push({ path: url.pathname, method: options.method || 'GET', workspace: url.searchParams.get('workspace'), scope: url.searchParams.get('scope'), body });
  if ((options.method || 'GET') !== 'GET') {
    if (refusal) return response({ error: refusal }, 400);
    if (url.pathname.startsWith('/api/config/keys/')) workspaceBaseBranch = body.value;
    return response({ key: 'workflow.base_branch', scope: 'workspace', rows: [] });
  }
  return response(effective());
};

initConfig();
setWorkspace('one');
setConfigSubtab('effective');
await fetchAndRenderConfig();

// Every section the API describes is rendered, in its order, with the counts
// the API resolved — the client never regroups or recounts.
const titles = withClass('config-body', 'config-section-title').map(node => node.textContent);
assert(
  JSON.stringify(titles) === JSON.stringify([
    'Delivery (workflow.*)',
    'Crews (crews.*)',
    'Execution (execution.*)',
    'Operation mode (operation.*)',
    'Housekeeping',
    'Paths',
  ]),
  `sections render in the API's order, got ${JSON.stringify(titles)}`,
);
const counts = withClass('config-body', 'count').map(node => node.textContent);
assert(counts.includes('1 set · 1 default · 0 unset'), `count chip renders the API counts, got ${JSON.stringify(counts)}`);

// The two surprising layering facts are visible without opening anything.
const bodyText = panel('config-body').textContent;
assert(bodyText.includes('overrides global: trunk'), 'a shadowed global value is named on its row');
assert(
  bodyText.includes('global sets danger-full-access — not inherited while a workspace file exists'),
  'a non-inherited global execution value is named on its row',
);
assert(bodyText.includes('1 global value not inherited'), 'the execution section carries the non-inheritance badge');
assert(bodyText.includes('execution.* keys do not inherit from global'), 'the layers strip warns about the security exception');
assert(bodyText.includes('/repo/.orbit/config.toml'), 'the layers strip names the workspace file');
assert(bodyText.includes('matches workflow.base_branch'), 'the registry strip confirms the branch agreement');
assert(bodyText.includes('referenced by workflow.default_crew'), 'a crew names the keys that point at it');

// Source chips carry provenance per row.
const chips = withClass('config-body', 'config-source').map(node => node.textContent);
assert(chips.includes('workspace') && chips.includes('default'), `source chips render per row, got ${JSON.stringify(chips)}`);

// An unset-only section collapses until it is asked for.
assert(bodyText.includes('Show 1 unset keys'), 'a section whose keys are all unset collapses to a summary');

// Editing a row writes one key to the workspace file and re-reads the view.
const before = requests.length;
const pencils = withClass('config-body', 'config-pencil');
pencils[0].listeners.click({ stopPropagation() {} });
const input = descendants(panel('config-body')).find(node => String(node.className || '').includes('config-input'));
input.value = 'main';
button('config-body', 'Save').listeners.click({ stopPropagation() {} });
await new Promise(resolve => setTimeout(resolve, 0));
await new Promise(resolve => setTimeout(resolve, 0));
const write = requests.slice(before).find(request => request.method === 'PUT');
assert(write, 'saving a row issues a PUT');
assert(write.path === '/api/config/keys/workflow.base_branch', `the write names the key, got ${write.path}`);
assert(write.workspace === 'one', 'the write is scoped to the selected workspace');
assert(write.body.value === 'main' && write.body.scope === 'workspace', `the write targets the workspace file, got ${JSON.stringify(write.body)}`);
assert(
  requests.slice(before).some(request => request.method === 'GET'),
  'an accepted write re-reads the view rather than patching the row locally',
);

// A refused write renders the admission error verbatim on the row.
refusal = "execution.codex.sandbox has invalid value 'wide-open'; expected one of: read-only, workspace-write, danger-full-access";
withClass('config-body', 'config-pencil')[0].listeners.click({ stopPropagation() {} });
button('config-body', 'Save').listeners.click({ stopPropagation() {} });
await new Promise(resolve => setTimeout(resolve, 0));
await new Promise(resolve => setTimeout(resolve, 0));
assert(
  panel('config-body').textContent.includes(refusal),
  'the admission error renders verbatim on the row',
);

// A caller without the operator capability sees the rows, not an editor.
refusal = null;
const authorized = effective;
globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  requests.push({ path: url.pathname, method: options.method || 'GET' });
  const payload = authorized();
  payload.config_set = { authorized: false, reason: 'Test session cannot perform this action.' };
  return response(payload);
};
await fetchAndRenderConfig();
assert(
  withClass('config-body', 'config-pencil').length === 0 ||
    !withClass('config-body', 'config-row-main').some(node => String(node.className).includes('clickable')),
  'a denied caller gets read-only rows',
);

console.log('dashboard config scenario ok');
