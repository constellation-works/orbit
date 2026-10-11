// Settings › Effective [ORB-15430]: what this workspace runs on, organised
// around where each value comes from.
//
// A layer strip counts what each file supplies, callouts name the workspace
// keys that only restate the global value and the security keys that skip
// it, the review layers read as three steps, and a key table feeds an
// inspector that shows how the selected key resolves and edits it.
//
// Every fact is the payload's: the layer that won, the global value it
// shadows, the built-in default, the fallback a removal reaches and the
// equality flags all come from `/api/config/effective`. A fact the payload
// does not carry reads "not reported"; nothing is inferred from a key name.
// Writes go through the same PUT and DELETE key routes as every other
// Settings view, then the view re-reads the payload, so what it shows after
// a save is the server's answer.
//
// The value helpers and the typed value field are exported for the Workspace
// file view (config-workspace-file.js), which edits keys the same way.

import { el, formatDateTime, getWorkspace, requestJson } from './common.js';

const LAYERS = ["built-in", "environment", "global", "workspace"];
const FILTERS = [
  { id: "set", label: "All set" },
  { id: "workspace", label: "Workspace" },
  { id: "global", label: "Global" },
  { id: "restates", label: "Restates global" },
];

// Operator state that outlives a re-render (a poll, a save): the table's
// filter and search, the selected key, the sections showing their default
// keys, and the open edit with its draft value.
let filter = "set";
let query = "";
let showDescriptions = true;
let selectedKey = null;
const expandedSections = new Set();
let draft = null;
let notice = null;
let pending = false;
let lastRender = null;

/// Forget the selection and draft; a different workspace has other keys.
export function resetEffective() {
  filter = "set";
  query = "";
  selectedKey = null;
  expandedSections.clear();
  draft = null;
  notice = null;
  pending = false;
}

/// Render the Effective view into `body`. `context` carries what the caller
/// owns: whether this session may write (`canEdit`, with `refusal` saying
/// why not), `render`, which rebuilds the tab from a payload, and `reload`,
/// which re-reads the payload and renders it.
export function renderEffective(body, payload, context) {
  lastRender = { body, payload, context };
  const model = buildModel(payload);
  if (selectedKey && !model.byKey.has(selectedKey)) selectedKey = null;
  if (!selectedKey) selectedKey = firstVisible(model)?.row.key || model.rows[0]?.row.key || null;
  if (draft && (draft.key !== selectedKey || draft.workspace !== getWorkspace())) draft = null;

  const root = el("div", { class: "cfx" });
  root.appendChild(layerStrip(payload, model));
  const callouts = calloutRow(payload, model);
  if (callouts) root.appendChild(callouts);
  const review = reviewStrip(payload.review);
  if (review) root.appendChild(review);
  root.appendChild(el("div", { class: "cfx-main" }, [keyTable(model), inspector(payload, model, context)]));
  root.appendChild(crewsSection(payload.crews || []));
  root.appendChild(pathsSection(payload.paths || []));
  body.appendChild(root);
}

function rerender() {
  if (lastRender) lastRender.context.render(lastRender.payload);
}

// ------------------------------------------------------------------ model

/// One table row: the payload row plus what the view reads off it.
function describe(row) {
  const layer = row.state === "set" ? row.source?.layer || "built-in" : "built-in";
  const shadows = Array.isArray(row.shadowed_by) ? row.shadowed_by : [];
  const globalShadow = shadows.find((shadow) => shadow.layer === "global") || null;
  const global = layer === "global" ? { value: row.value } : globalShadow ? { value: globalShadow.value } : null;
  // A global value is not inherited when the layering said so, or when the
  // workspace sets the key and removing it skips the global value.
  const notInherited = globalShadow?.reason === "not-inherited"
    || (layer === "workspace" && global !== null && row.fallback?.layer === "built-in");
  const restates = layer === "workspace" && !notInherited && row.equals?.global === true;
  return { row, layer, global, notInherited, restates, status: status(row, layer, global, notInherited, restates) };
}

function status(row, layer, global, notInherited, restates) {
  if (row.settable === false) {
    return { cls: "ro", text: "read-only", title: row.read_only_reason || "orbit config set refuses this key" };
  }
  if (notInherited) {
    return { cls: "pin", text: "global not inherited", title: "While a workspace file exists this key comes from it or the built-in default; the global value is ignored" };
  }
  if (restates) {
    return { cls: "same", text: "= global", title: "The global file sets the same value; this override pins it against later global edits" };
  }
  if (layer === "workspace" && global) {
    return { cls: "diff", text: `global: ${display(global.value)}`, title: `Overrides the global value ${display(global.value)}` };
  }
  if (layer !== "built-in" && row.equals?.built_in === true && row.built_in?.state === "default") {
    return { cls: "def", text: "= built-in default", title: "Set explicitly to the built-in default" };
  }
  return { cls: "", text: "", title: "" };
}

function buildModel(payload) {
  const groups = [];
  const rows = [];
  const byKey = new Map();
  for (const section of payload.sections || []) {
    if (section.kind === "crews") continue;
    const described = (section.keys || []).map(describe);
    groups.push({ section, rows: described });
    for (const info of described) {
      rows.push(info);
      byKey.set(info.row.key, info);
    }
  }
  const count = (predicate) => rows.filter(predicate).length;
  const counts = {
    set: count((info) => info.row.state === "set"),
    total: rows.length,
    atDefault: count((info) => info.row.state === "default"),
    unset: count((info) => info.row.state === "unset"),
    workspace: count((info) => info.layer === "workspace"),
    global: count((info) => info.layer === "global"),
    globalInFile: count((info) => info.global !== null),
    restates: count((info) => info.restates),
  };
  return { groups, rows, byKey, counts };
}

