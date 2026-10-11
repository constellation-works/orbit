// Settings › Workspace file [ORB-15431]: which keys in this workspace's
// `.orbit/config.toml` change what it runs, and which only restate a value
// it would get anyway.
//
// Summary cards count the keys by what removing them would do and filter the
// table; a tidy callout pre-selects the restating keys. The table groups the
// keys the file sets by TOML table, and each row says what the key falls back
// to if removed: the layer, its value, and whether the effective value
// changes. Rows edit inline, a bulk bar removes the checked keys, and an Add
// key row offers the registered keys this file does not set yet.
//
// Every fact is the payload's (`GET /api/config/file?scope=workspace`): the
// fallback a removal reaches and the equality flags come from the server, and
// a row whose fallback the payload omits reads "not reported" and "may
// change". Nothing is inferred from a key name. Save and Remove use the same
// PUT and DELETE key routes as every Settings view, one key per request, so a
// refused removal is reported on its own row; the view then re-reads the file.

import { copyWithFeedback, el, getWorkspace, requestJson } from './common.js';
import { button, cliCommand, display, draftText, draftValue, pill, valueField, valueTokens } from './config-effective.js';

const CATEGORIES = [
  { id: "own", label: "Own value", hint: "Removing it changes what this workspace runs." },
  { id: "global", label: "Restates global", hint: "Same value as the global file." },
  { id: "default", label: "Restates built-in default", short: "Restates default", hint: "Same value Orbit ships with." },
];
const EFFECTS = {
  same: { cls: "same", text: "no change" },
  changes: { cls: "changes", text: "value changes" },
  maybe: { cls: "maybe", text: "may change" },
};

// Operator state that outlives a re-render (a poll, a save): the filter and
// search, the checked rows, the open inline edit or Add key draft, the last
// write's outcome and the refusals still shown on their rows.
let filter = "all";
let query = "";
const checked = new Set();
let draft = null;
let adding = null;
let pending = false;
let status = null;
const refusals = new Map();
let lastRender = null;

/// Forget the selection and drafts; a different workspace has another file.
export function resetWorkspaceFile() {
  filter = "all";
  query = "";
  checked.clear();
  draft = null;
  adding = null;
  pending = false;
  status = null;
  refusals.clear();
}

/// Render the Workspace file view into `body`. `context` is the one Settings
/// passes every view: `canEdit` and its `refusal`, `render` (rebuild from a
/// payload) and `reload` (re-read the payload, then render).
export function renderWorkspaceFile(body, payload, context) {
  lastRender = { body, payload, context };
  const model = buildModel(payload);
  for (const key of [...checked]) if (!model.byKey.has(key)) checked.delete(key);
  for (const key of [...refusals.keys()]) if (!model.byKey.has(key)) refusals.delete(key);
  if (draft && (!model.byKey.has(draft.key) || draft.workspace !== getWorkspace())) draft = null;
  if (adding && adding.workspace !== getWorkspace()) adding = null;
  const canEdit = context.canEdit;

  const root = el("div", { class: "cfx cwf" });
  root.appendChild(header(payload, model, canEdit));
  if (!canEdit) {
    root.appendChild(el("p", { class: "cfx-readonly", text: `Read-only here: ${context.refusal || "this session cannot change configuration"}.` }));
  }
  root.appendChild(summaryCards(model));
  const tidy = tidyCallout(model, canEdit);
  if (tidy) root.appendChild(tidy);
  root.appendChild(el("div", { class: "cwf-main" }, [
    keysPanel(payload, model, canEdit),
    sidebar(payload, model),
  ]));
  body.appendChild(root);
}

export function workspaceFileCount(payload) {
  const n = buildModel(payload).rows.length;
  return `${n} ${n === 1 ? "key" : "keys"} in file`;
}

function rerender() {
  if (lastRender) lastRender.context.render(lastRender.payload);
}

// ------------------------------------------------------------------ model

