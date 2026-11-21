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
//
// Topic pages (Workflow, Execution, Review, Machine, Other) live in
// config-topics.js and the blocks every page shares in settings-ui.js; this
// module owns the tab, Crews and the throttle panel.

import { captureFocus, captureWorkspaceVisit, el, fetchJson, formatDateTime, getWorkspace, hostWriteRefusal, isAggregateView, onWorkspaceChange, renderPanelPlaceholder, requestJson, requestPanel, teardownNode } from './common.js';
import { renderExecution, renderMachine, renderOther, renderReview, renderWorkflow, resetTopics, workspacePolicyMissing } from './config-topics.js';
import { fetchHostResourcePayload, hostReading, hostVerdict, onHostResources } from './host-resources.js';
import { renderSettingsBar, settingsEditButton, settingsKeyName, settingsRow, settingsRowSource, settingsSearchField, settingsSection, settingsSource, settingsSourceIsSet } from './settings-ui.js';

const $ = (id) => document.getElementById(id);

/// Rendered in place of a value that does not exist, matching `config show`.
const NO_VALUE = "–";
const EMPTY_ARRAY = "—";

/// Which file a sub-view reads and writes. `effective` never writes global:
/// the layered view's edits go to the workspace file, which is the per-user,
/// git-ignored one. `system` is the exception: the topbar chips and admission
/// read the serving machine's global settings, so that is what it edits.
const SUBTAB_SOURCES = {
  workflow: { path: "/api/config/effective", scope: "workspace" },
  execution: { path: "/api/config/effective", scope: "workspace" },
  review: { path: "/api/config/effective", scope: "workspace" },
  crews: { path: "/api/config/effective", scope: "workspace" },
  machine: { path: "/api/config/file?scope=global", scope: "global" },
  other: { path: "/api/config/effective", scope: "workspace" },
};
const LEGACY_SUBTABS = { effective: "workflow", "workspace-file": "other", "global-file": "machine", system: "machine", keys: "other", "all-keys": "other" };

/// The System tab is the resource throttle only: its keys, in display order.
const THROTTLE_PREFIX = "workflow.resource_throttle.";
const THROTTLE_RESOURCES = ["cpu", "memory", "disk", "io"];
// The `io` marks are fixed, not configurable [ORB-22808]: [throttle at, resume below].
const FIXED_THROTTLE_MARKS = { io: [20, 10] };

/// Disk I/O pressure exists only on hosts that report it (Linux with PSI), so
/// its row appears only when the host's payload carries a reading.
function throttleResources(host) {
  return THROTTLE_RESOURCES.filter((resource) => resource !== "io" || host?.io);
}

let activeSubtab = "workflow";
let lastPayload = null;
// The row (or crew) currently being edited with its draft values. It is
// operator state, so a background refresh must not discard it.
let editing = null;
let unsubscribeWorkspace = null;

export function initConfig() {
  unsubscribeWorkspace?.();
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastPayload = null;
    editing = null;
    resetCrewList();
    resetTopics();
  });
  // Live readings follow the topbar's poll. A rebuild would discard an open
  // editor's focus, so it waits until nothing is being edited.
  onHostResources((host) => {
    if (activeSubtab !== "machine" || !lastPayload || editing) return;
    lastPayload = { ...lastPayload, host };
    render(lastPayload);
  });
}