function passesFilter(info) {
  if (filter === "workspace") return info.layer === "workspace";
  if (filter === "global") return info.layer === "global";
  if (filter === "restates") return info.restates;
  return info.row.state === "set";
}

function matchesQuery(info) {
  if (!query) return true;
  const { row } = info;
  return `${row.key} ${display(row.value)} ${row.description || ""}`.toLowerCase().includes(query);
}

/// The rows a section lists now, and how many default keys it holds back.
function visibleRows(group) {
  const expanded = filter === "set" && expandedSections.has(group.section.token);
  const rows = group.rows.filter((info) => {
    if (!matchesQuery(info)) return false;
    if (passesFilter(info)) return true;
    // A search reaches keys at their default too, so any key can be found
    // and set; so does a section the operator expanded.
    return filter === "set" && info.row.state !== "set" && (query !== "" || expanded);
  });
  const held = filter === "set" && !query ? group.rows.filter((info) => info.row.state !== "set") : [];
  return { rows, held, expanded };
}

function firstVisible(model) {
  for (const group of model.groups) {
    const { rows } = visibleRows(group);
    if (rows.length) return rows[0];
  }
  return null;
}

// --------------------------------------------------------------- values

/// A value as text, the way `orbit config show` prints it.
export function display(value) {
  if (value == null) return "unset";
  if (Array.isArray(value)) return value.length ? value.map(display).join(" ") : "empty";
  if (typeof value === "string") return value === "" ? "empty" : value;
  return String(value);
}

/// A value as tokens: one per array entry, with a pool weight dimmed.
export function valueTokens(value) {
  const node = el("span", { class: "cfx-value" });
  if (value == null || value === "" || (Array.isArray(value) && !value.length)) {
    node.appendChild(el("span", { class: "cfx-empty", text: value == null ? "unset" : "empty" }));
    return node;
  }
  const entries = Array.isArray(value) ? value : [value];
  for (const entry of entries) {
    const text = typeof entry === "string" ? entry : JSON.stringify(entry);
    const weighted = Array.isArray(value) ? /^([^\s:]+):(\d+)$/.exec(text) : null;
    node.appendChild(weighted
      ? el("span", { class: "cfx-tok mono" }, [weighted[1], el("span", { class: "cfx-weight", text: `:${weighted[2]}` })])
      : el("span", { class: "cfx-tok mono", text }));
  }
  return node;
}

export function pill(layer) {
  return el("span", { class: `cfx-pill ${layer}`, text: layer });
}

export function button(label, onClick, cls = "cfx-btn") {
  const node = el("button", { class: cls, type: "button", text: label });
  node.addEventListener("click", onClick);
  return node;
}

function arrow() {
  return el("span", { class: "cfx-arrow", text: "→", "aria-hidden": "true" });
}

function setFilter(next) {
  filter = filter === next && next !== "set" ? "set" : next;
  selectedKey = null;
  rerender();
}

// ---------------------------------------------------------- layer strip

function layerStrip(payload, model) {
  const layers = payload.layers || {};
  const { counts } = model;
  const builtIn = el("div", { class: "cfx-layer static" }, [
    pill("built-in"),
    el("span", { class: "cfx-layer-count" }, [
      el("span", { class: "cfx-layer-number", text: String(counts.atDefault) }),
      " keys at default",
    ]),
    el("span", { class: "cfx-layer-sub mono", text: `compiled into orbit · ${counts.unset} unset` }),
  ]);
  builtIn.dataset.layer = "built-in";
  const row = el("div", { class: "cfx-layer-row" }, [
    builtIn,
    arrow(),
    layerCard("global", layers.global, counts.global, `${counts.globalInFile} in file`),
    arrow(),
    layerCard("workspace", layers.workspace, counts.workspace, `all ${counts.workspace} in file`),
  ]);
  const binding = registryCard(payload.workspace_binding);
  if (binding) row.appendChild(binding);
  return el("section", { class: "cfx-layers", "aria-label": "Where values come from" }, [
    el("h2", { class: "cfx-eyebrow", text: "Where values come from · later layers win" }),
    row,
  ]);
}

function layerCard(layer, file, won, inFile) {
  const on = filter === layer;
  const card = el("button", { class: "cfx-layer", type: "button", "aria-pressed": String(on) }, [
    pill(layer),
    el("span", { class: "cfx-layer-count" }, [
      el("span", { class: "cfx-layer-number", text: String(won) }),
      ` ${won === 1 ? "key wins" : "keys win"} · ${inFile}`,
    ]),
    el("span", {
      class: `cfx-layer-sub mono${file?.exists === false ? " absent" : ""}`,
      text: file?.exists === false ? `${file?.path || "config.toml"} (no file)` : file?.path || "path not reported",
    }),
  ]);
  card.dataset.layer = layer;
  card.title = on ? "Show every set key again" : `Show only the keys the ${layer} file supplies`;
  card.addEventListener("click", () => setFilter(layer));
  return card;
}

function registryCard(binding) {
  if (!binding) return null;
  const fact = (label, value) => el("span", { class: "cfx-fact" }, [
    el("span", { class: "cfx-fact-label", text: label }),
    el("span", { class: "mono", text: value == null || value === "" ? "–" : String(value), title: value == null ? "" : String(value) }),
  ]);
  let match = null;
  if (binding.base_branch_matches_workflow === true) {
    match = el("span", { class: "cfx-match ok", text: "✓ base_branch matches workflow.base_branch" });
  } else if (binding.base_branch_matches_workflow === false) {
    match = el("span", {
      class: "cfx-match warn",
      text: `⚠ workflow.base_branch is ${binding.workflow_base_branch} — delivery uses the registered ${binding.base_branch}`,
    });
  }
  const card = el("div", { class: "cfx-layer static registry" }, [
    el("span", { class: "cfx-registry-head" }, [
      pill("registry"),
      el("span", { class: "cfx-layer-sub", text: "bound outside config.toml" }),
    ]),
    el("span", { class: "cfx-facts" }, [
      fact("base_branch", binding.base_branch),
      fact("ship_mode", binding.ship_mode),
      fact("owner", binding.owner_machine_id),
    ]),
    match,
  ]);
  card.dataset.layer = "registry";
  return card;
}