/// One key this file sets, with what removing it would do.
function describe(row) {
  const fallback = row.fallback && typeof row.fallback === "object" ? row.fallback : null;
  const equals = row.equals && typeof row.equals === "object" ? row.equals : {};
  let category = "own";
  let effect = "maybe";
  if (fallback?.layer === "global") {
    category = equals.global === true ? "global" : "own";
    effect = category === "global" ? "same" : "changes";
  } else if (fallback?.layer === "built-in") {
    category = fallback.state === "default" && equals.built_in === true ? "default" : "own";
    effect = category === "default" ? "same" : "changes";
  } else if (fallback) {
    effect = JSON.stringify(fallback.value) === JSON.stringify(row.value) ? "same" : "changes";
  }
  // The global file sets the key (its equality flag is reported), yet the
  // removal reaches the built-in value: the global value is not inherited.
  const notInherited = fallback?.layer === "built-in" && typeof equals.global === "boolean";
  const table = row.key.includes(".") ? row.key.slice(0, row.key.lastIndexOf(".")) : row.key;
  return {
    row, fallback, category, effect, notInherited, table,
    name: row.key.slice(table.length + 1) || row.key,
    note: note(row, fallback, equals, notInherited),
  };
}

function note(row, fallback, equals, notInherited) {
  if (!fallback) return "The fallback is not reported: the global and workspace files did not load together.";
  if (notInherited) {
    return `The global file sets ${equals.global ? "the same value" : "a different value"}, but it is not inherited while this file exists, so removing the key falls to the built-in default.`;
  }
  if (fallback.layer === "built-in" && (fallback.state === "unset" || fallback.value == null)) {
    return `Orbit ships no default for this key, so removing it leaves it unset.${row.description ? ` ${row.description}` : ""}`;
  }
  if (fallback.layer === "environment") return "Removing it hands the key to the environment value shown.";
  return "";
}

function buildModel(payload) {
  const rows = [];
  const byKey = new Map();
  const tables = new Map();
  const candidates = [];
  const unsetSections = [];
  let total = 0;
  for (const section of payload.sections || []) {
    if (section.kind === "crews") continue;
    const keys = section.keys || [];
    total += keys.length;
    const set = keys.filter((row) => row.state === "set");
    if (keys.length && !set.length) unsetSections.push(section);
    for (const row of keys) {
      if (row.state === "set") continue;
      if (row.settable !== false && row.global_only !== true) candidates.push({ section, row });
    }
    for (const row of set) {
      const info = describe(row);
      rows.push(info);
      byKey.set(row.key, info);
      if (!tables.has(info.table)) tables.set(info.table, []);
      tables.get(info.table).push(info);
    }
  }
  const counts = { all: rows.length };
  for (const { id } of CATEGORIES) counts[id] = rows.filter((info) => info.category === id).length;
  const redundant = rows.filter((info) => info.category !== "own").map((info) => info.row.key);
  return { rows, byKey, tables, counts, redundant, candidates, unsetSections, total };
}

function visible(info) {
  if (filter !== "all" && info.category !== filter) return false;
  if (!query) return true;
  return `${info.row.key} ${display(info.row.value)}`.toLowerCase().includes(query);
}

