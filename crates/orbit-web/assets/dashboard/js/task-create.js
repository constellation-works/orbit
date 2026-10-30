// One durable, frozen submission per browser tab. A lost response is retried
// with the same key, body and addressed URL; scope changes only hide its UI.
// The form is a native modal <dialog>: it paints in the top layer, so opening,
// resizing or dismissing it never changes the board's or dock's layout, and
// dismissing it never cancels a request already sent.
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
  const close = () => { if ($('panel').open) $('panel').close(); };
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
    if (pending) feedback('Creating proposed task…');
    else if (!saved.receipt) feedback('The saved submission may already exist. Retry to recover its original receipt.');
  };
  $('open').addEventListener('click', () => {
    const select = $('workspace');
    const chosen = $('title').value ? select.value : '';
    select.replaceChildren();
    const placeholder = document.createElement('option');
    placeholder.value = ''; placeholder.textContent = 'Select a workspace';
    select.append(placeholder);
    for (const workspace of getWorkspaces().filter(row => row.status === 'active' && row.creation_binding)) {
      const option = document.createElement('option');
      option.value = workspace.id; option.textContent = workspace.name;
      select.append(option);
    }
    // An aggregate-scope pick survives only with a dismissed, unsent draft.
    select.value = getWorkspace() || chosen;
    feedback('');
    fill(); paint();
    if (!$('panel').open) $('panel').showModal();
    $('open').setAttribute('aria-expanded', 'true');
    (saved ? $('feedback') : $('title')).focus();
  });
  // Escape and Cancel both land here: the native dialog closes, then focus
  // returns to New task. Only Cancel discards an unsent draft.
  $('panel').addEventListener('close', () => {
    $('open').setAttribute('aria-expanded', 'false');
    const active = document.activeElement;
    if (!active || active === document.body || $('panel').contains(active)) $('open').focus();
  });
  // A modal dialog does not wrap Tab by itself: past its last control focus
  // would leave for the browser's own chrome, so wrap it here.
  $('panel').addEventListener('keydown', event => {
    if (event.key !== 'Tab' || event.altKey || event.ctrlKey || event.metaKey) return;
    const controls = [...$('panel').querySelectorAll('select, input, textarea, button, a[href]')]
      .filter(node => !node.disabled && !node.closest('[hidden]') && node.getClientRects().length);
    if (!controls.length) return;
    const active = document.activeElement;
    // Shift+Tab also leaves from the focusable feedback line when every field above it is disabled.
    const atStart = active === controls[0] || !controls.some(node => node.compareDocumentPosition(active) & Node.DOCUMENT_POSITION_FOLLOWING);
    if (event.shiftKey ? !atStart : active !== controls[controls.length - 1]) return;
    event.preventDefault();
    (event.shiftKey ? controls[controls.length - 1] : controls[0]).focus();
  });
  $('cancel').addEventListener('click', () => {
    if (!saved) $('form').reset();
    close();
  });
  // Following the success link, or leaving Tasks, must not leave a modal over
  // the destination. The saved receipt stays available on reopening.
  $('success').addEventListener('click', close);
  window.addEventListener('hashchange', () => {
    if (!window.location.hash.startsWith('#tasks')) close();
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