export function setConfigSubtab(name) {
  if (LEGACY_SUBTABS[name]) name = LEGACY_SUBTABS[name];
  if (!SUBTAB_SOURCES[name]) name = "workflow";
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
    const bar = $("config-controls");
    if (bar) bar.hidden = true;
    return;
  }
  const source = SUBTAB_SOURCES[activeSubtab] || SUBTAB_SOURCES.workflow;
  await requestPanel(
    "config-body",
    `${activeSubtab}:${getWorkspace() || ""}`,
    async () => {
      try {
        if (activeSubtab === "machine") return await fetchMachine(source);
        return await fetchJson(source.path);
      } catch (error) {
        if (error.status === 400 && source.path.startsWith("/api/config/file")) {
          error.remedy = "Correct the configuration in the named file, then reload this view.";
        }
        throw error;
      }
    },
    (payload) => {
      lastPayload = payload;
      render(payload);
    },
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
  // replaceChildren detaches without running teardown, so a poll would leave
  // the previous render's clamp spans observed. Release them first.
  teardownNode(body);
  body.replaceChildren();
  const ctx = viewContext(payload);
  let bar;
  if (activeSubtab === "crews") {
    const crews = crewsPanel(payload);
    body.appendChild(crews.panel);
    bar = { lead: crews.lead, actions: [editable(payload) ? addCrewButton() : null] };
  } else if (activeSubtab === "machine") bar = renderMachine(body, payload, ctx, systemPanel(payload));
  else if (activeSubtab === "execution") bar = renderExecution(body, payload, ctx);
  else if (activeSubtab === "review") bar = renderReview(body, payload, ctx);
  else if (activeSubtab === "other") bar = renderOther(body, payload, ctx);
  else bar = renderWorkflow(body, payload, ctx);
  renderSettingsBar({
    ...bar,
    onReload: () => {
      editing = null;
      fetchAndRenderConfig().catch((error) => console.error("Failed to reload config", error));
    },
  });
}

/// What a sub-view module needs from the tab: whether this session may write
/// and why not, and how to rebuild or re-read the view.
function viewContext(payload) {
  return {
    canEdit: editable(payload),
    refusal: hostWriteRefusal() || payload?.config_set?.reason || "",
    render,
    rerender: () => { if (lastPayload) render(lastPayload); },
    reload: fetchAndRenderConfig,
  };
}

function displayValue(value) {
  if (value == null) return NO_VALUE;
  if (Array.isArray(value)) return value.length ? value.join(" ") : EMPTY_ARRAY;
  if (typeof value === "string") return value === "" ? '""' : value;
  return String(value);
}

// ----------------------------------------------------------------- editing

/// An edit session. `draft` holds what the operator typed, field by field, so
/// every re-render — a background refresh, a save in flight, a refused write —
/// rebuilds the editor from the draft rather than the last server payload. The
/// session is bound to the host/workspace visit it was opened in. A successful
/// save, Cancel, Reload, a workspace or sub-tab switch, or a payload that withdraws
/// operator authority ends it.
function startEdit(next) {
  if (!editable(lastPayload)) return;
  if (next.kind === "key" && !keySettable(next.key, lastPayload)) return;
  editing = { ...next, error: null, pending: false, draft: {}, visit: captureWorkspaceVisit() };
  if (lastPayload) render(lastPayload);
  // The control that opened the editor is gone; land in the editor's first field.
  const field = $("config-body")?.querySelector(".config-editor input, .config-editor select, .config-editor textarea");
  if (field && typeof field.focus === "function") field.focus();
}

function keySettable(key, payload) {
  const row = (payload?.sections || []).flatMap((section) => section.keys || []).find((entry) => entry.key === key);
  return row?.settable !== false;
}

function cancelEdit() {
  const closing = editing;
  editing = null;
  if (lastPayload) render(lastPayload);
  focusEditAffordance(closing);
}

