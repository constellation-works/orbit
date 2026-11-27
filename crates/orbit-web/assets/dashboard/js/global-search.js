// Shared top-bar search. Its requests, selection and feedback never touch the board.
// It always searches every available workspace, whatever workspace is selected;
// the selected workspace only ranks its own matches first.
import { captureWorkspaceVisit, el, getWorkspace, isAggregateView, isOwnedWorkspace, onWorkspaceChange, pushScopedHash, withHost } from './common.js';
import { navigateToRun } from './router.js';
import { STATUS_LABELS } from './task-shared.js';

const TASK_ID = /^[A-Z]{2,5}-\d+$/i;
const RUN_ID = /^jrun-[\w.-]+$/i;

export function initGlobalSearch({ workspaces, workspaceChanged }) {
  const input = document.getElementById('task-search');
  const wrap = input.closest('.task-search-wrap');
  const popover = document.getElementById('global-search-popover');
  const list = document.getElementById('global-search-results');
  const status = document.getElementById('global-search-status');
  const unavailableLine = document.getElementById('global-search-unavailable');
  const phone = window.matchMedia('(max-width: 760px)');
  const topbar = wrap.closest('.topbar');
  const openButton = document.getElementById('phone-search-open');
  const closeButton = document.getElementById('phone-search-close');
  const isOverlaySearch = () => phone.matches || Boolean(topbar?.classList.contains('topbar-fit-search'));
  let generation = 0;
  let controller = null;
  let debounce = null;
  let matches = [];
  let highlighted = -1;
  let keyboardSelection = false;

  function cancelSearchRequest() {
    generation += 1;
    controller?.abort();
    controller = null;
    clearTimeout(debounce);
    debounce = null;
  }

  function closeSearchResults() {
    cancelSearchRequest();
    keyboardSelection = false;
    popover.hidden = true;
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
    wrap.classList.remove('error');
  }

  function clearGlobalSearch() {
    closeSearchResults();
    input.value = '';
    matches = [];
    list.replaceChildren();
    status.textContent = '';
    showUnavailable([]);
  }

  function showSearchField() {
    if (isOverlaySearch()) {
      wrap.classList.add('search-open');
      openButton.setAttribute('aria-expanded', 'true');
    }
    input.focus();
    input.select();
  }

  function closePhoneSearch({ restoreFocus = false } = {}) {
    closeSearchResults();
    if (!wrap.classList.contains('search-open')) return;
    wrap.classList.remove('search-open');
    openButton.setAttribute('aria-expanded', 'false');
    if (restoreFocus) openButton.focus();
  }

  // One line counts the workspaces that could not be read; its tooltip names them.
  function showUnavailable(names) {
    unavailableLine.textContent = names.length
      ? `${names.length} ${names.length === 1 ? 'workspace' : 'workspaces'} unavailable` : '';
    if (names.length) unavailableLine.title = names.join(', ');
    else unavailableLine.removeAttribute('title');
  }

  function showSearchMessage(message, kind = '', unavailable = []) {
    matches = [];
    highlighted = -1;
    list.replaceChildren();
    status.textContent = message;
    status.dataset.kind = kind;
    showUnavailable(unavailable);
    wrap.classList.toggle('error', kind === 'error');
    input.removeAttribute('aria-activedescendant');
    input.setAttribute('aria-expanded', 'true');
    popover.hidden = false;
  }

  function beginSearchRequest() {
    cancelSearchRequest();
    controller = new AbortController();
    const requestController = controller;
    const seq = generation;
    const visit = captureWorkspaceVisit();
    const hash = window.location.hash;
    return {
      signal: requestController.signal,
      abort: () => requestController.abort(),
      current: () => seq === generation && visit.isCurrent() && hash === window.location.hash,
    };
  }

  async function searchJson(path, request) {
    const timeout = setTimeout(() => request.abort(), 30000);
    try {
      const response = await fetch(withHost(path), {
        headers: { accept: 'application/json' }, signal: request.signal,
      });
      if (!response.ok) {
        let detail = `Lookup failed (${response.status})`;
        try {
          const body = await response.json();
          if (body?.error) detail = body.error;
        } catch (_) { /* a non-JSON failure keeps the HTTP status */ }
        const error = new Error(detail);
        error.status = response.status;
        throw error;
      }
      return await response.json();
    } finally {
      clearTimeout(timeout);
    }
  }

  function showSearchFailure(error, request) {
    if (!request.current()) return;
    const reason = error.name === 'AbortError' ? 'Lookup timed out. Try again.'
      : error instanceof TypeError ? 'Network error. Try again.' : error.message;
    showSearchMessage(reason, 'error');
  }

  function openSearchTask(task) {
    clearGlobalSearch();
    const hash = `#tasks/${encodeURIComponent(task.id)}`;
    // Keep the origin history entry on its own workspace/aggregate scope when
    // jumping across workspaces; push the destination scope as a new entry.
    if (task.workspace_id && task.workspace_id !== getWorkspace()) {
      pushScopedHash(hash, { workspace: task.workspace_id, remember: true });
      workspaceChanged();
    } else {
      // Assign the destination once. Routing it programmatically first would
      // replace the origin entry when normalizing a task-page deep link.
      window.location.hash = hash;
    }
    input.blur();
    closePhoneSearch();
  }

  function highlightSearchResult(index) {
    highlighted = index;
    Array.from(list.children).forEach((node, at) => node.setAttribute('aria-selected', String(at === index)));
    const node = list.children[index];
    if (node) {
      input.setAttribute('aria-activedescendant', node.id);
      node.scrollIntoView({ block: 'nearest' });
    }
  }

  function paintSearchResults(payload) {
    keyboardSelection = false;
    matches = payload.items;
    list.replaceChildren(...matches.map((task, index) => {
      const metadata = [el('span', { class: 'mono', text: task.id })];
      // Name the workspace on every row in All workspaces, otherwise only on
      // rows from a workspace other than the selected one.
      if (isAggregateView() || task.workspace_id !== getWorkspace()) {
        metadata.push(el('span', { text: task.workspace_name }));
      }
      const option = el('div', { id: `global-search-option-${index}`, class: 'global-search-option', role: 'option', 'aria-selected': 'false' }, [
        el('span', { class: 'global-search-task' }, [
          el('span', { class: 'global-search-title', text: task.title }),
          el('span', { class: 'global-search-meta' }, metadata),
        ]),
        el('span', { class: 'global-search-task-status', 'data-status': task.status, text: STATUS_LABELS[task.status] || task.status }),
      ]);
      option.addEventListener('pointerdown', event => event.preventDefault());
      option.addEventListener('click', () => openSearchTask(task));
      return option;
    }));
    const more = Math.max(0, payload.total - matches.length);
    status.textContent = more ? `${more} more` : matches.length ? '' : 'No tasks match';
    status.dataset.kind = '';
    showUnavailable((payload.unavailable || []).map(source => source.workspace_name));
    wrap.classList.remove('error');
    popover.hidden = false;
    input.setAttribute('aria-expanded', 'true');
    if (matches.length) highlightSearchResult(0);
    else input.removeAttribute('aria-activedescendant');
  }

  async function requestSearchResults({ chooseFirst = false } = {}) {
    const query = input.value.trim();
    if (!query) { clearGlobalSearch(); return; }
    const request = beginSearchRequest();
    const params = new URLSearchParams({ q: query });
    if (getWorkspace()) params.set('prefer', getWorkspace());
    try {
      const payload = await searchJson(`/api/tasks/search?${params}`, request);
      if (!request.current()) return;
      paintSearchResults(payload);
      if (chooseFirst && matches.length) openSearchTask(matches[0]);
    } catch (error) {
      showSearchFailure(error, request);
    }
  }

  // Exact IDs probe the selected workspace first, then every other available
  // workspace. A confirmed 404 is a miss; a workspace that cannot be read is
  // counted as unavailable instead of failing the lookup, unless none answered.
  async function lookupSearchId(id, kind) {
    const request = beginSearchRequest();
    showSearchMessage(`Looking up ${id}…`);
    const selected = getWorkspace();
    const available = workspaces().filter(ws => ws.status === 'active' && isOwnedWorkspace(ws));
    const others = available.filter(ws => ws.id !== selected).map(ws => ws.id);
    const nameOf = workspace => available.find(ws => ws.id === workspace)?.name || workspace;
    const path = workspace => `/api/${kind === 'task' ? 'tasks' : 'runs'}/${encodeURIComponent(id)}?workspace=${encodeURIComponent(workspace)}`;
    const unavailable = [];
    let failure = null;
    let answered = 0;
    const read = async workspace => {
      try {
        const body = await searchJson(path(workspace), request);
        answered += 1;
        // The server may answer a run from the workspace that holds it rather
        // than the one probed, and names that holder; trust it over the probe.
        return { body, workspace: kind === 'run' && body.workspace_id ? body.workspace_id : workspace };
      } catch (error) {
        if (error.status === 404 && !/unknown workspace/i.test(error.message)) { answered += 1; return null; }
        failure ??= error;
        unavailable.push(nameOf(workspace));
        return null;
      }
    };
    try {
      let hit = selected ? await read(selected) : null;
      if (!request.current()) return;
      if (!hit) {
        const probed = await Promise.all(others.map(read));
        if (!request.current()) return;
        hit = probed.find(Boolean);
      }
      if (!hit) {
        if (failure && !answered) throw failure;
        showSearchMessage(`No ${kind} ${id} in any workspace`, '', unavailable.sort());
        return;
      }
      if (kind === 'task') openSearchTask({ ...hit.body, workspace_id: hit.workspace });
      else {
        clearGlobalSearch();
        if (hit.workspace && hit.workspace !== getWorkspace()) {
          pushScopedHash(`#runs/${encodeURIComponent(id)}`, { workspace: hit.workspace, remember: true });
          workspaceChanged();
        } else {
          navigateToRun(id);
        }
        input.blur();
        closePhoneSearch();
      }
    } catch (error) {
      showSearchFailure(error, request);
    }
  }

  input.addEventListener('input', () => {
    closeSearchResults();
    matches = [];
    if (input.value.trim()) debounce = setTimeout(() => requestSearchResults(), 200);
  });
  input.addEventListener('focus', () => {
    if (input.value.trim() && popover.hidden) requestSearchResults();
  });
  input.addEventListener('keydown', event => {
    if (event.isComposing) return;
    if (event.key === 'Escape' && !isOverlaySearch()) {
      event.preventDefault();
      event.stopPropagation();
      if (!popover.hidden) closeSearchResults(); else clearGlobalSearch();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (popover.hidden || !matches.length) { requestSearchResults(); return; }
      keyboardSelection = true;
      const delta = event.key === 'ArrowDown' ? 1 : -1;
      highlightSearchResult((highlighted + delta + matches.length) % matches.length);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      const query = input.value.trim();
      if (keyboardSelection && !popover.hidden && matches[highlighted]) openSearchTask(matches[highlighted]);
      else if (TASK_ID.test(query)) lookupSearchId(query.toUpperCase(), 'task');
      else if (RUN_ID.test(query)) lookupSearchId(query.toLowerCase(), 'run');
      else if (!popover.hidden && matches[highlighted]) openSearchTask(matches[highlighted]);
      else requestSearchResults({ chooseFirst: true });
    }
  });
  openButton.addEventListener('click', showSearchField);
  closeButton.addEventListener('click', () => closePhoneSearch({ restoreFocus: true }));
  wrap.addEventListener('keydown', event => {
    if (event.key !== 'Escape' || event.isComposing || !isOverlaySearch()) return;
    event.preventDefault();
    event.stopPropagation();
    closePhoneSearch({ restoreFocus: true });
  });
  document.getElementById('task-search-clear').addEventListener('click', () => {
    clearGlobalSearch();
    input.focus();
  });
  document.addEventListener('pointerdown', event => {
    if (!wrap.contains(event.target)) closePhoneSearch();
  });
  document.addEventListener('focusin', event => {
    if (!wrap.contains(event.target)) closePhoneSearch();
  });
  document.addEventListener('keydown', event => {
    if (event.key !== '/' || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (target?.isContentEditable || target?.closest('input, textarea, select, dialog[open]')) return;
    event.preventDefault();
    showSearchField();
  });
  onWorkspaceChange(clearGlobalSearch);
  phone.addEventListener('change', () => closePhoneSearch());
  window.addEventListener('hashchange', () => closePhoneSearch());
  window.addEventListener('popstate', () => closePhoneSearch());
  return id => lookupSearchId(id, 'task');
}
