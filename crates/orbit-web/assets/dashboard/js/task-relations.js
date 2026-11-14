// Orbit dashboard task relation rows: external refs, the regression head and the delivery card's relation rows.

import { el, isHttpUrl, pinnedInfoHint } from './common.js';
import { drainReadinessView } from './drain-waits.js';
import { GITHUB_PR_REF_SYSTEM } from './task-shared.js';
import { openVisibleTask } from './task-nav.js';

const RELATION_GROUPS = [
  ["blocked_by", "blocked by"],
  ["child_of", "child of"],
  ["spawned_from", "spawned from"],
  ["regression_from", "regression from"],
  ["supersedes", "supersedes"],
  ["related_to", "related to"],
];
const RELATION_GROUP_LABELS = new Map(RELATION_GROUPS);

function relationTypeKey(value) {
  if (!value) return "";
  return String(value)
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/-/g, "_")
    .toLowerCase();
}

export function buildExternalRefs(refs) {
  const wrap = el("div");
  for (const ref of refs) {
    const label = ref.system === GITHUB_PR_REF_SYSTEM
      ? `PR #${ref.id || ""}`
      : `${ref.system || "external"}:${ref.id || ""}`;
    const line = el("div", { class: "external-ref-line" });
    // Task records are agent-writable, so a non-http(s) URL stays text.
    if (isHttpUrl(ref.url)) {
      const link = el("a", { text: label });
      link.href = ref.url;
      link.rel = "noopener noreferrer";
      link.target = "_blank";
      line.appendChild(link);
    } else {
      line.textContent = label;
    }
    wrap.appendChild(line);
  }
  return wrap;
}

// Readable words for a `regression_from` link's stored snake_case enums, so
// the Regression from row reads labels rather than raw values. Unknown values
// fall back to words split on underscores.
const REGRESSION_VALUE_LABELS = {
  broke_existing: "broke existing",
  defect_in_new: "defect in new code",
  merge_reintroduced: "merge reintroduced",
  contract_drift: "contract drift",
  no_test: "no test",
  test_not_selected: "test not selected",
  platform_only: "platform only",
  review_missed: "review missed",
  spec_ambiguous: "spec ambiguous",
};
function regressionValueLabel(value) {
  if (value == null) return "";
  const raw = String(value);
  if (Object.hasOwn(REGRESSION_VALUE_LABELS, raw)) return REGRESSION_VALUE_LABELS[raw];
  return raw.replace(/_/g, " ");
}

// The typed relations in RELATION_GROUPS order, one entry per relation type,
// carrying the label that says how its targets relate. A `regression_from`
// target keeps its optional `regression` classification with it, so the row
// can show each link's own nature, escape, severity and sites; every other type stays a
// plain id string as before.
export function relationTargets(relations) {
  const byType = new Map(RELATION_GROUPS.map(([key]) => [key, []]));
  const seenByType = new Map(RELATION_GROUPS.map(([key]) => [key, new Set()]));
  for (const relation of relations) {
    const key = relationTypeKey(relation.relation_type || relation.type);
    const target = relation.target == null ? "" : String(relation.target);
    if (!RELATION_GROUP_LABELS.has(key) || !target) continue;
    if (seenByType.get(key).has(target)) continue;
    seenByType.get(key).add(target);
    if (key === "regression_from") {
      const regression = relation && typeof relation.regression === "object" && relation.regression
        ? relation.regression
        : null;
      byType.get(key).push({ target, regression });
    } else {
      byType.get(key).push(target);
    }
  }
  return RELATION_GROUPS.flatMap(([key, label]) =>
    byType.get(key).length ? [{ key, label, targets: byType.get(key) }] : []);
}

function regressionTargetId(entry) {
  return typeof entry === "string" ? entry : entry.target;
}

function regressionSitesOf(entry) {
  const sites = entry && typeof entry === "object" && entry.regression
    && Array.isArray(entry.regression.sites) ? entry.regression.sites : [];
  return sites.filter((site) => typeof site === "string" && site !== "");
}

function buildRegressionEntry(entry, context) {
  const target = regressionTargetId(entry);
  const item = el("span", { class: "regression-entry" });
  const open = el("button", { class: "mono task-delivery-relation", type: "button", text: target, title: `Open ${target}` });
  open.addEventListener("click", (event) => {
    event.stopPropagation();
    openVisibleTask(target, context);
  });
  item.appendChild(open);
  const regression = entry && typeof entry === "object" ? entry.regression : null;
  const parts = [];
  if (regression?.nature) parts.push(regressionValueLabel(regression.nature));
  if (regression?.escape) parts.push(`escaped: ${regressionValueLabel(regression.escape)}`);
  if (regression?.severity) {
    const source = regression.severity_source;
    const suffix = source && source !== "filer" ? ` (${source === "default" ? "unrated" : source})` : "";
    parts.push(`severity: ${regression.severity}${suffix}`);
  }
  const sites = regressionSitesOf(entry);
  if (parts.length || sites.length) {
    const hint = pinnedInfoHint("");
    hint.setAttribute("aria-label", `Details for ${target}`);
    const note = hint.querySelector(".info-hint-text");
    if (parts.length) note.appendChild(el("div", { text: parts.join(" · ") }));
    for (const site of sites) note.appendChild(el("div", { class: "mono", text: site }));
    item.appendChild(hint);
  }
  return item;
}