function setFilter(next) {
  filter = filter === next ? "all" : next;
  rerender();
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

// ----------------------------------------------------------------- header

function header(payload, model, canEdit) {
  const file = payload.layers?.workspace || {};
  const path = file.path || "";
  const copy = el("button", { class: "cfx-btn quiet cwf-copy", type: "button", text: "Copy path", "aria-label": "Copy the file path" });
  copy.addEventListener("click", () => copyWithFeedback(copy, path));
  const facts = file.exists === false
    ? "Per-user and git-ignored · not created yet"
    : `Per-user and git-ignored · ${plural(model.rows.length, "key")} · ${plural(model.tables.size, "TOML table")}`;
  const actions = el("div", { class: "cwf-head-actions" }, [
    button("Reload", () => {
      draft = null;
      adding = null;
      lastRender?.context.reload();
    }),
  ]);
  if (canEdit) {
    const add = button("+ Add key", () => startAdd(model), "cfx-btn primary");
    add.disabled = pending || !model.candidates.length;
    if (!model.candidates.length) add.title = "Every registered key the workspace file may carry is already set";
    actions.appendChild(add);
  }
  const node = el("div", { class: "cwf-head" }, [
    el("div", { class: "cwf-head-text" }, [
      el("span", { class: "cfx-section-note", text: `Settings · ${getWorkspace() || "workspace"}` }),
      el("h2", { class: "cwf-title" }, ["Workspace file ", el("span", { class: "cfx-pill workspace", text: "workspace layer · wins over global" })]),
      el("div", { class: "cwf-path" }, [
        el("code", { class: "mono", text: path || "path not reported", title: path }),
        path ? copy : null,
      ]),
      el("span", { class: "cfx-section-note", text: facts }),
    ]),
    actions,
  ]);
  node.dataset.key = "cwf-head";
  return node;
}

// ----------------------------------------------------------- summary cards

function summaryCards(model) {
  const row = el("div", { class: "cwf-cards" });
  for (const category of CATEGORIES) {
    const on = filter === category.id;
    const card = el("button", { class: `cwf-card ${category.id}`, type: "button", "aria-pressed": String(on) }, [
      el("span", { class: "cwf-card-label" }, [el("span", { class: "cfx-dot", "aria-hidden": "true" }), category.label]),
      el("span", { class: "cwf-card-n mono", text: String(model.counts[category.id]) }),
      el("span", { class: "cwf-card-hint", text: category.hint }),
    ]);
    card.dataset.category = category.id;
    card.title = on ? "Show every key in this file again" : `Show only the keys that ${category.label.toLowerCase()}`;
    card.addEventListener("click", () => setFilter(category.id));
    row.appendChild(card);
  }
  const node = el("section", { class: "cwf-summary", "aria-label": "What each key in this file does" }, [
    el("h2", { class: "cfx-eyebrow", text: "What each key in this file does for this workspace" }),
    row,
  ]);
  node.dataset.key = "cwf-summary";
  return node;
}

function tidyCallout(model, canEdit) {
  const n = model.redundant.length;
  if (!n) return null;
  const body = [
    el("strong", { text: `${plural(n, "key restates", "keys restate")} a value the workspace would get anyway` }),
    el("span", { text: "Removing them changes nothing today. Keep a key only to pin it against a later global edit." }),
  ];
  const node = el("div", { class: "cfx-callout cwf-tidy" }, [
    el("span", { class: "cfx-dot", "aria-hidden": "true" }),
    el("div", { class: "cfx-callout-body" }, body),
  ]);
  if (canEdit) {
    const select = button(`Select the ${n}`, () => {
      checked.clear();
      for (const key of model.redundant) checked.add(key);
      filter = "all";
      status = null;
      rerender();
    });
    select.disabled = pending;
    node.appendChild(select);
  }
  node.dataset.key = "cwf-tidy";
  node.dataset.callout = "tidy";
  return node;
}

// -------------------------------------------------------------- key table

function keysPanel(payload, model, canEdit) {
  const panel = el("section", { class: "cfx-card cwf-keys", "aria-labelledby": "cwf-keys-title" });
  const title = el("h2", { class: "cfx-h2", text: "Keys in this file" });
  title.id = "cwf-keys-title";
  const search = el("input", { class: "cfx-input cfx-search", type: "search" });
  search.id = "cwf-search";
  search.placeholder = "Filter keys and values";
  search.value = query;
  search.autocomplete = "off";
  const label = el("label", { class: "visually-hidden", text: "Filter keys and values in this file" });
  label.htmlFor = "cwf-search";
  const groups = el("div", { class: "cwf-groups" });
  search.addEventListener("input", () => {
    query = search.value.trim().toLowerCase();
    fillGroups(groups, payload, model, canEdit);
  });
  const toolbar = el("div", { class: "cfx-toolbar" }, [title, el("div", { class: "cfx-search-wrap" }, [label, search])]);
  toolbar.dataset.key = "cwf-toolbar";
  panel.appendChild(toolbar);

  if (canEdit && checked.size) panel.appendChild(bulkBar(model));
  if (canEdit && adding) panel.appendChild(addForm(payload, model));

  const segments = el("div", { class: "cfx-segs", role: "group", "aria-label": "Which keys" });
  for (const { id, label: text, short } of [{ id: "all", label: "All" }, ...CATEGORIES]) {
    const seg = el("button", { class: "cfx-seg", type: "button", "aria-pressed": String(filter === id) }, [
      short || text, el("span", { class: "cfx-seg-n mono", text: String(model.counts[id]) }),
    ]);
    seg.dataset.filter = id;
    seg.addEventListener("click", () => {
      filter = id;
      rerender();
    });
    segments.appendChild(seg);
  }
  const segRow = el("div", { class: "cwf-segrow" }, [segments]);
  segRow.dataset.key = "cwf-segments";
  panel.appendChild(segRow);
  panel.appendChild(el("div", { class: "cwf-colhead", "aria-hidden": "true" }, [
    el("span"),
    el("span", { text: "Key" }),
    el("span", { text: "In this file" }),
    el("span", { text: "If removed" }),
    el("span"),
  ]));
  fillGroups(groups, payload, model, canEdit);
  panel.appendChild(groups);
  panel.appendChild(statusNode());
  return panel;
}

/// The table groups under the toolbar; the search box refills only these, so
/// the field being typed in is never rebuilt.
function fillGroups(container, payload, model, canEdit) {
  container.replaceChildren();
  let shown = 0;
  for (const [table, infos] of model.tables) {
    const rows = infos.filter(visible);
    if (!rows.length) continue;
    shown += rows.length;
    const group = el("div", { class: "cwf-group" }, [
      el("div", { class: "cwf-group-head" }, [
        el("h3", { class: "cwf-table mono", text: `[${table}]` }),
        el("span", { class: "cfx-section-note", text: plural(rows.length, "key") }),
      ]),
    ]);
    group.dataset.table = table;
    for (const info of rows) group.appendChild(keyRow(payload, info, canEdit));
    container.appendChild(group);
  }
  if (!shown) {
    const empty = model.rows.length
      ? "No key in this file matches."
      : payload.workspace_file_exists === false
        ? "There is no workspace config.toml yet; Add key creates it."
        : "This file sets no key; every value comes from the global file or the built-in defaults.";
    container.appendChild(el("p", { class: "cfx-none", text: empty }));
  }
}

function keyRow(payload, info, canEdit) {
  const { row } = info;
  const editable = canEdit && row.settable !== false;
  const on = checked.has(row.key);
  const node = el("div", { class: `cwf-row${on ? " checked" : ""}${draft?.key === row.key ? " editing" : ""}` });
  node.dataset.key = row.key;
  node.dataset.category = info.category;

  const box = el("input", { type: "checkbox" });
  box.checked = on;
  box.disabled = !editable || pending;
  box.addEventListener("change", () => {
    if (box.checked) checked.add(row.key);
    else checked.delete(row.key);
    status = null;
    rerender();
  });
  node.appendChild(el("label", { class: "cwf-check" }, [box, el("span", { class: "visually-hidden", text: `Select ${row.key}` })]));
  node.appendChild(el("span", { class: "cfx-key mono cwf-name", text: info.name, title: row.key }));
  node.appendChild(el("span", { class: "cwf-value" }, [valueTokens(row.value)]));
  node.appendChild(fallbackCell(info));

  const actions = el("span", { class: "cwf-actions" });
  if (editable) {
    const edit = button(draft?.key === row.key ? "Close" : "Edit", () => toggleEdit(info), "cfx-btn quiet");
    edit.setAttribute("aria-expanded", String(draft?.key === row.key));
    edit.disabled = pending;
    const remove = button("Remove", () => removeKeys([row.key]), "cfx-btn quiet");
    remove.disabled = pending;
    remove.setAttribute("aria-label", `Remove ${row.key} from the file`);
    actions.append(edit, remove);
  } else if (row.settable === false) {
    actions.appendChild(el("span", { class: "cfx-status ro", text: "read-only", title: row.read_only_reason || "orbit config set refuses this key" }));
  }
  node.appendChild(actions);
  if (info.note) node.appendChild(el("span", { class: "cwf-note", text: info.note }));
  const refusal = refusals.get(row.key);
  if (refusal) node.appendChild(el("p", { class: "cfx-notice error cwf-row-error", role: "alert", text: `Not removed: ${refusal}` }));
  if (editable && draft?.key === row.key) node.appendChild(editForm(payload, info));
  return node;
}

function fallbackCell(info) {
  const { fallback } = info;
  const effect = EFFECTS[info.effect];
  let value = "not reported";
  if (fallback) value = fallback.layer === "built-in" && (fallback.state === "unset" || fallback.value == null) ? "unset" : display(fallback.value);
  const cell = el("span", { class: "cwf-fallback" }, [
    fallback ? pill(fallback.layer) : el("span", { class: "cfx-pill unknown", text: "not reported" }),
    fallback ? el("span", { class: "cwf-fallback-value mono", text: value }) : null,
    el("span", { class: `cwf-effect ${effect.cls}`, text: `· ${effect.text}` }),
  ]);
  cell.dataset.layer = fallback?.layer || "not-reported";
  return cell;
}

// ---------------------------------------------------------------- bulk bar

function bulkBar(model) {
  const keys = [...checked].filter((key) => model.byKey.has(key));
  const clear = button("Clear", () => {
    checked.clear();
    rerender();
  }, "cfx-btn quiet");
  const remove = button(pending ? "Removing…" : "Remove from file", () => removeKeys(keys), "cfx-btn danger");
  clear.disabled = pending;
  remove.disabled = pending;
  const node = el("div", { class: "cwf-bulk", role: "region", "aria-label": "Bulk actions" }, [
    el("strong", { text: `${keys.length} selected` }),
    el("span", { class: "cfx-hint", text: "The workspace falls back to the value in the If removed column." }),
    el("span", { class: "cwf-bulk-actions" }, [clear, remove]),
  ]);
  node.dataset.key = "cwf-bulk";
  return node;
}

/// Remove `keys` from the workspace file, one DELETE each and in order, so a
/// refused key leaves the others removed and its reason lands on its row.
async function removeKeys(keys) {
  if (pending || !keys.length || !lastRender) return;
  const { context, payload } = lastRender;
  const before = buildModel(payload).byKey;
  const workspace = getWorkspace();
  pending = true;
  status = null;
  for (const key of keys) refusals.delete(key);
  rerender();
  const outcomes = [];
  for (const key of keys) {
    try {
      await requestJson(`/api/config/keys/${encodeURIComponent(key)}?scope=workspace`, "DELETE");
      outcomes.push({ key, ok: true });
    } catch (error) {
      outcomes.push({ key, ok: false, text: error.message || String(error) });
    }
    if (workspace !== getWorkspace()) return;
  }
  pending = false;
  for (const outcome of outcomes) {
    if (outcome.ok) {
      checked.delete(outcome.key);
      if (draft?.key === outcome.key) draft = null;
    } else {
      refusals.set(outcome.key, outcome.text);
    }
  }
  const removed = outcomes.filter((outcome) => outcome.ok);
  const refused = outcomes.length - removed.length;
  const parts = [];
  if (removed.length) parts.push(`Removed ${plural(removed.length, "key")} from the workspace file.`);
  if (refused) parts.push(`${plural(refused, "key was", "keys were")} not removed; the reason is on ${refused === 1 ? "its row" : "each row"}.`);
  status = {
    kind: refused ? "error" : "ok",
    text: parts.join(" "),
    items: outcomes.map((outcome) => ({
      key: outcome.key,
      ok: outcome.ok,
      text: outcome.ok ? `now ${fallbackSummary(before.get(outcome.key))}` : `refused: ${outcome.text}`,
    })),
  };
  await context.reload();
}

function fallbackSummary(info) {
  const fallback = info?.fallback;
  if (!fallback) return "the value of a lower layer (not reported)";
  if (fallback.layer === "built-in") {
    return fallback.state === "unset" || fallback.value == null ? "unset (no built-in default)" : `${display(fallback.value)} from the built-in default`;
  }
  return `${display(fallback.value)} from the ${fallback.layer} ${fallback.layer === "global" ? "file" : "layer"}`;
}

function statusNode() {
  const node = el("div", { class: `cwf-status${status ? ` ${status.kind}` : ""}`, role: "status" });
  node.dataset.key = "cwf-status";
  if (!status) return node;
  node.appendChild(el("p", { class: `cfx-notice ${status.kind}`, text: status.text }));
  if (status.items?.length) {
    const list = el("ul", { class: "cwf-outcomes" });
    for (const item of status.items) {
      const entry = el("li", { class: item.ok ? "ok" : "error" }, [
        el("span", { class: "mono", text: item.key }),
        el("span", { text: ` ${item.text}` }),
      ]);
      entry.dataset.outcome = item.key;
      list.appendChild(entry);
    }
    node.appendChild(list);
  }
  return node;
}

// ------------------------------------------------------------ inline edit

function toggleEdit(info) {
  if (pending) return;
  if (draft?.key === info.row.key) {
    draft = null;
    rerender();
    return;
  }
  draft = { key: info.row.key, workspace: getWorkspace(), text: draftText(info.row), scope: "workspace", error: null };
  adding = null;
  status = null;
  rerender();
  document.getElementById("cwf-edit-value")?.focus();
}

function editForm(payload, info) {
  const { row } = info;
  const form = el("form", { class: "cwf-editor" });
  form.noValidate = true;
  const field = valueField(row, draft, { id: "cwf-edit-value", disabled: pending });
  const label = el("label", { class: "cfx-label", text: `${field.label} for ${row.key}` });
  label.htmlFor = "cwf-edit-value";
  const cli = el("code", { class: "cfx-cli mono cwf-cli", text: cliCommand(row, draft) });
  field.onInput(() => {
    cli.textContent = cliCommand(row, draft);
  });
  const save = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Saving…" : "Save" });
  save.disabled = pending;
  const cancel = button("Cancel", () => {
    draft = null;
    rerender();
  }, "cfx-btn quiet");
  cancel.disabled = pending;
  form.append(
    el("div", { class: "cwf-field" }, [label, field.node, field.hint ? el("span", { class: "cfx-hint", text: field.hint }) : null]),
    el("div", { class: "cfx-actions" }, [cancel, save]),
    el("div", { class: "cfx-cli-wrap cwf-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]),
  );
  if (draft.error) form.appendChild(el("p", { class: "cfx-notice error", role: "alert", text: draft.error }));
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveKey(row, draft);
  });
  for (const node of initChoices(payload, draft, (init) => saveKey(row, draft, init))) form.appendChild(node);
  return cancelOnEscape(form, () => {
    draft = null;
  });
}

function cancelOnEscape(form, close) {
  form.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || pending) return;
    event.stopPropagation();
    close();
    rerender();
  });
  return form;
}

