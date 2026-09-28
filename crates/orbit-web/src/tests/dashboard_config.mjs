// Drives the shipped Config module against a fetch stub [ORB-12724]: what it
// paints for a layered payload, and what it writes when a row is edited.
const { setWorkspace } = await import('./js/common.js');
const { fetchAndRenderConfig, initConfig, setConfigSubtab } = await import('./js/config.js');

const panel = id => document.getElementById(id);
const descendants = node => [node, ...Array.from(node.children || []).flatMap(descendants)];
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const withClass = (id, name) => descendants(panel(id)).filter(node => String(node.className || '').split(/\s+/).includes(name));
const button = (id, label) => descendants(panel(id)).find(node => node.textContent === label && node.type === 'button');

const requests = [];
let workspaceBaseBranch = 'agent-main';
let refusal = null;
let emulateMissingWorkspaceConfig = false;
let workspaceConfig = null;
const globalConfig = {
  workflow: { base_branch: 'trunk' },
  execution: { codex: { sandbox: 'danger-full-access' } },
};

const effective = () => ({
  scope: 'effective',
  config_set: { authorized: true, reason: null },
  crew_fields: ['enabled', 'provider', 'model', 'effort', 'tags', 'description'],
  workspace_file_exists: !emulateMissingWorkspaceConfig || workspaceConfig !== null,
  write_scope_default: 'workspace',
  layers: {
    built_in: { label: 'built-in' },
    global: { path: '/home/op/.orbit/config.toml', exists: true },
    workspace: {
      path: '/repo/.orbit/config.toml',
      exists: !emulateMissingWorkspaceConfig || workspaceConfig !== null,
    },
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
      title: 'Review (operation.*)',
      blurb: 'automatic review policy',
      key_prefix: 'operation',
      kind: 'keys',
      counts: { set: 0, default: 0, unset: 1, total: 1 },
      not_inherited: 0,
      keys: [
        {
          key: 'operation.review_policy',
          label: 'review_policy',
          value: null,
          value_type: 'string',
          options: ['none', 'before-pr', 'after-landing'],
          state: 'unset',
          section: 'operation',
          description: 'Automatic review timing.',
          source: { layer: 'built-in', path: null },
          shadowed_by: [],
        },
      ],
    },
    { token: 'housekeeping', title: 'Housekeeping', blurb: 'logs, scoring, ids, and PR links', key_prefix: null, kind: 'keys', counts: { set: 0, default: 0, unset: 0, total: 0 }, not_inherited: 0, keys: [] },
  ],
  crews: emulateMissingWorkspaceConfig
    ? [
        { name: 'opus', enabled: true, provider: 'claude', model: 'opus', effort: null, tags: [], description: null, source: 'built-in', referenced_by: ['workflow.default_crew'] },
        ...Object.entries(workspaceConfig?.crews || {}).map(([name, crew]) => ({
          name,
          ...crew,
          source: 'workspace',
          referenced_by: [],
        })),
      ]
    : [
        { name: 'opus', enabled: true, provider: 'claude', model: 'opus', effort: null, tags: [], description: null, source: 'built-in', referenced_by: ['workflow.default_crew'] },
        { name: 'grok', enabled: false, provider: 'grok', model: 'grok-4.7', effort: null, tags: [], description: null, source: 'workspace', referenced_by: [] },
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
    if (emulateMissingWorkspaceConfig && url.pathname.startsWith('/api/config/crews/')) {
      if (workspaceConfig === null) {
        if (!body.init) return response({ error: 'workspace config does not exist' }, 400);
        workspaceConfig = body.init === 'seed-from-global' ? structuredClone(globalConfig) : {};
      }
      const name = decodeURIComponent(url.pathname.split('/').at(-1));
      workspaceConfig.crews ||= {};
      workspaceConfig.crews[name] = body.fields;
    }
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
    'Review (operation.*)',
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

// A disabled crew stays listed, marked disabled; an enabled one is not marked.
const crewRows = withClass('config-body', 'config-crew-row');
const grokRow = crewRows.find(node => node.dataset.key === 'crews.grok');
const opusRow = crewRows.find(node => node.dataset.key === 'crews.opus');
assert(grokRow && String(grokRow.className).includes('disabled'), 'a disabled crew row is listed and marked disabled');
assert(descendants(grokRow).some(node => node.textContent === 'disabled'), 'a disabled crew carries a disabled badge');
assert(opusRow && !String(opusRow.className).includes('disabled'), 'an enabled crew row is not marked disabled');

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

// Enabling a crew from its editor writes a boolean `enabled`, and only that
// toggle: an unchanged toggle never writes the key.
refusal = null;
const crewBefore = requests.length;
const grokEdit = descendants(withClass('config-body', 'config-crew-row').find(node => node.dataset.key === 'crews.grok'))
  .find(node => node.type === 'button' && node.listeners && node.listeners.click && String(node.className || '').includes('pencil'));
assert(grokEdit, 'a disabled crew row offers an editor');
grokEdit.listeners.click({ stopPropagation() {} });
const toggle = descendants(panel('config-body')).find(node => node.type === 'checkbox');
assert(toggle && toggle.checked === false, 'the editor shows the crew disabled');
toggle.checked = true;
button('config-body', 'Save').listeners.click({ stopPropagation() {} });
await new Promise(resolve => setTimeout(resolve, 0));
await new Promise(resolve => setTimeout(resolve, 0));
const crewWrite = requests.slice(crewBefore).find(request => request.method === 'PUT');
assert(crewWrite && crewWrite.path === '/api/config/crews/grok', `enabling writes the crew, got ${JSON.stringify(crewWrite)}`);
assert(crewWrite.body.fields.enabled === true, `enabled is written as a boolean, got ${JSON.stringify(crewWrite.body.fields)}`);

const opusBefore = requests.length;
descendants(withClass('config-body', 'config-crew-row').find(node => node.dataset.key === 'crews.opus'))
  .find(node => node.type === 'button' && node.listeners && node.listeners.click && String(node.className || '').includes('pencil'))
  .listeners.click({ stopPropagation() {} });
button('config-body', 'Save').listeners.click({ stopPropagation() {} });
await new Promise(resolve => setTimeout(resolve, 0));
await new Promise(resolve => setTimeout(resolve, 0));
const opusWrite = requests.slice(opusBefore).find(request => request.method === 'PUT');
assert(opusWrite && !('enabled' in opusWrite.body.fields), `an untouched toggle is not written, got ${JSON.stringify(opusWrite && opusWrite.body.fields)}`);

// A crew's first write offers the same two initialization choices as key edits,
// and the selected choice determines the new workspace file before the crew is applied.
const settle = async () => {
  await new Promise(resolve => setTimeout(resolve, 0));
  await new Promise(resolve => setTimeout(resolve, 0));
};
for (const [label, init] of [
  ['Start empty', 'fresh'],
  ['Copy global policy', 'seed-from-global'],
]) {
  emulateMissingWorkspaceConfig = true;
  workspaceConfig = null;
  refusal = null;
  await fetchAndRenderConfig();
  assert(!effective().workspace_file_exists, 'the first-write scenario starts without a workspace file');
  button('config-body', '+ Add crew').listeners.click();
  const fillNewCrew = () => {
    const textInputs = descendants(panel('config-body')).filter(node => node.type === 'text');
    assert(textInputs.length >= 3, 'a new crew editor exposes a name, provider, and model input');
    textInputs[0].value = 'review-bot';
    textInputs[1].value = 'openai';
    textInputs[2].value = 'gpt-5.6-sol';
  };
  fillNewCrew();
  const firstWriteStart = requests.length;
  button('config-body', 'Save').listeners.click({ stopPropagation() {} });
  await settle();
  const refusedWrite = requests.slice(firstWriteStart).find(request => request.method === 'PUT');
  assert(refusedWrite && refusedWrite.body.init === undefined, 'the initial crew write asks before creating a workspace file');
  const choice = button('config-body', label);
  assert(choice, `a missing workspace file offers ${label}`);
  // The refused write re-renders this new-crew form; enter the intended fields
  // again before retrying so this scenario covers init policy, not draft retention.
  fillNewCrew();
  const selectedWriteStart = requests.length;
  choice.listeners.click({ stopPropagation() {} });
  await settle();
  const selectedWrite = requests.slice(selectedWriteStart).find(request => request.method === 'PUT');
  assert(selectedWrite && selectedWrite.body.init === init, `${label} sends init=${init}`);
  assert(selectedWrite.body.fields.provider === 'openai', `${label} preserves the intended provider edit`);
  assert(selectedWrite.body.fields.model === 'gpt-5.6-sol', `${label} preserves the intended model edit`);
  const expectedConfig = init === 'fresh'
    ? { crews: { 'review-bot': selectedWrite.body.fields } }
    : { ...globalConfig, crews: { 'review-bot': selectedWrite.body.fields } };
  assert(
    JSON.stringify(workspaceConfig) === JSON.stringify(expectedConfig),
    `${label} initializes the workspace policy and applies the crew edit, got ${JSON.stringify(workspaceConfig)}`,
  );
  assert(
    withClass('config-body', 'config-crew-row').some(node => node.dataset.key === 'crews.review-bot'),
    `${label} reloads the resulting crew into the workspace view`,
  );
}
emulateMissingWorkspaceConfig = false;

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
