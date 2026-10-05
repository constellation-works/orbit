// Config tab: grouped effective settings, provenance, and inline editing
// [ORB-12724].
//
// Everything structural comes from the API: sections, their order and blurbs,
// key labels, descriptions, value types, and the choices an enum key accepts.
// No key name, section name, or enum option is written here, so a key added,
// retired, or re-typed in `orbit-config` changes this tab without a JS edit.
//
// Each row saves on its own — there is no pending-changes gate. A save writes
// one key to one file and re-renders that row from the response, so provenance
// after the write is the server's answer, never a local guess.

import { captureFocus, el, fetchJson, getWorkspace, isAggregateView, onWorkspaceChange, renderPanelPlaceholder, requestJson, requestPanel } from './common.js';
import { hostReading, hostVerdict, onHostResources } from './host-resources.js';

const $ = (id) => document.getElementById(id);

/// Rendered in place of a value that does not exist, matching `config show`.
const NO_VALUE = "–";

/// Which file a sub-view reads and writes. `effective` never writes global:
/// the layered view's edits go to the workspace file, which is the per-user,
/// git-ignored one. `system` is the exception: the topbar chips and admission
/// read the serving machine's global settings, so that is what it edits.
const SUBTAB_SOURCES = {
  effective: { path: "/api/config/effective", scope: "workspace" },
  "workspace-file": { path: "/api/config/file?scope=workspace", scope: "workspace" },
  "global-file": { path: "/api/config/file?scope=global", scope: "global" },
  crews: { path: "/api/config/effective", scope: "workspace" },
  keys: { path: "/api/config/keys", scope: null },
  system: { path: "/api/config/file?scope=global", scope: "global" },
};

/// The System tab is the resource throttle only: its keys, in display order.
const THROTTLE_PREFIX = "workflow.resource_throttle.";
const THROTTLE_RESOURCES = ["cpu", "memory", "disk"];

let activeSubtab = "effective";
let lastPayload = null;
let filterText = "";
let showAllKeys = false;
// Sections an operator expanded past their collapsed "all unset" summary, and
// the row (or crew) currently being edited with its draft values. Both are
// operator state, so a background refresh must not discard them.
const expandedSections = new Set();
let editing = null;
let unsubscribeWorkspace = null;

export function initConfig() {
  unsubscribeWorkspace?.();
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastPayload = null;
    editing = null;
    expandedSections.clear();
  });
  // Live readings follow the topbar's poll. A rebuild would discard an open
  // editor's focus, so it waits until nothing is being edited.
  onHostResources((host) => {
    if (activeSubtab !== "system" || !lastPayload || editing) return;
    lastPayload = { ...lastPayload, host };
    render(lastPayload);
  });
}

export function setConfigSubtab(name) {
  if (!SUBTAB_SOURCES[name]) name = "effective";
  if (name !== activeSubtab) {
    editing = null;
    lastPayload = null;
  }
  activeSubtab = name;
}

export function getConfigSubtab() {
  return activeSubtab;
}

export async function fetchAndRenderConfig() {
  if (isAggregateView()) {
    renderPanelPlaceholder("config-body");
    $("config-count").textContent = "—";
    return;
  }
  const source = SUBTAB_SOURCES[activeSubtab] || SUBTAB_SOURCES.effective;
  await requestPanel(
    "config-body",
    `${activeSubtab}:${getWorkspace() || ""}`,
    () => (activeSubtab === "system" ? fetchSystem(source) : fetchJson(source.path)),
    (payload) => {
      lastPayload = payload;
      render(payload);
    },
    "config-count",
  );
}

// Every interaction here re-renders the whole tab (a chip, Reload, opening or
// closing an editor), which removes the control that was just used. Handing
// focus to its rebuilt counterpart keeps a keyboard user where they were.
function render(payload) {
  // A refresh that withdraws operator authority ends the edit session. Leaving
  // it set would hide the editor and still freeze the host poll that waits on it.
  if (!editable(payload)) editing = null;
  const body = $("config-body");
  if (!body) return;
  const restoreBody = captureFocus(body);
  const restoreControls = captureFocus($("config-controls"));
  renderPanels(payload, body);
  restoreBody();
  restoreControls();
}

function renderPanels(payload, body) {
  body.replaceChildren();
  renderControls(payload);
  if (activeSubtab === "keys") {
    renderKeyReference(body, payload);
    return;
  }
  if (activeSubtab === "system") {
    body.appendChild(systemPanel(payload));
    $("config-count").textContent = `${THROTTLE_RESOURCES.length} resources`;
    return;
  }
  if (activeSubtab === "crews") {
    body.appendChild(crewsPanel(payload, { standalone: true }));
    $("config-count").textContent = `${(payload.crews || []).length} crews`;
    return;
  }
  body.appendChild(layersStrip(payload));
  const binding = registryStrip(payload);
  if (binding) body.appendChild(binding);
  const review = reviewStrip(payload);
  if (review) body.appendChild(review);
  for (const section of payload.sections || []) {
    body.appendChild(section.kind === "crews" ? crewsPanel(payload, {}) : sectionPanel(section, payload));
  }
  body.appendChild(pathsPanel(payload));
  $("config-count").textContent = countSummary(payload);
}

function countSummary(payload) {
  const totals = (payload.sections || []).reduce(
    (acc, section) => {
      const counts = section.counts || {};
      acc.set += counts.set || 0;
      acc.total += counts.total || 0;
      return acc;
    },
    { set: 0, total: 0 },
  );
  return `${totals.set} set / ${totals.total} keys`;
}