/// The fail-closed first write: with no workspace file yet, a refused write
/// offers the same two choices `orbit config set` does.
function initChoices(payload, session, retry) {
  if (!session.error || payload.workspace_file_exists !== false) return [];
  return [el("div", { class: "cfx-actions" }, [
    el("span", { class: "cfx-hint", text: "Create the workspace file:" }),
    button("Copy global policy", () => retry("seed-from-global")),
    button("Start empty", () => retry("fresh")),
  ])];
}

/// One PUT to the workspace file, then a re-read. The outcome lands only on
/// the draft that issued it, and only in the workspace it was opened in.
async function saveKey(row, session, init) {
  if (pending || !lastRender) return;
  const parsed = draftValue(row, session.text);
  if (parsed.error) {
    session.error = parsed.error;
    rerender();
    return;
  }
  const { context } = lastRender;
  pending = true;
  session.error = null;
  rerender();
  try {
    await requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, "PUT", {
      value: parsed.value,
      scope: "workspace",
      ...(init ? { init } : {}),
    });
    pending = false;
    if (session.workspace !== getWorkspace()) return;
    if (draft === session) draft = null;
    if (adding === session) adding = null;
    refusals.delete(row.key);
    status = { kind: "ok", text: `Saved ${row.key} to the workspace file.` };
    await context.reload();
  } catch (error) {
    pending = false;
    if (session.workspace !== getWorkspace()) return;
    session.error = error.message || String(error);
    rerender();
    // The Save button was disabled while the write was in flight, which drops
    // focus; the refused value is what the operator corrects next.
    document.getElementById(session === adding ? "cwf-add-value" : "cwf-edit-value")?.focus();
  }
}

