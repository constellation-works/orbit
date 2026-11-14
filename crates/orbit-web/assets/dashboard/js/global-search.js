// Shared top-bar search. Its requests, selection and feedback never touch the board.
import { captureWorkspaceVisit, el, getSelectedWorkspaces, getWorkspace, isAggregateView, onWorkspaceChange, persistScopeToUrl, rememberWorkspace, setWorkspace, withHost } from './common.js';
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
  }

  function showSearchMessage(message, kind = '') {
    matches = [];
    highlighted = -1;
    list.replaceChildren();
    status.textContent = message;
    status.dataset.kind = kind;
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
      const body = await response.json();
      if (!response.ok) {
        const error = new Error(body?.error || `Lookup failed (${response.status})`);
        error.status = response.status;
        throw error;
      }
      return body;
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

  function adoptSearchWorkspace(workspace) {
    if (!workspace || workspace === getWorkspace()) return;
    setWorkspace(workspace);
    rememberWorkspace();
    persistScopeToUrl();
    workspaceChanged();
  }

  function openSearchTask(task) {
    clearGlobalSearch();
    adoptSearchWorkspace(task.workspace_id);
    // Assign the destination once. Routing it programmatically first would
    // replace the origin entry when normalizing a task-page deep link.
    window.location.hash = `#tasks/${encodeURIComponent(task.id)}`;
    input.blur();
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
      if (isAggregateView()) metadata.push(el('span', { text: task.workspace_name }));
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
    const unavailable = (payload.unavailable || []).map(source => `${source.workspace_name}: ${source.error}`);
    status.textContent = [more ? `${more} more` : matches.length ? '' : unavailable.length ? '' : 'No tasks match', ...unavailable].filter(Boolean).join(' · ');
    status.dataset.kind = unavailable.length ? 'error' : '';
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
    if (getWorkspace()) params.set('workspace', getWorkspace());
    else for (const id of getSelectedWorkspaces()) params.append('workspaces', id);
    try {
      const payload = await searchJson(`/api/tasks/search?${params}`, request);
      if (!request.current()) return;
      paintSearchResults(payload);
      if (chooseFirst && matches.length) openSearchTask(matches[0]);
    } catch (error) {
      showSearchFailure(error, request);
    }
  }

  // Exact IDs probe the selected workspace first, then the remaining active
  // workspaces. Only confirmed 404 misses permit the search to continue.
  async function lookupSearchId(id, kind) {
    const request = beginSearchRequest();
    showSearchMessage(`Looking up ${id}…`);
    const selected = getWorkspace();
    const others = workspaces().filter(ws => ws.status === 'active' && ws.id !== selected).map(ws => ws.id);
    const path = workspace => `/api/${kind === 'task' ? 'tasks' : 'runs'}/${encodeURIComponent(id)}?workspace=${encodeURIComponent(workspace)}`;
    const read = async workspace => {
      try {
        return { body: await searchJson(path(workspace), request), workspace };
      } catch (error) {
        if (error.status === 404 && !/unknown workspace/i.test(error.message)) return null;
        throw error;
      }
    };
    try {
      let hit = selected ? await read(selected) : null;
      if (!request.current()) return;
      if (!hit) {
        const probed = await Promise.allSettled(others.map(read));
        if (!request.current()) return;
        hit = probed.find(result => result.status === 'fulfilled' && result.value)?.value;
        if (!hit) {
          const failure = probed.find(result => result.status === 'rejected');
          if (failure) throw failure.reason;
        }
      }
      if (!hit) { showSearchMessage(`${id} not found`); return; }
      if (kind === 'task') openSearchTask({ ...hit.body, workspace_id: hit.workspace });
      else {
        clearGlobalSearch();
        adoptSearchWorkspace(hit.workspace);
        navigateToRun(id);
        input.blur();
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
    if (event.key === 'Escape') {
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
  document.getElementById('task-search-clear').addEventListener('click', () => {
    clearGlobalSearch();
    input.focus();
  });
  document.addEventListener('pointerdown', event => {
    if (!wrap.contains(event.target)) closeSearchResults();
  });
  document.addEventListener('focusin', event => {
    if (!wrap.contains(event.target)) closeSearchResults();
  });
  document.addEventListener('keydown', event => {
    if (event.key !== '/' || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (target?.isContentEditable || target?.closest('input, textarea, select, dialog[open]')) return;
    event.preventDefault();
    input.focus();
    input.select();
  });
  onWorkspaceChange(clearGlobalSearch);
  window.addEventListener('hashchange', closeSearchResults);
  window.addEventListener('popstate', closeSearchResults);
  return id => lookupSearchId(id, 'task');
}