// ---------------------------------------------------------------- controls

function renderControls(payload) {
  const controls = $("config-controls");
  if (!controls) return;
  controls.replaceChildren();
  if (activeSubtab === "crews") return;

  if (activeSubtab === "system") {
    controls.appendChild(reloadButton());
    return;
  }

  const filter = el("input", { class: "config-filter" });
  filter.type = "search";
  filter.placeholder = "Filter keys…";
  filter.value = filterText;
  filter.setAttribute("aria-label", "Filter configuration keys");
  filter.addEventListener("input", () => {
    filterText = filter.value.trim().toLowerCase();
    if (lastPayload) render(lastPayload);
    $("config-controls").querySelector(".config-filter")?.focus();
  });
  controls.appendChild(filter);

  if (activeSubtab !== "keys") {
    const chips = el("div", { class: "config-chips", title: "Show only keys a file sets, or every key" });
    chips.appendChild(toggleChip("Set only", !showAllKeys, () => setShowAll(false)));
    chips.appendChild(toggleChip("All keys", showAllKeys, () => setShowAll(true)));
    controls.appendChild(chips);
  }

  controls.appendChild(reloadButton());

  const workspaceFile = payload?.layers?.workspace?.path;
  if (workspaceFile) {
    const reveal = el("button", {
      class: "config-action ghost",
      text: "Open .orbit/config.toml",
      title: "Reveal the workspace config path",
    });
    reveal.type = "button";
    reveal.addEventListener("click", () => {
      reveal.textContent = workspaceFile;
      reveal.classList.add("revealed");
    });
    controls.appendChild(reveal);
  }
}

function reloadButton() {
  const reload = el("button", { class: "config-action", text: "Reload" });
  reload.type = "button";
  reload.addEventListener("click", () => {
    editing = null;
    fetchAndRenderConfig().catch((error) => console.error("Failed to reload config", error));
  });
  return reload;
}

function setShowAll(next) {
  showAllKeys = next;
  if (lastPayload) render(lastPayload);
}

function toggleChip(label, on, onClick) {
  const chip = el("button", { class: `config-chip${on ? " on" : ""}`, text: label });
  chip.type = "button";
  chip.setAttribute("aria-pressed", String(on));
  chip.addEventListener("click", onClick);
  return chip;
}

// ------------------------------------------------------------------ strips

function layersStrip(payload) {
  const layers = payload.layers || {};
  const strip = el("div", { class: "config-strip" });
  const chain = el("div", { class: "config-layer-chain" }, [
    layerChip("built-in", null, true),
    el("span", { class: "config-arrow", text: "→" }),
    layerChip("global", layers.global),
    el("span", { class: "config-arrow", text: "→" }),
    layerChip("workspace", layers.workspace),
  ]);
  strip.appendChild(chain);
  if (layers.execution_not_inherited) {
    strip.appendChild(
      el("div", {
        class: "config-warning",
        text: "execution.* keys do not inherit from global while a workspace file exists — this workspace uses its own values or the built-in defaults.",
      }),
    );
  }
  if (payload.workspace_file_exists === false) {
    strip.appendChild(
      el("div", {
        class: "config-note",
        text: "No workspace config.toml yet. The first write asks whether to copy the current global policy or start empty.",
      }),
    );
  }
  return strip;
}

function layerChip(label, layer, builtIn = false) {
  const chip = el("span", { class: `config-layer ${label}` }, [
    el("span", { class: "config-layer-name", text: label }),
  ]);
  if (builtIn) return chip;
  const path = layer?.path || "";
  chip.appendChild(
    el("span", {
      class: `config-layer-path mono${layer?.exists ? "" : " absent"}`,
      text: layer?.exists ? path : `${path} (absent)`,
      title: path,
    }),
  );
  return chip;
}

function registryStrip(payload) {
  const binding = payload.workspace_binding;
  if (!binding) return null;
  const strip = el("div", { class: "config-strip config-registry" });
  const left = el("div", { class: "config-registry-facts" }, [
    el("span", { class: "config-source registry", text: "registry" }),
    fact("base_branch", binding.base_branch),
    fact("ship_mode", binding.ship_mode),
    fact("owner", binding.owner_machine_id),
    el("span", {
      class: "config-note inline",
      text: "from the workspace registry, not config.toml",
    }),
  ]);
  strip.appendChild(left);
  if (binding.base_branch_matches_workflow === true) {
    strip.appendChild(
      el("span", {
        class: "config-match ok",
        text: `matches workflow.base_branch (${binding.workflow_base_branch})`,
      }),
    );
  } else if (binding.base_branch_matches_workflow === false) {
    strip.appendChild(
      el("span", {
        class: "config-match warn",
        text: `workflow.base_branch is ${binding.workflow_base_branch} — delivery uses the registered ${binding.base_branch}`,
      }),
    );
  }
  return strip;
}

