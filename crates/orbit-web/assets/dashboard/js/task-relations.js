// Orbit dashboard task relation rows: external refs, the regression head and the delivery card's relation rows.

import { el, isHttpUrl } from './common.js';
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

function buildRegressionHead(entry, context) {
  const target = regressionTargetId(entry);
  const head = el("span", { class: "regression-head" });
  const open = el("button", { class: "mono task-delivery-relation", type: "button", text: target, title: `Open ${target}` });
  open.addEventListener("click", (e) => {
    e.stopPropagation();
    openVisibleTask(target, context);
  });
  head.appendChild(open);
  const regression = entry && typeof entry === "object" ? entry.regression : null;
  if (regression) {
    const nature = regression.nature == null ? "" : String(regression.nature);
    const escape = regression.escape == null ? "" : String(regression.escape);
    if (nature) {
      head.append(document.createTextNode(" · "));
      const natureLabel = el("span", {
        class: nature === "unclassified" ? "regression-label regression-unclassified" : "regression-label",
        text: regressionValueLabel(nature),
      });
      head.appendChild(natureLabel);
    }
    if (escape) {
      head.append(document.createTextNode(" · escaped: "));
      const escapeLabel = el("span", {
        class: escape === "unclassified" ? "regression-label regression-unclassified" : "regression-label",
        text: regressionValueLabel(escape),
      });
      head.appendChild(escapeLabel);
    }
    const severity = regression.severity == null ? "" : String(regression.severity);
    if (severity) {
      head.append(document.createTextNode(" · severity: "));
      head.appendChild(buildRegressionSeverity(severity, regression.severity_source));
    }
  }
  return head;
}

// A link's severity, with who set it when that is not its filer. A link
// nobody rated reads as medium by default and is shown muted.
function buildRegressionSeverity(severity, source) {
  const from = source == null ? "" : String(source);
  const unrated = from === "default";
  const label = el("span", {
    class: unrated ? "regression-label regression-unclassified" : "regression-label",
    text: from && from !== "filer" ? `${severity} (${unrated ? "unrated" : from})` : severity,
    title: unrated ? "Not rated: reads as medium" : `Severity ${severity}, set by ${from || "filer"}`,
  });
  label.dataset.severity = severity;
  if (from) label.dataset.severitySource = from;
  return label;
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

// One delivery-card relation row: the type's ids sit inline, wrapping as
// needed, and each id opens its task. The row keeps the `data-relation` key.
// A `regression_from` row keeps each target's own classification beside its
// id, with that target's sites listed under it; a row with no classification
// and no sites keeps the plain inline ids, like every other relation type.
export function buildRelationRow(key, label, targets, context) {
  if (key === "regression_from") {
    const entries = targets.map((entry) =>
      typeof entry === "string" ? { target: entry, regression: null } : entry);
    const hasDetail = entries.some((entry) => {
      const regression = entry.regression;
      if (!regression) return false;
      const nature = regression.nature == null ? "" : String(regression.nature);
      const escape = regression.escape == null ? "" : String(regression.escape);
      return Boolean(nature || escape || regressionSitesOf(entry).length);
    });
    if (hasDetail) {
      const value = el("span", { class: "value task-delivery-relations regression" });
      for (const entry of entries) {
        const item = el("div", { class: "regression-entry" });
        item.appendChild(buildRegressionHead(entry, context));
        const sites = regressionSitesOf(entry);
        if (sites.length) {
          const siteList = el("div", { class: "regression-site-list" });
          const sitesWrap = el("details", { class: "regression-sites" }, [
            el("summary", { text: `${sites.length} ${sites.length === 1 ? "site" : "sites"}` }),
            siteList,
          ]);
          for (const site of sites) {
            siteList.appendChild(el("div", { class: "mono regression-site", text: site }));
          }
          item.appendChild(sitesWrap);
        }
        value.appendChild(item);
      }
      const relationRow = el("div", { class: "property-row fact task-delivery-row relation" }, [
        el("span", { class: "label", text: label }),
        value,
      ]);
      relationRow.dataset.relation = key;
      return relationRow;
    }
    targets = entries.map((entry) => entry.target);
  }
  const value = el("span", { class: "value task-delivery-relations" });
  targets.forEach((target, index) => {
    const id = regressionTargetId(target);
    if (index > 0) value.append(", ");
    const open = el("button", { class: "mono task-delivery-relation", type: "button", text: id, title: `Open ${id}` });
    open.addEventListener("click", (e) => {
      e.stopPropagation();
      openVisibleTask(id, context);
    });
    value.appendChild(open);
  });
  const relationRow = el("div", { class: "property-row fact task-delivery-row relation" }, [
    el("span", { class: "label", text: label }),
    value,
  ]);
  relationRow.dataset.relation = key;
  return relationRow;
}