// Detail panels are rebuilt on refresh; release observers when their rows leave.
const regressionObservers = new Map();
let regressionRemovalObserver = null;
function observeRegressionList(list, observer) {
  regressionObservers.set(list, observer);
  if (!regressionRemovalObserver) {
    regressionRemovalObserver = new MutationObserver(() => {
      for (const [node, resizeObserver] of regressionObservers) {
        if (!node.isConnected) {
          resizeObserver.disconnect();
          regressionObservers.delete(node);
        }
      }
    });
    regressionRemovalObserver.observe(document.body, { childList: true, subtree: true });
  }
  observer.observe(list);
}

// Measure complete entries at the available width, then hide only whole ids
// after the third line. ResizeObserver also handles a resized detail pane.
function buildRegressionRelations(targets, context) {
  const value = el("span", { class: "value task-delivery-relations regression" });
  const list = el("span", { class: "regression-list" });
  const items = targets.map((entry) => buildRegressionEntry(entry, context));
  for (const item of items) list.appendChild(item);
  value.appendChild(list);
  let expanded = false;
  let hiddenCount = 0;
  const toggle = buildRelationMoreToggle(0, () => {
    expanded = !expanded;
    updateVisibility();
  });
  value.appendChild(toggle);
  function updateVisibility() {
    items.forEach((item, index) => { item.hidden = !expanded && index >= items.length - hiddenCount; });
    toggle.hidden = hiddenCount === 0;
    syncRelationMoreToggle(toggle, hiddenCount, expanded);
  }
  let lastWidth = -1;
  const observer = new ResizeObserver(([entry]) => {
    if (!value.isConnected) { observer.disconnect(); return; }
    const width = entry.contentRect.width;
    if (!width || width === lastWidth) return;
    lastWidth = width;
    for (const item of items) item.hidden = false;
    let line = 0;
    let previousTop = null;
    let firstHidden = items.length;
    items.forEach((item, index) => {
      const top = item.getBoundingClientRect().top;
      if (previousTop === null || top > previousTop + 1) { line++; previousTop = top; }
      if (line === 4 && firstHidden === items.length) firstHidden = index;
    });
    hiddenCount = items.length - firstHidden;
    if (!hiddenCount) expanded = false;
    updateVisibility();
  });
  observeRegressionList(list, observer);
  return value;
}

// The blocked_by ids the Readiness card already lists under "Depends on".
// They come from the same drain entry the card renders, so the two agree;
// without an entry (another status, a pending or failed read) nothing is
// covered and every blocked_by target keeps its row.
export function drainDependencyIds(drain) {
  const entry = drain && drain.entry;
  if (!entry) return new Set();
  const badge = drainReadinessView(entry).badge;
  if (!badge) return new Set();
  return new Set(badge.dependencies.map(({ taskId }) => taskId));
}

// A long relation list collapses after this many targets (STD-07 §R17): the
// row shows the first few and a "View more (N)" control for the rest. Five
// applies to the panel's non-regression relation rows.
export const RELATION_VISIBLE_LIMIT = 5;

// The collapsed row's toggle: "View more (N)" closed, "Show fewer" open. The
// count names the hidden entries, so the operator knows what expanding shows.
function buildRelationMoreToggle(hiddenCount, onToggle) {
  const toggle = el("button", {
    class: "relation-more",
    type: "button",
    text: `View more (${hiddenCount})`,
    "aria-expanded": "false",
  });
  toggle.addEventListener("click", (event) => {
    event.stopPropagation();
    onToggle(toggle);
  });
  return toggle;
}

function syncRelationMoreToggle(toggle, hiddenCount, expanded) {
  toggle.setAttribute("aria-expanded", String(expanded));
  toggle.textContent = expanded ? "Show fewer" : `View more (${hiddenCount})`;
}

// One delivery-card relation row. Regression links collapse by wrapped lines;
// other relation types keep their count-based limit.
export function buildRelationRow(key, label, targets, context) {
  if (key === "regression_from") {
    const relationRow = el("div", { class: "property-row fact task-delivery-row relation" }, [
      el("span", { class: "label", text: label }),
      buildRegressionRelations(targets, context),
    ]);
    relationRow.dataset.relation = key;
    return relationRow;
  }
  const value = el("span", { class: "value task-delivery-relations" });
  const ids = targets.map((target) => regressionTargetId(target));
  const visible = ids.slice(0, RELATION_VISIBLE_LIMIT);
  visible.forEach((id, index) => {
    if (index > 0) value.append(", ");
    const open = el("button", { class: "mono task-delivery-relation", type: "button", text: id, title: `Open ${id}` });
    open.addEventListener("click", (e) => {
      e.stopPropagation();
      openVisibleTask(id, context);
    });
    value.appendChild(open);
  });
  if (ids.length > RELATION_VISIBLE_LIMIT) {
    const hiddenCount = ids.length - RELATION_VISIBLE_LIMIT;
    const overflow = el("span", { class: "relation-overflow" });
    for (const id of ids.slice(RELATION_VISIBLE_LIMIT)) {
      overflow.append(", ");
      const open = el("button", { class: "mono task-delivery-relation", type: "button", text: id, title: `Open ${id}` });
      open.addEventListener("click", (e) => {
        e.stopPropagation();
        openVisibleTask(id, context);
      });
      overflow.appendChild(open);
    }
    overflow.hidden = true;
    value.appendChild(overflow);
    value.append(" ");
    const toggle = buildRelationMoreToggle(hiddenCount, (node) => {
      const expanded = node.getAttribute("aria-expanded") !== "true";
      overflow.hidden = !expanded;
      syncRelationMoreToggle(node, hiddenCount, expanded);
    });
    value.appendChild(toggle);
  }
  const relationRow = el("div", { class: "property-row fact task-delivery-row relation" }, [
    el("span", { class: "label", text: label }),
    value,
  ]);
  relationRow.dataset.relation = key;
  return relationRow;
}