// Both automatic-review switches, each with its source: before-PR review is
// `review.before_pr`; after-landing review is the delivery-code-review
// auto-task's own `enabled` flag.
function reviewStrip(payload) {
  const review = payload.review;
  if (!review) return null;
  const strip = el("div", { class: "config-strip config-review" });
  if (review.error) {
    strip.appendChild(el("span", { class: "config-match warn", text: `review: ${review.error}` }));
    return strip;
  }
  for (const [label, line] of [
    ["before-PR review", review.before_pr?.line],
    ["after-landing review", review.after_landing?.line],
  ]) {
    strip.appendChild(fact(label, line));
  }
  if (review.healthy === false) {
    strip.appendChild(
      el("span", {
        class: "config-match warn",
        text: "a review switch is on but cannot run here — see orbit doctor",
      }),
    );
  }
  return strip;
}

function fact(label, value) {
  return el("span", { class: "config-fact" }, [
    el("span", { class: "config-fact-label", text: label }),
    el("span", { class: "config-fact-value mono", text: value == null || value === "" ? NO_VALUE : String(value) }),
  ]);
}

// ---------------------------------------------------------------- sections

function sectionPanel(section, payload) {
  const rows = (section.keys || []).filter(matchesFilter);
  const panel = el("section", { class: "panel config-section" });
  if ((section.not_inherited || 0) > 0) panel.classList.add("not-inherited");
  const header = el("header", {}, [
    el("span", {}, [
      el("span", { class: "config-section-title", text: section.title }),
      section.key_prefix ? el("span", { class: "config-section-prefix mono", text: `${section.key_prefix}.*` }) : null,
      el("span", { class: "config-section-blurb", text: section.blurb }),
    ]),
    el("span", { class: "config-header-right" }, [
      (section.not_inherited || 0) > 0
        ? el("span", {
            class: "config-badge warn",
            text: `${section.not_inherited} global value${section.not_inherited === 1 ? "" : "s"} not inherited`,
          })
        : null,
      el("span", { class: "count", text: countChip(section.counts) }),
    ]),
  ]);
  panel.appendChild(header);

  const settable = section.keys || [];
  const allUnset = settable.length > 0 && (section.counts?.unset || 0) === settable.length;
  const collapsed = allUnset && !showAllKeys && !expandedSections.has(section.token) && !filterText;
  const body = el("div", { class: "config-section-body" });
  if (collapsed) {
    const reveal = el("button", {
      class: "config-action ghost",
      text: `Show ${settable.length} unset keys`,
    });
    reveal.type = "button";
    reveal.addEventListener("click", () => {
      expandedSections.add(section.token);
      if (lastPayload) render(lastPayload);
    });
    body.appendChild(el("div", { class: "config-collapsed" }, [
      el("span", { class: "config-note", text: "No key in this section is set; the built-in policy applies." }),
      reveal,
    ]));
    panel.appendChild(body);
    return panel;
  }

  // `Set only` hides the keys nothing sets — except one a lower layer defines
  // and lost: a global value that was overridden or not inherited is the
  // reason this view exists, so it stays visible at every filter setting.
  const visible = rows.filter(
    (row) => showAllKeys || filterText || row.state === "set" || (row.shadowed_by || []).length > 0,
  );
  if (!visible.length) {
    body.appendChild(
      el("div", { class: "config-empty", text: filterText ? "No key in this section matches the filter." : "No key in this section is set. Switch to All keys to see them." }),
    );
  }
  for (const row of visible) body.appendChild(keyRow(row, payload));
  panel.appendChild(body);
  return panel;
}

function countChip(counts = {}) {
  return `${counts.set || 0} set · ${counts.default || 0} default · ${counts.unset || 0} unset`;
}

function matchesFilter(row) {
  if (!filterText) return true;
  return String(row.key || "").toLowerCase().includes(filterText);
}

function keyRow(row, payload) {
  const node = el("div", { class: `config-row state-${row.state}` });
  node.dataset.key = row.key;
  if (editable(payload) && editing && editing.kind === "key" && editing.key === row.key) {
    node.classList.add("editing");
    node.appendChild(keyEditor(row, payload, node));
    return node;
  }
  node.appendChild(keyCells(row, payload, node));
  return node;
}

function keyCells(row, payload, node) {
  const prefix = row.key.slice(0, row.key.length - String(row.label || row.key).length);
  const cells = el("div", { class: "config-row-main" }, [
    el("span", { class: "config-key mono" }, [
      prefix ? el("span", { class: "config-key-prefix", text: prefix }) : null,
      el("span", { class: "config-key-name", text: row.label || row.key }),
    ]),
    el("span", { class: "config-value mono", text: displayValue(row.value), title: displayValue(row.value) }),
    sourceChip(row),
    el("span", { class: "config-description" }, [
      row.description ? el("span", { text: row.description }) : null,
      ...(row.shadowed_by || []).map((shadow) =>
        el("span", { class: "config-shadow", text: shadow.note }),
      ),
    ]),
    editable(payload) ? editButton(() => startEdit({ kind: "key", key: row.key }), "Edit this key") : null,
  ]);
  if (editable(payload)) {
    cells.classList.add("clickable");
    cells.addEventListener("click", (event) => {
      if (event.target.closest("button")) return;
      startEdit({ kind: "key", key: row.key });
    });
  }
  const error = pendingError(node, row.key);
  if (error) cells.appendChild(error);
  return cells;
}

function sourceChip(row) {
  const layer = row.state === "set" ? row.source?.layer || "set" : row.state;
  const chip = el("span", { class: `config-source ${layer}`, text: layer });
  if (row.source?.path) chip.title = row.source.path;
  return chip;
}