/// After an editor closes, focus returns to the Edit control that opened it.
function focusEditAffordance(session) {
  if (!session) return;
  // A new crew that was never saved has no row yet: focus the Add button.
  if (session.kind === "crew" && !session.name) {
    $("config-controls")?.querySelector(".config-add-crew")?.focus();
    return;
  }
  const key = session.kind === "crew" ? `crews.${session.name}` : session.key;
  const rows = $("config-body")?.querySelectorAll("[data-key]") || [];
  for (const row of rows) {
    if (row.dataset.key !== key) continue;
    row.querySelector(".settings-edit")?.focus();
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
  return payload?.config_set?.authorized !== false && !hostWriteRefusal();
}

function writeScope() {
  if (activeSubtab === "machine") return "global";
  return (SUBTAB_SOURCES[activeSubtab] || SUBTAB_SOURCES.workflow).scope || "workspace";
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

/// The fail-closed first write: when the workspace has no policy yet,
/// the refusal is shown with the same two explicit choices `orbit config set`
/// offers, rather than a silent file creation.
function initChoices(payload, retry) {
  if (!editing?.error || !workspacePolicyMissing(payload) || writeScope() !== "workspace") {
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
  const button = el("button", { class: `settings-btn ${kind}`.trim(), text: label });
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
    const select = el("select", { class: "settings-input mono" });
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
    const input = el("input", { class: "settings-input mono" });
    input.type = "number";
    // The key's declared range bounds the field [ORB-22809].
    if (row.allowed?.kind === "range") {
      if (row.allowed.min != null) input.min = String(row.allowed.min);
      if (row.allowed.max != null) input.max = String(row.allowed.max);
    }
    const sync = bindDraft(input, "value", draft, "value", row.value == null ? "" : String(row.value));
    return {
      node: input,
      read: () => (sync(), input.value.trim() === "" ? null : Number(input.value)),
    };
  }
  if (String(row.value_type || "").startsWith("array")) {
    return chipListInput(Array.isArray(row.value) ? row.value : [], draft, "value");
  }
  const input = el("input", { class: "settings-input mono" });
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
      const chip = el("span", { class: "chip mono" }, [
        el("span", { class: "chip-label", text: entry, title: entry }),
      ]);
      const remove = el("button", { class: "config-entry-remove", text: "×", "aria-label": `Remove ${entry}` });
      remove.type = "button";
      remove.addEventListener("click", (event) => {
        event.stopPropagation();
        entries.splice(index, 1);
        draw();
      });
      chip.appendChild(remove);
      node.appendChild(chip);
    });
    const input = el("input", { class: "settings-input mono" });
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
  await submitConfigMutation((visit) =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, "PUT", {
      value,
      scope: writeScope(),
      ...(init ? { init } : {}),
    }, visit),
  );
}

async function submitKeyClear(row) {
  await submitConfigMutation((visit) =>
    requestJson(
      `/api/config/keys/${encodeURIComponent(row.key)}?scope=${encodeURIComponent(writeScope())}`,
      "DELETE",
      undefined,
      visit,
    ),
  );
}

/// One save: apply, then re-read the view so the row shows the provenance the
/// server resolved rather than an optimistic local edit. The outcome lands
/// only on the session that issued it: a workspace switch or Reload while the
/// write was in flight has already ended that session, and a later edit must
/// not inherit its error or be closed by its success.
async function submitConfigMutation(request, afterWrite) {
  const session = editing;
  if (!session || session.pending || !session.visit.isCurrent()) return;
  const ownsSubmission = () => editing === session && session.visit.isCurrent();
  session.pending = true;
  session.error = null;
  if (lastPayload) render(lastPayload);
  try {
    await request(session.visit);
  } catch (error) {
    // The admission layer's message is the useful part; it names the key, the
    // value, and what was expected instead. The draft is untouched, so the
    // editor re-renders with the values that were refused.
    if (!ownsSubmission()) return;
    session.pending = false;
    session.error = error.message || String(error);
    if (editing === session && lastPayload) render(lastPayload);
    return;
  }
  if (!ownsSubmission()) return;
  afterWrite?.(session);
  try {
    await fetchAndRenderConfig();
  } catch (_) {
    // The write landed; only the re-read failed. requestPanel already left
    // its stale-data note on the panel, so close as a successful save rather
    // than reporting the re-read error as a refused write.
  }
  if (!ownsSubmission()) return;
  editing = null;
  if (lastPayload) render(lastPayload);
  focusEditAffordance(session);
}

// ------------------------------------------------------------------ system

/// The System tab reads three things: the global file (the values being
/// edited and their default/global provenance), the effective view (to see a
/// workspace file overriding a key), and the serving host's live resources.
/// The host read is best effort — a throttle panel with no readings is still
/// an editable panel.
async function fetchMachine(source) {
  const [file, effective, host] = await Promise.all([
    fetchJson(source.path),
    fetchJson("/api/config/effective"),
    fetchHostResources(),
  ]);
  return { ...file, effective, workspace_overrides: workspaceOverrides(effective), host };
}