// -------------------------------------------------------------- callouts

function calloutRow(payload, model) {
  const items = [];
  const globalPath = payload.layers?.global?.path || "the global config.toml";
  if (model.counts.restates > 0) {
    const n = model.counts.restates;
    const show = button(filter === "restates" ? "Show every set key" : `Show the ${n} ${n === 1 ? "key" : "keys"}`, () => setFilter("restates"));
    show.setAttribute("aria-pressed", String(filter === "restates"));
    items.push(callout("restates", `${n} workspace ${n === 1 ? "key restates" : "keys restate"} the global value`, [
      el("span", { text: `They pin today's value: a later change to ${globalPath} will not reach this workspace for ${n === 1 ? "it" : "these keys"}.` }),
      el("span", {}, [show]),
    ]));
  }
  if (payload.layers?.execution_not_inherited) {
    const ignored = model.rows.filter((info) => info.notInherited).map((info) => info.row.key);
    items.push(callout("not-inherited", "execution.* skips the global file here", [
      el("span", {
        text: `While a workspace file exists, execution keys come from it or from the built-in defaults.${ignored.length ? ` Global values for ${listText(ignored)} are ignored.` : ""}`,
      }),
    ]));
  }
  if (payload.workspace_file_exists === false) {
    items.push(callout("note", "No workspace config.toml yet", [
      el("span", { text: "The first workspace write asks whether to copy the current global policy or start empty." }),
    ]));
  }
  return items.length ? el("div", { class: "cfx-callouts" }, items) : null;
}

function callout(kind, title, body) {
  const node = el("div", { class: `cfx-callout ${kind}` }, [
    el("span", { class: "cfx-dot", "aria-hidden": "true" }),
    el("div", { class: "cfx-callout-body" }, [el("strong", { text: title }), ...body]),
  ]);
  node.dataset.callout = kind;
  return node;
}

function listText(items) {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;
}

// ---------------------------------------------------------- review strip

// Before-PR review is `review.before_pr`, before-landing review is
// `review.before_landing` (or the hosts `review.before_landing_hosts` lists),
// and after-landing review is the delivery-code-review auto-task's own flag.
function reviewStrip(review) {
  if (!review) return null;
  const section = el("section", { class: "cfx-review cfx-card", "aria-label": "Review layers" });
  const head = el("div", { class: "cfx-section-head" }, [el("h2", { class: "cfx-h2", text: "Review layers" })]);
  section.appendChild(head);
  if (review.error) {
    section.appendChild(reviewProblem("Review status is unavailable", review.error));
    return section;
  }
  const before = review.before_pr || {};
  const landing = review.before_landing || {};
  const after = review.after_landing || {};
  const crews = Array.isArray(before.crews) && before.crews.length ? before.crews.join(" ") : null;
  const hosts = Array.isArray(landing.hosts) ? landing.hosts : [];
  const steps = [
    {
      id: "before-pr", label: "Before PR", on: before.enabled === true,
      source: `review.before_pr = ${before.enabled === true}${before.source ? ` (${before.source})` : ""}`,
      problems: before.problems, warnings: [],
    },
    {
      id: "before-landing", label: "Before landing", on: landing.enabled === true,
      source: `review.before_landing = ${landing.enabled === true}${landing.source ? ` (${landing.source})` : ""}${
        landing.enabled === true ? "" : hosts.length ? ` · on for claims from ${hosts.join(", ")}` : " · no hosts listed"}`,
      problems: landing.problems, warnings: landing.warnings,
    },
    {
      id: "after-landing", label: "After landing", on: after.enabled === true,
      source: `${after.source || "auto-task not reported"}${crews ? ` · review crew ${crews}` : ""}`,
      problems: after.health?.problems, warnings: [],
    },
  ];
  const running = steps.filter((step) => step.on);
  head.appendChild(el("span", {
    class: "cfx-section-note",
    text: running.length
      ? `${running.length === 1 ? "One review layer runs" : `${running.length} review layers run`} today: ${listText(running.map((step) => step.label.toLowerCase()))}.`
      : "No review layer runs today.",
  }));
  const row = el("div", { class: "cfx-steps" });
  steps.forEach((step, index) => {
    if (index) row.appendChild(arrow());
    row.appendChild(reviewStep(step));
  });
  section.appendChild(row);
  if (review.healthy === false && !steps.some((step) => step.on && (step.problems || []).length)) {
    section.appendChild(reviewProblem("Review is unhealthy", "The review switches could not be confirmed healthy."));
  }
  const lines = [
    before.line ? `Before-PR review: ${before.line}` : null,
    landing.line ? `Before-landing review: ${landing.line}` : null,
    after.line ? `After-landing review: ${after.line}` : null,
    after.health?.line ? `After-landing health: ${after.health.line}` : null,
  ].filter(Boolean);
  if (lines.length) {
    section.appendChild(el("details", { class: "cfx-details" }, [
      el("summary", { text: "Full review diagnostics" }),
      el("pre", { class: "cfx-diagnostic", text: lines.join("\n") }),
    ]));
  }
  return section;
}