function editButton(onClick, title) {
  const button = el("button", { class: "config-pencil", text: "✎", title });
  button.type = "button";
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

function displayValue(value) {
  if (value == null) return NO_VALUE;
  if (Array.isArray(value)) return value.length ? value.join(" ") : "[]";
  if (typeof value === "string") return value === "" ? '""' : value;
  return String(value);
}

// ----------------------------------------------------------------- editing

/// An edit session. `draft` holds what the operator typed, field by field, so
/// every re-render — a background refresh, a save in flight, a refused write —
/// rebuilds the editor from the draft rather than the last server payload. The
/// session is bound to the workspace it was opened in. A successful save,
/// Cancel, Reload, a workspace or sub-tab switch, or a payload that withdraws
/// operator authority ends it.
function startEdit(next) {
  if (!editable(lastPayload)) return;
  editing = { ...next, error: null, pending: false, draft: {}, workspace: getWorkspace() };
  if (lastPayload) render(lastPayload);
  // The control that opened the editor is gone; land in the editor's first field.
  const field = $("config-body")?.querySelector(".config-editor input, .config-editor select, .config-editor textarea");
  if (field && typeof field.focus === "function") field.focus();
}

function cancelEdit() {
  const closing = editing;
  editing = null;
  if (lastPayload) render(lastPayload);
  focusEditAffordance(closing);
}

/// After an editor closes, focus returns to the pencil that opened it.
function focusEditAffordance(session) {
  if (!session) return;
  const key = session.kind === "crew" ? `crews.${session.name}` : session.key;
  const rows = $("config-body")?.querySelectorAll("[data-key]") || [];
  for (const row of rows) {
    if (row.dataset.key !== key) continue;
    row.querySelector(".config-pencil")?.focus();
    return;
  }
}

/// Escape leaves an editor the way Cancel does, unless a save is in flight.
function cancelOnEscape(editor) {
  editor.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || editing?.pending) return;
    event.stopPropagation();
    cancelEdit();
  });
  return editor;
}

/// Writes are governed; a caller without the operator capability sees the
/// rows read-only rather than an edit that 403s on save.
function editable(payload) {
  return payload?.config_set?.authorized !== false;
}

function writeScope() {
  return (SUBTAB_SOURCES[activeSubtab] || SUBTAB_SOURCES.effective).scope || "workspace";
}

function pendingError(node, key) {
  if (!editing || editing.error == null) return null;
  if (editing.kind === "key" && editing.key !== key) return null;
  if (editing.kind === "crew" && `crews.${editing.name}` !== key) return null;
  return el("div", { class: "config-row-error", text: editing.error });
}

function keyEditor(row, payload, node) {
  const input = valueInput(row, editing.draft);
  const target = writeScope() === "global" ? payload?.layers?.global?.path : payload?.layers?.workspace?.path;
  const editor = el("div", { class: "config-editor" }, [
    el("div", { class: "config-editor-head" }, [
      el("span", { class: "config-key mono", text: row.key }),
      el("span", {
        class: "config-note",
        text: `Was ${displayValue(row.value)} (${row.state === "set" ? row.source?.layer : row.state})`,
      }),
    ]),
    input.node,
    el("div", { class: "config-editor-foot" }, [
      el("span", {
        class: "config-note",
        text: `Writes ${writeScope()}: ${target || "config.toml"}`,
      }),
      el("span", { class: "config-editor-actions" }, [
        saveButton(() => submitKey(row, input.read())),
        cancelButton(),
        row.state === "set"
          ? clearButton(() => submitKeyClear(row))
          : null,
      ]),
    ]),
    editing.error ? el("div", { class: "config-row-error", text: editing.error }) : null,
    ...initChoices(payload, (init) => submitKey(row, input.read(), init)),
  ]);
  return cancelOnEscape(editor);
}

/// The fail-closed first write: when the workspace file does not exist yet,
/// the refusal is shown with the same two explicit choices `orbit config set`
/// offers, rather than a silent file creation.
function initChoices(payload, retry) {
  if (!editing?.error || payload?.workspace_file_exists !== false || writeScope() !== "workspace") {
    return [];
  }
  return [
    el("div", { class: "config-editor-foot" }, [
      el("span", { class: "config-note", text: "Create the workspace file:" }),
      el("span", { class: "config-editor-actions" }, [
        actionButton("Copy global policy", () => retry("seed-from-global")),
        actionButton("Start empty", () => retry("fresh")),
      ]),
    ]),
  ];
}

function saveButton(onClick) {
  return actionButton(editing?.pending ? "Saving…" : "Save", onClick, "primary");
}

function cancelButton() {
  return actionButton("Cancel", cancelEdit, "ghost");
}

function clearButton(onClick) {
  return actionButton("Clear", onClick, "ghost");
}