// ---------------------------------------------------------------- add key

function startAdd(model) {
  if (pending || !model.candidates.length) return;
  const first = model.candidates[0].row;
  adding = { key: first.key, workspace: getWorkspace(), text: draftText(first), scope: "workspace", error: null };
  draft = null;
  status = null;
  rerender();
  document.getElementById("cwf-add-key")?.focus();
}

function addForm(payload, model) {
  const candidate = model.candidates.find((entry) => entry.row.key === adding.key) || model.candidates[0];
  if (!candidate) return el("p", { class: "cfx-muted", text: "Every registered key the workspace file may carry is already set." });
  const { row } = candidate;
  adding.key = row.key;
  const form = el("form", { class: "cwf-add" });
  form.noValidate = true;
  form.dataset.key = "cwf-add";

  const picker = el("select", { class: "cfx-input mono" });
  picker.id = "cwf-add-key";
  picker.disabled = pending;
  let group = null;
  for (const entry of model.candidates) {
    if (group?.dataset.section !== entry.section.token) {
      group = el("optgroup");
      group.label = entry.section.title || entry.section.token || "Settings";
      group.dataset.section = entry.section.token || "";
      picker.appendChild(group);
    }
    const option = el("option", { text: entry.row.key });
    option.value = entry.row.key;
    group.appendChild(option);
  }
  picker.value = row.key;
  picker.addEventListener("change", () => {
    const next = model.candidates.find((entry) => entry.row.key === picker.value);
    if (!next) return;
    adding = { ...adding, key: next.row.key, text: draftText(next.row), error: null };
    rerender();
    document.getElementById("cwf-add-key")?.focus();
  });
  const keyLabel = el("label", { class: "cfx-label", text: "Key" });
  keyLabel.htmlFor = "cwf-add-key";

  const field = valueField(row, adding, { id: "cwf-add-value", disabled: pending });
  const valueLabel = el("label", { class: "cfx-label", text: field.label });
  valueLabel.htmlFor = "cwf-add-value";
  const cli = el("code", { class: "cfx-cli mono cwf-cli", text: cliCommand(row, adding) });
  field.onInput(() => {
    cli.textContent = cliCommand(row, adding);
  });
  const submit = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Adding…" : "Add to file" });
  submit.disabled = pending;
  const cancel = button("Cancel", () => {
    adding = null;
    rerender();
  }, "cfx-btn quiet");
  cancel.disabled = pending;
  form.append(
    el("div", { class: "cwf-field" }, [keyLabel, picker, row.description ? el("span", { class: "cfx-hint", text: row.description }) : null]),
    el("div", { class: "cwf-field" }, [valueLabel, field.node, field.hint ? el("span", { class: "cfx-hint", text: field.hint }) : null]),
    el("div", { class: "cfx-actions" }, [cancel, submit]),
    el("div", { class: "cfx-cli-wrap cwf-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]),
  );
  if (adding.error) form.appendChild(el("p", { class: "cfx-notice error", role: "alert", text: adding.error }));
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveKey(row, adding);
  });
  for (const node of initChoices(payload, adding, (init) => saveKey(row, adding, init))) form.appendChild(node);
  return cancelOnEscape(form, () => {
    adding = null;
  });
}

