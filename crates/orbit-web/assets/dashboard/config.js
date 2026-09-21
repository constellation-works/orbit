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

import { el, fetchJson, getWorkspace, isAggregateView, onWorkspaceChange, renderPanelPlaceholder, requestJson, requestPanel } from './common.js';

const $ = (id) => document.getElementById(id);

/// Rendered in place of a value that does not exist, matching `config show`.
const NO_VALUE = "–";

/// Which file a sub-view reads and writes. `effective` never writes global:
/// the layered view's edits go to the workspace file, which is the per-user,
/// git-ignored one.
const SUBTAB_SOURCES = {
  effective: { path: "/api/config/effective", scope: "workspace" },
  "workspace-file": { path: "/api/config/file?scope=workspace", scope: "workspace" },
  "global-file": { path: "/api/config/file?scope=global", scope: "global" },
  crews: { path: "/api/config/effective", scope: "workspace" },
  keys: { path: "/api/config/keys", scope: null },
};

let activeSubtab = "effective";
let lastPayload = null;
let filterText = "";
let showAllKeys = false;
// Sections an operator expanded past their collapsed "all unset" summary, and
// the row (or crew) currently being edited. Both are operator state, so a
// background refresh must not discard them.
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
    () => fetchJson(source.path),
    (payload) => {
      lastPayload = payload;
      render(payload);
    },
    "config-count",
  );
}

function render(payload) {
  const body = $("config-body");
  if (!body) return;
  body.replaceChildren();
  renderControls(payload);
  if (activeSubtab === "keys") {
    renderKeyReference(body, payload);
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

  const reload = el("button", { class: "config-action", text: "Reload" });
  reload.type = "button";
  reload.addEventListener("click", () => {
    editing = null;
    fetchAndRenderConfig().catch((error) => console.error("Failed to reload config", error));
  });
  controls.appendChild(reload);

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
  if (editing && editing.kind === "key" && editing.key === row.key) {
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
    editButton(() => startEdit({ kind: "key", key: row.key }), "Edit this key"),
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

function startEdit(next) {
  editing = { ...next, error: null, pending: false };
  if (lastPayload) render(lastPayload);
}

function cancelEdit() {
  editing = null;
  if (lastPayload) render(lastPayload);
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
  const input = valueInput(row);
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
  return editor;
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

/// An editor typed by the registry: a choice list for an enum key, a toggle
/// for a bool, a number field for an integer, a chip list for an array, and a
/// text field otherwise.
function valueInput(row) {
  const options = row.options || [];
  if (options.length) {
    const select = el("select", { class: "config-input mono" });
    for (const option of options) {
      const node = el("option", { text: option });
      node.value = option;
      if (option === row.value) node.selected = true;
      select.appendChild(node);
    }
    return { node: select, read: () => select.value };
  }
  if (row.value_type === "bool") {
    const label = el("label", { class: "config-toggle" });
    const input = el("input", {});
    input.type = "checkbox";
    input.checked = row.value === true;
    label.appendChild(input);
    label.appendChild(el("span", { class: "mono", text: "true / false" }));
    return { node: label, read: () => input.checked };
  }
  if (row.value_type === "integer") {
    const input = el("input", { class: "config-input mono" });
    input.type = "number";
    input.value = row.value == null ? "" : String(row.value);
    return {
      node: input,
      read: () => (input.value.trim() === "" ? null : Number(input.value)),
    };
  }
  if (String(row.value_type || "").startsWith("array")) {
    return chipListInput(Array.isArray(row.value) ? row.value : []);
  }
  const input = el("input", { class: "config-input mono" });
  input.type = "text";
  input.value = row.value == null ? "" : String(row.value);
  return { node: input, read: () => input.value };
}

function chipListInput(initial) {
  const entries = [...initial];
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
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      const value = input.value.trim();
      if (!value) return;
      entries.push(value);
      draw();
      node.querySelector("input")?.focus();
    });
    node.appendChild(input);
  };
  draw();
  return {
    node,
    read: () => {
      const pending = node.querySelector("input")?.value.trim();
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
/// server resolved rather than an optimistic local edit.
async function submit(request) {
  if (!editing) return;
  editing.pending = true;
  editing.error = null;
  if (lastPayload) render(lastPayload);
  try {
    await request();
    editing = null;
    await fetchAndRenderConfig();
  } catch (error) {
    // The admission layer's message is the useful part; it names the key, the
    // value, and what was expected instead.
    editing.pending = false;
    editing.error = error.message || String(error);
    if (lastPayload) render(lastPayload);
  }
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
  if (editing && editing.kind === "crew" && editing.name === crew.name) {
    node.classList.add("editing");
    node.appendChild(crewEditor(crew, payload, false));
    return node;
  }
  const cells = el("div", { class: "config-crew-cells" }, [
    el("span", { class: "config-key mono", text: crew.name }),
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
  const fields = {};
  const rows = [];
  if (isNew) {
    const name = el("input", { class: "config-input mono" });
    name.type = "text";
    name.placeholder = "crew name";
    rows.push(fieldRow("name", name));
    fields.name = () => name.value.trim();
  }
  // The field list is the registry's, so a crew field added or retired in
  // `orbit-config` changes this editor without a JS edit.
  const crewFields = payload.crew_fields || [];
  for (const field of crewFields) {
    const current = crew[field];
    if (field === "tags") {
      const list = chipListInput(Array.isArray(current) ? current : []);
      rows.push(fieldRow(field, list.node));
      fields[field] = list.read;
      continue;
    }
    const input = el("input", { class: "config-input mono" });
    input.type = "text";
    input.value = current == null ? "" : String(current);
    rows.push(fieldRow(field, input));
    fields[field] = () => (input.value.trim() === "" ? null : input.value.trim());
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
    ...initChoices(payload, () => submitCrew(crew, fields, isNew, "seed-from-global")),
  ]);
  return editor;
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
    body[field] = read();
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