function actionButton(label, onClick, kind = "") {
  const button = el("button", { class: `config-action ${kind}`.trim(), text: label });
  button.type = "button";
  button.disabled = Boolean(editing?.pending);
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

/// Seeds `input[prop]` from the draft when the operator already changed this
/// field, else from the server value, and mirrors every change back into the
/// draft. The returned `sync` also runs on read, so a save captures the field
/// even when no input event fired.
function bindDraft(input, prop, draft, field, initial) {
  input[prop] = Object.hasOwn(draft, field) ? draft[field] : initial;
  const sync = () => {
    draft[field] = input[prop];
  };
  input.addEventListener(prop === "checked" ? "change" : "input", sync);
  return sync;
}

/// An editor typed by the registry: a choice list for an enum key, a toggle
/// for a bool, a number field for an integer, a chip list for an array, and a
/// text field otherwise.
function valueInput(row, draft) {
  const options = row.options || [];
  if (options.length) {
    const select = el("select", { class: "config-input mono" });
    for (const option of options) {
      const node = el("option", { text: option });
      node.value = option;
      select.appendChild(node);
    }
    const current = options.includes(row.value) ? row.value : options[0];
    const sync = bindDraft(select, "value", draft, "value", current);
    select.addEventListener("change", sync);
    return { node: select, read: () => (sync(), select.value) };
  }
  if (row.value_type === "bool") {
    const label = el("label", { class: "config-toggle" });
    const input = el("input", {});
    input.type = "checkbox";
    const sync = bindDraft(input, "checked", draft, "value", row.value === true);
    label.appendChild(input);
    label.appendChild(el("span", { class: "mono", text: "true / false" }));
    return { node: label, read: () => (sync(), input.checked) };
  }
  if (row.value_type === "integer") {
    const input = el("input", { class: "config-input mono" });
    input.type = "number";
    const sync = bindDraft(input, "value", draft, "value", row.value == null ? "" : String(row.value));
    return {
      node: input,
      read: () => (sync(), input.value.trim() === "" ? null : Number(input.value)),
    };
  }
  if (String(row.value_type || "").startsWith("array")) {
    return chipListInput(Array.isArray(row.value) ? row.value : [], draft, "value");
  }
  const input = el("input", { class: "config-input mono" });
  input.type = "text";
  const sync = bindDraft(input, "value", draft, "value", row.value == null ? "" : String(row.value));
  return { node: input, read: () => (sync(), input.value) };
}

/// The chip list's draft is its entries plus the half-typed entry, so both
/// survive a re-render.
function chipListInput(initial, draft, field) {
  if (!Object.hasOwn(draft, field)) draft[field] = { entries: [...initial], pending: "" };
  const state = draft[field];
  const entries = state.entries;
  let pendingInput = null;
  const node = el("div", { class: "config-chiplist" });
  const draw = () => {
    node.replaceChildren();
    entries.forEach((entry, index) => {
      const chip = el("span", { class: "config-entry mono", text: entry });
      const remove = el("button", { class: "config-entry-remove", text: "×", title: `Remove ${entry}` });
      remove.type = "button";
      remove.addEventListener("click", (event) => {
        event.stopPropagation();
        entries.splice(index, 1);
        draw();
      });
      chip.appendChild(remove);
      node.appendChild(chip);
    });
    const input = el("input", { class: "config-input mono" });
    input.type = "text";
    input.placeholder = "add entry, Enter";
    input.value = state.pending;
    input.addEventListener("input", () => {
      state.pending = input.value;
    });
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      const value = input.value.trim();
      if (!value) return;
      entries.push(value);
      state.pending = "";
      draw();
      pendingInput?.focus();
    });
    pendingInput = input;
    node.appendChild(input);
  };
  draw();
  return {
    node,
    read: () => {
      state.pending = pendingInput?.value ?? state.pending;
      const pending = state.pending.trim();
      return pending ? [...entries, pending] : [...entries];
    },
  };
}

async function submitKey(row, value, init) {
  await submit(() =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, "PUT", {
      value,
      scope: writeScope(),
      ...(init ? { init } : {}),
    }),
  );
}

async function submitKeyClear(row) {
  await submit(() =>
    requestJson(
      `/api/config/keys/${encodeURIComponent(row.key)}?scope=${encodeURIComponent(writeScope())}`,
      "DELETE",
    ),
  );
}

/// One save: apply, then re-read the view so the row shows the provenance the
/// server resolved rather than an optimistic local edit. The outcome lands
/// only on the session that issued it: a workspace switch or Reload while the
/// write was in flight has already ended that session, and a later edit must
/// not inherit its error or be closed by its success.
async function submit(request) {
  const session = editing;
  if (!session) return;
  session.pending = true;
  session.error = null;
  if (lastPayload) render(lastPayload);
  try {
    await request();
    if (editing === session) editing = null;
    if (session.workspace !== getWorkspace()) return;
    await fetchAndRenderConfig();
    if (editing === null) focusEditAffordance(session);
  } catch (error) {
    // The admission layer's message is the useful part; it names the key, the
    // value, and what was expected instead. The draft is untouched, so the
    // editor re-renders with the values that were refused.
    session.pending = false;
    session.error = error.message || String(error);
    if (editing === session && lastPayload) render(lastPayload);
  }
}

// ------------------------------------------------------------------ system

/// The System tab reads three things: the global file (the values being
/// edited and their default/global provenance), the effective view (to see a
/// workspace file overriding a key), and the serving host's live resources.
/// The host read is best effort — a throttle panel with no readings is still
/// an editable panel.
async function fetchSystem(source) {
  const [file, effective, host] = await Promise.all([
    fetchJson(source.path),
    fetchJson(SUBTAB_SOURCES.effective.path),
    fetchHostResources(),
  ]);
  return { ...file, workspace_overrides: workspaceOverrides(effective), host };
}

async function fetchHostResources() {
  try {
    // The host is the serving machine, whatever workspace is selected, so this
    // read deliberately skips the workspace parameter the other reads carry.
    const response = await fetch("/api/host/resources");
    return response.ok ? await response.json() : null;
  } catch {
    return null;
  }
}

