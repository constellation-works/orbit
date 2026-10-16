// One durable, frozen submission per browser tab. A lost response is retried
// with the same key, body and addressed URL; scope changes only hide its UI.
import { captureWorkspaceVisit, getHost, getServingHost, getWorkspace, onWorkspaceChange, taskHref, withHost } from './common.js';

const STORAGE = 'orbit.proposed-task.v1';
const $ = name => document.getElementById(`task-create-${name}`);

export function initTaskCreate({ getWorkspaces }) {
  let saved = null;
  let pending = false;
  let storageError = '';
  try { saved = JSON.parse(sessionStorage.getItem(STORAGE) || 'null'); }
  catch { storageError = 'Saved submission is unavailable. Restore browser storage before creating a task.'; }
  const host = () => getHost() || getServingHost()?.name || null;
  const inScope = () => !saved || (saved.host === host() && (!getWorkspace() || saved.workspace === getWorkspace()));
  const feedback = message => { $('feedback').textContent = message; };
  const persist = value => {
    // Saving must succeed before a request can begin, including on reload.
    if (value) sessionStorage.setItem(STORAGE, JSON.stringify(value));
    else sessionStorage.removeItem(STORAGE);
    saved = value;
  };
  const close = () => {
    $('panel').hidden = true;
    $('open').setAttribute('aria-expanded', 'false');
  };
  const paint = () => {
    const foreign = !inScope();
    $('fields').disabled = pending || Boolean(saved);
    $('submit').disabled = pending || foreign || Boolean(saved?.receipt) || Boolean(storageError);
    $('submit').textContent = pending ? 'Creating…' : saved ? 'Retry saved submission' : 'Create proposed task';
    $('form').setAttribute('aria-busy', String(pending));
    $('cancel').textContent = saved ? 'Close' : 'Cancel';
    $('success').hidden = !saved?.receipt || foreign;
    $('new').hidden = !saved?.receipt || foreign;
    if (foreign) {
      feedback(`An unresolved submission belongs to ${saved.host || 'this host'} / ${saved.workspace}. Return there to recover it before starting another proposal.`);
    } else if (saved?.receipt) {
      feedback('Task saved. It awaits approval.');
      $('success').textContent = `Open ${saved.receipt.id}`;
      $('success').href = taskHref(saved.receipt.id, { host: saved.host, workspace: saved.workspace });
    } else if (storageError) feedback(storageError);
  };
  const fill = () => {
    if (!saved || !inScope()) return;
    $('workspace').value = saved.workspace;
    $('title').value = saved.body.title;
    $('description').value = saved.body.description;
    $('complexity').value = saved.body.complexity;
    $('criteria').value = saved.body.acceptance_criteria.join('\n');
    if (!saved.receipt) feedback('The saved submission may already exist. Retry to recover its original receipt.');
  };
  $('open').addEventListener('click', () => {
    const select = $('workspace');
    select.replaceChildren();
    const placeholder = document.createElement('option');
    placeholder.value = ''; placeholder.textContent = 'Select a workspace';
    select.append(placeholder);
    for (const workspace of getWorkspaces().filter(row => row.status === 'active' && row.creation_binding)) {
      const option = document.createElement('option');
      option.value = workspace.id; option.textContent = workspace.name;
      select.append(option);
    }
    select.value = getWorkspace() || '';
    feedback('');
    fill(); paint();
    $('panel').hidden = false;
    $('open').setAttribute('aria-expanded', 'true');
    (saved ? $('feedback') : $('title')).focus();
  });
  $('cancel').addEventListener('click', () => {
    if (!saved) $('form').reset();
    close(); $('open').focus();
  });
  $('new').addEventListener('click', () => {
    if (!saved?.receipt) return;
    try { persist(null); } catch (error) { feedback(error.message); return; }
    $('form').reset(); $('workspace').value = getWorkspace() || '';
    feedback(''); paint(); $('title').focus();
  });
  $('form').addEventListener('submit', async event => {
    event.preventDefault();
    if (pending || !inScope() || saved?.receipt || storageError) return;
    if (!saved) {
      const workspace = getWorkspaces().find(row => row.id === $('workspace').value && row.status === 'active');
      const title = $('title').value.trim();
      const criteria = $('criteria').value.split('\n').map(value => value.trim()).filter(Boolean);
      for (const field of ['title', 'criteria']) $(field).removeAttribute('aria-invalid');
      if (!title || !criteria.length || !workspace?.creation_binding || !$('complexity').value) {
        feedback('Select a workspace and complexity, enter a meaningful title and at least one acceptance criterion.');
        if (!title) $('title').setAttribute('aria-invalid', 'true');
        if (!criteria.length) $('criteria').setAttribute('aria-invalid', 'true');
        $('feedback').focus(); return;
      }
      const body = {
        request_id: crypto.randomUUID(), binding: workspace.creation_binding,
        title, description: $('description').value,
        complexity: $('complexity').value, acceptance_criteria: criteria,
      };
      try {
        persist({ host: host(), workspace: workspace.id,
          path: withHost(`/api/tasks/create?workspace=${encodeURIComponent(workspace.id)}`), body, uncertain: false });
      } catch { feedback('Browser storage is unavailable. The task was not submitted.'); return; }
    }
    const submission = saved;
    const visit = captureWorkspaceVisit();
    pending = true; feedback('Creating proposed task…'); paint();
    // Mark uncertainty durably BEFORE sending, so a reload during the request
    // never treats an authorization refusal on retry as proof of no commit.
    const wasUncertain = submission.uncertain;
    try {
      persist({ ...submission, uncertain: true });
      const response = await fetch(submission.path, {
        method: 'POST', headers: { 'accept': 'application/json', 'content-type': 'application/json' },
        body: JSON.stringify(submission.body),
      });
      const receipt = await response.json();
      if (!response.ok) {
        if (!wasUncertain && [400, 401, 403, 404, 405, 413, 415, 422].includes(response.status)) persist(null);
        throw new Error(receipt.error || `Request refused (HTTP ${response.status}).`);
      }
      if (!receipt.id || receipt.workspace_id !== submission.workspace || receipt.request_id !== submission.body.request_id) {
        throw new Error('The creation receipt is incomplete. Retry the saved submission.');
      }
      persist({ ...submission, receipt });
    } catch (error) {
      if (visit.isCurrent()) feedback(`${error.message} ${saved ? 'Keep this submission and retry to recover its receipt.' : 'Correct the fields and submit again.'}`);
    } finally {
      pending = false;
      if (visit.isCurrent()) paint();
    }
  });
  onWorkspaceChange(() => {
    close();
    $('success').hidden = true;
    feedback('');
  });
}