// ---------------------------------------------------------------- sidebar

function sidebar(payload, model) {
  const fact = (term, detail, cls = "") => el("div", { class: `cwf-fact ${cls}`.trim() }, [el("dt", { text: term }), el("dd", {}, detail)]);
  const about = el("dl", { class: "cwf-facts" }, [
    fact("Scope", ["This workspace only, for this user. Each clone keeps its own copy; git ignores it."]),
    fact("Written by", ["Settings edits aimed at the workspace file, and ", el("code", { class: "mono", text: "orbit config set" }), " run in the workspace."]),
    fact("Precedence", ["Beats the global file and the built-in defaults for every key it sets."]),
  ]);
  if (payload.layers?.execution_not_inherited) {
    about.appendChild(fact("execution.*", ["While this file exists, the workspace takes no execution key from the global file."], "warn"));
  }
  const crewTables = Array.isArray(payload.crew_tables) ? payload.crew_tables : null;
  if (crewTables?.length) {
    const link = el("a", { text: "Crews" });
    link.href = "#config/crews";
    about.appendChild(fact("Crews", [`Defines ${plural(crewTables.length, "crew table")}: ${crewTables.join(", ")}. Edit them under `, link, "."]));
  }

  const notSet = el("ul", { class: "cwf-notset" });
  for (const section of model.unsetSections) {
    const keys = section.keys || [];
    const globalOnly = keys.every((row) => row.global_only === true);
    const item = el("li", {}, [
      el("span", { text: section.title || section.token }),
      el("span", { class: "cfx-section-note", text: globalOnly ? "global file only" : `0 of ${plural(keys.length, "key")}` }),
    ]);
    item.dataset.section = section.token || "";
    notSet.appendChild(item);
  }
  if (crewTables && !crewTables.length) {
    const item = el("li", {}, [el("span", { text: "Crews" }), el("span", { class: "cfx-section-note", text: "none defined here" })]);
    item.dataset.section = "crews";
    notSet.appendChild(item);
  }
  if (!notSet.children.length) notSet.appendChild(el("li", { class: "cfx-muted", text: "Every section has a key set here." }));
  const effective = el("a", { class: "cwf-effective-link", text: `See all ${model.total} keys in Effective →` });
  effective.href = "#config/effective";

  const aboutTitle = el("h2", { class: "cfx-h2", text: "About this file" });
  aboutTitle.id = "cwf-about-title";
  const aboutPanel = el("section", { class: "cfx-card cwf-side", "aria-labelledby": aboutTitle.id }, [aboutTitle, about]);
  const notSetTitle = el("h2", { class: "cfx-h2", text: "Not set in this file" });
  notSetTitle.id = "cwf-notset-title";
  const notSetPanel = el("section", { class: "cfx-card cwf-side", "aria-labelledby": notSetTitle.id }, [notSetTitle, notSet, effective]);
  notSetPanel.dataset.key = "cwf-notset";
  return el("aside", { class: "cwf-aside" }, [aboutPanel, notSetPanel]);
}