/// Throttle keys whose effective value comes from the workspace file, by key.
function workspaceOverrides(effective) {
  const overrides = {};
  for (const section of effective?.sections || []) {
    for (const row of section.keys || []) {
      if (row.key?.startsWith(THROTTLE_PREFIX) && row.state === "set" && row.source?.layer === "workspace") {
        overrides[row.key] = row;
      }
    }
  }
  return overrides;
}

function throttleRows(payload) {
  const rows = {};
  for (const section of payload.sections || []) {
    for (const row of section.keys || []) {
      if (row.key?.startsWith(THROTTLE_PREFIX)) rows[row.key] = row;
    }
  }
  return rows;
}

function systemPanel(payload) {
  const rows = throttleRows(payload);
  const host = payload.host;
  const verdict = hostVerdict(host);
  const panel = el("section", { class: "panel config-section config-system" });
  panel.appendChild(
    el("header", {}, [
      el("span", {}, [
        el("span", { class: "config-section-title", text: "Resource throttle" }),
        el("span", { class: "config-section-prefix mono", text: `${THROTTLE_PREFIX}*` }),
        el("span", { class: "config-section-blurb", text: "new work is held while a resource stays at its throttle-at mark" }),
      ]),
      el("span", { class: "config-header-right" }, [
        el("span", { class: `config-verdict ${verdict.status}`, text: verdict.status }),
      ]),
    ]),
  );
  const body = el("div", { class: "config-section-body" });
  body.appendChild(el("div", { class: "config-strip" }, [
    el("div", { class: "config-note", text: `Edits write the global file${payload.layers?.global?.path ? ` (${payload.layers.global.path})` : ""}: the topbar readings and admission on this host read it.` }),
    ...Object.keys(payload.workspace_overrides || {}).length
      ? [el("div", { class: "config-warning", text: "The workspace file overrides some of these keys; its value wins for this workspace's runtimes." })]
      : [],
  ]));
  body.appendChild(systemVerdict(host, verdict, rows, payload));
  const head = el("div", { class: "config-sys-grid config-sys-head" }, [
    el("span", { text: "resource" }),
    el("span", { text: "live reading" }),
    el("span", { text: "throttle at" }),
    el("span", { text: "resume below" }),
  ]);
  body.appendChild(head);
  for (const resource of THROTTLE_RESOURCES) body.appendChild(systemResource(resource, rows, payload, host));
  panel.appendChild(body);
  return panel;
}

function systemVerdict(host, verdict, rows, payload) {
  const node = el("div", { class: "config-sys-verdict" });
  const enabledKey = `${THROTTLE_PREFIX}enabled`;
  node.appendChild(el("div", { class: "config-sys-enabled" }, [
    el("span", { class: "config-key mono", text: "enabled" }),
    rows[enabledKey] ? systemValueCell(rows[enabledKey], payload) : el("span", { class: "config-value mono", text: NO_VALUE }),
  ]));
  const pressures = THROTTLE_RESOURCES.flatMap((resource) => hostReading(host, resource).pressures);
  let text;
  if (verdict.status === "held") {
    text = `Holding new admissions: ${pressures.map(describePressure).join("; ")}`;
  } else if (verdict.status === "disabled") {
    text = "Throttle disabled — pressure is still reported but holds nothing.";
  } else if (verdict.status === "open") {
    text = "Open — no resource is held.";
  } else {
    text = `Verdict unknown — ${verdict.reason}`;
  }
  node.appendChild(el("div", { class: `config-sys-verdict-text ${verdict.status}`, text }));
  const editor = openEditor(rows[enabledKey], payload);
  if (editor) node.appendChild(editor);
  return node;
}

/// The open editor for `row`, or null; it sits under the row it belongs to.
function openEditor(row, payload) {
  if (!editable(payload) || !row || !editing || editing.kind !== "key" || editing.key !== row.key) return null;
  const node = el("div", { class: "config-row editing" });
  node.dataset.key = row.key;
  node.appendChild(keyEditor(row, payload, node));
  return node;
}

function describePressure(pressure) {
  const since = new Date(pressure.since);
  const when = Number.isNaN(since.getTime()) ? pressure.since : `${since.toISOString().slice(0, 16).replace("T", " ")}Z`;
  return `${pressure.resource} ${Math.round(pressure.percent)}% ≥ ${pressure.high_percent}% since ${when}`;
}

function systemResource(resource, rows, payload, host) {
  const reading = hostReading(host, resource);
  const high = rows[`${THROTTLE_PREFIX}${resource}_high_percent`];
  const resume = rows[`${THROTTLE_PREFIX}${resource}_resume_percent`];
  const node = el("div", { class: "config-sys-resource" });
  node.dataset.resource = resource;
  const live = el("span", { class: `config-sys-reading ${reading.severity}${reading.held ? " throttled" : ""}` }, [
    el("span", { class: "mono", text: reading.known ? `${reading.reading.percent.toFixed(1)}%` : NO_VALUE }),
    el("span", { class: "config-sys-severity", text: reading.known ? reading.severity : reading.note }),
    ...(reading.held ? [el("span", { class: "host-resource-held", text: "held" })] : []),
  ]);
  node.appendChild(el("div", { class: "config-sys-grid" }, [
    el("span", { class: "config-key mono", text: resource }),
    live,
    high ? systemValueCell(high, payload) : el("span", { class: "config-value mono", text: NO_VALUE }),
    resume ? systemValueCell(resume, payload) : el("span", { class: "config-value mono", text: NO_VALUE }),
  ]));
  for (const row of [high, resume]) {
    const editor = openEditor(row, payload);
    if (editor) node.appendChild(editor);
  }
  return node;
}