function reviewStep(step) {
  const problems = Array.isArray(step.problems) ? step.problems : [];
  const unhealthy = step.on && problems.length > 0;
  const node = el("div", { class: `cfx-step ${unhealthy ? "alert" : step.on ? "on" : "off"}` }, [
    el("span", { class: "cfx-step-title" }, [el("span", { class: "cfx-dot", "aria-hidden": "true" }), step.label]),
    el("span", { class: "cfx-step-state", text: `${step.on ? "On" : "Off"}${unhealthy ? " · unhealthy" : step.on ? " · healthy" : ""}` }),
    el("span", { class: "cfx-step-source mono", text: step.source }),
  ]);
  node.dataset.step = step.id;
  if (unhealthy) {
    node.setAttribute("role", "alert");
    node.appendChild(el("span", { class: "cfx-step-problem" }, [
      `${String(problems[0]).replace(/[.!?]+$/, "")}. Run `,
      el("code", { text: "orbit doctor" }),
      ".",
    ]));
  }
  for (const warning of Array.isArray(step.warnings) ? step.warnings : []) {
    node.appendChild(el("span", { class: "cfx-step-warning", text: warning }));
  }
  return node;
}

function reviewProblem(title, reason) {
  const node = el("div", { class: "cfx-step alert", role: "alert" }, [
    el("span", { class: "cfx-step-title", text: title }),
    el("span", { class: "cfx-step-problem" }, [`${reason} Run `, el("code", { text: "orbit doctor" }), "."]),
  ]);
  return node;
}

// ------------------------------------------------------------- key table

function keyTable(model) {
  const panel = el("section", { class: "cfx-keys cfx-card", "aria-labelledby": "cfx-keys-title" });
  const title = el("h2", { class: "cfx-h2" }, [
    "Keys ",
    el("span", { class: "cfx-section-note mono", text: `${model.counts.set} set of ${model.counts.total}` }),
  ]);
  title.id = "cfx-keys-title";

  const search = el("input", { class: "cfx-input cfx-search", type: "search" });
  search.id = "cfx-search";
  search.placeholder = "Filter keys, values, descriptions";
  search.value = query;
  search.autocomplete = "off";
  const label = el("label", { class: "visually-hidden", text: "Filter keys, values and descriptions" });
  label.htmlFor = "cfx-search";
  const groups = el("div", { class: "cfx-groups" });
  search.addEventListener("input", () => {
    query = search.value.trim().toLowerCase();
    fillGroups(groups, model);
  });

  const segments = el("div", { class: "cfx-segs", role: "group", "aria-label": "Which keys" });
  const segmentCounts = { set: model.counts.set, workspace: model.counts.workspace, global: model.counts.global, restates: model.counts.restates };
  for (const { id, label: text } of FILTERS) {
    const seg = el("button", { class: "cfx-seg", type: "button", "aria-pressed": String(filter === id) }, [
      text, el("span", { class: "cfx-seg-n mono", text: String(segmentCounts[id]) }),
    ]);
    seg.dataset.filter = id;
    seg.addEventListener("click", () => setFilter(id));
    segments.appendChild(seg);
  }
  const descriptions = button("Descriptions", () => {
    showDescriptions = !showDescriptions;
    rerender();
  }, "cfx-seg cfx-desc-toggle");
  descriptions.setAttribute("aria-pressed", String(showDescriptions));
  descriptions.title = showDescriptions ? "Hide the one-line description under each key" : "Show a one-line description under each key";

  panel.appendChild(el("div", { class: "cfx-toolbar" }, [
    title,
    el("div", { class: "cfx-search-wrap" }, [label, search]),
    segments,
    descriptions,
  ]));
  panel.appendChild(el("div", { class: "cfx-colhead", "aria-hidden": "true" }, [
    el("span", { class: "cfx-col-key", text: "Key" }),
    el("span", { class: "cfx-col-value", text: "Value" }),
    el("span", { class: "cfx-col-src", text: "Source" }),
  ]));
  fillGroups(groups, model);
  panel.appendChild(groups);
  return panel;
}

/// The section groups under the toolbar; the search box refills only these,
/// so the field being typed in is never rebuilt.
function fillGroups(container, model) {
  container.replaceChildren();
  let shown = 0;
  for (const group of model.groups) {
    const { rows, held, expanded } = visibleRows(group);
    if (!rows.length && !held.length) continue;
    if (!rows.length && filter !== "set") continue;
    shown += rows.length;
    const node = el("div", { class: "cfx-group" });
    node.dataset.section = group.section.token || "";
    const counts = group.section.counts || {};
    node.appendChild(el("div", { class: "cfx-group-head" }, [
      el("h3", { class: "cfx-group-title", text: group.section.title || group.section.token || "Settings" }),
      group.section.blurb ? el("span", { class: "cfx-group-blurb", text: group.section.blurb }) : null,
      el("span", { class: "cfx-group-counts mono", text: `${counts.set || 0} set · ${counts.default || 0} default${counts.unset ? ` · ${counts.unset} unset` : ""}` }),
    ]));
    for (const info of rows) node.appendChild(keyRow(info));
    if (held.length) {
      const atDefault = held.filter((info) => info.row.state === "default").length;
      const unset = held.length - atDefault;
      const parts = [];
      if (atDefault) parts.push(`${atDefault} ${atDefault === 1 ? "key" : "keys"} at ${atDefault === 1 ? "its" : "their"} built-in default`);
      if (unset) parts.push(`${unset} unset`);
      const toggle = button(expanded ? "Hide" : "Show", () => {
        if (expanded) expandedSections.delete(group.section.token);
        else expandedSections.add(group.section.token);
        rerender();
      }, "cfx-btn quiet cfx-held-toggle");
      toggle.setAttribute("aria-expanded", String(expanded));
      node.appendChild(el("div", { class: "cfx-held" }, [el("span", { text: parts.join(", ") }), toggle]));
    }
    container.appendChild(node);
  }
  if (!shown) {
    const clear = button("Clear filters", () => {
      query = "";
      setFilter("set");
    }, "cfx-btn quiet");
    container.appendChild(el("div", { class: "cfx-none" }, [
      el("span", { text: filter === "set" && !query ? "No key is set; every value is a built-in default." : "No key matches." }),
      filter === "set" && !query ? null : clear,
    ]));
  }
}