async function fetchHostResources() {
  try {
    return await fetchHostResourcePayload();
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

const VERDICT_LABELS = { open: "Open", held: "Held", disabled: "Disabled", unknown: "Unknown" };

function systemPanel(payload) {
  const rows = throttleRows(payload);
  const host = payload.host;
  const verdict = hostVerdict(host);
  const { section, body } = settingsSection("Resource throttle", {
    state: {
      text: VERDICT_LABELS[verdict.status] || verdict.status,
      tone: verdict.status === "open" ? "ok" : verdict.status === "held" ? "bad" : "",
    },
  });
  // The header pill is the verdict; a line under it says why only when the
  // throttle is not simply open.
  const detail = verdictDetail(host, verdict);
  if (detail) body.appendChild(el("div", { class: `settings-note${verdict.status === "held" ? " bad" : ""}`, text: detail }));
  if (Object.keys(payload.workspace_overrides || {}).length) {
    body.appendChild(el("div", { class: "settings-note warn", text: "The workspace file overrides some of these keys; its value wins for this workspace's runtimes." }));
  }
  const enabled = rows[`${THROTTLE_PREFIX}enabled`];
  if (enabled) {
    body.appendChild(throttleEnabledRow(enabled, payload));
    const editor = openEditor(enabled, payload);
    if (editor) body.appendChild(editor);
  }
  const table = el("div", { class: "settings-table", role: "table", "aria-label": "Resource throttle marks" });
  table.appendChild(el("div", { class: "settings-table-row col-head", role: "row" },
    ["Resource", "Live", "Throttle at", "Resume below"].map((text) => el("div", { text, role: "columnheader" }))));
  for (const resource of throttleResources(host)) table.append(...systemResource(resource, rows, payload, host));
  body.appendChild(table);
  return section;
}

function verdictDetail(host, verdict) {
  if (verdict.status === "held") {
    const pressures = throttleResources(host).flatMap((resource) => hostReading(host, resource).pressures);
    return `Holding new admissions: ${pressures.map(describePressure).join("; ")}`;
  }
  if (verdict.status === "disabled") return "Pressure is still reported but holds nothing.";
  if (verdict.status === "open") return null;
  return `Verdict unknown: ${verdict.reason}`;
}

/// `enabled` reads like any other key row; its inline editor opens under it.
function throttleEnabledRow(row, payload) {
  const override = payload.workspace_overrides?.[row.key];
  const node = settingsRow({
    name: settingsKeyName(row.key, { prefix: THROTTLE_PREFIX, description: "New work is held while a resource stays at its throttle-at mark." }),
    value: [
      el("div", { class: "settings-value", text: displayValue(row.value) }),
      override ? el("div", { class: "settings-meta", title: overrideTitle(row, override) }, ["Workspace: ", el("span", { class: "mono", text: displayValue(override.value) })]) : null,
    ],
    source: settingsRowSource(row),
    action: editable(payload)
      ? el("div", { class: "settings-act" }, [settingsEditButton(() => startEdit({ kind: "key", key: row.key }), `Edit ${row.key}`)])
      : null,
  });
  if (override) node.classList.add("overridden");
  node.dataset.key = row.key;
  return node;
}

function overrideTitle(row, override) {
  return `The workspace file sets ${row.key} to ${displayValue(override.value)}; that value wins for this workspace's runtimes. The global value shown here applies to this host's topbar and admission.`;
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
  return `${pressure.resource} ${Math.round(pressure.percent)}% ≥ ${pressure.high_percent}% since ${formatDateTime(pressure.since)}`;
}

/// One resource: its table row, then the open editor of either mark.
function systemResource(resource, rows, payload, host) {
  const reading = hostReading(host, resource);
  const high = rows[`${THROTTLE_PREFIX}${resource}_high_percent`];
  const resume = rows[`${THROTTLE_PREFIX}${resource}_resume_percent`];
  const fixed = FIXED_THROTTLE_MARKS[resource];
  const markCell = (row, index) => {
    if (row) return systemValueCell(row, payload, `Edit ${resource} ${index ? "resume-below" : "throttle-at"} mark`);
    return el("div", { class: "settings-table-cell", role: "cell" }, [
      el("span", { class: "settings-value", text: fixed ? String(fixed[index]) : NO_VALUE }),
      fixed ? el("span", { class: "settings-table-note", text: "fixed" }) : null,
    ]);
  };
  const node = el("div", { class: "settings-table-row", role: "row" }, [
    el("div", { class: "settings-key", text: resource, role: "cell" }),
    el("div", { class: `settings-table-cell settings-reading ${reading.severity}${reading.held ? " throttled" : ""}`, role: "cell" }, [
      el("span", { class: "settings-value", text: reading.known ? `${reading.reading.percent.toFixed(1)}%` : NO_VALUE }),
      el("span", { class: "settings-table-note", text: reading.known ? reading.severity : reading.note }),
      reading.held ? el("span", { class: "host-resource-held", text: "held" }) : null,
    ]),
    markCell(high, 0),
    markCell(resume, 1),
  ]);
  node.dataset.resource = resource;
  return [node, ...[high, resume].map((row) => openEditor(row, payload)).filter(Boolean)];
}

/// One mark: its value, its source where it is not the default, a note when
/// the workspace file overrides it, and the edit pencil.
function systemValueCell(row, payload, editLabel) {
  const override = payload.workspace_overrides?.[row.key];
  const cell = el("div", { class: `settings-table-cell${override ? " overridden" : ""}`, role: "cell" }, [
    el("span", { class: "settings-value", text: displayValue(row.value) }),
    row.state === "set" ? settingsRowSource(row) : null,
    override ? el("span", { class: "settings-table-note", text: `Workspace ${displayValue(override.value)}`, title: overrideTitle(row, override) }) : null,
    editable(payload) ? settingsEditButton(() => startEdit({ kind: "key", key: row.key }), editLabel, { icon: true }) : null,
  ]);
  cell.dataset.key = row.key;
  return cell;
}

// ------------------------------------------------------------------- crews

// Limit, Effort, Tags / fallbacks, Source and Used by. On a narrow screen they
// sit behind each row's "Details" control; at desktop width every column shows.
const CREW_DETAIL_COLUMNS = new Set([3, 4, 5, 6, 7]);
const CREW_COLUMNS = ["Name", "Provider", "Model", "Limit", "Effort", "Tags / fallbacks", "Source", "Used by", "Actions"];

// The crew list is bounded: the first CREW_PAGE matches render, and the rest
// stay one control away ("Show more", "Show all"). The filter, the reveal
// window and each row's open detail group are operator state that survives
// a background refresh; a workspace switch starts them over.
const CREW_PAGE = 6;
let crewQuery = "";
let crewLimit = CREW_PAGE;
const crewExpanded = new Set();

function resetCrewList() {
  crewQuery = "";
  crewLimit = CREW_PAGE;
  crewExpanded.clear();
}

/// Every whitespace-separated term of the query must appear in the crew's
/// name, provider, model, effort, tags or description.
function crewMatchesQuery(crew, query) {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return true;
  const haystack = [crew.name, crew.provider, crew.model, crew.effort, crew.description, ...(Array.isArray(crew.tags) ? crew.tags : [crew.tags])]
    .filter((value) => value != null)
    .join(" ")
    .toLowerCase();
  return terms.every((term) => haystack.includes(term));
}

/// The Crews table and the filter that goes in the page bar.
function crewsPanel(payload) {
  const crews = payload.crews || [];
  const { section: panel, body } = settingsSection("Crews", { count: "" });
  panel.classList.add("config-crews");
  const count = panel.querySelector(".settings-section-count");
  const list = el("div", { class: "config-crew-list" });
  const more = el("div", { class: "config-crew-more" });
  const status = el("span", { class: "config-crew-status" });
  status.setAttribute("role", "status");
  const refresh = (revealFrom) => {
    const matches = crews.filter((crew) => crewMatchesQuery(crew, crewQuery));
    // A crew being edited stays on screen even when the filter or window hides it.
    const editingName = editing && editing.kind === "crew" ? editing.name : null;
    const shown = matches.slice(0, crewLimit);
    if (editingName && crews.some((crew) => crew.name === editingName) && !shown.some((crew) => crew.name === editingName)) {
      shown.push(crews.find((crew) => crew.name === editingName));
    }
    list.replaceChildren(...shown.map((crew) => crewRow(crew, payload)));
    if (crewQuery && !matches.length) {
      list.appendChild(el("div", { class: "settings-empty", text: `No crew matches “${crewQuery}”.` }));
    }
    const total = `${crews.length} crew${crews.length === 1 ? "" : "s"}`;
    count.textContent = crewQuery ? `${matches.length} of ${total}` : total;
    status.textContent = matches.length ? `Showing ${Math.min(crewLimit, matches.length)} of ${matches.length}${crewQuery ? " matching" : ""} crews` : "";
    renderCrewMore(more, status, matches.length, () => refresh(), (from) => refresh(from));
    if (revealFrom != null) list.children[revealFrom]?.focus();
  };
  const lead = crews.length
    ? [settingsSearchField({
        cls: "config-crew-filter",
        placeholder: "Filter by name, provider or model",
        label: "Filter crews by name, provider or model",
        value: crewQuery,
        onInput: (input) => {
          crewQuery = input.value.trim();
          crewLimit = CREW_PAGE;
          refresh();
        },
      })]
    : [];
  const head = el("div", { class: "config-crew-cells config-crew-head col-head" });
  head.setAttribute("role", "row");
  for (const label of CREW_COLUMNS) {
    const cell = el("span", { text: label });
    cell.setAttribute("role", "columnheader");
    head.appendChild(cell);
  }
  body.appendChild(head);
  if (!crews.length && !(editing && editing.kind === "crew" && editing.name === "")) {
    body.appendChild(el("div", { class: "settings-empty", text: "No crew is defined for this workspace." }));
  }
  if (editing && editing.kind === "crew" && editing.name === "") {
    body.appendChild(crewEditor({ name: "", tags: [] }, payload, true));
  }
  body.appendChild(list);
  body.appendChild(more);
  refresh();
  return { panel, lead };
}

/// The reveal controls under the list: the status line, "Show more", "Show all"
/// and, once the window has grown, "Show fewer". Each keeps every crew reachable.
function renderCrewMore(container, status, total, onShrink, onReveal) {
  container.replaceChildren(status);
  const control = (label, onClick) => {
    const button = el("button", { class: "settings-btn", text: label });
    button.type = "button";
    button.addEventListener("click", onClick);
    container.appendChild(button);
    return button;
  };
  if (total > crewLimit) {
    const remaining = total - crewLimit;
    control(`Show ${Math.min(CREW_PAGE, remaining)} more`, () => {
      const from = crewLimit;
      crewLimit += CREW_PAGE;
      onReveal(from);
    });
    if (remaining > CREW_PAGE) {
      control(`Show all ${total}`, () => {
        const from = crewLimit;
        crewLimit = total;
        onReveal(from);
      });
    }
  }
  if (crewLimit > CREW_PAGE && total > CREW_PAGE) {
    const fewer = control("Show fewer", () => {
      crewLimit = CREW_PAGE;
      onShrink();
      container.querySelector("button")?.focus();
    });
    fewer.classList.add("config-crew-fewer");
  }
}

function addCrewButton() {
  const button = el("button", { class: "settings-btn config-add-crew", text: "Add crew" });
  button.type = "button";
  button.addEventListener("click", () => startEdit({ kind: "crew", name: "" }));
  return button;
}

function crewRow(crew, payload) {
  const referenced = crew.referenced_by || [];
  const node = el("div", { class: "config-row config-crew-row" });
  node.dataset.key = `crews.${crew.name}`;
  node.tabIndex = -1;
  if (referenced.length) node.classList.add("referenced");
  // A table without `enabled` is enabled; only an explicit false disables.
  const disabled = crew.enabled === false;
  if (disabled) node.classList.add("disabled");
  if (editing && editing.kind === "crew" && editing.name === crew.name) {
    node.classList.add("editing");
    node.appendChild(crewEditor(crew, payload, false));
    return node;
  }
  const identity = el("span", { class: "config-crew-identity" }, [
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
    crew.description ? el("span", { class: "config-crew-description", text: crew.description }) : null,
    crewEnvLine(crew.env),
  ]);
  const usedBy = el("span", { class: "config-crew-usage" }, referenced.length
    ? referenced.map((reference) => {
        const isKey = reference.includes(".");
        const page = reference.includes("review") ? "review" : "workflow";
        const node = isKey ? el("a", { class: "config-crew-use", text: reference }) : el("span", { class: "config-crew-use", text: reference });
        if (isKey) node.href = `#config/${page}`;
        return node;
      })
    : [el("span", { class: "config-crew-use", text: EMPTY_ARRAY })]);
  const cells = el("div", { class: "config-crew-cells" }, [
    identity,
    providerCell(crew.provider),
    el("span", { class: "config-value mono", text: displayCrewValue(crew.model) }),
    crewLimitCell(crew.limit),
    el("span", { class: "config-value mono", text: displayCrewValue(crew.effort) }),
    el("span", { class: "config-value mono", text: displayCrewValue(crew.tags) }),
    crew.source ? settingsSource(crew.source, { set: settingsSourceIsSet(crew.source) }) : el("span", { class: "settings-value none", text: EMPTY_ARRAY }),
    usedBy,
    editable(payload) ? settingsEditButton(() => startEdit({ kind: "crew", name: crew.name }), `Edit crew ${crew.name}`) : null,
  ].map((value, index) => el("div", { class: `config-crew-cell${CREW_DETAIL_COLUMNS.has(index) ? " config-crew-detail" : ""}${value ? "" : " config-crew-blank"}` }, [
    el("span", { class: "config-crew-label", text: CREW_COLUMNS[index] }),
    value,
  ])));
  node.appendChild(cells);
  node.appendChild(crewDetailToggle(node, crew.name));
  const error = pendingError(node, `crews.${crew.name}`);
  if (error) node.appendChild(error);
  return node;
}

function crewDetailToggle(node, name) {
  const open = crewExpanded.has(name);
  node.classList.toggle("expanded", open);
  const button = el("button", { class: "config-crew-toggle", text: open ? "Hide details" : "Details" });
  button.type = "button";
  button.setAttribute("aria-expanded", String(open));
  button.setAttribute("aria-label", `${open ? "Hide" : "Show"} details for crew ${name}`);
  button.addEventListener("click", () => {
    const next = !crewExpanded.has(name);
    if (next) crewExpanded.add(name);
    else crewExpanded.delete(name);
    node.classList.toggle("expanded", next);
    button.textContent = next ? "Hide details" : "Details";
    button.setAttribute("aria-expanded", String(next));
    button.setAttribute("aria-label", `${next ? "Hide" : "Show"} details for crew ${name}`);
  });
  return button;
}

// A crew's `env` entries (`NAME` or `NAME=SOURCE`): the variables its agents
// receive on top of `execution.env.pass`. The server sends names only.
function crewEnvLine(env) {
  if (!Array.isArray(env) || env.length === 0) return null;
  return el("span", {
    class: "config-crew-env mono",
    text: `env ${env.join(", ")}`,
    title: "Variables this crew's agents receive on top of execution.env.pass (NAME, or NAME=SOURCE to read another variable). Set only in the global config.",
  });
}

function displayCrewValue(value) {
  return value == null || value === "" ? EMPTY_ARRAY : displayValue(value);
}

// [ORB-14698] The crew's tightest live usage window on this host: how much of
// it is used, when it resets, and a badge while admission skips the crew.
// The server's provider-limit view decides all of it; nothing is judged here.
function crewLimitCell(limit) {
  const cell = el("span", { class: "config-crew-limit" });
  if (!limit) {
    cell.appendChild(el("span", { class: "config-value mono", text: EMPTY_ARRAY, title: "No live usage reading covers this crew" }));
    return cell;
  }
  const used = limit.used_percent == null
    ? (limit.exhausted ? "exhausted" : "–")
    : `${Math.round(Number(limit.used_percent))}%`;
  const windowLabel = limit.window || "usage window";
  cell.appendChild(el("span", { class: "config-crew-limit-line" }, [
    el("span", { class: "config-value mono", text: used }),
    el("span", { class: "config-crew-limit-window mono", text: limit.scope ? `${windowLabel} · ${limit.scope}` : windowLabel }),
    limit.gated
      ? el("span", {
          class: "config-crew-gated",
          text: "gated",
          title: `Admission skips this crew until ${formatDateTime(limit.until)} (limit ${limit.threshold}%)`,
        })
      : limit.threshold === "off"
        ? el("span", {
            class: "config-crew-limit-window",
            text: "gating off",
            title: "This provider's usage gate is off. Readings stay visible. A usage-limit failure still holds the task.",
          })
        : null,
  ]));
  const resets = limit.resets_at || limit.until;
  cell.appendChild(el("span", {
    class: "config-crew-limit-reset",
    text: `${limit.resets_at ? "resets" : "counts until"} ${formatDateTime(resets)}`,
  }));
  return cell;
}

function providerCell(provider) {
  const cell = el("span", { class: "config-provider mono" });
  const dot = el("span", { class: "config-provider-dot" });
  // The family palette is the dashboard's existing one; an unknown provider
  // keeps the neutral accent rather than inventing a colour.
  dot.style.background = `var(--ag-${String(provider || "").toLowerCase()}, var(--accent))`;
  cell.appendChild(dot);
  cell.appendChild(el("span", { text: displayCrewValue(provider) }));
  return cell;
}

function crewEditor(crew, payload, isNew) {
  const draft = editing.draft;
  const fields = {};
  const rows = [];
  if (isNew) {
    const name = el("input", { class: "settings-input mono" });
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
      const toggle = el("input", { class: "settings-input" });
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
    if (field === "env") {
      // Global config only: `[crews.X].env` is refused as a workspace setting,
      // so an editor writing the workspace file shows it read-only.
      const was = Array.isArray(current) ? current : [];
      if (writeScope() !== "global") {
        rows.push(fieldRow(field, el("span", {
          class: "config-note mono",
          text: `${was.length ? was.join(", ") : EMPTY_ARRAY} · set in the global config`,
        })));
        continue;
      }
      // An unchanged list is omitted so saving another field never rewrites it.
      const list = chipListInput(was, draft, field);
      rows.push(fieldRow(field, list.node));
      fields[field] = () => {
        const next = list.read();
        if (next.length === was.length && next.every((entry, index) => entry === was[index])) return undefined;
        return next.length ? next : null;
      };
      continue;
    }
    const input = el("input", { class: "settings-input mono" });
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
  await submitConfigMutation((visit) =>
    requestJson(`/api/config/crews/${encodeURIComponent(name)}`, "PUT", {
      fields: body,
      scope: writeScope(),
      ...(init ? { init } : {}),
    }, visit),
  // Only a landed write earns the typed name, so a refused save keeps the
  // new-crew editor (keyed by "") open and focus finds the row on success.
  isNew ? (session) => { session.name = name; } : undefined);
}

async function submitCrewDelete(crew) {
  await submitConfigMutation((visit) =>
    requestJson(
      `/api/config/crews/${encodeURIComponent(crew.name)}?scope=${encodeURIComponent(writeScope())}`,
      "DELETE",
      undefined,
      visit,
    ),
  );
}

// --------------------------------------------------------------- key table