/// One key's value, its default/global source, a marker when the workspace
/// file overrides it, and the edit pencil.
function systemValueCell(row, payload) {
  const override = payload.workspace_overrides?.[row.key];
  const cell = el("span", { class: `config-sys-cell${override ? " overridden" : ""}` }, [
    el("span", { class: "config-value mono", text: displayValue(row.value) }),
    sourceChip(row),
    override
      ? el("span", {
          class: "config-source workspace",
          text: `workspace ${displayValue(override.value)}`,
          title: `The workspace file sets ${row.key} to ${displayValue(override.value)}; that value wins for this workspace's runtimes. The global value shown here applies to this host's topbar and admission.`,
        })
      : null,
    editable(payload) ? editButton(() => startEdit({ kind: "key", key: row.key }), `Edit ${row.key}`) : null,
  ]);
  cell.dataset.key = row.key;
  return cell;
}

// ------------------------------------------------------------------- crews

function crewsPanel(payload, { standalone }) {
  const crews = payload.crews || [];
  const panel = el("section", { class: "panel config-section config-crews" });
  const section = (payload.sections || []).find((entry) => entry.kind === "crews");
  panel.appendChild(
    el("header", {}, [
      el("span", {}, [
        el("span", { class: "config-section-title", text: section?.title }),
        el("span", { class: "config-section-blurb", text: section?.blurb }),
      ]),
      el("span", { class: "config-header-right" }, [
        el("span", { class: "count", text: `${crews.length} defined` }),
        editable(payload) ? addCrewButton() : null,
      ]),
    ]),
  );
  const body = el("div", { class: "config-section-body" });
  if (!crews.length && !(editing && editing.kind === "crew" && editing.name === "")) {
    body.appendChild(
      el("div", {
        class: "config-empty",
        text: standalone
          ? "No crew is defined for this workspace."
          : "No crew is defined; the built-in crew registry applies.",
      }),
    );
  }
  if (editing && editing.kind === "crew" && editing.name === "") {
    body.appendChild(crewEditor({ name: "", tags: [] }, payload, true));
  }
  for (const crew of crews) body.appendChild(crewRow(crew, payload));
  panel.appendChild(body);
  return panel;
}

function addCrewButton() {
  const button = el("button", { class: "config-action", text: "+ Add crew" });
  button.type = "button";
  button.addEventListener("click", () => startEdit({ kind: "crew", name: "" }));
  return button;
}

function crewRow(crew, payload) {
  const referenced = crew.referenced_by || [];
  const node = el("div", { class: "config-row config-crew-row" });
  node.dataset.key = `crews.${crew.name}`;
  if (referenced.length) node.classList.add("referenced");
  // A table without `enabled` is enabled; only an explicit false disables.
  const disabled = crew.enabled === false;
  if (disabled) node.classList.add("disabled");
  if (editing && editing.kind === "crew" && editing.name === crew.name) {
    node.classList.add("editing");
    node.appendChild(crewEditor(crew, payload, false));
    return node;
  }
  const cells = el("div", { class: "config-crew-cells" }, [
    el("span", { class: "config-key mono" }, [
      el("span", { text: crew.name }),
      disabled
        ? el("span", {
            class: "config-crew-disabled",
            text: "disabled",
            title: `Dispatch refuses this crew; set crews.${crew.name}.enabled = true to use it`,
          })
        : null,
    ]),
    providerCell(crew.provider),
    el("span", { class: "config-value mono", text: displayValue(crew.model) }),
    el("span", { class: "config-value mono", text: displayValue(crew.effort) }),
    el("span", { class: "config-value mono", text: displayValue(crew.tags) }),
    el("span", { class: `config-source ${crew.source}`, text: crew.source }),
    el("span", { class: "config-description" }, [
      crew.description ? el("span", { text: crew.description }) : null,
      referenced.length
        ? el("span", { class: "config-referenced", text: `referenced by ${referenced.join(", ")}` })
        : null,
    ]),
    editable(payload) ? editButton(() => startEdit({ kind: "crew", name: crew.name }), "Edit this crew") : null,
  ]);
  node.appendChild(cells);
  const error = pendingError(node, `crews.${crew.name}`);
  if (error) node.appendChild(error);
  return node;
}

function providerCell(provider) {
  const cell = el("span", { class: "config-provider mono" });
  const dot = el("span", { class: "config-provider-dot" });
  // The family palette is the dashboard's existing one; an unknown provider
  // keeps the neutral accent rather than inventing a colour.
  dot.style.background = `var(--ag-${String(provider || "").toLowerCase()}, var(--accent))`;
  cell.appendChild(dot);
  cell.appendChild(el("span", { text: displayValue(provider) }));
  return cell;
}