function keyRow(info) {
  const { row } = info;
  const label = String(row.label || row.key);
  const prefix = row.key.endsWith(label) ? row.key.slice(0, row.key.length - label.length) : "";
  const selected = row.key === selectedKey;
  const node = el("button", { class: `cfx-row${selected ? " selected" : ""}`, type: "button", "aria-pressed": String(selected) }, [
    el("span", { class: "cfx-key mono" }, [
      prefix ? el("span", { class: "cfx-key-prefix", text: prefix }) : null,
      el("span", { class: "cfx-key-name", text: prefix ? label : row.key }),
    ]),
    valueTokens(row.value),
    el("span", { class: "cfx-src" }, [
      pill(info.layer),
      info.status.text ? el("span", { class: `cfx-status ${info.status.cls}`, text: info.status.text, title: info.status.title }) : null,
    ]),
    showDescriptions && row.description ? el("span", { class: "cfx-desc", text: row.description, title: row.description }) : null,
  ]);
  node.dataset.key = row.key;
  node.dataset.layer = info.layer;
  node.addEventListener("click", () => {
    if (selectedKey === row.key) return;
    selectedKey = row.key;
    draft = null;
    notice = null;
    rerender();
  });
  return node;
}

// -------------------------------------------------------------- inspector

function inspector(payload, model, context) {
  const panel = el("aside", { class: "cfx-inspector cfx-card", "aria-labelledby": "cfx-inspector-key" });
  const info = selectedKey ? model.byKey.get(selectedKey) : null;
  if (!info) {
    panel.appendChild(el("p", { class: "cfx-muted", text: "Select a key to see how it resolves." }));
    return panel;
  }
  const { row } = info;
  const heading = el("h2", { class: "cfx-inspector-key mono", text: row.key });
  heading.id = "cfx-inspector-key";
  panel.appendChild(el("div", { class: "cfx-inspector-head" }, [
    el("span", { class: "cfx-src" }, [
      pill(info.layer),
      el("span", { class: `cfx-status wrap ${info.status.cls}`, text: info.status.title || (info.layer === "built-in" ? "No file sets it" : `Set in the ${info.layer} ${info.layer === "environment" ? "" : "file "}only`) }),
    ]),
    heading,
    row.description ? el("p", { class: "cfx-inspector-desc", text: row.description }) : null,
  ]));
  panel.appendChild(resolutionChain(payload, info));
  if (row.settable === false) {
    panel.appendChild(el("p", { class: "cfx-readonly" }, [
      el("strong", { text: "Read-only. " }),
      row.read_only_reason ? `${capitalize(row.read_only_reason)}.` : "orbit config set refuses this key.",
    ]));
  } else if (!context.canEdit) {
    panel.appendChild(el("p", { class: "cfx-readonly", text: `Not editable here: ${context.refusal || "this session cannot change configuration"}.` }));
  } else {
    panel.appendChild(editor(payload, info, context));
  }
  return panel;
}

function capitalize(text) {
  return text ? text[0].toUpperCase() + text.slice(1) : text;
}

/// How the selected key resolves: each layer that could supply it, lowest
/// first, with the winner marked and the values it overrides struck.
function resolutionChain(payload, info) {
  const { row } = info;
  const layers = payload.layers || {};
  const builtIn = row.built_in;
  const steps = [{
    layer: "built-in", label: "Built-in default", path: "compiled into orbit",
    value: !builtIn ? "not reported" : builtIn.state === "unset" ? "unset (no built-in default)" : display(builtIn.value),
    has: builtIn?.state === "default",
  }];
  if (info.layer === "environment" || row.fallback?.layer === "environment") {
    steps.push({
      layer: "environment", label: "Environment", path: info.layer === "environment" ? row.source?.path || "process environment" : "process environment",
      value: info.layer === "environment" ? display(row.value) : display(row.fallback.value), has: true,
    });
  }
  steps.push({
    layer: "global", label: "Global file", path: layers.global?.path || "global config.toml",
    value: info.global ? display(info.global.value) : "not set", has: info.global !== null,
    note: info.notInherited ? "Not inherited: while a workspace file exists this key comes from it or the built-in default." : "",
  });
  steps.push({
    layer: "workspace", label: "Workspace file", path: layers.workspace?.path || "workspace config.toml",
    value: info.layer === "workspace" ? display(row.value) : "not set", has: info.layer === "workspace",
    note: row.global_only ? "Only the global file may carry this key." : "",
  });
  const winner = LAYERS.indexOf(info.layer);
  const list = el("ol", { class: "cfx-chain" });
  for (const step of steps) {
    const at = LAYERS.indexOf(step.layer);
    let cls = "none";
    if (step.layer === info.layer) cls = "win";
    else if (step.layer === "global" && info.notInherited) cls = "ignored";
    else if (step.has && at < winner) cls = "over";
    const item = el("li", { class: `cfx-chain-step ${cls}` }, [
      el("span", { class: "cfx-chain-dot", "aria-hidden": "true" }),
      el("span", { class: "cfx-chain-body" }, [
        el("span", { class: "cfx-chain-label" }, [
          step.label,
          cls === "win" ? el("span", { class: "cfx-wins", text: "effective" }) : null,
          cls === "over" ? el("span", { class: "visually-hidden", text: " (overridden)" }) : null,
        ]),
        el("span", { class: "cfx-chain-path mono", text: step.path }),
        el("span", { class: "cfx-chain-value mono", text: step.layer === info.layer && info.layer === "built-in" && row.state === "unset" ? "unset" : step.value }),
        step.note ? el("span", { class: "cfx-chain-note", text: step.note }) : null,
      ]),
    ]);
    item.dataset.layer = step.layer;
    list.appendChild(item);
  }
  return el("div", { class: "cfx-inspector-block" }, [el("h3", { class: "cfx-eyebrow", text: "How it resolves" }), list]);
}

