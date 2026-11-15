// Siblings of one documented semantic kind share padding, font size and weight.
//
// A tag plus the first class is only a candidate cluster. It is not identity:
// experiment badges share `.ex-badge` and then a role the stylesheet documents
// (status, mode, verdict). A verdict's font-weight 600 is that role, not a
// spacing defect. Selected state is exempt and is not itself a kind.
// A cluster that shares a shell but diverges by an undocumented class is
// ambiguous: the rule records it and does not fail it, and it does not skip
// the rest of the view.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.sibling-spacing",
  title: "Siblings of one kind share spacing and type",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const measured = await measureInPage(ctx, (H) => {
    const MIN_PEERS = 3;
    // Whole class tokens only. `active-triage-chip` is not selected, and `on`
    // is not a dashboard state class.
    const STATE_TOKENS = new Set(["active", "selected"]);

    function tokensOf(el) {
      if (typeof el.className !== "string") return [];
      return el.className.trim().split(/\s+/).filter(Boolean);
    }

    // aria-selected and aria-pressed count only when true. aria-current is
    // any presence, matching the attribute the rule already honored.
    function selectedState(el) {
      if (el.getAttribute("aria-selected") === "true") return "aria-selected";
      if (el.getAttribute("aria-pressed") === "true") return "aria-pressed";
      if (el.hasAttribute("aria-current")) return "aria-current";
      for (const token of tokensOf(el)) if (STATE_TOKENS.has(token)) return token;
      return "";
    }

    // experiments.css: status, mode and verdict are roles on `.ex-badge`.
    // `ex-status-running` and `ex-status-paused` are one role. `ex-verdict`
    // and `ex-verdict-*` are the verdict role, which sets font-weight 600.
    function semanticKind(token) {
      if (token.startsWith("ex-status-")) return "status";
      if (token.startsWith("ex-mode-")) return "mode";
      if (token === "ex-verdict" || token.startsWith("ex-verdict-")) return "verdict";
      return "";
    }

    function profileOf(el) {
      const tokens = tokensOf(el);
      const kinds = [];
      const shell = [];
      for (const token of tokens) {
        const kind = semanticKind(token);
        if (kind) {
          if (!kinds.includes(kind)) kinds.push(kind);
        } else shell.push(token);
      }
      kinds.sort();
      shell.sort();
      return { tokens, kinds, shell };
    }

    function measuredItem(el) {
      const style = getComputedStyle(el);
      return {
        ...H.elementFacts(el),
        padding: style.padding,
        font_size: style.fontSize,
        font_weight: style.fontWeight,
      };
    }

    const groups = [];
    const coverage = [];

    for (const parent of H.all("*")) {
      const children = [...parent.children].filter((child) => H.visible(child));
      if (children.length < MIN_PEERS) continue;
      const clusters = new Map();
      for (const child of children) {
        const tokens = tokensOf(child);
        const bucket = child.tagName + "." + (tokens[0] ?? "");
        if (!clusters.has(bucket)) clusters.set(bucket, []);
        clusters.get(bucket).push(child);
      }
      for (const [bucket, members] of clusters) {
        if (members.length < MIN_PEERS) continue;
        const parentSelector = H.selectorOf(parent);
        const exempt = [];
        const rest = [];
        for (const el of members) {
          const state = selectedState(el);
          if (state) exempt.push({ selector: H.selectorOf(el), state });
          else rest.push(el);
        }
        if (exempt.length > 0) {
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "state-exempt",
            members: exempt.slice(0, 12),
          });
        }
        if (rest.length < MIN_PEERS) continue;

        const profiles = rest.map((el) => ({ el, ...profileOf(el) }));
        const shellKey = (item) => item.shell.join(".");
        const shells = new Set(profiles.map(shellKey));
        const kindKeys = profiles.map((item) => item.kinds.join("+"));
        const sameKind = shells.size === 1 && new Set(kindKeys).size === 1;
        const documentedSplit = shells.size === 1
          && profiles.every((item) => item.kinds.length === 1)
          && new Set(kindKeys).size > 1;

        if (sameKind) {
          const kind = profiles[0].kinds[0] || shellKey(profiles[0]) || bucket;
          groups.push({
            parent: parentSelector,
            kind,
            bucket,
            items: profiles.slice(0, 12).map((item) => measuredItem(item.el)),
          });
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "compared",
            kind,
            count: profiles.length,
          });
        } else if (documentedSplit) {
          const byKind = new Map();
          for (const item of profiles) {
            const kind = item.kinds[0];
            if (!byKind.has(kind)) byKind.set(kind, []);
            byKind.get(kind).push(item);
          }
          const kinds = [];
          for (const [kind, items] of byKind) {
            kinds.push({ kind, count: items.length, compared: items.length >= MIN_PEERS });
            if (items.length < MIN_PEERS) continue;
            groups.push({
              parent: parentSelector,
              kind,
              bucket,
              items: items.slice(0, 12).map((item) => measuredItem(item.el)),
            });
          }
          coverage.push({ parent: parentSelector, bucket, coverage: "distinct-kind", kinds });
        } else {
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "ambiguous",
            members: profiles.slice(0, 12).map((item) => ({
              selector: H.selectorOf(item.el),
              tokens: item.tokens,
              kinds: item.kinds,
              shell: item.shell,
            })),
          });
        }
        if (groups.length >= 40) return { groups, coverage: coverage.slice(0, 40) };
      }
    }
    return { groups, coverage: coverage.slice(0, 40) };
  });

  let matched = 0;
  const failures = [];
  for (const group of measured.groups) {
    matched += group.items.length;
    const modal = (key) => [...group.items].sort((a, b) =>
      group.items.filter((item) => item[key] === b[key]).length
      - group.items.filter((item) => item[key] === a[key]).length)[0][key];
    for (const key of ["padding", "font_size", "font_weight"]) {
      const expected = modal(key);
      for (const item of group.items) {
        if (item[key] === expected) continue;
        failures.push({
          selector: item.selector,
          element: item.element,
          discriminator: key,
          parent: group.parent,
          kind: group.kind,
          property: key,
          value: item[key],
          sibling_value: expected,
        });
      }
    }
  }
  return verdict(ctx, rule, matched, failures, { coverage: measured.coverage },
    measured.groups.flatMap((group) => group.items), ["padding", "font_size", "font_weight"]);
}