function crewEditor(crew, payload, isNew) {
  const draft = editing.draft;
  const fields = {};
  const rows = [];
  if (isNew) {
    const name = el("input", { class: "config-input mono" });
    name.type = "text";
    name.placeholder = "crew name";
    const sync = bindDraft(name, "value", draft, "name", "");
    rows.push(fieldRow("name", name));
    fields.name = () => (sync(), name.value.trim());
  }
  // The field list is the registry's, so a crew field added or retired in
  // `orbit-config` changes this editor without a JS edit.
  const crewFields = payload.crew_fields || [];
  for (const field of crewFields) {
    const current = crew[field];
    if (field === "enabled") {
      const toggle = el("input", { class: "config-input" });
      toggle.type = "checkbox";
      const was = current !== false;
      const sync = bindDraft(toggle, "checked", draft, field, was);
      rows.push(fieldRow(field, toggle));
      // Unchanged state is omitted so saving another field never writes an
      // `enabled` key into a table that relied on the enabled default.
      fields[field] = () => (sync(), toggle.checked === was ? undefined : toggle.checked);
      continue;
    }
    if (field === "tags") {
      const list = chipListInput(Array.isArray(current) ? current : [], draft, field);
      rows.push(fieldRow(field, list.node));
      fields[field] = list.read;
      continue;
    }
    const input = el("input", { class: "config-input mono" });
    input.type = "text";
    const sync = bindDraft(input, "value", draft, field, current == null ? "" : String(current));
    rows.push(fieldRow(field, input));
    fields[field] = () => (sync(), input.value.trim() === "" ? null : input.value.trim());
  }
  const editor = el("div", { class: "config-editor" }, [
    el("div", { class: "config-editor-head" }, [
      el("span", { class: "config-key mono", text: isNew ? "new crew" : `crews.${crew.name}` }),
    ]),
    ...rows,
    el("div", { class: "config-editor-foot" }, [
      el("span", {
        class: "config-note",
        text: `Writes ${writeScope()}: ${(writeScope() === "global" ? payload?.layers?.global?.path : payload?.layers?.workspace?.path) || "config.toml"}`,
      }),
      el("span", { class: "config-editor-actions" }, [
        saveButton(() => submitCrew(crew, fields, isNew)),
        cancelButton(),
        !isNew ? actionButton("Delete", () => submitCrewDelete(crew), "ghost") : null,
      ]),
    ]),
    editing.error ? el("div", { class: "config-row-error", text: editing.error }) : null,
    ...initChoices(payload, (init) => submitCrew(crew, fields, isNew, init)),
  ]);
  return cancelOnEscape(editor);
}

function fieldRow(label, input) {
  return el("div", { class: "config-field" }, [
    el("span", { class: "config-field-label", text: label }),
    input,
  ]);
}

async function submitCrew(crew, fields, isNew, init) {
  const name = isNew ? fields.name() : crew.name;
  if (!name) {
    editing.error = "a crew needs a name";
    if (lastPayload) render(lastPayload);
    return;
  }
  const body = {};
  for (const [field, read] of Object.entries(fields)) {
    if (field === "name") continue;
    const value = read();
    if (value !== undefined) body[field] = value;
  }
  await submit(() =>
    requestJson(`/api/config/crews/${encodeURIComponent(name)}`, "PUT", {
      fields: body,
      scope: writeScope(),
      ...(init ? { init } : {}),
    }),
  );
}

async function submitCrewDelete(crew) {
  await submit(() =>
    requestJson(
      `/api/config/crews/${encodeURIComponent(crew.name)}?scope=${encodeURIComponent(writeScope())}`,
      "DELETE",
    ),
  );
}

// ------------------------------------------------------------------- paths

function pathsPanel(payload) {
  const panel = el("section", { class: "panel config-section" });
  panel.appendChild(
    el("header", {}, [
      el("span", {}, [
        el("span", { class: "config-section-title", text: "Paths" }),
        el("span", { class: "config-section-blurb", text: "where this workspace reads and writes" }),
      ]),
      el("span", { class: "count", text: `${(payload.paths || []).length} locations` }),
    ]),
  );
  const grid = el("div", { class: "config-paths" });
  for (const entry of payload.paths || []) {
    grid.appendChild(el("span", { class: "config-path-label", text: entry.label }));
    grid.appendChild(el("span", { class: "config-path-value mono", text: entry.value, title: entry.value }));
  }
  panel.appendChild(grid);
  return panel;
}

// --------------------------------------------------------------- key table

function renderKeyReference(body, payload) {
  const keys = (payload.keys || []).filter((key) =>
    !filterText || String(key.key).toLowerCase().includes(filterText),
  );
  const panel = el("section", { class: "panel config-section" });
  panel.appendChild(
    el("header", {}, [
      el("span", {}, [
        el("span", { class: "config-section-title", text: "Settable keys" }),
        el("span", { class: "config-section-blurb", text: "every key orbit config set admits" }),
      ]),
      el("span", { class: "count", text: `${keys.length} keys` }),
    ]),
  );
  const table = el("div", { class: "config-section-body" });
  for (const key of keys) {
    table.appendChild(
      el("div", { class: "config-row config-key-row" }, [
        el("span", { class: "config-key mono", text: key.key }),
        el("span", { class: "config-value mono", text: key.value_type }),
        el("span", { class: "config-source registry", text: key.section }),
        el("span", { class: "config-description" }, [
          el("span", { text: key.description }),
          (key.options || []).length
            ? el("span", { class: "config-options mono", text: `one of: ${key.options.join(", ")}` })
            : null,
        ]),
      ]),
    );
  }
  if (!keys.length) table.appendChild(el("div", { class: "config-empty", text: "No key matches the filter." }));
  panel.appendChild(table);
  body.appendChild(panel);
  $("config-count").textContent = `${keys.length} keys`;
}