/// What removing the workspace override reaches, from the payload's fallback.
function fallbackText(info) {
  const fallback = info.row.fallback;
  if (!fallback) return "not reported";
  const value = display(fallback.value);
  if (fallback.layer === "global") return `the global value, ${value}`;
  if (fallback.layer === "environment") return `the environment value, ${value}`;
  const base = fallback.state === "unset" || fallback.value == null ? "unset (no built-in default)" : `the built-in default, ${value}`;
  return info.notInherited ? `${base} (the global value is not inherited)` : base;
}

// ----------------------------------------------------------------- editor

function editor(payload, info, context) {
  const { row } = info;
  if (!draft) {
    draft = {
      key: row.key,
      workspace: getWorkspace(),
      text: draftText(row),
      scope: row.global_only || info.layer === "global" ? "global" : "workspace",
    };
  }
  if (row.global_only) draft.scope = "global";
  const form = el("form", { class: "cfx-editor" });
  form.noValidate = true;
  form.appendChild(el("h3", { class: "cfx-eyebrow", text: "Change value" }));
  const field = valueField(row, draft, { id: "cfx-edit-value", disabled: pending });
  const fieldLabel = el("label", { class: "cfx-label", text: field.label });
  fieldLabel.htmlFor = "cfx-edit-value";
  form.appendChild(fieldLabel);
  form.appendChild(field.node);
  if (field.hint) form.appendChild(el("span", { class: "cfx-hint", text: field.hint }));

  const files = el("fieldset", { class: "cfx-target" }, [el("legend", { class: "cfx-label", text: "Write to" })]);
  const layers = payload.layers || {};
  for (const scope of ["workspace", "global"]) {
    const radio = el("input", { type: "radio" });
    radio.name = "cfx-scope";
    radio.value = scope;
    radio.checked = draft.scope === scope;
    radio.disabled = pending || (scope === "workspace" && row.global_only === true);
    radio.addEventListener("change", () => {
      if (!radio.checked) return;
      draft.scope = scope;
      notice = null;
      rerender();
    });
    const path = scope === "global" ? layers.global?.path : layers.workspace?.path;
    const option = el("label", { class: `cfx-radio${radio.disabled ? " disabled" : ""}` }, [
      radio,
      el("span", { text: scope === "global" ? "Global file" : "Workspace file" }),
      el("span", { class: "cfx-radio-path mono", text: path || "" }),
    ]);
    option.dataset.scope = scope;
    files.appendChild(option);
  }
  if (row.global_only) files.appendChild(el("span", { class: "cfx-hint", text: "Only the global file may carry this key." }));
  form.appendChild(files);

  const cli = el("code", { class: "cfx-cli mono", text: cliCommand(row, draft) });
  cli.id = "cfx-cli";
  form.appendChild(el("div", { class: "cfx-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]));
  field.onInput(() => {
    cli.textContent = cliCommand(row, draft);
    if (notice?.kind === "error") notice = null;
  });

  const save = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Saving…" : "Save" });
  save.disabled = pending;
  const actions = el("div", { class: "cfx-actions" }, [save]);
  if (info.layer === "workspace") {
    const remove = button("Remove workspace override", () => removeOverride(info, context));
    remove.disabled = pending;
    actions.appendChild(remove);
  }
  form.appendChild(actions);
  if (info.layer === "workspace") {
    form.appendChild(el("p", { class: "cfx-fallback", text: `Removing it falls back to ${fallbackText(info)}.` }));
  }
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveDraft(row, context);
  });
  const message = noticeNode(info);
  if (message) form.appendChild(message);
  if (notice?.kind === "error" && notice.offerInit) {
    form.appendChild(el("div", { class: "cfx-actions" }, [
      el("span", { class: "cfx-hint", text: "Create the workspace file:" }),
      button("Copy global policy", () => saveDraft(row, context, "seed-from-global")),
      button("Start empty", () => saveDraft(row, context, "fresh")),
    ]));
  }
  return form;
}

export function draftText(row) {
  const value = row.value;
  if (Array.isArray(value)) return value.map((entry) => (typeof entry === "string" ? entry : JSON.stringify(entry))).join("\n");
  if (value == null) return row.value_type === "bool" ? "false" : "";
  return String(value);
}

/// A field typed by the registry: a choice list for an enum or a bool, a
/// number field for an integer, one entry per line for an array, and a text
/// field otherwise. It edits `state.text`, so a re-render keeps the input.
export function valueField(row, state, { id, disabled }) {
  const type = String(row.value_type || "");
  const options = Array.isArray(row.options) ? row.options : [];
  let node;
  let label = "New value";
  let hint = "";
  if (options.length || type === "bool") {
    const choices = options.length ? options : ["true", "false"];
    node = el("select", { class: "cfx-input mono" });
    if (!choices.includes(state.text)) state.text = choices[0];
    for (const choice of choices) {
      const option = el("option", { text: choice });
      option.value = choice;
      node.appendChild(option);
    }
    node.value = state.text;
  } else if (type.startsWith("array")) {
    node = el("textarea", { class: "cfx-input mono" });
    node.rows = Math.min(8, Math.max(3, state.text.split("\n").length + 1));
    node.value = state.text;
    label = "New value, one entry per line";
    hint = "An empty list clears every entry.";
  } else {
    node = el("input", { class: "cfx-input mono", type: type === "integer" ? "number" : "text" });
    if (type === "integer") node.inputMode = "numeric";
    node.value = state.text;
  }
  node.id = id;
  node.disabled = disabled;
  const listeners = [];
  node.addEventListener(node.tagName === "SELECT" ? "change" : "input", () => {
    state.text = node.value;
    for (const listener of listeners) listener();
  });
  return { node, label, hint, onInput: (listener) => listeners.push(listener) };
}

/// The draft as the JSON value the key route admits, or an error message.
export function draftValue(row, text) {
  const type = String(row.value_type || "");
  if (type === "bool") return { value: text === "true" };
  if (type === "integer") {
    const trimmed = text.trim();
    if (!/^-?\d+$/.test(trimmed)) return { error: `${row.key} takes a whole number` };
    return { value: Number(trimmed) };
  }
  if (type.startsWith("array")) return { value: text.split("\n").map((entry) => entry.trim()).filter(Boolean) };
  return { value: text };
}

/// The `orbit config set` command that makes the same write. Its value is a
/// TOML literal the CLI parses, quoted for a POSIX shell.
export function cliCommand(row, current) {
  const type = String(row.value_type || "");
  const text = current.text;
  let literal;
  if (type === "bool" || type === "integer") {
    literal = text.trim() === "" ? "''" : shellWord(text.trim());
  } else if (type.startsWith("array")) {
    const entries = text.split("\n").map((entry) => entry.trim()).filter(Boolean);
    literal = shellQuote(`[${entries.map((entry) => JSON.stringify(entry)).join(", ")}]`);
  } else if (/^(true|false|[+-]?(\d|inf|nan).*|[[{"'].*)$/i.test(text) || text === "") {
    // A bare word the CLI would read as a non-string TOML literal is sent as
    // a quoted TOML string instead.
    literal = shellQuote(JSON.stringify(text));
  } else {
    literal = shellWord(text);
  }
  return `orbit config set ${current.scope === "global" ? "--global " : ""}${row.key} ${literal}`;
}

function shellWord(text) {
  return /^[A-Za-z0-9_.\/:@%+=,-]+$/.test(text) ? text : shellQuote(text);
}

function shellQuote(text) {
  return `'${text.replace(/'/g, "'\\''")}'`;
}

function noticeNode(info) {
  if (!notice || notice.key !== info.row.key) return null;
  if (notice.kind === "error") return el("p", { class: "cfx-notice error", role: "alert", text: notice.text });
  let text = notice.text;
  if (notice.kind === "removed") {
    text = `Removed the workspace override. ${info.row.key} now resolves to ${display(info.row.value)} from the ${info.layer} layer.`;
  }
  return el("p", { class: "cfx-notice ok", role: "status", text });
}

async function saveDraft(row, context, init) {
  if (pending || !draft) return;
  const parsed = draftValue(row, draft.text);
  if (parsed.error) {
    notice = { key: row.key, kind: "error", text: parsed.error };
    rerender();
    return;
  }
  const scope = draft.scope;
  await write(context, row.key, () => requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, "PUT", {
    value: parsed.value,
    scope,
    ...(init ? { init } : {}),
  }), { kind: "saved", text: `Saved ${row.key} to the ${scope} file.` }, scope);
}

async function removeOverride(info, context) {
  if (pending) return;
  await write(context, info.row.key, () => requestJson(
    `/api/config/keys/${encodeURIComponent(info.row.key)}?scope=workspace`,
    "DELETE",
  ), { kind: "removed" }, "workspace");
}

/// One write, then a re-read so the table and inspector show what the server
/// resolved. The outcome lands only if the operator is still on the same key
/// in the same workspace.
async function write(context, key, request, success, scope) {
  const workspace = getWorkspace();
  pending = true;
  notice = null;
  rerender();
  try {
    await request();
    pending = false;
    if (workspace !== getWorkspace()) return;
    draft = null;
    notice = { key, ...success };
    await context.reload();
  } catch (error) {
    pending = false;
    if (workspace !== getWorkspace()) return;
    notice = {
      key, kind: "error", text: error.message || String(error),
      offerInit: scope === "workspace" && lastRender?.payload?.workspace_file_exists === false,
    };
    rerender();
  }
}

// ------------------------------------------------------------------ crews

/// Crews grouped by provider: one usage bar per provider, from the live
/// limits the payload attaches to its crews, and each crew's model, effort
/// and the settings and auto-tasks that name it.
function crewsSection(crews) {
  const providers = new Map();
  for (const crew of crews) {
    const name = crew.provider || "provider not reported";
    if (!providers.has(name)) providers.set(name, []);
    providers.get(name).push(crew);
  }
  const unreferenced = crews.filter((crew) => !(crew.referenced_by || []).length && crew.source !== "built-in");
  const heading = el("h2", { class: "cfx-h2" }, [
    "Crews ",
    el("span", { class: "cfx-section-note mono", text: `${crews.length} defined · grouped by provider limit` }),
  ]);
  heading.id = "cfx-crews-title";
  const edit = el("a", { class: "cfx-btn", text: "Edit crews" });
  edit.href = "#config/crews";
  const section = el("section", { class: "cfx-crews", "aria-labelledby": "cfx-crews-title" }, [
    el("div", { class: "cfx-section-head" }, [
      el("div", { class: "cfx-crews-title" }, [
        heading,
        unreferenced.length
          ? el("span", { class: "cfx-unref-note", text: `${listText(unreferenced.map((crew) => crew.name))} ${unreferenced.length === 1 ? "has" : "have"} no setting or auto-task reference.` })
          : null,
      ]),
      edit,
    ]),
  ]);
  if (!crews.length) {
    section.appendChild(el("p", { class: "cfx-muted", text: "No crew is defined; the built-in crew registry applies." }));
    return section;
  }
  const grid = el("div", { class: "cfx-providers" });
  for (const [name, members] of providers) grid.appendChild(providerCard(name, members));
  section.appendChild(grid);
  return section;
}

/// The provider's bar is its provider-wide reading when a crew carries one,
/// else the most used reading among its crews. A crew whose own reading is
/// a different window or scope says so on its line.
function providerLimit(members) {
  const limits = members.map((crew) => crew.limit).filter(Boolean);
  if (!limits.length) return null;
  const used = (limit) => (limit.exhausted ? Infinity : Number(limit.used_percent ?? 0));
  const wide = limits.filter((limit) => !limit.scope);
  const pool = wide.length ? wide : limits;
  return pool.reduce((best, limit) => (used(limit) > used(best) ? limit : best));
}

function sameReading(left, right) {
  return left && right && left.scope === right.scope && left.window === right.window;
}

function providerCard(name, members) {
  const limit = providerLimit(members);
  const card = el("div", { class: "cfx-provider cfx-card" }, [
    el("div", { class: "cfx-provider-head" }, [
      el("h3", { class: "cfx-provider-name", text: name }),
      el("span", { class: "cfx-section-note mono", text: `${members.length} ${members.length === 1 ? "crew" : "crews"}` }),
    ]),
  ]);
  card.dataset.provider = name;
  card.appendChild(limit ? limitBar(limit) : el("span", { class: "cfx-muted cfx-limit-none", text: "No usage limit reported" }));
  const list = el("ul", { class: "cfx-crew-list" });
  for (const crew of members) list.appendChild(crewLine(crew, limit));
  card.appendChild(list);
  return card;
}

function limitBar(limit) {
  const percent = limit.used_percent == null ? null : Math.max(0, Math.min(100, Math.round(Number(limit.used_percent))));
  const usedText = limit.exhausted ? "exhausted" : percent == null ? "–" : `${percent}%`;
  const window = limit.window ? `${limit.window} limit used` : "limit used";
  const fill = el("span", { class: "cfx-meter-fill" });
  fill.style.width = `${limit.exhausted ? 100 : percent ?? 0}%`;
  const meter = el("div", { class: `cfx-meter${limit.gated ? " gated" : ""}`, role: "img", "aria-label": `${usedText} of the ${window.replace(/ used$/, "")}` }, [fill]);
  const resets = limit.resets_at || limit.until;
  return el("div", { class: "cfx-limit" }, [
    el("div", { class: "cfx-limit-line" }, [
      el("span", { text: limit.scope ? `${window} · ${limit.scope}` : window }),
      el("span", { class: "cfx-limit-used mono", text: usedText }),
    ]),
    meter,
    el("span", { class: "cfx-limit-reset" }, [
      resets ? `${limit.resets_at ? "Resets" : "Counts until"} ${formatDateTime(resets)}` : "Reset not reported",
      limit.gated ? el("span", { class: "cfx-gated", text: "gated", title: `Admission skips these crews until ${formatDateTime(limit.until)} (limit ${limit.threshold}%)` }) : null,
    ]),
  ]);
}

function crewLine(crew, providerReading) {
  const references = crew.referenced_by || [];
  let use;
  if (references.length) {
    use = el("span", {
      class: "cfx-crew-use",
      text: `used by ${references.length}${references.length === 1 ? `: ${references[0]}` : " settings and auto-tasks"}`,
      title: references.join("\n"),
    });
  } else if (crew.source === "built-in") {
    use = el("span", { class: "cfx-crew-use", text: "built-in crew, not referenced" });
  } else {
    use = el("span", { class: "cfx-crew-use unreferenced", text: "not referenced by any setting or auto-task" });
  }
  const own = crew.limit && !sameReading(crew.limit, providerReading)
    ? el("span", {
      class: "cfx-crew-limit mono",
      text: `own limit: ${crew.limit.exhausted ? "exhausted" : crew.limit.used_percent == null ? "–" : `${Math.round(Number(crew.limit.used_percent))}%`}${crew.limit.window ? ` · ${crew.limit.window}` : ""}${crew.limit.scope ? ` · ${crew.limit.scope}` : ""}`,
    })
    : null;
  const line = el("li", { class: `cfx-crew${crew.enabled === false ? " disabled" : ""}` }, [
    el("span", { class: "cfx-crew-name" }, [
      crew.name,
      crew.enabled === false ? el("span", { class: "cfx-crew-flag", text: "disabled" }) : null,
    ]),
    el("span", { class: "cfx-crew-model mono", text: crew.model || "model not set" }),
    el("span", { class: "cfx-effort", text: crew.effort || "default effort" }),
    use,
    own,
  ]);
  line.dataset.crew = crew.name;
  return line;
}

// ------------------------------------------------------------------ paths

function pathsSection(paths) {
  const workspaceRoot = paths.find((entry) => entry.label === "workspace root")?.value;
  const heading = el("h2", { class: "cfx-h2" }, [
    "Paths ",
    el("span", { class: "cfx-section-note", text: "where this workspace reads and writes" }),
  ]);
  heading.id = "cfx-paths-title";
  const list = el("dl", { class: "cfx-paths" });
  for (const entry of paths) {
    const same = entry.label === "local root" && workspaceRoot && entry.value === workspaceRoot;
    list.appendChild(el("div", { class: "cfx-path" }, [
      el("dt", { text: entry.label }),
      el("dd", { class: same ? "cfx-path-same" : "mono", text: same ? "same as workspace root" : entry.value, title: entry.value }),
    ]));
  }
  if (!paths.length) list.appendChild(el("div", { class: "cfx-path" }, [el("dt", { text: "paths" }), el("dd", { text: "not reported" })]));
  return el("section", { class: "cfx-paths-panel cfx-card", "aria-labelledby": "cfx-paths-title" }, [heading, list]);
}
